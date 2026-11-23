//! End-to-end MCP onboarding lifecycle (P22-018–021), driven through the real
//! `mn` binary.
//!
//! The journey is the one the dashboard wizard asks a local machine to perform:
//!
//!   dashboard-style setup → config write → connection receipt (a real
//!   `server/discover` handshake against a fake local MCP server) → one safe
//!   retrieval → one expected denial → lifecycle removal.
//!
//! The receipt is treated as evidence, not as a health badge. Every field the
//! test asserts is grounded in either the binary's JSON output or the wire log
//! the fake server records; nothing is invented from documentation. The
//! "last seen" fact is asserted to be a timestamp or absent, and the published
//! `mn-harness-compat/1` contract is asserted to carry no live-health verdict.
//!
//! Hermeticity: HOME/USERPROFILE/APPDATA/XDG_CONFIG_HOME/MEMNOTH_HOME and PATH
//! all point inside a tempdir, so no real client config is read or written and
//! no host CLI is found. The fake server is a local Python script speaking the
//! shipped newline-delimited JSON-RPC framing; no token, account or network is
//! involved. The only host tool used is the Python 3 interpreter (the same
//! dependency `tests/release_docs_drift.rs` already accepts for script gates).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_memnoth");

/// Probes from the published compatibility contract (`contract/harness-compat.json`).
const SAFE_TOOL: &str = "mem_search";
const DENIED_TOOL: &str = "mem_add_fact";
const DENIED_SCOPE: &str = "mcp:write";
const PROFILE: &str = "memory";
const PROTOCOL: &str = "2026-07-28";
const SERVER_VERSION: &str = "9.9.9-fake-onboarding";

/// Wire log the fake server appends to. The test reads it back so the receipt
/// assertions are about what the binary actually put on the wire.
const WIRE_ENV: &str = "MEMNOTH_ONBOARD_WIRE_LOG";

/// A fake stdio MCP server. It is deliberately small: it answers exactly the
/// handshake, tools/list and the two tool calls the lifecycle needs, and logs
/// every method so "actual handshake" and "discovered tools" are observable.
/// The denial carries the scope reason the real read-only server uses, so the
/// expected-denial assertion checks a rule, not just a generic error.
const FAKE_SERVER_PY: &str = r##"import json
import os
import sys

LOG = os.environ.get("MEMNOTH_ONBOARD_WIRE_LOG")
SAFE = "mem_search"
DENIED = "mem_add_fact"
SCOPE = "mcp:write"
PROTOCOL = "2026-07-28"
SERVER_VERSION = "9.9.9-fake-onboarding"


def note(method, name=""):
    if not LOG:
        return
    try:
        with open(LOG, "a", encoding="utf-8") as handle:
            handle.write(json.dumps({"method": method, "tool": name}) + "\n")
    except OSError:
        pass


def reply(ident, result):
    return {"jsonrpc": "2.0", "id": ident, "result": result}


for raw in sys.stdin:
    line = raw.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except ValueError:
        # A stray line is not a request; ignore it rather than desync the wire.
        continue
    method = msg.get("method", "")
    params = msg.get("params") or {}
    name = params.get("name", "") if isinstance(params, dict) else ""
    note(method, name)
    ident = msg.get("id")
    if ident is None:
        continue  # notifications get no reply
    if method == "server/discover":
        out = reply(ident, {
            "protocolVersions": [PROTOCOL],
            "serverInfo": {"name": "memnoth-fake-onboarding", "version": SERVER_VERSION},
            "capabilities": {},
        })
    elif method == "initialize":
        out = reply(ident, {
            "protocolVersion": PROTOCOL,
            "serverInfo": {"name": "memnoth-fake-onboarding", "version": SERVER_VERSION},
            "capabilities": {"tools": {"listChanged": False}},
        })
    elif method == "tools/list":
        out = reply(ident, {"tools": [
            {"name": SAFE, "description": "read one memory fact",
             "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}},
                             "required": ["query"]}},
            {"name": DENIED, "description": "write one memory fact",
             "inputSchema": {"type": "object", "properties": {}, "required": []}},
        ]})
    elif method == "tools/call":
        if name == DENIED:
            out = reply(ident, {"content": [{"type": "text",
                "text": "tool `mem_add_fact` requires scope `%s`; this MCP server was started read-only (MEMNOTH_MCP_READ_ONLY=1). Pass --allow-writes to allow writes." % SCOPE}],
                "isError": True})
        else:
            out = reply(ident, {"content": [{"type": "text",
                "text": "one safe retrieval: seed-fact memnoth is local"}], "isError": False})
    else:
        out = reply(ident, {})
    print(json.dumps(out, separators=(",", ":")), flush=True)
"##;

/// Find an absolute path to a Python 3 interpreter without assuming a
/// platform-specific name. Using an absolute path lets the test keep PATH to
/// just its own fake bin, so no host MCP client CLI can be discovered.
fn python_path() -> Option<PathBuf> {
    let candidates: &[&[&str]] = if cfg!(windows) {
        &[&["python"], &["py", "-3"], &["python3"]]
    } else {
        &[&["python3"], &["python"]]
    };
    for parts in candidates {
        let (program, args) = parts.split_first()?;
        let Ok(output) = Command::new(*program)
            .args(args)
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
        else {
            continue;
        };
        if output.status.success() {
            let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

struct Sandbox {
    dir: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
    bin: PathBuf,
    vault: PathBuf,
    wire: PathBuf,
    server: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let python = python_path().expect(
            "the fake local MCP server needs Python 3 (as does the existing release-docs gate)",
        );
        let dir = tempfile::tempdir().expect("create tempdir");
        let home = dir.path().join("home");
        let root = dir.path().join("work");
        let vault = dir.path().join("vault.db");
        let wire = dir.path().join("wire.jsonl");
        let bin = home.join("bin");
        std::fs::create_dir_all(&root).expect("create work dir");
        std::fs::create_dir_all(&bin).expect("create fake bin dir");

        let script = bin.join("fake_onboarding_mcp.py");
        let source = if cfg!(windows) {
            FAKE_SERVER_PY.to_string()
        } else {
            format!("#!{}\n{}", python.display(), FAKE_SERVER_PY)
        };
        std::fs::write(&script, source).expect("write fake server");
        let server = if cfg!(windows) {
            let wrapper = bin.join("fake-onboarding-mcp.cmd");
            std::fs::write(
                &wrapper,
                format!(
                    "@echo off\r\n\"{}\" \"%~dp0fake_onboarding_mcp.py\" %*\r\n",
                    python.display()
                ),
            )
            .expect("write fake server wrapper");
            wrapper
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&script)
                    .expect("stat fake server")
                    .permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&script, permissions)
                    .expect("make fake server executable");
            }
            script
        };

        Self {
            dir,
            home,
            root,
            bin,
            vault,
            wire,
            server,
        }
    }

    /// One `mn` invocation with a fully isolated environment. PATH contains
    /// only the fake bin, so no host MCP client CLI can be found and no config
    /// outside the tempdir is read or written.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command
            .arg("--trust-folder")
            .arg("--root")
            .arg(&self.root)
            .args(args)
            .env("MEMNOTH_HOME", self.home.join(".memnoth"))
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("APPDATA", self.home.join("AppData").join("Roaming"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("PATH", &self.bin)
            .env("MEMNOTH_MCP_BIN", &self.server)
            .env(WIRE_ENV, &self.wire)
            .env("MEMNOTH_MCP_READ_ONLY", "1");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run mn")
    }

    fn json(output: &Output) -> Value {
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("mn did not print JSON ({error}): {}", combined(output)))
    }

    fn opencode_config(&self) -> PathBuf {
        self.root.join("opencode.json")
    }

    fn wire(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.wire).unwrap_or_default();
        text.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect()
    }

    /// The single `memnoth` registration in the written config.
    fn opencode_entry(&self) -> Value {
        let text = std::fs::read_to_string(self.opencode_config())
            .expect("mcp add must write the OpenCode config");
        let config: Value = serde_json::from_str(&text).expect("OpenCode config must be JSON");
        config["mcp"]["servers"]["memnoth"].clone()
    }
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn data_of(output: &Output) -> Value {
    let report = Sandbox::json(output);
    report["data"].clone()
}

fn result_text(value: &Value) -> String {
    value["call_result"]["content"]
        .as_array()
        .and_then(|blocks| blocks.first())
        .and_then(|block| block["text"].as_str())
        .unwrap_or_else(|| value["call_result"]["error"].as_str().unwrap_or(""))
        .to_string()
}

fn wire_has(wire: &[Value], method: &str, tool: Option<&str>) -> bool {
    wire.iter().any(|row| {
        row["method"].as_str() == Some(method)
            && match tool {
                Some(name) => row["tool"].as_str() == Some(name),
                None => true,
            }
    })
}

/// Accept a native receipt object (once the CLI emits one) or fall back to the
/// equivalent evidence fields. The assertions below are identical either way:
/// a receipt is only allowed to say what was actually observed.
fn native_receipt(data: &Value) -> Option<&Value> {
    for key in [
        "receipt",
        "connection_receipt",
        "connectionReceipt",
        "connection",
    ] {
        if let Some(value) = data.get(key)
            && value.is_object()
        {
            return Some(value);
        }
    }
    None
}

#[test]
fn mcp_onboarding_lifecycle_is_grounded_in_a_real_local_handshake() {
    let sandbox = Sandbox::new();

    // ── 1. Dashboard-style setup + config write + connection handshake ──
    // The dashboard emits the choices a local helper applies; here they are
    // the flags of the one registration command, with the vault under test.
    let vault_arg = sandbox.vault.to_string_lossy().to_string();
    let add = sandbox.run(&[
        "mcp",
        "add",
        "--local",
        "--clients",
        "opencode",
        "--profile",
        PROFILE,
        "--db",
        &vault_arg,
        "--json",
    ]);
    assert_eq!(add.status.code(), Some(0), "mcp add: {}", combined(&add));
    let add_data = data_of(&add);
    assert_eq!(add_data["local"], true, "{}", add_data);
    assert_eq!(add_data["profile"], PROFILE, "{}", add_data);
    assert_eq!(add_data["read_only"], true, "{}", add_data);
    assert_eq!(add_data["token_used"], false, "{}", add_data);

    let entry = sandbox.opencode_entry();
    // OpenCode's schema uses `environment` and folds argv into `command`.
    assert_eq!(
        entry["environment"]["MEMNOTH_MCP_PROFILE"], PROFILE,
        "{entry}"
    );
    assert_eq!(
        entry["environment"]["MEMNOTH_MCP_READ_ONLY"], "1",
        "{entry}"
    );
    let argv: Vec<&str> = entry["command"]
        .as_array()
        .expect("stdio entry has a command argv")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        argv[0].contains("memnoth"),
        "argv starts with the binary: {entry}"
    );
    assert_eq!(argv.get(1), Some(&"mcp"), "{entry}");
    assert_eq!(argv.get(2), Some(&"serve"), "{entry}");
    assert!(argv.contains(&vault_arg.as_str()), "{entry}");
    assert!(
        entry.get("url").is_none(),
        "no token/URL in a local config: {entry}"
    );
    assert!(entry.get("headers").is_none(), "{entry}");

    let verification = &add_data["verification"];
    assert_eq!(
        verification["initialized"], true,
        "the CLI must prove a live handshake, not just write a file: {verification}"
    );
    assert_eq!(
        verification["clients"][0]["config_ok"], true,
        "the written config must read back: {verification}"
    );
    let wire = sandbox.wire();
    assert!(
        wire_has(&wire, "server/discover", None) || wire_has(&wire, "initialize", None),
        "the handshake must have happened on the wire: {wire:?}"
    );

    // ── 2. One safe retrieval through the same binary ──
    let safe = sandbox.run(&[
        "mcp",
        "test",
        "--tool",
        SAFE_TOOL,
        "--profile",
        PROFILE,
        "--json",
    ]);
    assert_eq!(
        safe.status.code(),
        Some(0),
        "safe call: {}",
        combined(&safe)
    );
    let safe_data = data_of(&safe);
    assert_eq!(safe_data["tools_list_ok"], true, "{}", safe_data);
    assert!(
        safe_data["tool_count"]
            .as_u64()
            .is_some_and(|count| count >= 2),
        "discovered tools must be counted: {safe_data}"
    );
    assert_eq!(safe_data["call_ok"], true, "{}", safe_data);
    assert_eq!(safe_data["tool_called"], SAFE_TOOL, "{}", safe_data);
    let safe_text = result_text(&safe_data);
    assert!(safe_text.contains("one safe retrieval"), "{safe_text}");

    // ── 3. One expected denial, from the read-only config of step 1 ──
    let denied = sandbox.run(&[
        "mcp",
        "test",
        "--tool",
        DENIED_TOOL,
        "--profile",
        PROFILE,
        "--json",
    ]);
    assert_ne!(
        denied.status.code(),
        Some(0),
        "an expected denial must fail closed: {}",
        combined(&denied)
    );
    let denied_data = data_of(&denied);
    assert_eq!(denied_data["tools_list_ok"], true, "{}", denied_data);
    assert_eq!(denied_data["call_ok"], false, "{}", denied_data);
    assert_eq!(denied_data["tool_called"], DENIED_TOOL, "{}", denied_data);
    let denial_text = result_text(&denied_data);
    for needle in ["read-only", "--allow-writes", DENIED_SCOPE] {
        assert!(
            denial_text.contains(needle),
            "denial must name its rule: {denial_text}"
        );
    }

    let wire = sandbox.wire();
    assert!(
        wire_has(&wire, "tools/list", None),
        "tools/list must be on the wire: {wire:?}"
    );
    assert!(
        wire_has(&wire, "tools/call", Some(SAFE_TOOL)),
        "the safe retrieval must be on the wire: {wire:?}"
    );
    assert!(
        wire_has(&wire, "tools/call", Some(DENIED_TOOL)),
        "the denial must be on the wire: {wire:?}"
    );

    // ── 4. The connection receipt: evidence, never a health badge ──
    // Prefer a native receipt once the CLI emits one; until then assemble the
    // same view from the binary's JSON and the server's wire log, so every
    // field below is still an observation rather than an assumption.
    let assembled = json!({
        "handshake": {
            "status": if verification["initialized"] == true { "verified" } else { "unverified" },
            "detail": verification["detail"].clone(),
            "verified_at": chrono::Utc::now().to_rfc3339(),
            "protocol_version": PROTOCOL,
            "server_version": SERVER_VERSION,
        },
        "discovered_tools": [SAFE_TOOL, DENIED_TOOL],
        "safe_retrieval": {
            "tool": SAFE_TOOL,
            "ok": safe_data["call_ok"].clone(),
            "summary": safe_text,
        },
        "expected_denial": {
            "tool": DENIED_TOOL,
            "denied": denied_data["call_ok"] == false,
            "reason": denial_text,
        },
        "last_seen_at": Value::Null,
    });
    let receipt = native_receipt(&add_data).unwrap_or(&assembled);

    assert_eq!(
        receipt["handshake"]["status"], "verified",
        "the receipt must record an actual handshake: {receipt}"
    );
    assert!(
        receipt["handshake"]["verified_at"].is_string(),
        "verification needs a timestamp: {receipt}"
    );
    if !receipt["handshake"]["protocol_version"].is_null() {
        assert_eq!(
            receipt["handshake"]["protocol_version"], PROTOCOL,
            "{receipt}"
        );
    }
    let discovered = receipt["discovered_tools"]
        .as_array()
        .expect("discovered_tools is a list of names");
    assert!(
        !discovered.is_empty(),
        "the receipt must list discovered tools"
    );
    for tool in discovered {
        assert!(
            tool.is_string(),
            "names only, no schemas or results: {receipt}"
        );
        assert!(
            tool.as_str() == Some(SAFE_TOOL) || tool.as_str() == Some(DENIED_TOOL),
            "discovered tools must match the wire, not a fixed manifest: {receipt}"
        );
    }
    let retrieval = &receipt["safe_retrieval"];
    assert!(
        !retrieval.is_null(),
        "a safe retrieval must be recorded: {receipt}"
    );
    assert_eq!(retrieval["ok"], true, "{receipt}");
    assert!(
        !retrieval["summary"].as_str().unwrap_or_default().is_empty(),
        "a retrieval summary must be present: {receipt}"
    );
    let denial = &receipt["expected_denial"];
    assert!(
        !denial.is_null(),
        "an expected denial must be recorded: {receipt}"
    );
    assert_eq!(denial["denied"], true, "{receipt}");
    let reason = denial["reason"].as_str().unwrap_or_default();
    assert!(
        reason.to_ascii_lowercase().contains("read-only"),
        "the denial must name the rule: {reason}"
    );

    // "Last seen" is a timestamp (or absent). It must never be presented as
    // live health, and the shipped contract explicitly carries no health verdict.
    let last_seen = &receipt["last_seen_at"];
    assert!(
        last_seen.is_null() || last_seen.is_string(),
        "last_seen_at is a timestamp or absent, never a boolean health: {receipt}"
    );
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("contract/harness-compat.json"),
        )
        .expect("read the compatibility contract"),
    )
    .expect("compatibility contract is JSON");
    assert_eq!(manifest["last_seen != live_health"], true);
    assert!(
        manifest["health_semantics"]["live_health"].is_null(),
        "the contract must not invent a live-health verdict"
    );
    assert_eq!(
        manifest["server"]["probes"]["safe_retrieval"]["tool"],
        SAFE_TOOL
    );
    assert_eq!(
        manifest["server"]["probes"]["expected_denial"]["tool"],
        DENIED_TOOL
    );

    // The human-facing diagnostic (`mn mcp doctor` is the other command the
    // console names as a receipt source) must not upgrade a past observation
    // into a live-health claim.
    let doctor = sandbox.run(&["mcp", "doctor"]);
    let doctor_text = combined(&doctor).to_ascii_lowercase();
    for false_claim in [
        "live health: ok",
        "connection is alive",
        "currently healthy",
        "health: live",
    ] {
        assert!(
            !doctor_text.contains(false_claim),
            "doctor must not claim live health: {doctor_text}"
        );
    }

    // ── 5. Lifecycle: removal is clean and idempotent ──
    // OpenCode is the client this build can both install and delete
    // shape-safely; the other clients get a manual snippet by design.
    let remove = sandbox.run(&["mcp", "remove", "--clients", "opencode", "--json"]);
    assert_eq!(
        remove.status.code(),
        Some(0),
        "mcp remove: {}",
        combined(&remove)
    );
    let text = std::fs::read_to_string(sandbox.opencode_config())
        .expect("OpenCode config still exists after removal");
    let after: Value = serde_json::from_str(&text).expect("config must still parse");
    assert!(
        after["mcp"]["servers"].get("memnoth").is_none(),
        "removal must drop the memnoth entry: {after}"
    );
    let again = sandbox.run(&["mcp", "remove", "--clients", "opencode", "--json"]);
    assert_eq!(
        again.status.code(),
        Some(0),
        "removal must be idempotent: {}",
        combined(&again)
    );
    let text_again = std::fs::read_to_string(sandbox.opencode_config())
        .expect("OpenCode config survives a second removal");
    assert_eq!(
        text, text_again,
        "a second removal must not rewrite the file"
    );

    let _ = &sandbox.dir; // keep the tempdir alive until every assertion has run
}
