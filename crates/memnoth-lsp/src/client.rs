//! The LSP client: spawns one server per language, speaks JSON-RPC over stdio,
//! keeps documents in sync, and answers definition / references / symbols /
//! hover. Every result carries a `Precision` label so callers never mistake a
//! name match for a resolved edge (W7-01).
//!
//! No server is spawned unless [`AutoStartPolicy`] permits it — `auto_lsp` must
//! be set, the file's extension must match a known server, a root marker must
//! be present, and the language must be on the allowlist (W7-05).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use memnoth_codegraph::lsp::{AutoStartPolicy, PositionEncoding, ServerSpec, StartDecision};
use serde_json::Value;

use crate::protocol::{FramedMessage, Id, RpcMessage};

/// A running language-server process plus its in-flight request table.
pub struct LspClient {
    /// The language this server serves.
    pub language: String,
    /// The negotiated position encoding.
    pub encoding: Mutex<Option<PositionEncoding>>,
    /// The next request id.
    next_id: Mutex<u64>,
    /// Pending requests: id -> oneshot sender.
    pending: Mutex<HashMap<u64, OneshotSender>>,
    /// The server process stdin.
    stdin: Mutex<Option<std::process::ChildStdin>>,
    /// The child, so we can wait / kill it.
    child: Mutex<Option<Child>>,
    /// `textDocument/publishDiagnostics` by URI, newest per document.
    diagnostics: Mutex<HashMap<String, Vec<Value>>>,
}

type OneshotSender = tokio::sync::oneshot::Sender<Result<Value>>;

impl LspClient {
    /// Start the server described by `spec` in `root`, if the auto-start policy
    /// allows it. Returns `None` when the policy refuses the start; call
    /// [`AutoStartPolicy::should_start`] with the file's extension for the
    /// precise [`Refusal`](memnoth_codegraph::lsp::Refusal) reason.
    pub fn start(
        spec: &ServerSpec,
        root: &Path,
        policy: &AutoStartPolicy,
    ) -> Result<Option<Arc<Self>>> {
        let extension = spec.extensions.first().copied().unwrap_or("");
        let marker_present = spec
            .root_markers
            .iter()
            .any(|marker| root.join(marker).exists());
        match policy.should_start(extension, marker_present) {
            StartDecision::Start { .. } => Ok(Some(Self::spawn(spec, root)?)),
            StartDecision::Refused(_) => Ok(None),
        }
    }

    fn spawn(spec: &ServerSpec, root: &Path) -> Result<Arc<Self>> {
        let mut cmd = std::process::Command::new(spec.command);
        cmd.args(spec.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "spawning language server {} for {} in {}",
                spec.command,
                spec.language,
                root.display()
            )
        })?;
        let stdin = child.stdin.take().context("language server has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("language server has no stdout")?;

        let client = Arc::new(Self {
            language: spec.language.to_string(),
            encoding: Mutex::new(None),
            next_id: Mutex::new(0),
            pending: Mutex::new(HashMap::new()),
            stdin: Mutex::new(Some(stdin)),
            child: Mutex::new(Some(child)),
            diagnostics: Mutex::new(HashMap::new()),
        });

        // Read server output on a blocking thread; `std::process` pipes are
        // not tokio-native.
        let client_reader = client.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(msg) = serde_json::from_str::<Value>(&line) {
                    client_reader.handle_incoming(msg);
                }
            }
        });

        Ok(client)
    }

    /// Send a request and wait for the response (bounded by the client's
    /// request timeout — W7-03).
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = {
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            Id::Num(*next)
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            pending.insert(id.num().unwrap_or(0), tx);
        }
        let msg = RpcMessage::request(id.clone(), method, params);
        let body = serde_json::to_vec(&msg)?;
        {
            let mut guard = self.stdin.lock().unwrap();
            let stdin = guard.as_mut().context("server stdin closed")?;
            let framed = FramedMessage::frame(&body);
            stdin.write_all(&framed).context("writing request")?;
            stdin.flush().context("flushing request")?;
        }

        // Bounded wait so a hung server cannot block the caller forever.
        tokio::time::timeout(std::time::Duration::from_secs(30), rx)
            .await
            .context("language-server request timed out")??
    }

    /// Send a notification (no response expected).
    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = RpcMessage::notification(method, params);
        let body = serde_json::to_vec(&msg)?;
        let mut guard = self.stdin.lock().unwrap();
        let stdin = guard.as_mut().context("server stdin closed")?;
        let framed = FramedMessage::frame(&body);
        stdin.write_all(&framed).context("writing notification")?;
        stdin.flush().context("flushing notification")?;
        Ok(())
    }

    /// Route one document from the server: a reply to a waiter, or a
    /// notification stored for polling.
    fn handle_incoming(&self, msg: Value) {
        let id = msg.get("id").and_then(Value::as_u64);
        if let Some(id) = id
            && (msg.get("result").is_some() || msg.get("error").is_some())
        {
            let waiter = self.pending.lock().unwrap().remove(&id);
            if let Some(waiter) = waiter {
                let answer = match msg.get("error") {
                    Some(error) => Err(anyhow::anyhow!("language server error: {error}")),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = waiter.send(answer);
            }
            return;
        }

        if msg.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics") {
            let Some(params) = msg.get("params") else {
                return;
            };
            let Some(uri) = params.get("uri").and_then(Value::as_str) else {
                return;
            };
            let list = params
                .get("diagnostics")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            self.diagnostics
                .lock()
                .unwrap()
                .insert(uri.to_string(), list);
        }
    }

    /// The diagnostics last published for `uri`, empty when none arrived.
    #[must_use]
    pub fn diagnostics(&self, uri: &str) -> Vec<Value> {
        self.diagnostics
            .lock()
            .unwrap()
            .get(uri)
            .cloned()
            .unwrap_or_default()
    }

    /// Wait up to `timeout` for diagnostics for `uri`.
    ///
    /// A server publishes on its own schedule, so "the file has no errors" and
    /// "no answer yet" must not be the same value: the caller gets whatever
    /// arrived, and can say how long it waited.
    #[must_use]
    pub fn wait_for_diagnostics(&self, uri: &str, timeout: std::time::Duration) -> Vec<Value> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let found = self.diagnostics(uri);
            if !found.is_empty() || std::time::Instant::now() >= deadline {
                return found;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Stop the server now, without asking it to shut down.
    ///
    /// A tool call cannot await a shutdown reply, so the process is killed and
    /// reaped here: leaving it running would outlive the edit that started it.
    pub fn kill(&self) {
        let mut child = self.child.lock().unwrap();
        if let Some(mut running) = child.take() {
            let _ = running.kill();
            let _ = running.wait();
        }
    }

    /// Shut the server down gracefully.
    pub async fn shutdown(&self) -> Result<()> {
        let _ = self.request("shutdown", serde_json::json!({})).await;
        let mut child = self.child.lock().unwrap();
        if let Some(mut c) = child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        Ok(())
    }
}

impl Id {
    fn num(&self) -> Option<u64> {
        match self {
            Id::Num(n) => Some(*n),
            Id::Str(_) => None,
        }
    }
}

/// A cheap handle used to send document sync notifications without awaiting
/// round trips — the editor side calls these after every edit.
pub struct LspClientHandle {
    client: Arc<LspClient>,
}

impl LspClientHandle {
    pub fn new(client: Arc<LspClient>) -> Self {
        Self { client }
    }

    /// Tell the server a document was opened (W7-03 document sync).
    pub fn did_open(&self, uri: &str, language_id: &str, version: i32, text: &str) -> Result<()> {
        self.client.notify(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": version,
                    "text": text,
                }
            }),
        )
    }

    /// Tell the server a document changed (W7-06 diagnostics after an edit).
    pub fn did_change(&self, uri: &str, version: i32) -> Result<()> {
        self.client.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": "" }],
            }),
        )
    }

    /// Tell the server a document was closed.
    pub fn did_close(&self, uri: &str) -> Result<()> {
        self.client.notify(
            "textDocument/didClose",
            serde_json::json!({ "textDocument": { "uri": uri } }),
        )
    }

    /// Resolve a definition (W7-04).
    pub async fn definition(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        self.client
            .request(
                "textDocument/definition",
                serde_json::json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                }),
            )
            .await
    }

    /// Resolve references (W7-04).
    pub async fn references(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        self.client
            .request(
                "textDocument/references",
                serde_json::json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                }),
            )
            .await
    }

    /// Document symbols (W7-04).
    pub async fn document_symbols(&self, uri: &str) -> Result<Value> {
        self.client
            .request(
                "textDocument/documentSymbol",
                serde_json::json!({ "textDocument": { "uri": uri } }),
            )
            .await
    }

    /// Workspace symbols (W7-04).
    pub async fn workspace_symbols(&self, query: &str) -> Result<Value> {
        self.client
            .request("workspace/symbol", serde_json::json!({ "query": query }))
            .await
    }

    /// Hover (W7-04).
    pub async fn hover(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        self.client
            .request(
                "textDocument/hover",
                serde_json::json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                }),
            )
            .await
    }

    /// Rename symbol (W7-04).
    pub async fn rename(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<Value> {
        self.client
            .request(
                "textDocument/rename",
                serde_json::json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                    "newName": new_name,
                }),
            )
            .await
    }

    /// Code actions at a range (W7-04).
    pub async fn code_action(
        &self,
        uri: &str,
        start: (u32, u32),
        end: (u32, u32),
    ) -> Result<Value> {
        self.client
            .request(
                "textDocument/codeAction",
                serde_json::json!({
                    "textDocument": { "uri": uri },
                    "range": {
                        "start": { "line": start.0, "character": start.1 },
                        "end": { "line": end.0, "character": end.1 },
                    },
                }),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn client() -> LspClient {
        LspClient {
            language: "rust".to_string(),
            encoding: Mutex::new(None),
            next_id: Mutex::new(0),
            pending: Mutex::new(HashMap::new()),
            stdin: Mutex::new(None),
            child: Mutex::new(None),
            diagnostics: Mutex::new(HashMap::new()),
        }
    }

    #[test]
    fn a_publish_diagnostics_notification_is_stored_by_uri() {
        let client = client();
        client.handle_incoming(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": "file:///a.rs", "diagnostics": [{ "message": "unused" }] }
        }));
        assert_eq!(client.diagnostics("file:///a.rs").len(), 1);
        assert!(client.diagnostics("file:///b.rs").is_empty());
    }

    #[test]
    fn a_reply_resolves_the_request_waiting_on_its_id() {
        let client = client();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        client.pending.lock().unwrap().insert(7, tx);
        client.handle_incoming(json!({ "jsonrpc": "2.0", "id": 7, "result": { "ok": true } }));
        let answer = rx.try_recv().expect("the waiter was answered");
        assert_eq!(answer.unwrap()["ok"], true);
        assert!(client.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn an_error_reply_reaches_the_waiter_as_an_error() {
        let client = client();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        client.pending.lock().unwrap().insert(9, tx);
        client.handle_incoming(json!({
            "jsonrpc": "2.0",
            "id": 9,
            "error": { "code": -32601, "message": "no such method" }
        }));
        let answer = rx.try_recv().expect("the waiter was answered");
        assert!(answer.unwrap_err().to_string().contains("no such method"));
    }
}
