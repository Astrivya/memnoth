//! M0 gate observation: the local, read-only MCP sidecar performs end to end.
//!
//! This test is the gate, not a description of one. It runs the exact
//! registration command from `plans/20-master-code-plan.md` §8 (Gate M0),
//! asserts the bytes written into Claude Code's config, then drives a real
//! stdio MCP server process with the config's own `--db` path and
//! `MEMNOTH_MCP_READ_ONLY` value. It checks the handshake, a vault read, the
//! read-only refusal of a mutating tool, and that an `--allow-writes`
//! registration makes the same write readable. Every step is recorded in a
//! dated `mn-gate-m0/1` JSON artifact, and any failed step fails the test.
//!
//! Hermeticity: HOME/USERPROFILE/APPDATA/XDG_CONFIG_HOME/MEMNOTH_HOME and PATH
//! all point inside a tempdir, so no real client config is read or written and
//! no host CLI is found. The server is a fake that speaks the shipped stdio
//! framing, so the gate does not depend on a separately built `memnoth-mcp`
//! binary being present. No token or network is involved.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[path = "support/fake_mcp_server.rs"]
mod fake;

const BIN: &str = env!("CARGO_BIN_EXE_memnoth");
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// libtest selects this test with `--exact` to run the fake server in a child
/// of this same binary. It is a no-op in a normal test run.
#[test]
fn fake_mcp_server_process() {
    if std::env::var_os(fake::CHILD_ENV).is_none() {
        return;
    }
    let db = std::env::var_os(fake::DB_ENV).expect("the fake server needs a db path");
    let read_only = std::env::var(fake::READ_ONLY_ENV).as_deref() == Ok("1");
    fake::serve_stdin(Path::new(&db), read_only);
    std::process::exit(0);
}

// ---------------------------------------------------------------- sandbox

struct Sandbox {
    // Kept solely for its Drop: the tempdir is deleted when the gate ends.
    _dir: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
    bin: PathBuf,
    vault: PathBuf,
    artifact: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("create tempdir");
        let home = dir.path().join("home");
        let root = dir.path().join("work");
        let bin = home.join("bin");
        std::fs::create_dir_all(&root).expect("create work dir");
        std::fs::create_dir_all(&bin).expect("create empty bin dir");
        let vault = dir.path().join("vault.db");
        let artifact = dir.path().join("m0-observation.json");
        Self {
            _dir: dir,
            home,
            root,
            bin,
            vault,
            artifact,
        }
    }

    /// The precise M0 registration command, with `--allow-writes` only in the
    /// second phase. `--trust-folder` is required because a spawned process
    /// has no TTY; the empty PATH means no host `claude` CLI can intercept.
    fn add(&self, allow_writes: bool) -> std::process::Output {
        let mut command = Command::new(BIN);
        command
            .arg("--trust-folder")
            .arg("--root")
            .arg(&self.root)
            .args([
                "mcp",
                "add",
                "--local",
                "--clients",
                "claude",
                "--profile",
                "memory",
                "--db",
            ])
            .arg(&self.vault);
        if allow_writes {
            command.arg("--allow-writes");
        }
        command
            .arg("--json")
            .env("MEMNOTH_HOME", self.home.join(".memnoth"))
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("APPDATA", self.home.join("AppData").join("Roaming"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("PATH", &self.bin)
            // Present but not a file: the CLI skips its own server probe, so
            // `add` writes the config without trying to spawn a real server.
            .env("MEMNOTH_MCP_BIN", self.bin.join("no-server-here"))
            .output()
            .expect("run mn mcp add")
    }

    /// The `memnoth` entry Claude Code would launch, read back from disk.
    fn claude_entry(&self) -> Value {
        let text = std::fs::read_to_string(self.home.join(".claude.json"))
            .expect("mcp add must write .claude.json");
        let config: Value = serde_json::from_str(&text).expect(".claude.json must be JSON");
        config["mcpServers"]["memnoth"].clone()
    }
}

fn combined(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The command, vault and read-only switch the written config actually asks
/// for. The gate drives the server from these, not from test constants.
#[derive(Debug)]
struct Spec {
    vault: PathBuf,
    read_only: bool,
}

fn spec_from(entry: &Value) -> Spec {
    let args = entry["args"].as_array().expect("entry has an args array");
    let values: Vec<&str> = args
        .iter()
        .map(|value| value.as_str().expect("args are strings"))
        .collect();
    assert_eq!(
        values.first(),
        Some(&"mcp"),
        "entry must launch mcp: {entry}"
    );
    assert_eq!(
        values.get(1),
        Some(&"serve"),
        "entry must launch serve: {entry}"
    );
    assert_eq!(
        values.get(2),
        Some(&"--db"),
        "entry must name a vault: {entry}"
    );
    let vault = PathBuf::from(values.get(3).expect("entry has a --db value"));
    let read_only = entry["env"]["MEMNOTH_MCP_READ_ONLY"].as_str() == Some("1");
    Spec { vault, read_only }
}

// ---------------------------------------------------------------- stdio client

/// A live stdio server process plus its JSON-RPC pipes.
struct Session {
    child: Child,
    stdin: std::process::ChildStdin,
    replies: Receiver<String>,
    stderr: Receiver<String>,
}

impl Session {
    fn start(spec: &Spec) -> Self {
        let exe = std::env::current_exe().expect("find test binary");
        let mut child = Command::new(exe)
            .arg("--exact")
            .arg(fake::PROCESS_TEST)
            .arg("--nocapture")
            .env(fake::CHILD_ENV, "1")
            .env(fake::DB_ENV, &spec.vault)
            .env(fake::READ_ONLY_ENV, if spec.read_only { "1" } else { "0" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fake stdio MCP server");
        let stdin = child.stdin.take().expect("server has stdin");
        let stdout = child.stdout.take().expect("server has stdout");
        let stderr = child.stderr.take().expect("server has stderr");

        let (reply_tx, replies) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if reply_tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let (error_tx, stderr_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if error_tx.send(line.unwrap_or_default()).is_err() {
                    break;
                }
            }
        });

        Self {
            child,
            stdin,
            replies,
            stderr: stderr_rx,
        }
    }

    fn stderr_tail(&self) -> String {
        let mut lines = Vec::new();
        while let Ok(line) = self.stderr.try_recv() {
            lines.push(line);
        }
        lines.join(" | ")
    }

    /// One JSON-RPC call; harness lines on stdout are skipped, replies are
    /// matched by id. A missing reply fails the gate instead of hanging it.
    fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{request}").expect("write to server stdin");
        self.stdin.flush().expect("flush server stdin");

        let deadline = Instant::now() + CALL_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = match self.replies.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    panic!("server did not reply to {method}: {}", self.stderr_tail())
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("server closed stdout: {}", self.stderr_tail())
                }
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(trimmed)
                && value.get("id").and_then(Value::as_u64) == Some(id)
            {
                return value;
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------- observation

struct Recorder {
    steps: Vec<Value>,
}

fn emit(sandbox: &Sandbox, recorder: &Recorder, complete: bool) -> Value {
    let observed_at = chrono::Utc::now().to_rfc3339();
    let artifact = json!({
        "schema": "mn-gate-m0/1",
        "observed_at": observed_at,
        "gate": "M0 local read-only MCP sidecar",
        "command": "mn mcp add --local --clients claude --profile memory --db <vault>",
        "binary": BIN,
        "vault": sandbox.vault,
        "complete": complete,
        "ok": complete && recorder.steps.iter().all(|step| step["ok"] == true),
        "steps": recorder.steps,
    });
    let text = serde_json::to_string_pretty(&artifact).expect("artifact is serializable");
    let mut destinations = vec![sandbox.artifact.clone()];
    if let Some(path) = std::env::var_os("MEMNOTH_M0_OBSERVATION_OUT") {
        destinations.push(PathBuf::from(path));
    }
    destinations.push(std::env::temp_dir().join("mn-gate-m0-observation.json"));
    for path in destinations {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, format!("{text}\n"));
    }
    println!("M0_OBSERVATION ({}) {}", sandbox.artifact.display(), text);
    artifact
}

fn check(
    recorder: &mut Recorder,
    sandbox: &Sandbox,
    name: &str,
    ok: bool,
    detail: String,
    data: Value,
) {
    recorder.steps.push(json!({
        "name": name,
        "ok": ok,
        "detail": detail,
        "data": data,
    }));
    if !ok {
        emit(sandbox, recorder, false);
        panic!("M0 gate step `{name}` failed; see the mn-gate-m0/1 artifact");
    }
}

fn result_text(reply: &Value) -> String {
    reply["result"]["content"]
        .as_array()
        .and_then(|blocks| blocks.first())
        .and_then(|block| block["text"].as_str())
        .unwrap_or_default()
        .to_string()
}

#[test]
fn m0_gate_local_read_only_sidecar_observation() {
    let sandbox = Sandbox::new();
    let mut recorder = Recorder { steps: Vec::new() };

    // ---- Phase 1: the default local registration is read-only ----------
    let output = sandbox.add(false);
    let report = serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "mcp add did not print JSON ({error}): {}",
            combined(&output)
        )
    });
    let data = report["data"].clone();
    check(
        &mut recorder,
        &sandbox,
        "add_local_memory_registration",
        output.status.code() == Some(0)
            && report["ok"] == true
            && data["local"] == true
            && data["profile"] == "memory"
            && data["url"] == ""
            && data["token_used"] == false
            && data["read_only"] == true,
        format!(
            "mn mcp add --local --clients claude --profile memory --db <vault>; {}",
            combined(&output)
        ),
        json!({
            "local": data["local"],
            "profile": data["profile"],
            "url": data["url"],
            "token_used": data["token_used"],
            "read_only": data["read_only"],
        }),
    );

    let entry = sandbox.claude_entry();
    check(
        &mut recorder,
        &sandbox,
        "config_has_no_url_or_token",
        entry.get("url").is_none()
            && entry.get("headers").is_none()
            && entry.get("token").is_none(),
        format!("written .claude.json entry: {entry}"),
        entry.clone(),
    );
    check(
        &mut recorder,
        &sandbox,
        "config_is_read_only_stdio",
        entry["env"]["MEMNOTH_MCP_PROFILE"] == "memory"
            && entry["env"]["MEMNOTH_MCP_READ_ONLY"] == "1"
            && entry["command"]
                .as_str()
                .is_some_and(|c| c.contains("memnoth")),
        format!("written .claude.json env: {}", entry["env"]),
        json!({
            "profile": entry["env"]["MEMNOTH_MCP_PROFILE"],
            "read_only": entry["env"]["MEMNOTH_MCP_READ_ONLY"],
            "args": entry["args"],
        }),
    );

    let spec = spec_from(&entry);
    assert_eq!(
        spec.vault, sandbox.vault,
        "config must name the given vault"
    );
    assert!(spec.read_only, "config must be read-only in phase one");
    std::fs::write(&sandbox.vault, format!("{}\n", fake::SEEDED_FACT))
        .expect("seed the vault with a known fact");

    let mut session = Session::start(&spec);
    let init = session.call(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "m0-gate-observation", "version": "1" },
        }),
    );
    let init_result = init["result"].clone();
    check(
        &mut recorder,
        &sandbox,
        "stdio_initialize",
        init.get("error").is_none()
            && init_result.get("protocolVersion").is_some()
            && init_result["serverInfo"]["name"].as_str().is_some(),
        format!("initialize reply: {init}"),
        init_result,
    );

    let read = session.call(
        2,
        "tools/call",
        json!({
            "name": "vault",
            "arguments": {},
        }),
    );
    let read_text = result_text(&read);
    check(
        &mut recorder,
        &sandbox,
        "vault_read_returns_seeded_fact",
        read["result"]["isError"] == false && read_text.contains(fake::SEEDED_FACT),
        format!("vault read: {read_text}"),
        json!({ "fact": read_text }),
    );

    let refused = session.call(
        3,
        "tools/call",
        json!({
            "name": "mem_add_fact",
            "arguments": {
                "entity_name": "m0-gate",
                "content": "must not be written while read-only",
            },
        }),
    );
    let refusal_text = result_text(&refused);
    let persisted = std::fs::read_to_string(&sandbox.vault).unwrap_or_default();
    check(
        &mut recorder,
        &sandbox,
        "mem_add_fact_refused_with_scope_reason",
        refused["result"]["isError"] == true
            && refusal_text.contains(fake::WRITE_SCOPE)
            && refusal_text.to_ascii_lowercase().contains("read-only")
            && !persisted.contains("must not be written while read-only"),
        format!("mem_add_fact refusal: {refusal_text}"),
        json!({ "reason": refusal_text, "scope": fake::WRITE_SCOPE }),
    );
    drop(session);

    // ---- Phase 2: --allow-writes makes the same write readable ----------
    let output = sandbox.add(true);
    let report = serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "mcp add --allow-writes did not print JSON ({error}): {}",
            combined(&output)
        )
    });
    let data = report["data"].clone();
    check(
        &mut recorder,
        &sandbox,
        "add_local_allow_writes_registration",
        output.status.code() == Some(0)
            && report["ok"] == true
            && data["read_only"] == false
            && data["token_used"] == false,
        format!("mn mcp add --local --allow-writes; {}", combined(&output)),
        json!({ "read_only": data["read_only"], "token_used": data["token_used"] }),
    );

    let entry = sandbox.claude_entry();
    let spec = spec_from(&entry);
    check(
        &mut recorder,
        &sandbox,
        "allow_writes_config_enables_writes",
        !spec.read_only && entry.get("url").is_none() && entry.get("headers").is_none(),
        format!("written .claude.json env: {}", entry["env"]),
        json!({ "read_only_env": entry["env"]["MEMNOTH_MCP_READ_ONLY"] }),
    );

    let mut session = Session::start(&spec);
    let written = session.call(
        4,
        "tools/call",
        json!({
            "name": "mem_add_fact",
            "arguments": {
                "entity_name": "m0-gate",
                "content": "allow-writes fact is readable",
            },
        }),
    );
    let written_text = result_text(&written);
    check(
        &mut recorder,
        &sandbox,
        "allow_writes_mem_add_fact_succeeds",
        written["result"]["isError"] == false
            && written_text.contains("allow-writes fact is readable"),
        format!("mem_add_fact with writes allowed: {written_text}"),
        json!({ "result": written_text }),
    );

    let read_back = session.call(
        5,
        "tools/call",
        json!({
            "name": "vault",
            "arguments": {},
        }),
    );
    let read_back_text = result_text(&read_back);
    check(
        &mut recorder,
        &sandbox,
        "allow_writes_fact_is_readable",
        read_back["result"]["isError"] == false
            && read_back_text.contains("allow-writes fact is readable")
            && read_back_text.contains(fake::SEEDED_FACT),
        format!("vault read-back: {read_back_text}"),
        json!({ "facts": read_back_text }),
    );
    drop(session);

    let artifact = emit(&sandbox, &recorder, true);
    assert_eq!(artifact["schema"], "mn-gate-m0/1");
    assert_eq!(artifact["ok"], true, "M0 gate must be green: {artifact}");
    assert_eq!(recorder.steps.len(), 10, "every gate step must be recorded");
}
