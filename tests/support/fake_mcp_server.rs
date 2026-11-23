//! A minimal fake stdio MCP server for the M0 gate observation.
//!
//! It speaks the same newline-delimited JSON-RPC over stdin/stdout that the
//! shipped stdio transport uses. It is deliberately a stand-in, not a copy of
//! the real server: the gate observes that `mcp add` writes a read-only stdio
//! registration and that the flag in that config governs a live server
//! process, without depending on a separately built `memnoth-mcp` binary.
//!
//! The fake owns exactly three tools:
//!
//! * `initialize` — answers with a protocol version and server info.
//! * `vault` — returns the facts stored in the `--db` file as a JSON array.
//! * `mem_add_fact` — refused with the `mcp:write` scope reason when the
//!   process was started read-only; otherwise appends a fact to the file.

use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::{Value, json};

/// Set on a child process so it knows to become the server instead of running
/// the surrounding test suite normally.
pub const CHILD_ENV: &str = "MEMNOTH_FAKE_MCP_CHILD";
/// Path of the fact store the fake server reads and writes.
pub const DB_ENV: &str = "MEMNOTH_FAKE_MCP_DB";
/// `1` makes mutating tools fail; anything else allows them.
pub const READ_ONLY_ENV: &str = "MEMNOTH_FAKE_MCP_READ_ONLY";
/// libtest filter used to start the server in a child of the test binary.
pub const PROCESS_TEST: &str = "fake_mcp_server_process";

/// A fact seeded into the vault before the gate starts the server.
pub const SEEDED_FACT: &str = "seed-fact: memnoth is a local memory sidecar";
/// The smallest scope that permits a mutating tool, mirroring the real server.
pub const WRITE_SCOPE: &str = "mcp:write";

fn reply(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_result(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

fn read_facts(db: &Path) -> Vec<String> {
    match std::fs::read_to_string(db) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn append_fact(db: &Path, line: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(db)?;
    writeln!(file, "{line}")
}

/// One JSON-RPC request in, one reply out (`None` for a notification).
fn handle(request: &Value, db: &Path, read_only: bool) -> Option<Value> {
    let id = request.get("id").cloned()?;
    let method = request.get("method").and_then(Value::as_str)?;
    match method {
        "initialize" => Some(reply(
            id,
            json!({
                "protocolVersion": "2025-06-18",
                "serverInfo": { "name": "memnoth-fake-mcp", "version": "0.1.2" },
                "capabilities": { "tools": {} },
            }),
        )),
        "notifications/initialized" => None,
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let result = match name {
                "vault" => {
                    let facts = read_facts(db);
                    let text = serde_json::to_string(&facts).expect("facts are serializable");
                    tool_result(text, false)
                }
                "mem_add_fact" => {
                    if read_only {
                        tool_result(
                            format!(
                                "tool `mem_add_fact` requires scope `{WRITE_SCOPE}`; this MCP \
                                 server was started read-only."
                            ),
                            true,
                        )
                    } else {
                        let entity = arguments
                            .get("entity_name")
                            .and_then(Value::as_str)
                            .unwrap_or("fact");
                        let content = arguments
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let line = format!("{entity}: {content}");
                        match append_fact(db, &line) {
                            Ok(()) => {
                                let text = serde_json::to_string(&json!({ "written": line }))
                                    .expect("write report is serializable");
                                tool_result(text, false)
                            }
                            Err(error) => tool_result(format!("write failed: {error}"), true),
                        }
                    }
                }
                other => {
                    return Some(error(id, -32601, format!("unknown tool `{other}`")));
                }
            };
            Some(reply(id, result))
        }
        other => Some(error(id, -32601, format!("method not found: {other}"))),
    }
}

/// Serve newline-delimited JSON-RPC on stdin/stdout until stdin closes.
///
/// Non-JSON lines are ignored so a test harness banner on the child's stdout
/// cannot corrupt the framing; every reply is one JSON object on one line.
pub fn serve_stdin(db: &Path, read_only: bool) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(response) = handle(&request, db, read_only) {
            let mut encoded = serde_json::to_string(&response).expect("reply is serializable");
            encoded.push('\n');
            let _ = stdout.write_all(encoded.as_bytes());
            let _ = stdout.flush();
        }
    }
}
