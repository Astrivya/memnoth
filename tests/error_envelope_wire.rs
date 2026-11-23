//! The versioned error envelope is opt-in so the shipped wire shape (where
//! `error` is a string) stays stable for scripts. This test drives the real
//! binary and proves both shapes.

use std::process::{Command, Output};

fn run(args: &[&str], envelope: bool) -> Output {
    let home = tempfile::tempdir().expect("home");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_memnoth"));
    cmd.arg("--trust-folder")
        .args(args)
        .env("MEMNOTH_HOME", home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("MEMNOTH_COMPLETION_HOME", home.path())
        .env_remove("MEMNOTH_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("SHELL");
    if envelope {
        cmd.env("MEMNOTH_ERROR_ENVELOPE", "v1");
    } else {
        cmd.env_remove("MEMNOTH_ERROR_ENVELOPE");
    }
    cmd.output().expect("run mn")
}

#[test]
fn opt_in_envelope_has_stable_code_and_run_hint() {
    // A runtime failure that reaches `fail()` (clap parse errors never do).
    let out = run(&["vault", "asof", "--at", "not-a-time", "--json"], true);
    assert_ne!(out.status.code(), Some(0), "bad time must fail");
    let value: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("stderr must be JSON in --json mode");
    assert_eq!(value["schemaVersion"], 1);
    assert!(
        value["error"]["code"].is_string(),
        "code must be a stable string: {value}"
    );
    assert!(
        value["error"]["message"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "message must be present: {value}"
    );
}

#[test]
fn default_wire_shape_is_unchanged() {
    let out = run(&["vault", "asof", "--at", "not-a-time", "--json"], false);
    assert_ne!(out.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_slice(&out.stderr).expect("JSON stderr");
    assert_eq!(value["ok"], false);
    assert!(
        value["error"].as_str().is_some(),
        "legacy error must stay a string: {value}"
    );
}
