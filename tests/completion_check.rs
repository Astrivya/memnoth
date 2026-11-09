//! Completions are generated, so the committed script can drift silently.
//!
//! These tests pin the three useful answers: write, match, and stale — plus the
//! plain-stdout form, which must contain the script and nothing else.

use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

fn run(args: &[&str], home: &std::path::Path) -> std::process::Output {
    Command::new(binary())
        .args(args)
        .env("MEMNOTH_HOME", home)
        .env_remove("MEMNOTH_VAULT")
        .output()
        .expect("run memnoth")
}

#[test]
fn stdout_is_a_sourceable_script_with_no_prose_tail() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        &["completion", "show", "--shell", "bash"],
        &dir.path().join("home"),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("complete") && text.contains("memnoth"),
        "the script must complete memnoth:\n{text}"
    );
    assert!(
        !text.contains("Wrote"),
        "prose after the script corrupts it:\n{text}"
    );
}

#[test]
fn a_written_script_matches_and_an_edited_one_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let path = dir.path().join("memnoth.bash");
    let text = path.to_string_lossy().into_owned();

    let written = run(
        &["completion", "show", "--shell", "bash", "--output", &text],
        &home,
    );
    assert!(
        written.status.success(),
        "{}",
        String::from_utf8_lossy(&written.stderr)
    );
    assert!(std::fs::read(&path).unwrap().len() > 100);

    let fresh = run(
        &[
            "completion",
            "show",
            "--shell",
            "bash",
            "--output",
            &text,
            "--check",
        ],
        &home,
    );
    assert!(
        fresh.status.success(),
        "{}",
        String::from_utf8_lossy(&fresh.stderr)
    );

    std::fs::write(&path, "stale\n").unwrap();
    let stale = run(
        &[
            "completion",
            "show",
            "--shell",
            "bash",
            "--output",
            &text,
            "--check",
        ],
        &home,
    );
    assert_eq!(
        stale.status.code(),
        Some(1),
        "drift is not a usage error: {}",
        String::from_utf8_lossy(&stale.stderr)
    );
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("stale"),
        "{}",
        String::from_utf8_lossy(&stale.stderr)
    );
}

#[test]
fn checking_without_an_output_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        &["completion", "show", "--shell", "bash", "--check"],
        &dir.path().join("home"),
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "clap must refuse the missing file"
    );
}
