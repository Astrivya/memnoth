//! How a message reaches a server: stdio, or Streamable HTTP.
//!
//! The trait exists so the client's logic (discovery, pagination, cache, error shapes) is testable
//! without a child process or a socket. The two real transports are deliberately thin: framing,
//! deadlines, and the `MCP-Protocol-Version` header — nothing that belongs to the client.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// One request/response exchange. The stateless revision has no
/// `notifications/initialized` to send, so there is no notify path.
pub trait Transport: Send {
    /// Send one message and wait at most `timeout` for its reply.
    fn round_trip(&mut self, message: &Value, timeout: Duration) -> Result<Value>;

    /// Where this transport goes, for a diagnostic line.
    fn describe(&self) -> String;

    /// The last lines the server wrote to stderr. Empty when the transport has no stderr.
    fn stderr_tail(&self) -> Vec<String> {
        Vec::new()
    }
}

fn reply_id(message: &Value) -> Result<u64> {
    match message.get("id").and_then(Value::as_u64) {
        Some(id) => Ok(id),
        None => bail!("a request needs a numeric id: {message}"),
    }
}

/// A server that can answer anything gets one deadline per request, not one per session.
fn expired(id: u64, timeout: Duration) -> anyhow::Error {
    anyhow::anyhow!("timed out after {timeout:?} waiting for the reply to request {id}")
}

// ---------------------------------------------------------------- stdio

/// A child process speaking newline-delimited JSON-RPC on its stdin/stdout.
pub struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<Result<Value, String>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    stashed: HashMap<u64, Value>,
    label: String,
}

/// How many stderr lines are kept for a diagnostic. A server that logs a thousand lines per call
/// is exactly the one whose last twenty matter.
const STDERR_TAIL: usize = 64;

impl StdioTransport {
    /// Start `command` and wire its streams.
    ///
    /// `env` is additive: the child inherits the parent's environment, and a configured key wins.
    /// That is what lets a user keep `PATH` and still point one server at a different store.
    pub fn spawn(command: &str, args: &[String], env: &BTreeMap<String, String>) -> Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot start `{command}`"))?;

        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => bail!("`{command}` has no stdin to write to"),
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => BufReader::new(stdout),
            None => bail!("`{command}` has no stdout to read"),
        };

        let (tx, replies) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in stdout.lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(error) => {
                        // The client hung up or the pipe broke; either way this thread is done.
                        drop(tx.send(Err(format!("cannot read stdout: {error}"))));
                        return;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                let message = match serde_json::from_str::<Value>(&line) {
                    Ok(message) => message,
                    Err(_) => {
                        // A server that prints logs on stdout breaks the framing; the line is
                        // reported as-is so the diagnosis is the server's own text.
                        drop(tx.send(Err(line)));
                        continue;
                    }
                };
                if tx.send(Ok(message)).is_err() {
                    return;
                }
            }
        });

        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stream) = child.stderr.take() {
            let tail = Arc::clone(&stderr);
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    let mut tail = match tail.lock() {
                        Ok(tail) => tail,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    if tail.len() == STDERR_TAIL {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            });
        }

        Ok(Self {
            child,
            stdin,
            replies,
            stderr,
            stashed: HashMap::new(),
            label: command.to_string(),
        })
    }

    fn write_line(&mut self, message: &Value) -> Result<()> {
        let mut line = serde_json::to_string(message).context("cannot serialise a request")?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .with_context(|| format!("cannot write to `{}`", self.label))
    }
}

impl Transport for StdioTransport {
    fn round_trip(&mut self, message: &Value, timeout: Duration) -> Result<Value> {
        let id = reply_id(message)?;
        // A reply that arrived while another request was waiting is an answer, not noise.
        if let Some(reply) = self.stashed.remove(&id) {
            return Ok(reply);
        }
        self.write_line(message)?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = match deadline.checked_duration_since(Instant::now()) {
                Some(remaining) => remaining,
                None => return Err(expired(id, timeout)),
            };
            match self.replies.recv_timeout(remaining) {
                Ok(Ok(value)) => match value.get("id").and_then(Value::as_u64) {
                    Some(other) if other == id => return Ok(value),
                    // A reply to something else: keep it, so a reordered answer is not lost.
                    Some(other) => {
                        self.stashed.insert(other, value);
                    }
                    // A notification, which has no id and no reply.
                    None => continue,
                },
                Ok(Err(line)) => {
                    bail!("`{}` wrote a line that is not JSON-RPC: {line}", self.label)
                }
                Err(RecvTimeoutError::Timeout) => return Err(expired(id, timeout)),
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("`{}` closed its stdout", self.label)
                }
            }
        }
    }

    fn describe(&self) -> String {
        format!("stdio:{}", self.label)
    }

    fn stderr_tail(&self) -> Vec<String> {
        match self.stderr.lock() {
            Ok(tail) => tail.iter().cloned().collect(),
            Err(poisoned) => poisoned.into_inner().iter().cloned().collect(),
        }
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // A server that outlives the client is a leaked process on someone's machine. A kill that
        // fails means it already exited — the normal case on the error path — so only an
        // unexpected failure is reported.
        if let Err(error) = self.child.kill()
            && error.kind() != std::io::ErrorKind::InvalidInput
        {
            eprintln!(
                "memnoth-mcp-client: could not stop `{}`: {error}",
                self.label
            );
        }
        if let Err(error) = self.child.wait() {
            eprintln!(
                "memnoth-mcp-client: could not reap `{}`: {error}",
                self.label
            );
        }
    }
}

// ---------------------------------------------------------------- Streamable HTTP

/// One POST per message, with a JSON or SSE reply. Stateless: no session id
/// is issued or sent; the named revision travels in the
/// `MCP-Protocol-Version` header on every request.
pub struct HttpTransport {
    client: reqwest::blocking::Client,
    url: String,
    label: String,
}

impl HttpTransport {
    pub fn new(url: impl Into<String>) -> Result<Self> {
        let url = url.into();
        let client = reqwest::blocking::Client::builder()
            .build()
            .context("cannot build the HTTP client")?;
        Ok(Self {
            label: url.clone(),
            client,
            url,
        })
    }

    fn post(&mut self, message: &Value, timeout: Duration) -> Result<reqwest::blocking::Response> {
        let request = self
            .client
            .post(&self.url)
            .timeout(timeout)
            .header("content-type", "application/json")
            // The spec requires both: a server may answer either way, per request.
            .header("accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", crate::protocol::PROTOCOL_VERSION)
            .json(message);
        let response = request
            .send()
            .with_context(|| format!("cannot reach `{}`", self.label))?;
        Ok(response)
    }
}

impl Transport for HttpTransport {
    fn round_trip(&mut self, message: &Value, timeout: Duration) -> Result<Value> {
        let id = reply_id(message)?;
        let response = self.post(message, timeout)?;
        let status = response.status();
        let content_type = match response.headers().get("content-type") {
            Some(value) => match value.to_str() {
                Ok(value) => value.to_string(),
                Err(_) => String::new(),
            },
            None => String::new(),
        };
        let body = response
            .text()
            .with_context(|| format!("cannot read the reply from `{}`", self.label))?;
        if !status.is_success() {
            bail!(
                "`{}` answered {status}: {}",
                self.label,
                first_line_of(&body)
            );
        }
        parse_http_body(&body, &content_type, id)
            .with_context(|| format!("`{}` sent a reply this client cannot read", self.label))
    }

    fn describe(&self) -> String {
        format!("http:{}", self.url)
    }
}

/// Read a reply that may arrive as JSON or as a request-scoped SSE stream.
///
/// Pure, so the two shapes are tested without a server. SSE frames are `data:` lines; the one whose
/// `id` matches is the reply, and any other frame is a notification the caller does not need.
pub fn parse_http_body(body: &str, content_type: &str, id: u64) -> Result<Value> {
    if content_type.contains("text/event-stream") {
        for line in body.lines() {
            let Some(payload) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(payload) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(value);
            }
        }
        bail!("the event stream carried no reply for request {id}");
    }
    serde_json::from_str(body).context("the reply body is not JSON")
}

fn first_line_of(body: &str) -> String {
    let line = match body.lines().next() {
        Some(line) => line.trim(),
        None => "",
    };
    line.chars().take(200).collect()
}
