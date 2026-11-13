//! `mn help` is a real verb, not clap's builtin, because `--all` must list the
//! hidden surface from the registry. These tests pin the three forms and the
//! failure path.

use std::process::Command;

use clap::CommandFactory;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(binary())
        .args(args)
        .env(
            "MEMNOTH_HOME",
            std::env::temp_dir().join("memnoth-help-test"),
        )
        .output()
        .expect("run memnoth")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn bare_help_renders_the_usage_and_a_verb() {
    let out = run(&["help"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(text.contains("Usage:"), "summary must show usage:\n{text}");
    assert!(
        text.contains("vault"),
        "summary must name a real verb:\n{text}"
    );
}

#[test]
fn help_for_one_verb_renders_that_verbs_help() {
    let out = run(&["help", "version"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(text.contains("version"), "must describe version:\n{text}");
    assert!(
        text.contains("--json"),
        "the verb's own flags must appear:\n{text}"
    );
}

#[test]
fn all_lists_every_verb_including_hidden_ones() {
    let out = run(&["help", "--all"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    // A public verb, a hidden-by-decision verb, and the count line.
    for expected in [
        "memnoth vault",
        "memnoth watch",
        "Every verb this build accepts",
    ] {
        assert!(text.contains(expected), "missing {expected:?}:\n{text}");
    }
    let listed = text
        .lines()
        .filter(|line| line.trim_start().starts_with("memnoth "))
        .count();
    assert!(listed >= 30, "expected the whole surface, listed {listed}");
}

#[test]
fn exactly_twelve_verbs_are_public() {
    let command = memnoth::cli::Cli::command();
    let mut visible: Vec<String> = command
        .get_subcommands()
        .filter(|verb| !verb.is_hide_set())
        .map(|verb| verb.get_name().to_string())
        .collect();
    visible.sort();
    // D7 / W2-02: the public surface is exactly twelve verbs. `budget` and
    // `trust` are real commands (they keep working and appear in `help --all`),
    // but they are advanced, not part of the twelve.
    let expected = [
        "ask",
        "auth",
        "completion",
        "config",
        "doctor",
        "kg",
        "mcp",
        "session",
        "skills",
        "update",
        "vault",
        "version",
    ];
    assert_eq!(
        visible, expected,
        "the public surface is twelve verbs; everything else is hidden"
    );
    let skills = command.find_subcommand("skills").expect("skills is public");
    assert!(
        skills.get_all_aliases().any(|alias| alias == "skill"),
        "the old spelling must keep working"
    );
}

#[test]
fn an_unknown_verb_is_not_found_and_names_help_all() {
    let out = run(&["help", "nonsense"]);
    assert_eq!(out.status.code(), Some(4), "{}", stdout(&out));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("nonsense"), "{err}");
    assert!(err.contains("help --all"), "{err}");
}
