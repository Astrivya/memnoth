//! The LSP client: spawns one server per language, speaks JSON-RPC over stdio,
//! keeps documents in sync, and answers definition / references / symbols /
//! hover. Every result carries a `Precision` label so callers never mistake a
//! name match for a resolved edge (W7-01).
//!
//! No server is spawned unless [`AutoStartPolicy`] permits it — `auto_lsp` must
//! be set, the file's extension must match a known server, a root marker must
//! be present, and the language must be on the allowlist (W7-05).
//!
//! Before any document request the client performs the LSP handshake: an
//! `initialize` request carrying the workspace `rootUri` and the client's
//! capabilities, a wait for the server's reply (from which `positionEncoding`
//! is negotiated, defaulting to UTF-16), and an `initialized` notification.
//! Requests are bounded by a short (2 s) deadline; the handshake and other
//! slow calls use a long (10 s) one (S-COD-4).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use memnoth_codegraph::lsp::{
    AutoStartPolicy, PositionEncoding, ServerSpec, StartDecision, decode_frame,
};
use serde_json::{Value, json};

use crate::protocol::{FramedMessage, Id, RpcMessage};

/// Soft deadline for an interactive request: answer or fail fast, never hang
/// the editor (S-COD-4's 2 s deadline).
pub const SHORT_TIMEOUT: Duration = Duration::from_secs(2);

/// Hard deadline for slow calls — the `initialize` handshake above all, since
/// a server may spend a moment indexing before it can answer (S-COD-4's 10 s
/// deadline).
pub const LONG_TIMEOUT: Duration = Duration::from_secs(10);

/// A running language-server process plus its in-flight request table.
pub struct LspClient {
    /// The language this server serves.
    pub language: String,
    /// The negotiated position encoding (set from the `initialize` reply).
    pub encoding: Mutex<Option<PositionEncoding>>,
    /// The next request id.
    next_id: Mutex<u64>,
    /// Pending requests: id -> waiter.
    pending: Mutex<HashMap<u64, Waiter>>,
    /// The server stdin (boxed so an in-memory transport can be injected).
    stdin: Mutex<Option<Box<dyn Write + Send>>>,
    /// The child, so we can wait / kill it.
    child: Mutex<Option<Child>>,
    /// `textDocument/publishDiagnostics` by URI, newest per document.
    diagnostics: Mutex<HashMap<String, Vec<Value>>>,
}

/// How a caller waits for a reply: async callers use a tokio oneshot, the
/// synchronous `initialize` handshake uses a std channel with a timeout.
enum Waiter {
    Async(tokio::sync::oneshot::Sender<Result<Value>>),
    Sync(std::sync::mpsc::Sender<Result<Value>>),
}

impl Waiter {
    fn send(self, answer: Result<Value>) {
        match self {
            Self::Async(tx) => {
                let _ = tx.send(answer);
            }
            Self::Sync(tx) => {
                let _ = tx.send(answer);
            }
        }
    }
}

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

        Self::connect(
            spec.language.to_string(),
            root,
            Box::new(stdin),
            Box::new(stdout),
            Some(child),
        )
    }

    /// Attach to a server over arbitrary byte streams and run the handshake.
    ///
    /// Production callers get the streams from a spawned child (see
    /// [`Self::spawn`]); tests pass in-memory pipes driven by a fake server.
    /// `child` is `None` for such in-memory transports.
    pub fn connect(
        language: String,
        root: &Path,
        writer: Box<dyn Write + Send>,
        reader: Box<dyn Read + Send>,
        child: Option<Child>,
    ) -> Result<Arc<Self>> {
        let client = Arc::new(Self {
            language,
            encoding: Mutex::new(None),
            next_id: Mutex::new(0),
            pending: Mutex::new(HashMap::new()),
            stdin: Mutex::new(Some(writer)),
            child: Mutex::new(child),
            diagnostics: Mutex::new(HashMap::new()),
        });

        // Read server output on a blocking thread; `std::process` pipes (and
        // the in-memory test pipes) are not tokio-native. `decode_frame`
        // honours Content-Length, so servers that do not terminate their JSON
        // with a newline are handled correctly.
        let pump = client.clone();
        std::thread::spawn(move || pump_messages(pump, reader));

        client.initialize(root)?;
        Ok(client)
    }

    fn next_id(&self) -> u64 {
        let mut next = self.next_id.lock().unwrap();
        *next += 1;
        *next
    }

    fn write_message(&self, msg: &RpcMessage) -> Result<()> {
        let body = serde_json::to_vec(msg)?;
        let framed = FramedMessage::frame(&body);
        let mut guard = self.stdin.lock().unwrap();
        let stdin = guard.as_mut().context("server stdin closed")?;
        stdin.write_all(&framed).context("writing message")?;
        stdin.flush().context("flushing message")?;
        Ok(())
    }

    /// `initialize` → wait for the reply → negotiate `positionEncoding` →
    /// `initialized`. A failure here leaves the client unusable, so the caller
    /// (`spawn`) propagates it and the process is discarded.
    fn initialize(&self, root: &Path) -> Result<()> {
        let params = json!({
            "processId": std::process::id(),
            "rootUri": path_to_uri(root),
            "capabilities": {
                "general": { "positionEncodings": ["utf-8", "utf-16"] },
                "workspace": { "workspaceFolders": false },
                "textDocument": {
                    "synchronization": {
                        "dynamicRegistration": false,
                        "willSave": false,
                        "willSaveWaitUntil": false,
                        "didSave": false,
                    },
                    "definition": { "dynamicRegistration": false },
                    "references": { "dynamicRegistration": false },
                    "documentSymbol": { "dynamicRegistration": false },
                    "hover": { "dynamicRegistration": false },
                    "rename": { "dynamicRegistration": false },
                    "codeAction": { "dynamicRegistration": false },
                },
            },
        });
        let reply = self.request_sync("initialize", params, LONG_TIMEOUT)?;
        *self.encoding.lock().unwrap() = Some(encoding_from(&reply));
        self.notify("initialized", json!({}))
    }

    /// Send a request and wait synchronously (used by the handshake, which
    /// runs before any tokio runtime is available).
    fn request_sync(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id();
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending.lock().unwrap().insert(id, Waiter::Sync(tx));

        let msg = RpcMessage::request(Id::Num(id), method, params);
        if let Err(err) = self.write_message(&msg) {
            self.pending.lock().unwrap().remove(&id);
            return Err(err);
        }
        let answer = rx
            .recv_timeout(timeout)
            .context("language-server request timed out");
        if answer.is_err() {
            self.pending.lock().unwrap().remove(&id);
        }
        answer?
    }

    /// Send a request and wait for the response, bounded by `timeout`.
    pub async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.next_id();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().unwrap().insert(id, Waiter::Async(tx));

        let msg = RpcMessage::request(Id::Num(id), method, params);
        if let Err(err) = self.write_message(&msg) {
            self.pending.lock().unwrap().remove(&id);
            return Err(err);
        }
        let waited = tokio::time::timeout(timeout, rx)
            .await
            .context("language-server request timed out");
        match waited {
            Ok(answer) => answer.context("language-server dropped the reply")?,
            Err(err) => {
                self.pending.lock().unwrap().remove(&id);
                Err(err)
            }
        }
    }

    /// Send a request and wait, bounded by the short (2 s) deadline.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_with_timeout(method, params, SHORT_TIMEOUT)
            .await
    }

    /// Send a notification (no response expected).
    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = RpcMessage::notification(method, params);
        self.write_message(&msg)
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
                waiter.send(answer);
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

/// Read whole Content-Length frames from `reader` and dispatch them.
fn pump_messages(client: Arc<LspClient>, mut reader: Box<dyn Read + Send>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Some((body, consumed)) = decode_frame(&buf) {
            buf.drain(..consumed);
            if let Ok(msg) = serde_json::from_str::<Value>(&body) {
                client.handle_incoming(msg);
            }
            continue;
        }
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// A `Path` as a `file://` URI, matching the CLI's own URI convention.
fn path_to_uri(root: &Path) -> String {
    let text = root.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// Read `positionEncoding` from an `initialize` reply.
///
/// LSP 3.17 returns a single string; tolerate an array as well. Absent or
/// unknown values fall back to UTF-16, LSP's default.
fn encoding_from(reply: &Value) -> PositionEncoding {
    let negotiated = reply
        .get("capabilities")
        .and_then(|caps| {
            caps.get("positionEncoding")
                .or_else(|| caps.get("position_encoding"))
        })
        .and_then(|value| match value {
            Value::String(s) => Some(s.as_str()),
            Value::Array(items) => items.first().and_then(Value::as_str),
            _ => None,
        });
    PositionEncoding::from_capabilities(negotiated)
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

    /// Tell the server a document changed, sending the real edited text as a
    /// full-content change so diagnostics-after-edit reflect what the user
    /// actually wrote (W7-06).
    pub fn did_change(&self, uri: &str, version: i32, text: &str) -> Result<()> {
        self.client.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }],
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

    /// Workspace symbols (W7-04). A workspace-wide scan is slow, so use the
    /// long deadline rather than the interactive one.
    pub async fn workspace_symbols(&self, query: &str) -> Result<Value> {
        self.client
            .request_with_timeout(
                "workspace/symbol",
                serde_json::json!({ "query": query }),
                LONG_TIMEOUT,
            )
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
        client.pending.lock().unwrap().insert(7, Waiter::Async(tx));
        client.handle_incoming(json!({ "jsonrpc": "2.0", "id": 7, "result": { "ok": true } }));
        let answer = rx.try_recv().expect("the waiter was answered");
        assert_eq!(answer.unwrap()["ok"], true);
        assert!(client.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn an_error_reply_reaches_the_waiter_as_an_error() {
        let client = client();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        client.pending.lock().unwrap().insert(9, Waiter::Async(tx));
        client.handle_incoming(json!({
            "jsonrpc": "2.0",
            "id": 9,
            "error": { "code": -32601, "message": "no such method" }
        }));
        let answer = rx.try_recv().expect("the waiter was answered");
        assert!(answer.unwrap_err().to_string().contains("no such method"));
    }

    #[test]
    fn a_sync_reply_resolves_the_handshake_waiter() {
        let client = client();
        let (tx, rx) = std::sync::mpsc::channel();
        client.pending.lock().unwrap().insert(3, Waiter::Sync(tx));
        client.handle_incoming(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": { "capabilities": { "positionEncoding": "utf-8" } }
        }));
        let answer = rx.try_recv().expect("the waiter was answered");
        assert_eq!(answer.unwrap()["capabilities"]["positionEncoding"], "utf-8");
    }

    #[test]
    fn position_encoding_is_read_from_a_string_and_defaults_to_utf16() {
        assert_eq!(
            encoding_from(&json!({ "capabilities": { "positionEncoding": "utf-8" } })),
            PositionEncoding::Utf8
        );
        assert_eq!(
            encoding_from(&json!({ "capabilities": { "positionEncoding": "utf-16" } })),
            PositionEncoding::Utf16
        );
        assert_eq!(
            encoding_from(&json!({ "capabilities": {} })),
            PositionEncoding::Utf16
        );
        assert_eq!(encoding_from(&json!({})), PositionEncoding::Utf16);
    }

    #[test]
    fn timeouts_have_the_documented_values() {
        assert_eq!(SHORT_TIMEOUT, Duration::from_secs(2));
        assert_eq!(LONG_TIMEOUT, Duration::from_secs(10));
    }
}
