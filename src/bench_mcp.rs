//! Model-free MCP protocol diagnostic: one real stdio server, five honest gates.
//!
//! `mn bench mcp-verify` spawns the built `memnoth-mcp` server against a fresh
//! temporary vault (`--db <tmp>/vault.db`, `MEMNOTH_MCP_PROFILE=memory`,
//! `MEMNOTH_MCP_READ_ONLY=1`) and drives it with the crate's own framed JSON-RPC
//! client. It does **not** call a model, open a socket, or touch the user's
//! vault. Five gates are measured on that one process: handshake, tools/list
//! stability, read-tool latency, read-only denial with a vault byte hash, and
//! the shape of the local stdio registration this CLI writes.
//!
//! HONESTY. This is a *derived* conformance/throughput diagnostic, not an
//! official benchmark score. Every number below was observed in this process;
//! nothing is simulated, cached from a prior run, or filled in when a call
//! failed. A failed call produces a failed gate and a non-zero exit, never a
//! plausible-looking metric. Latency has no invented threshold — the gate is
//! about whether the calls answered, and the p50/p95 are reported as measured.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use memnoth_mcp_client::{Client, StdioTransport, ToolInfo};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::context::AppContext;
use crate::output::{ExitCode, Outcome};

/// Schema tag of the result envelope this lane emits.
pub const SCHEMA: &str = "mn-mcp-result/1";
/// The mandatory honesty note, carried on every result.
pub const NOTE: &str =
    "Derived MCP conformance/throughput diagnostic, not an official benchmark score";
/// The five gates, in report order. A result is a pass only when all ran and passed.
pub const GATE_NAMES: [&str; 5] = [
    "handshake",
    "tools_list_stability",
    "call_latency",
    "read_only_denial",
    "config_parity",
];

const PROFILE: &str = "memory";
const HANDSHAKE_SOFT_MS: u64 = 200;
const HANDSHAKE_HARD_MS: u64 = 500;
const REQUEST_TIMEOUT: Duration = Duration::from_millis(600);
const LIST_CALLS: usize = 3;
const LATENCY_CALLS: usize = 10;
const READ_TOOL: &str = "mem_search";
const WRITE_TOOL: &str = "mem_add_fact";

/// CLI arguments for `mn bench mcp-verify`. Defined here so the only wiring in
/// `bench.rs` is one enum variant and one dispatch arm.
#[derive(Debug, clap::Args)]
pub struct McpVerifyArgs {
    /// Path to the `memnoth-mcp` server binary (auto-discovered when omitted).
    #[arg(long)]
    pub server: Option<PathBuf>,
}

/// One measured gate. `ms` is always the wall time of that gate's actual work;
/// a gate that could not run reports its real elapsed time or an explicit zero
/// together with the reason in `detail`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Gate {
    pub name: String,
    pub passed: bool,
    pub ms: u64,
    pub detail: String,
}

fn gate(name: &str, started: Instant, passed: bool, detail: impl Into<String>) -> Gate {
    Gate {
        name: name.to_string(),
        passed,
        ms: started.elapsed().as_millis() as u64,
        detail: detail.into(),
    }
}

/// A result passes only when every expected gate is present and passed.
/// Exposed so the integration test can drive the negative control directly.
pub fn overall_gate(gates: &[Gate]) -> &'static str {
    if gates.len() == GATE_NAMES.len() && gates.iter().all(|item| item.passed) {
        "pass"
    } else {
        "fail"
    }
}

/// Assemble the result envelope from measured gates and measured context.
/// Pure: the negative control can feed it a deliberately failing gate.
pub fn envelope(gates: &[Gate], context: &Value) -> Value {
    let passed = gates.iter().filter(|item| item.passed).count();
    json!({
        "schema": SCHEMA,
        "version": env!("CARGO_PKG_VERSION"),
        "observed_at": context.get("observed_at").cloned().unwrap_or(Value::Null),
        "diagnostic": "derived",
        "official": false,
        "gate": overall_gate(gates),
        "note": NOTE,
        "label": "Derived MCP conformance/throughput diagnostic (NOT an official benchmark score)",
        "disclaimer": "Facts observed from one local stdio server process on a fresh temporary vault. Not an official benchmark score, not a model evaluation, and not extrapolated to any published suite.",
        "scope": "one memnoth-mcp process; profile=memory; read-only=1; 5 gates; no model, no network, no user vault",
        "server": context.get("server").cloned().unwrap_or(Value::Null),
        "gates": gates,
        "summary": {
            "gates_total": gates.len(),
            "gates_passed": passed,
            "gates_failed": gates.len() - passed,
            "denominator": gates.len(),
        },
        "metrics": context.get("metrics").cloned().unwrap_or(Value::Null),
        "config_parity": context.get("config_parity").cloned().unwrap_or(Value::Null),
    })
}

/// Where the server binary comes from, in the same order the CLI resolves it:
/// an explicit override, beside the running binary, one directory up (the
/// integration-test layout), the cargo target debug dir, then PATH.
pub fn discover_server() -> Option<PathBuf> {
    if let Some(override_path) = std::env::var_os("MEMNOTH_MCP_BIN").map(PathBuf::from) {
        let path = std::path::absolute(&override_path).unwrap_or(override_path);
        if path.is_file() {
            return Some(path);
        }
    }

    let names: [&str; 2] = if cfg!(windows) {
        ["memnoth-mcp.exe", "memnoth-mcp"]
    } else {
        ["memnoth-mcp", "memnoth-mcp"]
    };
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.to_path_buf());
        if let Some(parent) = dir.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    if let Some(target) = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) {
        dirs.push(target.join("debug"));
    }
    for dir in &dirs {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in names {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// ISO-8601 UTC timestamp sourced from `SystemTime`, not from a cached string.
fn observed_at() -> String {
    let instant = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| SystemTime::UNIX_EPOCH + elapsed)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    DateTime::<Utc>::from(instant).to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Nearest-rank percentile on an already sorted slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let last = sorted.len() - 1;
    sorted[((p * last as f64).round() as usize).min(last)]
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Fingerprint the vault database and its WAL sidecar. The `-shm` file is not
/// hashed: readers legitimately update its shared-memory lock bytes, which
/// would turn a read-only probe into a false "bytes written" report.
fn vault_digest(vault: &Path) -> Result<(String, u64, usize)> {
    let mut paths = vec![vault.to_path_buf()];
    let wal = PathBuf::from(format!("{}-wal", vault.display()));
    if wal.is_file() {
        paths.push(wal);
    }
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    for path in &paths {
        let data =
            std::fs::read(path).with_context(|| format!("read vault file {}", path.display()))?;
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update((data.len() as u64).to_le_bytes());
        hasher.update(&data);
        bytes += data.len() as u64;
    }
    Ok((hex::encode(hasher.finalize()), bytes, paths.len()))
}

/// The tool definitions as the wire exposes them, without the envelope's
/// per-response metadata. Only this half is compared for stability.
fn tools_snapshot(tools: &[ToolInfo]) -> Value {
    json!(
        tools
            .iter()
            .map(|tool| json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
            }))
            .collect::<Vec<_>>()
    )
}

/// A tool definition is "JSON Schema-ish" when it is an object declaring an
/// object type and carrying an object `properties` map. This is the shape the
/// server's own `schema()` builder emits.
fn schema_shape_ok(tool: &ToolInfo) -> bool {
    tool.input_schema.get("type").and_then(Value::as_str) == Some("object")
        && tool
            .input_schema
            .get("properties")
            .is_some_and(Value::is_object)
}

/// The local stdio registration shape, inspected without writing any host
/// config. Uses the real `Entry` type the CLI writes through `mcp add --local`.
fn config_parity_gate() -> (Gate, Value) {
    let started = Instant::now();
    let command = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "memnoth".to_string());
    let env = BTreeMap::from([
        ("MEMNOTH_MCP_PROFILE".to_string(), PROFILE.to_string()),
        ("MEMNOTH_MCP_READ_ONLY".to_string(), "1".to_string()),
    ]);
    let entry = crate::mcp::clients::Entry::stdio(
        command,
        vec![
            "mcp".to_string(),
            "serve".to_string(),
            "--db".to_string(),
            "<vault>".to_string(),
        ],
        env,
    );

    let url_fields = usize::from(!entry.url.is_empty());
    let token_fields = usize::from(entry.token.is_some());
    let rendered = json!({ "args": entry.args, "env": entry.env })
        .to_string()
        .to_ascii_lowercase();
    let markers = [
        "http://", "https://", "bearer ", "--token", "--url", "token=", "secret",
    ];
    let suspicious_fields = markers
        .iter()
        .filter(|marker| rendered.contains(**marker))
        .count();
    let env_secret_keys = entry
        .env
        .keys()
        .filter(|key| {
            let key = key.to_ascii_uppercase();
            key.contains("TOKEN") || key.contains("SECRET") || key.contains("API_KEY")
        })
        .count();
    let read_only = entry.env.get("MEMNOTH_MCP_READ_ONLY").map(String::as_str) == Some("1");
    let profile_ok = entry.env.get("MEMNOTH_MCP_PROFILE").map(String::as_str) == Some(PROFILE);
    let passed = entry.is_stdio()
        && url_fields == 0
        && token_fields == 0
        && suspicious_fields == 0
        && env_secret_keys == 0
        && read_only
        && profile_ok;

    let detail = format!(
        "local stdio entry: is_stdio={}, url_fields={url_fields}/1, token_fields={token_fields}/1, \
         suspicious_fields={suspicious_fields}/{} markers, secret_env_keys={env_secret_keys}/{} keys, \
         read_only=1, profile={PROFILE}",
        entry.is_stdio(),
        markers.len(),
        entry.env.len(),
    );
    let metrics = json!({
        "is_stdio": entry.is_stdio(),
        "url_fields": url_fields,
        "url_fields_checked": 1,
        "token_fields": token_fields,
        "token_fields_checked": 1,
        "suspicious_fields": suspicious_fields,
        "suspicious_markers_checked": markers.len(),
        "env_secret_keys": env_secret_keys,
        "env_keys_checked": entry.env.len(),
        "read_only_env": read_only,
        "profile_env": profile_ok,
    });
    (gate("config_parity", started, passed, detail), metrics)
}

/// Run all five gates against one server process and return the result envelope.
/// No number in here is produced unless the corresponding call was observed.
pub fn verify(binary: &Path) -> Result<Value> {
    let observed = observed_at();
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp = std::env::temp_dir().join(format!("mn-mcp-verify-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&temp)
        .with_context(|| format!("create temp dir {}", temp.display()))?;
    let vault = temp.join("vault.db");

    let mut server_env = BTreeMap::new();
    server_env.insert("MEMNOTH_MCP_PROFILE".to_string(), PROFILE.to_string());
    server_env.insert("MEMNOTH_MCP_READ_ONLY".to_string(), "1".to_string());
    let server_args = vec!["--db".to_string(), vault.to_string_lossy().into_owned()];

    let spawn_started = Instant::now();
    let transport = StdioTransport::spawn(&binary.to_string_lossy(), &server_args, &server_env)
        .with_context(|| format!("spawn MCP server {}", binary.display()))?;
    let spawn_ms = spawn_started.elapsed().as_millis() as u64;

    let mut gates = Vec::with_capacity(GATE_NAMES.len());
    let (parity_gate, parity) = config_parity_gate();

    // ---- gate 1: handshake ------------------------------------------------
    let handshake_started = Instant::now();
    let connected = Client::connect(Box::new(transport), REQUEST_TIMEOUT);
    let handshake_ms = handshake_started.elapsed().as_millis() as u64;
    let mut client = match connected {
        Ok(client) => client,
        Err(error) => {
            gates.push(gate(
                "handshake",
                handshake_started,
                false,
                format!("server/discover failed: {error:#}"),
            ));
            for name in &GATE_NAMES[1..4] {
                gates.push(Gate {
                    name: (*name).to_string(),
                    passed: false,
                    ms: 0,
                    detail: "not run: handshake failed".to_string(),
                });
            }
            gates.push(parity_gate);
            let metrics = json!({
                "spawn": { "calls": 1, "ms": spawn_ms },
                "handshake": {
                    "method": "server/discover",
                    "calls": 1,
                    "ms": handshake_ms,
                    "soft_threshold_ms": HANDSHAKE_SOFT_MS,
                    "hard_threshold_ms": HANDSHAKE_HARD_MS,
                    "soft_exceeded": handshake_ms > HANDSHAKE_SOFT_MS,
                    "protocol": Value::Null,
                    "server_name": Value::Null,
                    "server_version": Value::Null,
                },
            });
            let envelope = envelope(
                &gates,
                &json!({
                    "observed_at": observed,
                    "server": server_context(binary, &vault, spawn_ms),
                    "metrics": metrics,
                    "config_parity": parity,
                }),
            );
            let _ = std::fs::remove_dir_all(&temp);
            return Ok(envelope);
        }
    };

    let info = client.server();
    let protocol = info.protocol.clone();
    let server_name = info.name.clone();
    let server_version = info.version.clone();
    let soft_exceeded = handshake_ms > HANDSHAKE_SOFT_MS;
    let handshake_ok = handshake_ms < HANDSHAKE_HARD_MS
        && !protocol.is_empty()
        && !server_name.is_empty()
        && !server_version.is_empty();
    gates.push(gate(
        "handshake",
        handshake_started,
        handshake_ok,
        format!(
            "server/discover in {handshake_ms} ms; protocol={protocol}; serverInfo.name={server_name}; \
             serverInfo.version={server_version}; soft_threshold_ms={HANDSHAKE_SOFT_MS} \
             (exceeded={soft_exceeded}); hard_threshold_ms={HANDSHAKE_HARD_MS}"
        ),
    ));

    // ---- gate 2: tools/list stability ------------------------------------
    let list_started = Instant::now();
    let mut snapshots: Vec<Value> = Vec::with_capacity(LIST_CALLS);
    let mut first_tools: Vec<ToolInfo> = Vec::new();
    let mut list_error: Option<String> = None;
    for _ in 0..LIST_CALLS {
        match client.refresh_tools() {
            Ok(tools) => {
                first_tools = tools.to_vec();
                snapshots.push(tools_snapshot(&first_tools));
            }
            Err(error) => {
                list_error = Some(format!("{error:#}"));
                break;
            }
        }
    }
    let list_ms = list_started.elapsed().as_millis() as u64;
    let with_name = first_tools.iter().filter(|t| !t.name.is_empty()).count();
    let with_description = first_tools
        .iter()
        .filter(|t| !t.description.is_empty())
        .count();
    let with_schema = first_tools.iter().filter(|t| schema_shape_ok(t)).count();
    let names: Vec<&str> = first_tools.iter().map(|t| t.name.as_str()).collect();
    let names_stable = snapshots.iter().all(|snapshot| {
        snapshot.as_array().is_some_and(|rows| {
            rows.iter()
                .filter_map(|row| row["name"].as_str())
                .eq(names.iter().copied())
        })
    });
    let identical =
        snapshots.len() == LIST_CALLS && snapshots.iter().all(|snapshot| snapshot == &snapshots[0]);
    let definition_chars = snapshots
        .first()
        .map(Value::to_string)
        .unwrap_or_default()
        .len();
    let definition_tokens = definition_chars / 4;
    let list_ok = list_error.is_none()
        && !first_tools.is_empty()
        && with_name == first_tools.len()
        && with_description == first_tools.len()
        && with_schema == first_tools.len()
        && names_stable
        && identical;
    let list_detail = match &list_error {
        Some(error) => format!("tools/list failed: {error}"),
        None => format!(
            "{LIST_CALLS}/{LIST_CALLS} calls; tools={}; with_name={with_name}/{}, \
             with_description={with_description}/{}, with_schema={with_schema}/{}, \
             names_stable={names_stable}, identical_snapshots={}/{}; \
             definition_chars={definition_chars}, definition_tokens_approx={definition_tokens} \
             (chars/4, approximate)",
            first_tools.len(),
            first_tools.len(),
            first_tools.len(),
            first_tools.len(),
            snapshots.len(),
            LIST_CALLS,
        ),
    };
    gates.push(gate(
        "tools_list_stability",
        list_started,
        list_ok,
        list_detail,
    ));
    let mut list_metrics = json!({
        "calls": snapshots.len(),
        "calls_expected": LIST_CALLS,
        "tools": first_tools.len(),
        "with_name": with_name,
        "with_description": with_description,
        "with_schema": with_schema,
        "definition_count_denominator": first_tools.len(),
        "names_stable": names_stable,
        "identical_snapshots": snapshots.len(),
        "snapshots_expected": LIST_CALLS,
        "definition_chars": definition_chars,
        "definition_tokens_approx": definition_tokens,
        "token_divisor": 4,
        "token_count_label": "approximate (serialized chars / 4)",
    });
    list_metrics["ms"] = json!(list_ms);

    // ---- gate 3: read-tool latency ---------------------------------------
    let latency_started = Instant::now();
    let mut latencies: Vec<f64> = Vec::with_capacity(LATENCY_CALLS);
    let mut call_failures: Vec<String> = Vec::new();
    for _ in 0..LATENCY_CALLS {
        let call_started = Instant::now();
        match client.call(READ_TOOL, json!({ "query": "mcp verify", "limit": 5 })) {
            Ok(outcome) => {
                latencies.push(call_started.elapsed().as_secs_f64() * 1000.0);
                if outcome.is_error {
                    call_failures.push("tool answered isError=true".to_string());
                }
            }
            Err(error) => call_failures.push(format!("{error:#}")),
        }
    }
    let latency_ms = latency_started.elapsed().as_millis() as u64;
    let mut sorted = latencies.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let p50 = round3(percentile(&sorted, 0.50));
    let p95 = round3(percentile(&sorted, 0.95));
    let latency_ok = latencies.len() == LATENCY_CALLS && call_failures.is_empty();
    let latency_detail = format!(
        "{READ_TOOL}: {}/{} calls answered; p50={p50} ms, p95={p95} ms; failures={}",
        latencies.len(),
        LATENCY_CALLS,
        call_failures.len()
    );
    gates.push(gate(
        "call_latency",
        latency_started,
        latency_ok,
        latency_detail,
    ));
    let mut latency_metrics = json!({
        "tool": READ_TOOL,
        "unit": "ms",
        "calls": LATENCY_CALLS,
        "succeeded": latencies.len(),
        "failed": call_failures.len(),
        "sample_denominator": LATENCY_CALLS,
        "p50_ms": p50,
        "p95_ms": p95,
        "latency_threshold_ms": Value::Null,
        "threshold_note": "no latency threshold asserted; p50/p95 are observations only",
    });
    latency_metrics["ms"] = json!(latency_ms);

    // ---- gate 4: read-only denial and zero vault writes -------------------
    let denial_started = Instant::now();
    // Hash only after every preceding read has settled, so read-induced shm
    // churn cannot be mistaken for a write performed by the mutating call.
    let before = vault_digest(&vault);
    let denial = client.call(
        WRITE_TOOL,
        json!({ "entity_name": "mn-verify", "content": "must be refused" }),
    );
    let after = vault_digest(&vault);
    let denial_text = match &denial {
        Ok(outcome) if outcome.is_error => Some(outcome.text()),
        Ok(_) => None,
        Err(error) => Some(format!("{error:#}")),
    };
    let lower = denial_text.as_deref().unwrap_or("").to_ascii_lowercase();
    let names_read_only = lower.contains("read-only") || lower.contains("scope");
    let (before_digest, before_bytes, files_hashed) = before.unwrap_or((String::new(), 0, 0));
    let (after_digest, after_bytes, _) = after.unwrap_or((String::new(), 0, 0));
    let bytes_written = after_bytes.saturating_sub(before_bytes);
    let no_write = before_digest == after_digest && !before_digest.is_empty();
    let denial_ok = denial_text.is_some() && names_read_only && no_write;
    let denial_detail = format!(
        "{WRITE_TOOL}: write_attempts=1/1, denials={}/1, names_read_only={names_read_only}; \
         vault_files_hashed={files_hashed}, bytes_written={bytes_written}, \
         hash_before={before_digest}, hash_after={after_digest}",
        usize::from(denial_text.is_some()),
    );
    gates.push(gate(
        "read_only_denial",
        denial_started,
        denial_ok,
        denial_detail,
    ));
    let mut denial_metrics = json!({
        "tool": WRITE_TOOL,
        "write_attempts": 1,
        "denials": usize::from(denial_text.is_some()),
        "names_read_only": names_read_only,
        "vault_files_hashed": files_hashed,
        "bytes_written": bytes_written,
        "hash_before": before_digest,
        "hash_after": after_digest,
        "denial_detail": denial_text.unwrap_or_else(|| "no denial observed".to_string()),
    });
    denial_metrics["ms"] = json!(denial_started.elapsed().as_millis() as u64);

    // ---- gate 5: local stdio config parity -------------------------------
    gates.push(parity_gate);

    let metrics = json!({
        "spawn": { "calls": 1, "ms": spawn_ms },
        "handshake": {
            "method": "server/discover",
            "calls": 1,
            "ms": handshake_ms,
            "soft_threshold_ms": HANDSHAKE_SOFT_MS,
            "hard_threshold_ms": HANDSHAKE_HARD_MS,
            "soft_exceeded": soft_exceeded,
            "protocol": protocol,
            "server_name": server_name,
            "server_version": server_version,
        },
        "tools_list": list_metrics,
        "call_latency": latency_metrics,
        "read_only_denial": denial_metrics,
    });

    let result = envelope(
        &gates,
        &json!({
            "observed_at": observed,
            "server": server_context(binary, &vault, spawn_ms),
            "metrics": metrics,
            "config_parity": parity,
        }),
    );
    drop(client);
    let _ = std::fs::remove_dir_all(&temp);
    Ok(result)
}

fn server_context(binary: &Path, vault: &Path, spawn_ms: u64) -> Value {
    json!({
        "binary": binary.to_string_lossy(),
        "transport": "stdio",
        "profile": PROFILE,
        "read_only": true,
        "vault": "<isolated temp vault>",
        "vault_parent": vault.parent().map(|p| p.to_string_lossy()),
        "spawn_ms": spawn_ms,
    })
}

/// `mn bench mcp-verify` — non-zero when any gate failed or could not run.
pub fn run(_ctx: &AppContext, args: &McpVerifyArgs) -> Result<Outcome> {
    let binary = args
        .server
        .clone()
        .or_else(discover_server)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no memnoth-mcp binary found; build `crates/memnoth-mcp` or set MEMNOTH_MCP_BIN"
            )
        })?;
    let data = verify(&binary)?;
    let gate = data["gate"].as_str().unwrap_or("fail").to_string();
    let passed = data["summary"]["gates_passed"].as_u64().unwrap_or(0);
    let total = data["summary"]["gates_total"].as_u64().unwrap_or(0);
    let tools = data["metrics"]["tools_list"]["tools"].as_u64().unwrap_or(0);
    let handshake = data["metrics"]["handshake"]["ms"].as_u64().unwrap_or(0);
    let p50 = data["metrics"]["call_latency"]["p50_ms"]
        .as_f64()
        .unwrap_or(0.0);
    let p95 = data["metrics"]["call_latency"]["p95_ms"]
        .as_f64()
        .unwrap_or(0.0);
    let written = data["metrics"]["read_only_denial"]["bytes_written"]
        .as_u64()
        .unwrap_or(0);
    let text = format!(
        "mcp derived diagnostic — gate {gate} ({passed}/{total} gates); handshake={handshake} ms, \
         tools={tools}, read p50={p50} ms / p95={p95} ms, bytes_written={written} \
         (derived, NOT an official benchmark score)",
        gate = gate
    );
    let mut outcome = Outcome::new("bench.mcp-verify", text, data);
    if gate != "pass" {
        outcome.exit_code = ExitCode::Unexpected as u8;
    }
    Ok(outcome)
}
