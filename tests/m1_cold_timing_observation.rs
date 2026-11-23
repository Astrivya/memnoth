//! M1 gate OBSERVATION — a hermetic, timed cold first-cited-result run.
//!
//! This is an **observation**, not a benchmark. It runs the *built binary*
//! against a genuinely fresh HOME/vault/graph in a fresh working directory,
//! with every cloud/API environment variable removed, and times each step of
//! the offline "why is this code like this?" story:
//!
//!   1. `mn vault init`                  — create the vault from nothing
//!   2. `mn vault remember`              — record a belief
//!   3. `mn vault asof --at/--knew`      — read it back through both clocks
//!   4. `mn vault contradictions --name` — the contradiction query path
//!   5. `mn demo --json`                 — the five-part cited result
//!
//! The gate is deliberately honest: the run is `n=1`, a single process on a
//! single machine, and is **NOT a median across clients**. It measures a cold
//! first cited result, not a warm cache and not a fleet distribution. The
//! artifact below records that label explicitly.
//!
//! The test FAILS if any step fails, if the five-part story is incomplete, or
//! if the total is not strictly under five minutes. A dated JSON observation
//! artifact (schema `mn-gate-m1/1`) is emitted to `MN_M1_OBSERVATION_OUT` when
//! set, otherwise to a dated file in the OS temp dir, and is printed on stdout
//! so a single focused run carries the evidence with it.
//!
//! No network is required or used: demo and the vault commands build no cloud
//! client, and the child environment has the cloud URL and all provider keys
//! stripped before every spawn.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Five minutes, in milliseconds. The gate is strict: the total must be
/// **less than** this, not equal to it.
const FIVE_MINUTES_MS: u64 = 5 * 60 * 1_000;

const SCHEMA: &str = "mn-gate-m1/1";

/// A single observation, never a distribution. Stated in the artifact too.
const SAMPLE_LABEL: &str =
    "n=1 single local process, cold fresh state; NOT a median across clients";

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

// --------------------------------------------------------------------- fixture

struct Fixture {
    // Held for the whole test; dropping it removes the fresh state.
    _dir: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("allocate a fresh temp dir");
        let home = dir.path().join("home");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&home).expect("create fresh HOME");
        std::fs::create_dir_all(&workspace).expect("create fresh workspace");

        // The observation is only honest if the state really started empty.
        assert!(
            !home.join("vault.db").exists(),
            "fixture vault must start absent"
        );
        assert!(
            !workspace.join(".memnoth").join("graph.db").exists(),
            "fixture graph must start absent"
        );

        Fixture {
            _dir: dir,
            home,
            workspace,
        }
    }
}

/// Run one binary step against the fresh fixture with cloud/API env removed,
/// returning its output and the wall-clock milliseconds it took.
fn timed(fx: &Fixture, args: &[&str]) -> (Output, u64) {
    let mut cmd = Command::new(binary());
    cmd.args(args)
        .current_dir(&fx.workspace)
        // Fresh state: state dir, vault and graph all point inside the fixture.
        .env("MEMNOTH_HOME", &fx.home)
        .env_remove("MEMNOTH_VAULT")
        .env_remove("MEMNOTH_GRAPH")
        // Offline: no control plane, no provider keys, no inherited identity.
        .env_remove("MEMNOTH_CLOUD_URL")
        .env_remove("MEMNOTH_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("KILO_API_KEY")
        .env_remove("KILOCODE_API_KEY")
        .env("HOME", &fx.home)
        .env("USERPROFILE", &fx.home);

    let start = Instant::now();
    let out = cmd.output().expect("spawn the built memnoth binary");
    let ms = start.elapsed().as_millis() as u64;
    (out, ms)
}

fn snippet(bytes: &[u8], limit: usize) -> String {
    let text = String::from_utf8_lossy(bytes).replace(['\r', '\n'], " ");
    if text.chars().count() > limit {
        format!("{}…", text.chars().take(limit).collect::<String>())
    } else {
        text
    }
}

fn step_row(name: &str, ms: u64, ok: bool, out: &Output) -> Value {
    json!({
        "name": name,
        "ms": ms,
        "ok": ok,
        "exit_code": out.status.code(),
        "stdout": snippet(&out.stdout, 160),
        "stderr": snippet(&out.stderr, 160),
    })
}

// --------------------------------------------------------------- story checks

fn nonempty(value: &Value) -> bool {
    value
        .as_str()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

fn state_is(side: &Value, expected: &str) -> bool {
    side.get("state").and_then(Value::as_str) == Some(expected)
}

/// Presence of the five elements M1 promises, checked without panicking so the
/// artifact can record an incomplete story before the test fails it.
fn story_elements(data: &Value) -> Value {
    let sides = data["contradiction"]["sides"].as_array();
    let side_count = sides.map(Vec::len).unwrap_or(0);
    let sides_ok = sides
        .map(|rows| {
            rows.len() == 2
                && rows.iter().all(|side| {
                    nonempty(&side["label"])
                        && nonempty(&side["belief"])
                        && nonempty(&side["believed_at"]["rfc3339"])
                })
                && rows.iter().any(|side| state_is(side, "current"))
                && rows.iter().any(|side| state_is(side, "revoked"))
        })
        .unwrap_or(false);

    json!({
        // 1. the decision, and 2. when it was believed
        "decision": nonempty(&data["decision"]["belief"]),
        "believed_at": nonempty(&data["decision"]["believed_at"]["rfc3339"]),
        // 3. what superseded it, with the reason
        "superseded_by": nonempty(&data["superseded_by"]["belief"])
            && nonempty(&data["superseded_by"]["reason"]),
        // 4. the commit/source that introduced it
        "source": nonempty(&data["source_commit"]["entity"])
            && nonempty(&data["source_commit"]["message"]),
        // 5. both sides of a contradiction, each with its own date
        "contradiction_sides": side_count,
        "contradiction_sides_complete": sides_ok,
        "dates": {
            "decision": data["decision"]["believed_at"]["rfc3339"],
            "superseded": data["superseded_by"]["at"]["rfc3339"],
            "side_a": data["contradiction"]["sides"][0]["believed_at"]["rfc3339"],
            "side_b": data["contradiction"]["sides"][1]["believed_at"]["rfc3339"],
        },
    })
}

fn story_is_complete(elements: &Value) -> bool {
    elements["decision"] == true
        && elements["believed_at"] == true
        && elements["superseded_by"] == true
        && elements["source"] == true
        && elements["contradiction_sides_complete"] == true
}

// ------------------------------------------------------------------- artifacts

/// UTC `YYYY-MM-DD` and `YYYY-MM-DDTHH:MM:SSZ` without a date crate.
fn utc_now() -> (String, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);

    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };

    let date = format!("{year:04}-{month:02}-{day:02}");
    let stamp = format!(
        "{date}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    );
    (stamp, date)
}

fn observation_path(date: &str) -> PathBuf {
    if let Some(path) = std::env::var_os("MN_M1_OBSERVATION_OUT") {
        return PathBuf::from(path);
    }
    std::env::temp_dir().join(format!("mn-gate-m1-observation-{date}.json"))
}

fn machine_context() -> Value {
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "family": std::env::consts::FAMILY,
        "cpu_count": std::thread::available_parallelism().ok().map(|n| n.get()),
        "host": std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .ok(),
        "binary": "memnoth",
        "binary_version": env!("CARGO_PKG_VERSION"),
        "offline": true,
        "offline_basis": "cloud URL and all provider keys removed from every child process; demo builds no cloud client",
    })
}

// --------------------------------------------------------------------- the test

#[test]
fn m1_cold_first_cited_result_observation() {
    let fx = Fixture::new();
    let (observed_at, date) = utc_now();

    let mut steps: Vec<Value> = Vec::new();
    let mut problems: Vec<String> = Vec::new();

    // 1. vault init — from a vault that did not exist a moment ago.
    let (out, ms) = timed(&fx, &["vault", "init"]);
    let ok = out.status.success();
    if !ok {
        problems.push(format!("vault init failed: {}", snippet(&out.stderr, 200)));
    }
    if !fx.home.join("vault.db").is_file() {
        problems.push("vault init did not create a fresh vault.db".to_string());
    }
    steps.push(step_row("vault init", ms, ok, &out));

    // 2. vault remember — write a belief into the fresh vault.
    let (out, ms) = timed(
        &fx,
        &[
            "vault",
            "remember",
            "cold-probe",
            "--kind",
            "decision",
            "we chose SQLite for the local vault",
        ],
    );
    let ok = out.status.success();
    if !ok {
        problems.push(format!(
            "vault remember failed: {}",
            snippet(&out.stderr, 200)
        ));
    }
    steps.push(step_row("vault remember", ms, ok, &out));

    // 3. vault asof — read it back through valid time AND recorded time.
    // Epoch milliseconds are one of the three accepted --at/--knew forms.
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let day_ms = 86_400_i64;
    let at = (now_ms + 10 * day_ms).to_string();
    let knew = (now_ms + 5 * day_ms).to_string();
    let (out, ms) = timed(
        &fx,
        &["--json", "vault", "asof", "--at", &at, "--knew", &knew],
    );
    let ok = out.status.success();
    let asof_ok = ok
        && serde_json::from_slice::<Value>(&out.stdout)
            .map(|v| v["ok"] == true && v.get("data").is_some())
            .unwrap_or(false);
    if !asof_ok {
        problems.push(format!(
            "vault asof --at/--knew failed: {}",
            snippet(&out.stderr, 200)
        ));
    }
    steps.push(step_row("vault asof --at/--knew", ms, asof_ok, &out));

    // 4. vault contradictions --name — the named contradiction query path.
    let (out, ms) = timed(
        &fx,
        &["--json", "vault", "contradictions", "--name", "cold-probe"],
    );
    let ok = out.status.success();
    if !ok {
        problems.push(format!(
            "vault contradictions failed: {}",
            snippet(&out.stderr, 200)
        ));
    }
    steps.push(step_row("vault contradictions --name", ms, ok, &out));

    // 5. demo --json — the five-part cited result, offline, from this fresh
    // vault. This is the "first cited result" whose cold latency is observed.
    let (out, ms) = timed(&fx, &["demo", "--json"]);
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| json!({}));
    let demo_ok = out.status.success()
        && envelope["ok"] == true
        && envelope["command"] == "demo"
        && envelope.get("data").is_some();
    let elements = story_elements(&envelope["data"]);
    let complete = demo_ok && story_is_complete(&elements);
    if !demo_ok {
        problems.push(format!("demo --json failed: {}", snippet(&out.stderr, 200)));
    }
    if !story_is_complete(&elements) {
        problems.push(format!("M1 story incomplete; missing elements: {elements}"));
    }
    steps.push(step_row(
        "demo --json (five-part story)",
        ms,
        complete,
        &out,
    ));

    // Total is the sum of the five measured command steps (the end-to-end
    // cold path). The budget check is strict: under, never at, five minutes.
    let total_ms: u64 = steps
        .iter()
        .map(|row| row["ms"].as_u64().unwrap_or(0))
        .sum();
    let under_budget = total_ms < FIVE_MINUTES_MS;
    if !under_budget {
        problems.push(format!(
            "cold first cited result took {total_ms} ms; budget is < {FIVE_MINUTES_MS} ms"
        ));
    }

    // The observation artifact. Emitted before the assertions so even a
    // failing observation leaves its dated evidence behind.
    let step_count = steps.len();
    let artifact = json!({
        "schema": SCHEMA,
        "label": SAMPLE_LABEL,
        "observed_at": observed_at,
        "gate": {
            "name": "M1 cold first cited result",
            "result": if problems.is_empty() { "pass" } else { "fail" },
            "budget_ms": FIVE_MINUTES_MS,
            "budget_is_strict": true,
            "under_budget": under_budget,
            "story_complete": complete,
        },
        "sample": {
            "n": 1,
            "is_median": false,
            "across_clients": false,
            "note": SAMPLE_LABEL,
        },
        "story": {
            "complete": complete,
            "elements": elements,
        },
        "timing": {
            "total_ms": total_ms,
            "measured_steps": step_count,
            "steps": steps,
        },
        "context": machine_context(),
        "freshness": {
            "home": fx.home.display().to_string(),
            "workspace": fx.workspace.display().to_string(),
            "vault_started_absent": true,
            "graph_started_absent": true,
        },
        "problems": problems.clone(),
    });

    let path = observation_path(&date);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let rendered = serde_json::to_string_pretty(&artifact).unwrap();
    if let Err(err) = std::fs::write(&path, &rendered) {
        problems.push(format!(
            "could not write observation artifact to {}: {err}",
            path.display()
        ));
    }

    println!("{rendered}");
    eprintln!("M1 observation artifact: {}", path.display());

    // The gate: fail on an incomplete story or an over-budget cold run.
    assert!(complete, "M1 story incomplete: {}", problems.join("; "));
    assert!(
        under_budget,
        "cold first cited result took {total_ms} ms, expected < {FIVE_MINUTES_MS} ms"
    );
    assert!(
        problems.is_empty(),
        "M1 observation had failures: {}",
        problems.join("; ")
    );
}
