//! `mn schema` is the machine-readable registry, and `--check` holds a
//! committed copy still. These tests prove both directions and the failure
//! paths, not just that JSON is printed.

use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

fn run(args: &[&str], home: &std::path::Path) -> std::process::Output {
    Command::new(binary())
        .args(args)
        .env("MEMNOTH_HOME", home)
        .output()
        .expect("run memnoth")
}

#[test]
fn prints_a_versioned_registry_document() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(&["schema"], &dir.path().join("home"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("schema must emit JSON");
    assert_eq!(value["schema"], "memnoth.commands/1");
    assert_eq!(
        value["command_count"].as_u64().unwrap() as usize,
        value["commands"].as_array().unwrap().len()
    );
    let ids: Vec<&str> = value["commands"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|command| command["id"].as_str())
        .collect();
    assert!(ids.contains(&"vault.remember"), "ids: {ids:?}");
    assert!(ids.contains(&"schema"), "the schema row describes itself");
}

#[test]
fn a_fresh_file_matches_and_an_edited_one_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let path = dir.path().join("schema.json");

    let written = run(&["schema", "--output", &path.to_string_lossy()], &home);
    assert!(
        written.status.success(),
        "{}",
        String::from_utf8_lossy(&written.stderr)
    );

    let fresh = run(
        &["schema", "--check", "--output", &path.to_string_lossy()],
        &home,
    );
    assert!(
        fresh.status.success(),
        "{}",
        String::from_utf8_lossy(&fresh.stderr)
    );

    let original = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        original.replacen("vault.remember", "vault.forget", 1),
    )
    .unwrap();
    let stale = run(
        &["schema", "--check", "--output", &path.to_string_lossy()],
        &home,
    );
    assert_eq!(stale.status.code(), Some(1), "stale schema must fail");
}

#[test]
fn check_without_output_is_a_usage_error_and_a_missing_file_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");

    let no_output = run(&["schema", "--check"], &home);
    assert_eq!(no_output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&no_output.stderr).contains("needs --output"));

    let absent = run(
        &[
            "schema",
            "--check",
            "--output",
            &dir.path().join("absent.json").to_string_lossy(),
        ],
        &home,
    );
    assert_eq!(absent.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&absent.stderr).contains("schema --output"));
}
