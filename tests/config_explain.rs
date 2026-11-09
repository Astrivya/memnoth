//! `mn config explain` must name the layer that actually won.
//!
//! The failure this prevents is the one that costs an afternoon: a value edited
//! in the file that the program ignores, because an environment variable is set.
//! `config get` prints the value and says nothing about that.

use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

fn run(args: &[&str], home: &std::path::Path, env: &[(&str, &str)]) -> std::process::Output {
    let mut command = Command::new(binary());
    command.args(args).env("MEMNOTH_HOME", home);
    command.env_remove("MEMNOTH_VAULT");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("run memnoth")
}

fn text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn an_unset_key_reports_the_default_layer_and_how_to_change_it() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");

    let out = run(&["config", "explain", "mcp_url"], &home, &[]);
    assert!(out.status.success(), "{}", text(&out));
    let body = text(&out);
    assert!(body.contains("key:    mcp_url"), "{body}");
    assert!(body.contains("source: default"), "{body}");
    assert!(
        body.contains("does not set this key"),
        "an unset key should say the file does not hold it: {body}"
    );
    assert!(
        body.contains("change: mn config set mcp_url"),
        "the answer should name the command that changes it: {body}"
    );
}

#[test]
fn a_key_set_in_the_file_reports_the_file_layer() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");

    let set = run(&["config", "set", "profile", "code"], &home, &[]);
    assert!(set.status.success(), "{}", text(&set));

    let out = run(&["config", "explain", "profile"], &home, &[]);
    assert!(out.status.success(), "{}", text(&out));
    let body = text(&out);
    assert!(body.contains("value:  code"), "{body}");
    assert!(body.contains("source: config"), "{body}");
    assert!(body.contains("holds code"), "{body}");
}

#[test]
fn an_environment_override_is_named_and_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");

    // The file says one thing...
    let set = run(&["config", "set", "vault", "from-file.db"], &home, &[]);
    assert!(set.status.success(), "{}", text(&set));

    // ...and the environment says another.
    let out = run(
        &["config", "explain", "vault"],
        &home,
        &[("MEMNOTH_VAULT", "from-env.db")],
    );
    assert!(out.status.success(), "{}", text(&out));
    let body = text(&out);
    assert!(body.contains("value:  from-env.db"), "{body}");
    assert!(body.contains("source: env"), "{body}");
    assert!(body.contains("MEMNOTH_VAULT"), "{body}");
    assert!(
        body.contains("the environment wins over the file here"),
        "the override is the whole point of the verb: {body}"
    );
}

#[test]
fn an_unknown_key_is_not_found_and_points_at_the_list() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");

    let out = run(&["config", "explain", "nonsense.key"], &home, &[]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out));
    let body = text(&out);
    assert!(body.contains("nonsense.key"), "{body}");
    assert!(body.contains("mn config list"), "{body}");
}
