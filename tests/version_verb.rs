//! `mn version` is the scriptable form of `--version`.
//!
//! It exists so a bug report can name the artifact without parsing prose or
//! guessing which platform build produced it. The JSON shape is the contract;
//! these tests pin the keys and the values that must agree with Cargo.

use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(binary())
        .args(args)
        .env(
            "MEMNOTH_HOME",
            std::env::temp_dir().join("memnoth-version-test"),
        )
        .output()
        .expect("run memnoth")
}

#[test]
fn text_form_is_one_line_with_the_crate_version() {
    let out = run(&["version"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        text.trim(),
        format!("memnoth {}", env!("CARGO_PKG_VERSION")),
        "text output must match the crate version"
    );
    assert_eq!(text.lines().count(), 1, "one line, no banner");
}

#[test]
fn json_form_carries_version_target_and_commit() {
    let out = run(&["version", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("version --json must emit JSON");
    // `--json` is the shared envelope, so the payload is under `data`; the
    // top-level fields are the contract every verb shares.
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "version");
    let value = &envelope["data"];
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
    let target = value["target"].as_str().expect("target is a string");
    assert!(
        target.contains(std::env::consts::OS) && target.contains(std::env::consts::ARCH),
        "target must name this platform, got {target}"
    );
    assert!(
        value["commit"].as_str().is_some(),
        "commit is present even when the build did not set it"
    );
}

#[test]
fn the_clap_flag_still_works_and_agrees() {
    let out = run(&["--version"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "--version and version must not disagree: {text}"
    );
}
