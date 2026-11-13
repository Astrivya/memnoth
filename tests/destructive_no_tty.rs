//! W2-07: a destructive verb never blocks without a terminal.
//!
//! Each case runs the real binary with stdin closed, in an isolated home and
//! workspace, and asserts the documented contract: exit 2 (usage) and a fix
//! that names `--yes`. The failure this prevents is a piped script hanging on a
//! prompt nobody can answer, or worse, proceeding because silence was read as
//! consent.
//!
//! Nothing here passes `--yes`: these tests must never delete anything.

use std::process::{Command, Stdio};

/// The binary under test, as cargo built it.
fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

/// Run one destructive verb with no terminal and assert it refuses.
fn refuses_without_a_terminal(args: &[&str], what: &str) {
    let dir = tempfile::tempdir().expect("temp dir");
    let home = dir.path().join("home");
    let root = dir.path().join("ws");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&root).expect("workspace");
    // `worktree clean` resolves the repository before it asks, so the case that
    // needs one gets a repository. `git` is already a build requirement here.
    let _ = Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(&root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let output = Command::new(binary())
        .arg("--trust-folder")
        .args(args)
        .current_dir(&root)
        .env("MEMNOTH_HOME", &home)
        .env_remove("MEMNOTH_VAULT")
        .env_remove("MEMNOTH_GRAPH")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap_or_else(|error| panic!("cannot run `memnoth {args:?}`: {error}"));

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(2),
        "`memnoth {}` must refuse with exit 2 when stdin is not a terminal.\nstdout: {stdout}\nstderr: {stderr}",
        args.join(" ")
    );
    assert!(
        stderr.contains("--yes"),
        "`{what}` must name the flag that makes it deliberate.\nstderr: {stderr}"
    );
}

#[test]
fn keys_revoke_refuses_without_a_terminal() {
    refuses_without_a_terminal(&["keys", "revoke", "key_test"], "keys revoke");
}

#[test]
fn skill_remove_refuses_without_a_terminal() {
    refuses_without_a_terminal(&["skills", "remove", "demo"], "skills remove");
}

#[test]
fn worktree_clean_refuses_without_a_terminal() {
    refuses_without_a_terminal(&["worktree", "clean", "demo"], "worktree clean");
}

#[test]
fn uninstall_refuses_without_a_terminal() {
    refuses_without_a_terminal(&["uninstall"], "uninstall");
}
