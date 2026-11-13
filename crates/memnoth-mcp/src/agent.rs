//! Agent-mode bridge (strategy §4 P1.2): lets OTHER agents drive memnoth
//! sessions as MCP tools over the versioned harness API socket.
//!
//! Tool semantics mirror Session C's scenario contract exactly: prompts submit
//! like typing plus Enter, waits are conditions with deadlines polled every
//! 50ms (no blind sleeps — timeouts return structured results the model can
//! react to), and event tails carry the same lifecycle vocabulary
//! (`text_delta`, `tool_done`, `turn_done`, ...) as the harness API stream.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use memnoth_harness_api::{ApiEvent, ApiRequest, ClientFrame, ServerFrame, api_socket_path};
use memnoth_transport::Stream;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSCRIPT_CAP: usize = 256 * 1024;
const DEFAULT_TAIL_N: usize = 20;

pub const AGENT_TOOLS: &[&str] = &[
    "agent_session_new",
    "agent_session_close",
    "agent_prompt",
    "agent_wait",
    "agent_events",
    "agent_set_model",
];

/// Idle time after which an abandoned session connection is reaped.
const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(30 * 60);

/// How often the reaper sweeps for idle sessions: a quarter of the TTL,
/// clamped so tiny test TTLs do not spin and huge TTLs still get swept.
fn sweep_interval(ttl: Duration) -> Duration {
    (ttl / 4).clamp(Duration::from_millis(100), Duration::from_secs(60))
}

/// Entry point for one harness API endpoint. Connections are lazy: one is
/// opened per created session and owned by its collector task.
pub struct AgentBridge {
    api_socket: std::path::PathBuf,
    sessions: Mutex<HashMap<String, Arc<SessionHandle>>>,
    ttl: Duration,
    /// Current model name, updated via agent_set_model.
    model: Mutex<Option<String>>,
}

impl AgentBridge {
    /// Open a bridge to the memnoth API socket, using the default session TTL.
    pub fn new(api_socket: std::path::PathBuf) -> Self {
        Self {
            api_socket,
            sessions: Mutex::new(HashMap::new()),
            ttl: session_ttl_from_env(),
            model: Mutex::new(None),
        }
    }

    /// Open a bridge using the API socket discovered from the environment
    /// (`MEMNOTH_API_SOCKET`, falling back to the standard memnoth runtime
    /// socket path).
    pub fn from_env() -> Self {
        Self::new(api_socket_path())
    }

    /// Test/registry seam: explicit TTL instead of the env default.
    pub fn with_ttl(api_socket: std::path::PathBuf, ttl: Duration) -> Self {
        Self {
            api_socket,
            sessions: Mutex::new(HashMap::new()),
            ttl,
            model: Mutex::new(None),
        }
    }

    /// Start the background reaper for this bridge. Called once by the HTTP
    /// server after the bridge is constructed; sweeps on an interval derived
    /// from the TTL.
    pub fn spawn_reaper(self: &Arc<Self>) {
        let bridge = Arc::clone(self);
        tokio::spawn(async move {
            let interval = sweep_interval(bridge.ttl);
            loop {
                tokio::time::sleep(interval).await;
                bridge.reap_idle();
            }
        });
    }

    /// Close every session whose connection has been idle longer than the
    /// TTL. Reaping is loud, not silent: a lifecycle entry lands in the
    /// session's ring log (still readable via `agent_events`) before the
    /// connection is dropped, and later calls fail with a pointer to why.
    pub(crate) fn reap_idle(&self) {
        let now = Instant::now();
        let candidates: Vec<(String, Arc<SessionHandle>, Duration)> = {
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            sessions
                .iter()
                .filter_map(|(id, handle)| {
                    if handle.is_closed() {
                        return None;
                    }
                    let idle = now.duration_since(handle.last_activity());
                    if idle > self.ttl {
                        Some((id.clone(), Arc::clone(handle), idle))
                    } else {
                        None
                    }
                })
                .collect()
        };
        for (_id, handle, idle) in candidates {
            let reason = format!(
                "reaped after {}ms idle (MEMNOTH_MCP_AGENT_TTL_MS={})",
                idle.as_millis(),
                self.ttl.as_millis()
            );
            handle.close(Some(reason));
        }
    }

    /// Dispatch one `agent_*` tools/call. Returns the JSON result payload or
    /// an error rendered as `isError` content by the caller.
    pub async fn call(&self, name: &str, args: &Value) -> Result<Value> {
        match name {
            "agent_session_new" => self.session_new(args).await,
            "agent_session_close" => self.session_close(args).await,
            "agent_prompt" => self.prompt(args).await,
            "agent_wait" => self.wait(args).await,
            "agent_events" => self.events_tail(args),
            "agent_set_model" => self.set_model(args).await,
            other => anyhow::bail!("unknown agent tool: {other}"),
        }
    }

    async fn session_handle(&self, session: &str) -> Result<Arc<SessionHandle>> {
        let handle = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown agent session: {session}"))?;
        if let Some(reason) = handle.close_reason() {
            anyhow::bail!(
                "session {session} is no longer active ({reason}); its final events are still \
                 readable with agent_events, or start a new session with agent_session_new"
            );
        }
        Ok(handle)
    }

    async fn session_close(&self, args: &Value) -> Result<Value> {
        let session = args
            .get("session")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string argument: session"))?
            .to_string();
        let handle = {
            self.sessions
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&session)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown agent session: {session}"))?
        };
        if let Some(reason) = handle.close_reason() {
            return Ok(json!({
                "session_id": session,
                "closed": true,
                "already": reason,
            }));
        }
        handle.close(Some("closed via agent_session_close".to_string()));
        Ok(json!({
            "session_id": session,
            "closed": true,
            "note": "final events remain readable via agent_events",
        }))
    }

    async fn session_new(&self, args: &Value) -> Result<Value> {
        let cwd = arg_string(args, "cwd")?;
        let cols = arg_u16(args, "cols").unwrap_or(120);
        let rows = arg_u16(args, "rows").unwrap_or(40);

        let stream = Stream::connect(&self.api_socket).await.context(format!(
            "connecting to harness API socket {}",
            self.api_socket.display()
        ))?;
        let (reader, writer) = stream.into_split();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(32);
        let events = Arc::new(Mutex::new(EventLog::default()));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let activity = Arc::new(Mutex::new(Instant::now()));
        tokio::spawn(session_task(
            reader,
            writer,
            cmd_rx,
            Arc::clone(&events),
            pending,
            Arc::clone(&activity),
        ));

        let handle = Arc::new(SessionHandle {
            cmd_tx: Mutex::new(Some(cmd_tx)),
            events,
            closed: Mutex::new(None),
            activity,
            cols,
            rows,
        });
        let hello = handle
            .request(ApiRequest::Hello {
                min_version: memnoth_harness_api::API_VERSION_MAJOR,
                max_version: memnoth_harness_api::API_VERSION_MAJOR,
                client: "memnoth-mcp-agent".to_string(),
            })
            .await?;
        ensure_ok_frame(&hello, "handshake").context("handshake failed")?;
        let attached = handle
            .request(ApiRequest::CreateSession {
                working_dir: Some(cwd.clone()),
            })
            .await?;
        let session_id = match attached.event {
            ApiEvent::Attached { session } => session.session_id,
            ApiEvent::Error { message, .. } => anyhow::bail!("CreateSession failed: {message}"),
            other => anyhow::bail!("unexpected reply to CreateSession: {}", event_kind(&other)),
        };

        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(session_id.clone(), Arc::clone(&handle));

        Ok(json!({
            "session_id": session_id,
            "cwd": cwd,
            "grid": {"cols": cols, "rows": rows},
            "note": "drive it with agent_prompt; observe with agent_wait/agent_events",
        }))
    }

    async fn prompt(&self, args: &Value) -> Result<Value> {
        let session = arg_string(args, "session")?;
        let text = arg_string(args, "text")?;
        let handle = self.session_handle(&session).await?;
        let reply = handle
            .request(ApiRequest::SendMessage {
                session_id: session.clone(),
                content: text,
                images: vec![],
                no_reply: false,
            })
            .await?;
        ensure_ok_frame(&reply, "SendMessage")?;
        Ok(json!({
            "session_id": session,
            "submitted": true,
            "hint": "use agent_wait(text | stable_ms) before reading agent_events",
        }))
    }

    async fn wait(&self, args: &Value) -> Result<Value> {
        let session = arg_string(args, "session")?;
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(5000);
        let timeout = Duration::from_millis(timeout_ms);
        let handle = self.session_handle(&session).await?;

        if let Some(text) = args.get("text").and_then(Value::as_str) {
            let started = Instant::now();
            loop {
                let hit = {
                    let log = handle.events.lock().unwrap_or_else(|p| p.into_inner());
                    log.transcript.contains(text)
                };
                if hit {
                    return Ok(json!({
                        "condition": "text",
                        "needle": text,
                        "found": true,
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "transcript_tail": handle.transcript_tail(),
                    }));
                }
                if started.elapsed() >= timeout {
                    return Ok(json!({
                        "condition": "text",
                        "needle": text,
                        "found": false,
                        "timed_out": true,
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "transcript_tail": handle.transcript_tail(),
                    }));
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }

        if let Some(stable_ms) = args.get("stable_ms").and_then(Value::as_u64) {
            let stable_for = Duration::from_millis(stable_ms.max(1));
            let started = Instant::now();
            let mut last_change = Instant::now();
            loop {
                tokio::time::sleep(POLL_INTERVAL).await;
                let changed_recently = {
                    let log = handle.events.lock().unwrap_or_else(|p| p.into_inner());
                    log.last_change.map(|t| t > last_change).unwrap_or(false)
                };
                if changed_recently {
                    last_change = Instant::now();
                }
                if last_change.elapsed() >= stable_for {
                    return Ok(json!({
                        "condition": "stable",
                        "stable": true,
                        "stable_ms": stable_ms,
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "transcript_tail": handle.transcript_tail(),
                    }));
                }
                if started.elapsed() >= timeout {
                    return Ok(json!({
                        "condition": "stable",
                        "stable": false,
                        "timed_out": true,
                        "stable_ms": stable_ms,
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "transcript_tail": handle.transcript_tail(),
                    }));
                }
            }
        }

        anyhow::bail!("agent_wait requires either 'text' or 'stable_ms' plus 'timeout_ms'")
    }

    fn events_tail(&self, args: &Value) -> Result<Value> {
        let session = args
            .get("session")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string argument: session"))?
            .to_string();
        let tail_n = args
            .get("tail_n")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_TAIL_N)
            .clamp(1, 1000);
        let handle = {
            self.sessions
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&session)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown agent session: {session}"))?
        };
        let log = handle.events.lock().unwrap_or_else(|p| p.into_inner());
        let start = log.entries.len().saturating_sub(tail_n);
        let events: Vec<Value> = log.entries[start..].iter().map(event_summary).collect();
        Ok(json!({
            "session_id": session,
            "grid": {"cols": handle.cols, "rows": handle.rows},
            "status": log.status.as_deref().unwrap_or("unknown"),
            "transcript_chars": log.transcript.len(),
            "events": events,
        }))
    }

    async fn set_model(&self, args: &Value) -> Result<Value> {
        let session = arg_string(args, "session")?;
        let provider = args
            .get("provider")
            .and_then(Value::as_str)
            .map(str::to_string);
        let model = arg_string(args, "model")?;
        let handle = self.session_handle(&session).await?;

        let candidate = match &provider {
            Some(p) if !model.contains('/') => format!("{p}/{model}"),
            _ => model.clone(),
        };
        let reply = handle
            .request(ApiRequest::SetModel {
                session_id: session.clone(),
                model: candidate.clone(),
            })
            .await;
        let effective = match reply {
            Ok(frame) => {
                ensure_ok_frame(&frame, "SetModel")?;
                candidate
            }
            Err(_) => {
                // The daemon may key models bare ("claude-opus-5") rather than
                // provider-prefixed; retry the literal model once before failing.
                let reply = handle
                    .request(ApiRequest::SetModel {
                        session_id: session.clone(),
                        model: model.clone(),
                    })
                    .await?;
                ensure_ok_frame(&reply, "SetModel")?;
                model.clone()
            }
        };
        *self.model.lock().unwrap_or_else(|p| p.into_inner()) = Some(effective.clone());

        Ok(json!({
            "session_id": session,
            "model_set": effective,
            "requested": {"provider": provider, "model": model},
        }))
    }
}

struct SessionHandle {
    /// Taken (`None`) when the session is closed or reaped, which drops the
    /// connection: the collector task sees its command channel close and ends.
    cmd_tx: Mutex<Option<mpsc::Sender<Cmd>>>,
    events: Arc<Mutex<EventLog>>,
    /// `Some(reason)` once closed via agent_session_close or reaped for idle.
    closed: Mutex<Option<String>>,
    activity: Arc<Mutex<Instant>>,
    cols: u16,
    rows: u16,
}

impl SessionHandle {
    fn last_activity(&self) -> Instant {
        *self.activity.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn touch(&self) {
        *self.activity.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
    }

    fn is_closed(&self) -> bool {
        self.closed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    fn close_reason(&self) -> Option<String> {
        self.closed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Emit a lifecycle notice into the ring log, mark the session closed and
    /// drop its connection. The log stays readable through `agent_events`.
    fn close(&self, reason: Option<String>) {
        let reason = reason.unwrap_or_else(|| "closed".to_string());
        {
            let mut log = self.events.lock().unwrap_or_else(|p| p.into_inner());
            log.entries.push(Entry {
                at: Instant::now(),
                source: EntrySource::Notice(reason.clone()),
            });
            log.status = Some(if reason.starts_with("reaped") {
                "reaped".to_string()
            } else {
                "closed".to_string()
            });
            log.last_change = Some(Instant::now());
        }
        *self.closed.lock().unwrap_or_else(|p| p.into_inner()) = Some(reason);
        self.cmd_tx.lock().unwrap_or_else(|p| p.into_inner()).take();
    }

    async fn request(&self, req: ApiRequest) -> Result<ServerFrame> {
        let sender = {
            let guard = self.cmd_tx.lock().unwrap_or_else(|p| p.into_inner());
            guard
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session connection closed"))?
        };
        self.touch();
        let (tx, rx) = oneshot::channel();
        sender
            .send(Cmd::Request(req, tx))
            .await
            .context("session connection closed")?;
        let frame = tokio::time::timeout(REQUEST_TIMEOUT, rx)
            .await
            .context("harness API request timed out")?
            .context("session collector dropped the reply")??;
        Ok(frame)
    }

    fn transcript_tail(&self) -> String {
        let log = self.events.lock().unwrap_or_else(|p| p.into_inner());
        let len = log.transcript.len();
        if len <= 4000 {
            log.transcript.clone()
        } else {
            // `len - 4000` is a *byte* offset and a transcript full of prose will have
            // multi-byte characters (curly quotes, em-dashes) at some of them, which makes
            // `&transcript[len - 4000..]` panic. Step forward to the next char boundary.
            let mut start = len - 4000;
            while start < len && !log.transcript.is_char_boundary(start) {
                start += 1;
            }
            log.transcript[start..].to_string()
        }
    }
}

enum Cmd {
    Request(ApiRequest, oneshot::Sender<anyhow::Result<ServerFrame>>),
}

#[derive(Default)]
struct EventLog {
    entries: Vec<Entry>,
    transcript: String,
    status: Option<String>,
    last_change: Option<Instant>,
}

enum EntrySource {
    Frame(ApiEvent),
    Notice(String),
}

struct Entry {
    at: Instant,
    source: EntrySource,
}

async fn session_task(
    reader: impl tokio::io::AsyncRead + Unpin,
    mut writer: impl tokio::io::AsyncWrite + Unpin,
    mut cmd_rx: mpsc::Receiver<Cmd>,
    events: Arc<Mutex<EventLog>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<anyhow::Result<ServerFrame>>>>>,
    activity: Arc<Mutex<Instant>>,
) {
    let mut reader = BufReader::new(reader);
    let mut next_id: u64 = 1;
    loop {
        let write_result = tokio::select! {
            biased;
            cmd = cmd_rx.recv() => match cmd {
                None => break,
                Some(Cmd::Request(req, cb)) => {
                    let id = next_id;
                    next_id += 1;
                    pending.lock().unwrap_or_else(|p| p.into_inner()).insert(id, cb);
                    write_client_frame(&mut writer, &ClientFrame::new(id, req)).await
                }
            },
            frame = read_server_frame(&mut reader) => match frame {
                Ok(None) => break,
                Ok(Some(server_frame)) => {
                    *activity.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
                    match server_frame.reply_to {
                        Some(id) => {
                            if let Some(cb) = pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&id) {
                                let _ = cb.send(Ok(server_frame));
                            }
                        }
                        None => {
                            let mut log = events.lock().unwrap_or_else(|p| p.into_inner());
                            if let ApiEvent::SessionStatus { status, .. } = &server_frame.event {
                                log.status = Some(status.clone());
                            }
                            if let ApiEvent::TextDelta { text, .. } = &server_frame.event {
                                log.transcript.push_str(text);
                                if log.transcript.len() > TRANSCRIPT_CAP {
                                    let cut = log.transcript.len() - TRANSCRIPT_CAP;
                                    let boundary = (cut..log.transcript.len())
                                        .find(|i| log.transcript.is_char_boundary(*i))
                                        .unwrap_or(log.transcript.len());
                                    log.transcript.drain(..boundary);
                                }
                            }
                            log.last_change = Some(Instant::now());
                            log.entries.push(Entry {
                                at: Instant::now(),
                                source: EntrySource::Frame(server_frame.event),
                            });
                            if log.entries.len() > EVENT_LOG_CAP {
                                log.entries.remove(0);
                            }
                        }
                    }
                    continue;
                }
                Err(e) => {
                    let message = format!("harness API stream failed: {e:#}");
                    let pendings: HashMap<u64, oneshot::Sender<anyhow::Result<ServerFrame>>> =
                        std::mem::take(&mut *pending.lock().unwrap_or_else(|p| p.into_inner()));
                    for (_, cb) in pendings {
                        let _ = cb.send(Err(anyhow::anyhow!("{message}")));
                    }
                    return;
                }
            },
        };
        if write_result.is_err() {
            let pendings: HashMap<u64, oneshot::Sender<anyhow::Result<ServerFrame>>> =
                std::mem::take(&mut *pending.lock().unwrap_or_else(|p| p.into_inner()));
            for (_, cb) in pendings {
                let _ = cb.send(Err(anyhow::anyhow!("harness API stream write failed")));
            }
            return;
        }
    }
    let pendings: HashMap<u64, oneshot::Sender<anyhow::Result<ServerFrame>>> =
        std::mem::take(&mut *pending.lock().unwrap_or_else(|p| p.into_inner()));
    for (_, cb) in pendings {
        let _ = cb.send(Err(anyhow::anyhow!("harness API connection closed")));
    }
}

const EVENT_LOG_CAP: usize = 2000;

async fn write_client_frame<W: tokio::io::AsyncWrite + Unpin, T: serde::Serialize>(
    writer: &mut W,
    frame: &T,
) -> Result<()> {
    let mut line = serde_json::to_string(frame)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_frame_async<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: tokio::io::AsyncBufRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        if line.trim().is_empty() {
            continue;
        }
        return Ok(Some(serde_json::from_str(line.trim())?));
    }
}

async fn read_server_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<ServerFrame>> {
    read_frame_async(reader).await
}

fn ensure_ok_frame(frame: &ServerFrame, what: &str) -> Result<()> {
    match &frame.event {
        ApiEvent::Ok | ApiEvent::HelloOk { .. } => Ok(()),
        ApiEvent::Error { message, .. } => {
            anyhow::bail!("{what} failed: {message}")
        }
        other => anyhow::bail!("unexpected reply to {what}: {}", event_kind(other)),
    }
}

fn event_kind(event: &ApiEvent) -> &'static str {
    match event {
        ApiEvent::HelloOk { .. } => "hello_ok",
        ApiEvent::Ok => "ok",
        ApiEvent::Error { .. } => "error",
        ApiEvent::Sessions { .. } => "sessions",
        ApiEvent::Attached { .. } => "attached",
        ApiEvent::History { .. } => "history",
        ApiEvent::Pong => "pong",
        ApiEvent::TextDelta { .. } => "text_delta",
        ApiEvent::ReasoningDelta { .. } => "reasoning_delta",
        ApiEvent::ReasoningDone { .. } => "reasoning_done",
        ApiEvent::ToolStart { .. } => "tool_start",
        ApiEvent::ToolInputDelta { .. } => "tool_input_delta",
        ApiEvent::ToolExec { .. } => "tool_exec",
        ApiEvent::ToolDone { .. } => "tool_done",
        ApiEvent::TokenUsage { .. } => "token_usage",
        ApiEvent::TurnDone { .. } => "turn_done",
        ApiEvent::BackgroundProgress { .. } => "background_progress",
        ApiEvent::MessageAccepted { .. } => "message_accepted",
        ApiEvent::PermissionRequest { .. } => "permission_request",
        ApiEvent::SessionStatus { .. } => "session_status",
        ApiEvent::ConnectionPhase { .. } => "connection_phase",
        ApiEvent::ModelInfo { .. } => "model_info",
        ApiEvent::Models { .. } => "models",
        ApiEvent::RuntimeInfo { .. } => "runtime_info",
        ApiEvent::CredentialUpdated { .. } => "credential_updated",
        ApiEvent::FileContent { .. } => "file_content",
        ApiEvent::Files { .. } => "files",
        ApiEvent::TextMatches { .. } => "text_matches",
        ApiEvent::FileStatus { .. } => "file_status",
        ApiEvent::Compacted => "compacted",
        ApiEvent::SessionClosed { .. } => "session_closed",
        ApiEvent::SessionRenamed { .. } => "session_renamed",
        ApiEvent::Unknown => "unknown",
        ApiEvent::SuiteStarted { .. } => "suite_started",
        ApiEvent::SuiteProgress { .. } => "suite_progress",
        ApiEvent::TaskVerified { .. } => "task_verified",
        ApiEvent::LeakageFlag { .. } => "leakage_flag",
        ApiEvent::SuiteCompleted { .. } => "suite_completed",
        ApiEvent::SuiteResult { .. } => "suite_result",
    }
}

fn event_summary(entry: &Entry) -> Value {
    let (kind, detail) = match &entry.source {
        EntrySource::Notice(notice) => ("lifecycle", Some(notice.clone())),
        EntrySource::Frame(event) => {
            let kind = event_kind(event);
            let detail = match event {
                ApiEvent::TextDelta { text, .. } => Some(text.clone()),
                ApiEvent::ToolDone { name, output, .. } => {
                    Some(format!("{name}: {}", truncate(output, 300)))
                }
                ApiEvent::ToolStart { name, .. } | ApiEvent::ToolExec { name, .. } => {
                    Some(name.clone())
                }
                ApiEvent::SessionStatus { status, .. } => Some(status.clone()),
                ApiEvent::ConnectionPhase { phase, .. } => Some(phase.clone()),
                ApiEvent::ModelInfo {
                    provider, model, ..
                } => Some(format!("{}/{}", provider.as_str(), model.as_str())),
                ApiEvent::PermissionRequest {
                    tool_name,
                    description,
                    ..
                } => Some(format!("{tool_name}: {description}")),
                _ => None,
            };
            (kind, detail)
        }
    };
    let mut out = json!({
        "kind": kind,
        "age_ms": entry.at.elapsed().as_millis() as u64,
    });
    if let Some(detail) = detail {
        out["detail"] = Value::String(truncate(&detail, 300));
    }
    out
}

fn truncate(input: &str, max: usize) -> String {
    if input.len() <= max {
        input.to_string()
    } else {
        let mut end = max;
        while end > 0 && !input.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &input[..end])
    }
}

fn arg_string(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("missing string argument: {key}"))
}

fn arg_u16(args: &Value, key: &str) -> Option<u16> {
    args.get(key).and_then(Value::as_u64).map(|v| v as u16)
}

/// Idle TTL after which abandoned session connections are reaped.
/// `MEMNOTH_MCP_AGENT_TTL_MS` overrides; 0 falls back to the default.
fn session_ttl_from_env() -> Duration {
    let ms = std::env::var("MEMNOTH_MCP_AGENT_TTL_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_SESSION_TTL.as_millis() as u64);
    Duration::from_millis(ms)
}

#[cfg(test)]
pub(crate) mod tests;
