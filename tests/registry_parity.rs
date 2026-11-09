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
