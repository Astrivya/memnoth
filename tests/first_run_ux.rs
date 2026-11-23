//! First-run and empty-state UX for `mn doctor`, against the real binary.
//!
//! Doctor is the `git status` of the CLI. A first run must never end in a bare
//! "missing": each non-Ok check that has a local remedy names the one command
//! that unblocks it, the report names a single overall next step, and none of
//! it asks for an account or claims a cloud session. Sessions execute on the
//! user's machine; there is no hosted session to point at.
//!
//! Every run gets its own workspace and `MEMNOTH_HOME`, so nothing here reads
//! or writes a real vault, credential store or MCP client config.

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
        std::fs::create_dir_all(dir.path().join("ws/src")).expect("workspace");
        std::fs::write(
            dir.path().join("ws/src/lib.rs"),
            "pub fn add(a: u32, b: u32) -> u32 { a + b }\n",
        )
        .expect("source file");
        Self { dir }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("ws")
    }

    /// Run the built binary with an isolated home and no model keys, so the
    /// report describes this fixture rather than the developer's machine.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(binary())
            .arg("--trust-folder")
            .args(args)
            .arg("--root")
            .arg(self.root())
            .env("MEMNOTH_HOME", self.home())
            .env("HOME", self.home())
            .env("USERPROFILE", self.home())
            .env("MEMNOTH_COMPLETION_HOME", self.home())
            .env_remove("MEMNOTH_VAULT")
            .env_remove("MEMNOTH_GRAPH")
            .env_remove("MEMNOTH_MCP_BIN")
            .env_remove("MEMNOTH_MCP_TOKEN")
            .env_remove("MEMNOTH_API_KEY")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            // No shell means no completion writes into a real dotfile.
            .env_remove("SHELL")
            .output()
            .expect("spawn memnoth")
    }
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn data(output: &Output) -> Value {
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not a JSON envelope ({error}):\n{}\nstderr:\n{}",
            text(output),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(envelope["ok"], true, "{envelope}");
    envelope["data"].clone()
}

/// Verbs the binary actually parses. A next step that names one of these can
/// be typed; a next step that is only prose ("missing", "configure a key")
/// cannot. `doctor --fix` and `--trust-folder` are real flags on real verbs.
const VERBS: &[&str] = &[
    "vault",
    "kg",
    "mcp",
    "connect",
    "auth",
    "doctor",
    "completion",
    "session",
    "ask",
    "setup",
    "config",
];

fn names_real_verb(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    let binary = matches!(words.first(), Some(&"mn" | &"memnoth"));
    let verb = words.get(1).is_some_and(|verb| VERBS.contains(verb));
    binary && verb
}

/// The top-level step plus every per-check `next_step`, as strings.
fn next_steps(data: &Value) -> Vec<String> {
    let mut steps = Vec::new();
    if let Some(step) = data["next_step"].as_str() {
        steps.push(step.to_string());
    }
    if let Some(checks) = data["checks"].as_array() {
        for check in checks {
            if let Some(step) = check["next_step"].as_str() {
                steps.push(step.to_string());
            }
        }
    }
    steps
}

#[test]
fn an_empty_home_names_an_actionable_next_step_with_a_real_verb() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["doctor", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let report = data(&output);

    // One overall next command, and it is typeable — not a dead end.
    let top = report["next_step"]
        .as_str()
        .expect("empty home produced no next_step");
    assert!(names_real_verb(top), "not a real verb: {top}");

    let steps = next_steps(&report);
    assert!(!steps.is_empty(), "no actionable next steps in doctor JSON");
    assert!(
        steps.iter().any(|step| names_real_verb(step)),
        "no next step names a real verb: {steps:?}"
    );

    // The first thing a fresh machine needs is a vault. The hint is tied to a
    // check that is actually non-Ok, and names the command that creates it.
    let checks = report["checks"].as_array().expect("checks array");
    let vault = checks
        .iter()
        .find(|check| check["name"] == "vault")
        .expect("vault check");
    assert_ne!(
        vault["state"], "ok",
        "a fresh home cannot have a ready vault"
    );
    assert_eq!(vault["next_step"], "mn vault init");
    assert_eq!(report["vault"]["initialised"], false);

    // No check that is green invents a next step.
    for check in checks {
        if check.get("next_step").is_some() {
            assert_ne!(check["state"], "ok", "an Ok check got a next step: {check}");
        }
    }
}

#[test]
fn doctor_never_claims_a_cloud_session() {
    let sandbox = Sandbox::new();
    for args in [&["doctor"][..], &["doctor", "--json"][..]] {
        let output = sandbox.run(args);
        assert_eq!(output.status.code(), Some(0), "{}", text(&output));
        let combined = format!(
            "{}{}",
            text(&output),
            String::from_utf8_lossy(&output.stderr)
        );
        let lower = combined.to_lowercase();
        for claim in [
            "cloud session",
            "cloud sessions",
            "hosted session",
            "session runs in the cloud",
            "sessions run in the cloud",
            "cloud-hosted session",
        ] {
            assert!(
                !lower.contains(claim),
                "doctor claimed a cloud session ({claim}):\n{combined}"
            );
        }
    }
}

#[test]
fn a_vault_ready_report_still_names_a_next_command() {
    let sandbox = Sandbox::new();
    let init = sandbox.run(&["vault", "init"]);
    assert_eq!(
        init.status.code(),
        Some(0),
        "vault init failed:\n{}",
        text(&init)
    );
    assert!(
        sandbox.home().join("vault.db").is_file(),
        "vault init created no vault"
    );

    let output = sandbox.run(&["doctor", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let report = data(&output);
    assert_eq!(report["vault"]["initialised"], true);

    let next = report["next_step"]
        .as_str()
        .expect("vault-ready doctor named no next step");
    assert!(names_real_verb(next), "not a real verb: {next}");
    // The graph is still unbuilt, and building it is local: no sign-in prompt
    // is allowed between a fresh vault and a usable code graph.
    assert_eq!(next, "mn kg index");
    assert!(!next.to_lowercase().contains("login"));
}
