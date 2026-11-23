//! Focused integration tests for `lsp_diag`, driven by a fake language server
//! on stdio. They prove three things the old 2 s poll could not:
//!
//! 1. the server receives the *real* edited text in `didOpen`;
//! 2. the summary names the diagnostic code, file and line, capped at five rows;
//! 3. an enabled server that cannot be started reports "not available" instead
//!    of returning nothing (which would look like a clean file).
//!
//! The fake server is this test binary itself. `LspClient` spawns
//! `std::env::current_exe()` with a libtest filter that selects only
//! `fake_lsp_server_loop`; that one test reads framed JSON-RPC from stdin,
//! records the text it was handed, and publishes a canned diagnostic. It is a
//! no-op in a normal run, so a plain `cargo test` never blocks on stdin.

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use memnoth::lsp_diag::after_write_with;
use memnoth_codegraph::lsp::{AutoStartPolicy, ServerSpec};
use memnoth_lsp::{decode_frame, encode_frame};
use serde_json::{Value, json};
use tempfile::TempDir;

const FAKE_SERVER_FILTER: &str = "fake_lsp_server_loop";
const FAKE_ARGS: &[&str] = &[FAKE_SERVER_FILTER, "--nocapture"];
const EDITED_TEXT: &str = "fn main() { let x = missing_name; }\n";
const WAIT: Duration = Duration::from_secs(3);

/// The fake server: only runs when this binary is launched with the filter the
/// tests pass to `LspClient`. In every other run it returns immediately.
#[test]
fn fake_lsp_server_loop() {
    if !std::env::args().any(|arg| arg == FAKE_SERVER_FILTER) {
        return;
    }
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buffer.extend_from_slice(&chunk[..read]);
        while let Some((body, consumed)) = decode_frame(&buffer) {
            if let Ok(msg) = serde_json::from_str::<Value>(&body) {
                match msg["method"].as_str() {
                    // The client performs an initialize handshake before any
                    // document notification; without this reply it never gets
                    // as far as didOpen.
                    Some("initialize") => write_frame(&json!({
                        "jsonrpc": "2.0",
                        "id": msg.get("id").cloned().unwrap_or(Value::Null),
                        "result": { "capabilities": { "positionEncoding": "utf-16" } }
                    })),
                    Some("textDocument/didOpen") => publish_diagnostics(&msg),
                    _ => {}
                }
            }
            buffer.drain(..consumed);
        }
    }
}

/// Write one framed reply. The leading CRLF closes the libtest banner ("running
/// 1 test", "test …") that the harness prints on this process's stdout before
/// the test body runs — `decode_frame` splits the header on CRLF and would
/// otherwise see the banner glued to `Content-Length`.
fn write_frame(msg: &Value) {
    let mut bytes = vec![b'\r', b'\n'];
    bytes.extend_from_slice(&encode_frame(&msg.to_string()));
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = out.write_all(&bytes);
    let _ = out.flush();
}

/// Record the real text and answer with the diagnostics staged by the parent.
/// Both files are relative because `LspClient` runs the server in `root`.
fn publish_diagnostics(msg: &Value) {
    let uri = msg["params"]["textDocument"]["uri"].as_str().unwrap_or("");
    let text = msg["params"]["textDocument"]["text"].as_str().unwrap_or("");
    let _ = std::fs::write("received_text.txt", text.as_bytes());

    let diagnostics: Vec<Value> = std::fs::read_to_string("diagnostics.json")
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| {
            vec![json!({
                "code": "E9999",
                "severity": 1,
                "message": "default fake problem",
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 1 }
                }
            })]
        });
    write_frame(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": { "uri": uri, "diagnostics": diagnostics }
    }));
}

/// A workspace root with the `Cargo.toml` marker the policy requires.
fn root() -> TempDir {
    let dir = tempfile::tempdir().expect("temp workspace");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"fake\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    dir
}

fn edited_path(root: &Path) -> PathBuf {
    root.join("src").join("lib.rs")
}

/// Point the server table's Rust row at this test binary, in fake-server mode.
fn fake_server_spec() -> ServerSpec {
    let exe = std::env::current_exe().expect("test binary path");
    let command: &'static str = Box::leak(exe.to_string_lossy().into_owned().into_boxed_str());
    ServerSpec {
        id: "fake-lsp",
        language: "rust",
        extensions: &["rs"],
        root_markers: &["Cargo.toml"],
        command,
        args: FAKE_ARGS,
    }
}

fn missing_server_spec() -> ServerSpec {
    ServerSpec {
        id: "missing-lsp",
        language: "rust",
        extensions: &["rs"],
        root_markers: &["Cargo.toml"],
        command: "__memnoth_test_no_such_lsp_server__",
        args: &[],
    }
}

fn policy() -> AutoStartPolicy {
    AutoStartPolicy::allowing(&["rust"])
}

#[test]
fn fake_server_receives_the_exact_edited_text() {
    let dir = root();
    let spec = fake_server_spec();
    let summary = after_write_with(
        dir.path(),
        &edited_path(dir.path()),
        EDITED_TEXT,
        &spec,
        &policy(),
        WAIT,
    )
    .expect("the fake server answered");
    assert!(!summary.contains("not available"), "{summary}");
    assert!(summary.contains("default fake problem"), "{summary}");

    let received = std::fs::read_to_string(dir.path().join("received_text.txt"))
        .expect("the fake server recorded didOpen text");
    assert_eq!(
        received, EDITED_TEXT,
        "the server must diagnose the text the edit wrote, not an empty buffer"
    );
}

#[test]
fn summary_names_code_file_and_line_and_stays_bounded() {
    let dir = root();
    let diagnostics: Vec<Value> = (0..7)
        .map(|index| {
            json!({
                "code": format!("E{index:03}"),
                "severity": if index == 0 { 1 } else { 2 },
                "message": format!("fake problem {index}"),
                "range": {
                    "start": { "line": index + 4, "character": 0 },
                    "end": { "line": index + 4, "character": 1 }
                }
            })
        })
        .collect();
    std::fs::write(
        dir.path().join("diagnostics.json"),
        serde_json::to_vec(&diagnostics).unwrap(),
    )
    .unwrap();

    let spec = fake_server_spec();
    let displayed = edited_path(dir.path()).display().to_string();
    let summary = after_write_with(
        dir.path(),
        &edited_path(dir.path()),
        EDITED_TEXT,
        &spec,
        &policy(),
        WAIT,
    )
    .expect("the fake server answered");

    assert!(
        summary.starts_with("diagnostics after this edit: 1 error(s), 6 warning(s)"),
        "{summary}"
    );
    assert!(
        summary.contains(&format!("{displayed}:5 [E000]  fake problem 0")),
        "{summary}"
    );
    assert!(
        summary.contains(&format!("{displayed}:6 [E001]  fake problem 1")),
        "{summary}"
    );
    assert!(
        !summary.contains("fake problem 5"),
        "only five diagnostic rows are shown: {summary}"
    );
    assert!(summary.contains("… 2 more"), "{summary}");
    let rows = summary
        .lines()
        .filter(|line| line.contains("fake problem"))
        .count();
    assert_eq!(rows, 5, "bounded to five rows: {summary}");
}

#[test]
fn no_server_is_reported_as_not_available() {
    let dir = root();
    let spec = missing_server_spec();
    let summary = after_write_with(
        dir.path(),
        &edited_path(dir.path()),
        EDITED_TEXT,
        &spec,
        &policy(),
        Duration::from_millis(200),
    )
    .expect("an enabled server that cannot start must still be reported");
    assert!(summary.contains("not available"), "{summary}");
    assert!(
        !summary.contains("diagnostics after this edit"),
        "a failed start is not a clean file: {summary}"
    );
}
