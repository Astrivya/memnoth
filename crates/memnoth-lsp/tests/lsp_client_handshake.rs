//! End-to-end proof that the LSP client performs the real handshake and sends
//! the real edited text: a fake in-process LSP server speaks Content-Length
//! framed JSON-RPC over stdio-shaped in-memory pipes, and the client connects
//! to it through the same transport path a spawned server uses.
//!
//! The server records the `initialize` params, whether `initialized` arrived,
//! and every `textDocument/didChange` text, and answers a synthetic
//! `test/observations` request with that record so the assertions stay on the
//! wire rather than reaching into private state.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use memnoth_codegraph::lsp::PositionEncoding;
use memnoth_lsp::client::{LONG_TIMEOUT, SHORT_TIMEOUT};
use memnoth_lsp::{LspClient, LspClientHandle, encode_frame};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// In-memory blocking pipes: a Write half pushes bytes, a Read half blocks
// until bytes are available. Together they stand in for ChildStdin/ChildStdout.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct PipeInner {
    bytes: VecDeque<u8>,
    closed: bool,
}

struct PipeWriter {
    inner: Arc<(Mutex<PipeInner>, Condvar)>,
}

struct PipeReader {
    inner: Arc<(Mutex<PipeInner>, Condvar)>,
}

fn pipe() -> (PipeWriter, PipeReader) {
    let inner = Arc::new((Mutex::new(PipeInner::default()), Condvar::new()));
    (
        PipeWriter {
            inner: Arc::clone(&inner),
        },
        PipeReader { inner },
    )
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (lock, cv) = &*self.inner;
        let mut guard = lock.lock().unwrap();
        guard.bytes.extend(buf);
        cv.notify_all();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        let (lock, cv) = &*self.inner;
        lock.lock().unwrap().closed = true;
        cv.notify_all();
    }
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let (lock, cv) = &*self.inner;
        let mut guard = lock.lock().unwrap();
        while guard.bytes.is_empty() && !guard.closed {
            guard = cv.wait(guard).unwrap();
        }
        if guard.bytes.is_empty() {
            return Ok(0);
        }
        let n = buf.len().min(guard.bytes.len());
        for slot in buf.iter_mut().take(n) {
            *slot = guard.bytes.pop_front().unwrap();
        }
        Ok(n)
    }
}

// ---------------------------------------------------------------------------
// The fake server.
// ---------------------------------------------------------------------------

fn send(write: &mut impl Write, id: Value, result: Value) {
    let body = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
    write.write_all(&encode_frame(&body)).unwrap();
    write.flush().unwrap();
}

/// Speak enough LSP to prove the handshake and document sync. `encoding`, when
/// set, is advertised as `capabilities.positionEncoding`; when absent the
/// capability is omitted so the client must fall back to UTF-16.
fn fake_server(mut read: PipeReader, mut write: PipeWriter, encoding: Option<String>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut initialized = false;
    let mut init_params: Option<Value> = None;
    let mut did_change_texts: Vec<String> = Vec::new();

    loop {
        if let Some((body, consumed)) = memnoth_lsp::decode_frame(&buf) {
            buf.drain(..consumed);
            let msg: Value = serde_json::from_str(&body).expect("client sent valid JSON");
            let method = msg["method"].as_str().unwrap_or("").to_string();
            let id = msg.get("id").cloned();

            match method.as_str() {
                "initialize" => {
                    init_params = Some(msg["params"].clone());
                    let mut capabilities = json!({ "definitionProvider": true });
                    if let Some(enc) = &encoding {
                        capabilities["positionEncoding"] = json!(enc);
                    }
                    send(
                        &mut write,
                        id.unwrap(),
                        json!({ "capabilities": capabilities }),
                    );
                }
                "initialized" => {
                    initialized = true;
                }
                "textDocument/didChange" => {
                    let text = msg["params"]["contentChanges"][0]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    did_change_texts.push(text);
                }
                "test/observations" => {
                    send(
                        &mut write,
                        id.unwrap(),
                        json!({
                            "initialized": initialized,
                            "initializeParams": init_params,
                            "didChangeTexts": did_change_texts,
                        }),
                    );
                }
                "shutdown" => send(&mut write, id.unwrap(), json!(null)),
                "exit" => break,
                _ => {}
            }
            continue;
        }

        let n = read.read(&mut chunk).expect("pipe read");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

struct Rig {
    client: Arc<LspClient>,
    server: Option<JoinHandle<()>>,
}

impl Rig {
    fn new(encoding: Option<&str>) -> Self {
        let (client_out, server_in) = pipe();
        let (server_out, client_in) = pipe();
        let enc = encoding.map(str::to_string);
        let server = std::thread::spawn(move || fake_server(server_in, server_out, enc));
        let root = Path::new("E:/workspace/my-project");
        let client = LspClient::connect(
            "rust".to_string(),
            root,
            Box::new(client_out),
            Box::new(client_in),
            None,
        )
        .expect("initialize handshake succeeded");
        Self {
            client,
            server: Some(server),
        }
    }

    async fn observations(&self) -> Value {
        self.client
            .request("test/observations", json!({}))
            .await
            .expect("fake server answers observations")
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.client.notify("exit", json!({}));
        if let Some(handle) = self.server.take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialize_carries_root_uri_and_capabilities_and_initialized_follows() {
    let rig = Rig::new(Some("utf-8"));
    let obs = rig.observations().await;

    assert_eq!(
        obs["initialized"], true,
        "initialized notification was sent"
    );
    let params = &obs["initializeParams"];
    assert!(params.is_object(), "an initialize request was sent");
    assert_eq!(params["rootUri"], "file:///E:/workspace/my-project");
    assert!(params["processId"].is_number());
    assert_eq!(
        params["capabilities"]["general"]["positionEncodings"],
        json!(["utf-8", "utf-16"])
    );
    assert!(params["capabilities"]["textDocument"]["synchronization"].is_object());
}

#[tokio::test]
async fn a_server_advertising_utf8_negotiates_utf8() {
    let rig = Rig::new(Some("utf-8"));
    assert_eq!(
        *rig.client.encoding.lock().unwrap(),
        Some(PositionEncoding::Utf8)
    );
}

#[tokio::test]
async fn position_encoding_defaults_to_utf16_when_the_server_is_silent() {
    let rig = Rig::new(None);
    assert_eq!(
        *rig.client.encoding.lock().unwrap(),
        Some(PositionEncoding::Utf16)
    );
}

#[tokio::test]
async fn did_change_sends_the_real_edited_text() {
    let rig = Rig::new(Some("utf-16"));
    let handle = LspClientHandle::new(Arc::clone(&rig.client));
    let uri = "file:///E:/workspace/my-project/src/main.rs";
    let original = "fn main() {}\n";
    let edited = "fn main() { let x = 1; }\n";

    handle.did_open(uri, "rust", 1, original).unwrap();
    handle.did_change(uri, 2, edited).unwrap();

    let obs = rig.observations().await;
    assert_eq!(obs["didChangeTexts"], json!([edited]));
    assert_ne!(obs["didChangeTexts"][0], json!(original));
    assert_ne!(obs["didChangeTexts"][0], json!(""));
}

#[test]
fn request_timeouts_are_two_and_ten_seconds() {
    assert_eq!(SHORT_TIMEOUT, Duration::from_secs(2));
    assert_eq!(LONG_TIMEOUT, Duration::from_secs(10));
}

#[tokio::test]
async fn a_request_the_server_never_answers_is_bounded_by_the_timeout() {
    // The fake server answers `initialize` but has no handler for
    // `never/answers`, so it stays silent — exactly the hung-server case the
    // deadline exists for. A tiny timeout keeps the test fast while proving
    // `request_with_timeout` is the bound.
    let rig = Rig::new(Some("utf-8"));
    let err = rig
        .client
        .request_with_timeout("never/answers", json!({}), Duration::from_millis(150))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("timed out"), "{err}");
}
