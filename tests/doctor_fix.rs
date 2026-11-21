//! `mn doctor --fix` — the F-06 / definition-of-done contract.
//!
//! Four properties are promised: every check is a typed
//! `Ok | Warn { fixable } | Fail { fixable }` result, a second run with no
//! changes does nothing, `--dry-run` applies nothing, and a `.bak` is made
//! before a file is overwritten. The first three are exercised here against
//! the real binary. The fourth is covered by the existing unit test
//! `doctor::bundle_tests::backups_never_overwrite_an_earlier_one`, which proves
//! `backup_if_exists` preserves the pre-overwrite bytes and never clobbers an
//! older backup; `Fix::Config` and `path_fix` both call it before writing.
//!
//! Every test gets its own `MEMNOTH_HOME` and workspace, so nothing reads or
//! writes the developer's real vault.

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("ws/src")).unwrap();
        std::fs::write(
            dir.path().join("ws/src/lib.rs"),
            "pub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n",
        )
        .unwrap();
        Self { dir }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("ws")
    }

    /// Run `memnoth` with an isolated home. The trust gate refuses a headless
    /// folder; these tests exercise the fixer, not the gate, so they opt in for
    /// one run and write nothing.
    fn run(&self, args: &[&str]) -> Output {
        let root = self.root();
        let root = root.to_str().expect("utf8 workspace path");
        Command::new(binary())
            .arg("--trust-folder")
            .args(args)
            .arg("--root")
            .arg(root)
            .env("MEMNOTH_HOME", self.home())
            // MCP clients and the credential/completion paths resolve from the
            // user home; isolate them so a real `~/.zed/settings.json` cannot
            // make `--fix` fail on a file this test never created.
            .env("USERPROFILE", self.home())
            .env("HOME", self.home())
            .env_remove("MEMNOTH_VAULT")
            .env_remove("MEMNOTH_GRAPH")
            .env_remove("MEMNOTH_MCP_BIN")
            .env_remove("MEMNOTH_MCP_TOKEN")
            // Deterministic shell detection: no shell means no completion writes.
            .env_remove("SHELL")
            .output()
            .expect("spawn memnoth")
    }
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON ({error}):\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `Ok | Warn { fixable } | Fail { fixable }` in the JSON envelope: every row
/// carries a state from the closed set and a `fixable` flag.
#[test]
fn every_check_reports_a_state_and_a_fixable_flag() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["doctor", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));

    let checks = json(&output)["data"]["checks"].as_array().unwrap().clone();
    assert!(!checks.is_empty(), "doctor emitted no checks");
    for check in &checks {
        let state = check["state"].as_str().unwrap_or("<missing>");
        assert!(
            matches!(state, "ok" | "warn" | "fail"),
            "unexpected state in {check}"
        );
        assert!(check["fixable"].is_boolean(), "fixable missing in {check}");
        assert!(check["name"].as_str().is_some(), "name missing in {check}");
    }
}

/// `--dry-run` prints the plan, applies nothing, and leaves every check in the
/// same state. Files the fixes would create are still absent afterwards.
#[test]
fn dry_run_applies_no_fixes_and_changes_no_state() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["doctor", "--fix", "--dry-run", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));

    let data = json(&output)["data"].clone();
    assert_eq!(data["dry_run"], true);
    assert_eq!(
        data["fixed"].as_array().unwrap().len(),
        0,
        "dry-run applied a fix"
    );
    assert!(
        !data["would_fix"].as_array().unwrap().is_empty(),
        "a fresh workspace must have a plan"
    );
    assert_eq!(
        data["before"], data["checks"],
        "dry-run changed a check state"
    );

    assert!(
        !sandbox.home().join("config.toml").exists(),
        "dry-run wrote config.toml"
    );
    assert!(
        !sandbox.home().join("vault.db").exists(),
        "dry-run created the vault"
    );
    assert!(
        !sandbox.root().join(".memnoth/graph.db").exists(),
        "dry-run built the graph"
    );
}

/// Idempotency: the first run fixes the common failures; the second run
/// applies nothing and leaves the check set byte-identical to the first run's
/// post-fix set.
#[test]
fn a_second_fix_run_applies_nothing() {
    let sandbox = Sandbox::new();

    let first = sandbox.run(&["doctor", "--fix", "--json"]);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first));
    let first = json(&first)["data"].clone();
    assert!(
        !first["fixed"].as_array().unwrap().is_empty(),
        "the first run fixed nothing: {first}"
    );

    let second = sandbox.run(&["doctor", "--fix", "--json"]);
    assert_eq!(second.status.code(), Some(0), "{}", text(&second));
    let second = json(&second)["data"].clone();
    assert_eq!(
        second["fixed"].as_array().unwrap().len(),
        0,
        "the second run reapplied a fix: {second}"
    );
    assert_eq!(
        second["before"], second["checks"],
        "the second run changed a check state"
    );
    assert_eq!(
        first["checks"], second["checks"],
        "the check set drifted between runs"
    );
}
