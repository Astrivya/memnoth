//! D28 — the hidden `team` verb is a LOCAL-only surface.
//!
//! Before M2 + M5 + M8 there is no shared team memory. The singular `team`
//! surface must therefore answer only about this machine: local vault
//! namespaces, a local proposal review inbox, local attribution, and an honest
//! "requires two-machine restore; not available" state. It must never claim an
//! account, an organization, seats, or a remote screen, and it stays out of
//! public help while keeping its registry rows for parity.

use std::path::PathBuf;

use clap::{CommandFactory, Parser};
use memnoth::cli::{Cli, Verb};
use memnoth::context::{AppContext, Config};
use memnoth::output::Outcome;

fn context(dir: &std::path::Path, namespace: &str) -> AppContext {
    AppContext {
        root: dir.to_path_buf(),
        home: dir.join("home"),
        vault_path: dir.join("home").join(format!("vault-{namespace}.db")),
        namespace: namespace.to_string(),
        graph_path: dir.join("graph.db"),
        sessions_dir: dir.join("home").join("sessions"),
        config_path: dir.join("home").join("config.toml"),
        exe_path: PathBuf::from("memnoth"),
        config: Config::default(),
        json: false,
        quiet: true,
    }
}

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("memnoth-team-local-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home")).unwrap();
    dir
}

fn verb(args: &[&str]) -> Verb {
    let mut argv = vec!["mn"];
    argv.extend_from_slice(args);
    Cli::try_parse_from(argv).unwrap().verb.unwrap()
}

fn run(ctx: &AppContext, args: &[&str]) -> Outcome {
    memnoth::commands::execute(ctx, &verb(args)).unwrap()
}

/// The local qualifier and the absence of any remote/team-memory promise.
fn assert_local_only(out: &Outcome) {
    let text = out.text.to_lowercase();
    assert!(
        text.contains("on this machine"),
        "every team screen must be qualified on this machine; got:\n{}",
        out.text
    );
    for claim in [
        "account",
        "organization",
        "invite",
        "seats",
        "signed in",
        "cloud sync",
        "shared memory",
        "team memory is available",
        "team memory available",
        "synced to the cloud",
    ] {
        assert!(
            !text.contains(claim),
            "team screen made a remote/team-memory claim ({claim:?}):\n{}",
            out.text
        );
    }
}

#[test]
fn team_verb_is_hidden_but_registered_for_parity() {
    let cli = Cli::command();
    let team = cli.find_subcommand("team").expect("clap parses team");
    assert!(team.is_hide_set(), "team must be hidden from public help");

    let registry = memnoth_command::build_p0_registry().build();
    let projected = registry.build_clap();
    assert!(
        projected.find_subcommand("team").is_some(),
        "the registry must project the team verb"
    );

    let parsed: Vec<String> = cli
        .get_subcommands()
        .map(|verb| verb.get_name().to_string())
        .collect();
    let registered: Vec<String> = projected
        .get_subcommands()
        .map(|verb| verb.get_name().to_string())
        .collect();
    let missing: Vec<&String> = parsed
        .iter()
        .filter(|name| !registered.contains(name))
        .collect();
    let stray: Vec<&String> = registered
        .iter()
        .filter(|name| !parsed.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "verbs with no registry row: {missing:?}"
    );
    assert!(stray.is_empty(), "registry rows with no verb: {stray:?}");

    let rows: Vec<_> = registry
        .all()
        .filter(|spec| spec.cli_path.first().is_some_and(|word| word == "team"))
        .collect();
    assert!(
        rows.len() >= 5,
        "expected the team surface rows, found {}",
        rows.len()
    );
    for row in rows {
        assert!(
            row.slash.hidden,
            "{} must be hidden from public surfaces",
            row.id
        );
        assert_eq!(
            row.slash.auth,
            memnoth_command::AuthLevel::None,
            "{} must not imply a signed-in account",
            row.id
        );
    }
}

#[test]
fn every_team_screen_is_local_and_makes_no_remote_claim() {
    let dir = temp("screens");
    let ctx = context(&dir, "default");
    for args in [
        vec!["team"],
        vec!["team", "namespace"],
        vec!["team", "inbox"],
        vec!["team", "attribution"],
        vec!["team", "memory"],
    ] {
        let out = run(&ctx, &args);
        assert_local_only(&out);
    }
}

#[test]
fn team_memory_screen_is_honestly_unavailable() {
    let dir = temp("memory");
    let ctx = context(&dir, "default");
    let out = run(&ctx, &["team", "memory"]);
    assert_eq!(out.command, "team.memory");
    assert!(
        out.text.contains("two-machine restore"),
        "must name the gate:\n{}",
        out.text
    );
    assert!(
        out.text.contains("not available on this machine"),
        "must refuse the claim:\n{}",
        out.text
    );
}

#[test]
fn namespace_screen_lists_only_local_vault_files() {
    let dir = temp("namespace");
    let ctx = context(&dir, "work");
    std::fs::write(ctx.home.join("vault.db"), b"").unwrap();
    std::fs::write(ctx.home.join("vault-work.db"), b"").unwrap();

    let out = run(&ctx, &["team", "namespace"]);
    assert_local_only(&out);
    assert!(out.text.contains("default"), "default namespace missing");
    assert!(out.text.contains("work"), "work namespace missing");
    assert!(
        !out.text.to_lowercase().contains("shared vault namespace"),
        "must not claim a shared vault:\n{}",
        out.text
    );
    let names = out.data["namespaces"].as_array().expect("namespace array");
    assert_eq!(names.len(), 2, "only the two local files are listed");
}

#[test]
fn proposal_inbox_is_honest_when_no_local_review_exists() {
    let dir = temp("inbox");
    let ctx = context(&dir, "default");
    let out = run(&ctx, &["team", "inbox"]);
    assert_local_only(&out);
    assert!(
        out.text
            .contains("No review proposals are recorded on this machine"),
        "empty local inbox must say so:\n{}",
        out.text
    );
}

#[test]
fn attribution_screen_reports_only_local_authors() {
    let dir = temp("attribution");
    let ctx = context(&dir, "default");
    memnoth::vault::init(&ctx).unwrap();
    memnoth::vault::remember(
        &ctx,
        "local_fact",
        "fact",
        None,
        "concept",
        &["kept on this machine".into()],
        Some("local-author"),
    )
    .unwrap();

    let out = run(&ctx, &["team", "attribution"]);
    assert_local_only(&out);
    assert!(
        out.text.contains("local-author"),
        "local author must be attributed:\n{}",
        out.text
    );
}
