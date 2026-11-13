//! The registry and the binary must describe the same verbs.
//!
//! `AGENTS.md` §3 says nothing may hard-code a second list, and until Phase 2
//! `src/cli.rs` is the source of truth. This test is what keeps the second list
//! (`build_p0_registry`) honest while Phase 2 lands: every verb clap parses must
//! have a row, and every row must name a verb clap parses. A row for a verb that
//! does not exist is the same defect as a verb with no row — both make `mn docs`
//! and the completions describe a product that is not there.
//!
//! `help` is not exempt any more: the builtin help subcommand is disabled and
//! `Verb::Help` is a real command, so it must have a row like every other verb.

use clap::CommandFactory;
use memnoth::cli::Cli;

/// Top-level verb names the binary actually parses.
fn cli_verbs() -> Vec<String> {
    Cli::command()
        .get_subcommands()
        .map(|verb| verb.get_name().to_string())
        .collect()
}

/// Top-level verb names the registry would project into clap.
fn registry_verbs() -> Vec<String> {
    memnoth_command::build_p0_registry()
        .build()
        .build_clap()
        .get_subcommands()
        .map(|verb| verb.get_name().to_string())
        .collect()
}

#[test]
fn every_verb_the_binary_parses_has_a_registry_row() {
    let parsed = cli_verbs();
    let registered = registry_verbs();
    let missing: Vec<&String> = parsed
        .iter()
        .filter(|verb| !registered.contains(verb))
        .collect();
    assert!(
        missing.is_empty(),
        "these verbs parse but have no CommandSpec: {missing:?}"
    );
}

#[test]
fn every_registry_row_names_a_verb_the_binary_parses() {
    let parsed = cli_verbs();
    let projected = registry_verbs();
    let stray: Vec<&String> = projected
        .iter()
        .filter(|verb| !parsed.contains(verb))
        .collect();
    assert!(
        stray.is_empty(),
        "these registry rows name no verb in src/cli.rs: {stray:?}"
    );
}

/// Every destructive verb carries `--dry-run` and `--yes` and refuses to
/// prompt when stdin is not a terminal. Audited verbs: `keys revoke`,
/// `skill remove`, `worktree clean`, `uninstall`.
#[test]
fn destructive_verbs_have_dry_run_and_yes() {
    let registry = memnoth_command::build_p0_registry().build();
    // The verb is `skills` (plural) in src/cli.rs; the registry row has
    // always used that name, so the test must too.
    let destructive = [
        ("keys", "revoke"),
        ("skills", "remove"),
        ("worktree", "clean"),
    ];

    for (verb, sub) in destructive {
        let found = registry
            .all()
            .find(|spec| spec.cli_path == vec![verb.to_string(), sub.to_string()]);
        let spec = found.unwrap_or_else(|| {
            let paths: Vec<_> = registry
                .all()
                .map(|s| s.cli_path.join(" "))
                .filter(|p| p.starts_with(verb))
                .collect();
            panic!("destructive verb {verb} {sub} missing from registry; have: {paths:?}")
        });
        assert!(
            spec.slash.params.iter().any(|p| p.name == "dry_run"),
            "{verb} {sub} missing --dry-run param"
        );
        assert!(
            spec.slash.params.iter().any(|p| p.name == "yes"),
            "{verb} {sub} missing --yes param"
        );
    }

    // `uninstall` is a top-level verb
    let uninstall = registry
        .all()
        .find(|spec| spec.cli_path == vec!["uninstall".to_string()])
        .expect("uninstall missing from registry");
    assert!(
        uninstall.slash.params.iter().any(|p| p.name == "dry_run"),
        "uninstall missing --dry-run param"
    );
    assert!(
        uninstall.slash.params.iter().any(|p| p.name == "yes"),
        "uninstall missing --yes param"
    );
}

/// Every registry command resolves to a `Verb` the binary parses — walking
/// the full registry surface without arguments never reaches `Parsed::Unknown`.
#[test]
fn registry_walk_resolves_every_command() {
    let registry = memnoth_command::build_p0_registry().build();
    let mut unknown = Vec::new();
    for spec in registry.all() {
        if spec.slash.hidden {
            continue;
        }
        let path = spec.cli_path.join(" ");
        let parsed = memnoth::tui::app::parse_line(&path);
        match parsed {
            memnoth::tui::app::Parsed::Unknown(_) => unknown.push(path),
            memnoth::tui::app::Parsed::Help => {} // help is a real command
            _ => {}
        }
    }
    assert!(
        unknown.is_empty(),
        "registry commands that do not parse: {unknown:?}"
    );
}
