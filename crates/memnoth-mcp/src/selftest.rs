//! `memnoth-mcp --selftest`: the handshake, without a host.
//!
//! Split out of `lib.rs` to keep that file inside the module size budget; it is a self-contained
//! probe, and nothing else in the crate calls it.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{Coord, Profile, profile_name, serve};

/// What the probe found: enough for `mcp doctor` to print one line, and no more.
pub struct Probe {
    /// How many tools `tools/list` returned under the active profile.
    pub tools: usize,
    /// The profile those tools came from, so a reader can tell a narrow surface from a broken one.
    pub profile: &'static str,
}

/// Drive the real JSON-RPC loop with no transport in front of it.
///
/// This is the answer to "did the server I just installed actually answer?" without asking a
/// host to launch it: the same `serve` loop a stdio client reaches, fed four requests through an
/// in-memory pipe, with every reply checked for a `result` instead of an `error`. What it does
/// *not* do is simulate a host more eagerly than a host does — a failure here is the server's,
/// not the harness's.
pub async fn run(db_path: PathBuf) -> Result<()> {
    let probe = probe(db_path).await?;
    println!(
        "memnoth-mcp selftest: initialize, tools/list, resources/list, prompts/list all answered \
         — {} tools on profile '{}'",
        probe.tools, probe.profile
    );
    Ok(())
}

/// The same four requests, returning what they proved instead of printing it.
///
/// `run` is the flag; this is what `mcp doctor` uses, so the two cannot drift into a probe that
/// prints one thing and a doctor that checks another.
pub async fn probe(db_path: PathBuf) -> Result<Probe> {
    use tokio::io::BufReader;

    let profile = Profile::from_env();
    if profile == Profile::Agent {
        anyhow::bail!(
            "the agent profile needs the HTTP transport; --selftest covers the stdio surface"
        );
    }
    let engine = memnoth_engine::open_vault(&db_path)?;
    let coord = Coord::open(&db_path)?;

    let script = [
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "selftest", "version": env!("CARGO_PKG_VERSION")}
            }
        }),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "prompts/list", "params": {}}),
    ]
    .map(|request| request.to_string() + "\n")
    .concat();

    let mut out: Vec<u8> = Vec::new();
    serve(
        engine,
        coord,
        profile,
        Some(&db_path),
        BufReader::new(script.as_bytes()),
        &mut out,
        None,
    )
    .await?;

    let text = String::from_utf8_lossy(&out);
    let mut answered: Vec<u64> = Vec::new();
    let mut tools = 0usize;
    for line in text.lines() {
        let reply: Value =
            serde_json::from_str(line).with_context(|| format!("a reply was not JSON: {line}"))?;
        if let Some(error) = reply.get("error") {
            anyhow::bail!(
                "request {} failed: {error}",
                reply.get("id").and_then(Value::as_u64).unwrap_or(0)
            );
        }
        if let Some(id) = reply.get("id").and_then(Value::as_u64) {
            answered.push(id);
            if id == 2 {
                tools = reply
                    .pointer("/result/tools")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0);
            }
        }
    }
    for id in 1..=4u64 {
        if !answered.contains(&id) {
            anyhow::bail!("no reply to request {id} — the server stopped before answering it");
        }
    }
    Ok(Probe {
        tools,
        profile: profile_name(profile),
    })
}
