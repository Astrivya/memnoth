//! `mn docs --check` holds the generated reference still.
//!
//! The registry is the documentation's source, so a committed file that has
//! drifted from it is a lie with a version number on it. These tests exercise
//! the three answers a CI step can get: match (0), stale (1), and nothing to
//! compare (2 or 4, never a silent pass).

use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

fn run(check: bool, output: &std::path::Path) -> std::process::Output {
    let mut args = vec!["docs"];
    if check {
        args.push("--check");
    }
    args.push("--output");
    let output_text = output.to_string_lossy().into_owned();
    args.push(&output_text);
    Command::new(binary())
        .args(&args)
        .env("MEMNOTH_HOME", output.with_extension("home"))
        .output()
        .expect("run memnoth")
}

#[test]
fn a_file_the_registry_wrote_matches_and_a_stale_one_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("command-surface.md");

    let written = run(false, &path);
    assert!(
        written.status.success(),
        "writing failed: {}",
        String::from_utf8_lossy(&written.stderr)
    );
    let first = std::fs::read_to_string(&path).unwrap();
    assert!(
        first.contains("### `memnoth session start`"),
        "the reference should name a real verb"
    );

    let fresh = run(true, &path);
    assert!(
        fresh.status.success(),
        "a fresh file must pass: {}",
        String::from_utf8_lossy(&fresh.stderr)
    );

    // One edited line is drift, and drift must not pass.
    std::fs::write(&path, first.replace("session start", "session begin")).unwrap();
    let stale = run(true, &path);
    assert_eq!(
        stale.status.code(),
        Some(1),
        "stale output must exit 1, got {:?}: {}",
        stale.status.code(),
        String::from_utf8_lossy(&stale.stderr)
    );
}

#[test]
fn check_without_an_output_path_is_a_usage_error() {
    let output = Command::new(binary())
        .args(["docs", "--check"])
        .output()
        .expect("run memnoth");
    assert_eq!(output.status.code(), Some(2), "usage errors exit 2");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("needs --output"),
        "the error should name the missing flag: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn checking_a_missing_file_says_how_to_create_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent.md");
    let output = run(true, &path);
    assert_eq!(output.status.code(), Some(4), "a missing file is not found");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("docs --output"),
        "the error should name the command that writes it"
    );
}
