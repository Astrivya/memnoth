//! `memnoth` — one binary for the whole local product.
//!
//! | Surface | What it is |
//! |---|---|
//! | [`tui`] | the session: transcript, vault context and graph state in one screen |
//! | [`vault`] | the temporal vault — facts with the date they became true, corrections kept |
//! | [`kg`] | the code graph — symbols, references, impact |
//! | [`mcp`] | the MCP server and the clients registered against it |
//! | [`provider`] | the model a turn runs against, with the two stores as tools |
//! | [`doctor`] | which of the above is actually wired on this machine |
//!
//! Two rules hold the binary together:
//!
//! * **One dispatcher.** [`commands::execute`] answers every verb; the TUI calls
//!   it for its slash commands, so `memnoth kg status` and `/kg status` cannot
//!   drift.
//! * **One answer, two renderings.** Every verb returns an [`output::Outcome`]
//!   with text for a person and JSON for a program — the same JSON shape the
//!   MCP tool returns, which is the documented promise.
//!
//! The binary never fabricates an answer. When a component is absent from this
//! build (the MCP server crate is not published here; embeddings are not built),
//! the verb says so by name instead of returning an empty list that reads as
//! "nothing found".

pub mod audit_cmd;
pub mod auth;
pub mod automation;
pub mod cli;

pub use context::Config;
pub mod billing;
pub mod cloud;
pub mod commands;
pub mod completion;
pub mod config_cmd;
pub mod connect;
pub mod context;
pub mod docs;
pub mod doctor;
pub mod edit;
pub mod env_file;
pub mod feedback;
pub mod keys;
pub mod kg;
pub mod mcp;
pub mod models;
pub mod open;
pub mod org;
pub mod output;
pub mod projects;
pub mod provider;
pub mod receipt;
pub mod report;
pub mod serve;
pub mod session;
pub mod session_cmd;
pub mod setup;
pub mod stats;
pub mod sync;
pub mod team;
pub mod teams;
pub mod telemetry;
pub mod tui;
pub mod update_cmd;
pub mod usage;
pub mod vault;
pub mod watch;
pub mod worktree;
use clap::Parser;
use cli::{Cli, Verb};
use context::{AppContext, DEFAULT_MCP_URL, DEFAULT_PROFILE};
use output::ErrorOutcome;
use std::io::IsTerminal;
use std::process::ExitCode;
/// Parse the command line, run it, and turn the outcome into an exit code.
///
/// `main` is this call and nothing else: printing, JSON envelopes and exit codes
/// all live here so the TUI and the CLI cannot disagree about what a failure
/// looks like.
pub fn run() -> ExitCode {
    // Before anything reads the environment: a repository-local `.env` fills in
    // only the variables the shell has not already set, and never prints a value.
    env_file::load();
    let cli = Cli::parse();
    let json = cli.json;
    let ctx = match AppContext::new(&cli) {
        Ok(ctx) => ctx,
        Err(err) => return fail(json, &ErrorOutcome::from_anyhow(&err)),
    };

    // A terminal gets the screen; a pipe or `--json` gets the line. Without this
    // split, `memnoth` alone printed one sentence and exited, so the product's
    // whole face was unreachable.
    let interactive = !json && std::io::stdout().is_terminal();
    let session_name = match (&cli.verb, &cli.resume) {
        (Some(Verb::Session(args)), _) => match &args.action {
            cli::SessionCommand::Start(start) => start
                .resume
                .clone()
                .or_else(|| start.name.clone())
                .unwrap_or_else(|| "session".to_string()),
            _ => "session".to_string(),
        },
        (None, Some(name)) => name.clone(),
        (None, None) => "session".to_string(),
        _ => "session".to_string(),
    };

    // Every interactive launch opens on the brief, `--resume NAME` included:
    // naming a session chooses which conversation sits behind the brief, it does
    // not skip the screen. No invocation may land a person in the middle of last
    // Tuesday's transcript as their first frame; `esc` is the way in.
    if interactive && matches!(cli.verb, None | Some(Verb::Session(_))) {
        return match tui::run(&ctx, &session_name) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => fail(false, &ErrorOutcome::from_anyhow(&err)),
        };
    }

    if let Some(verb) = cli.verb {
        match commands::execute(&ctx, &verb) {
            Ok(outcome) => {
                output::emit(json, &outcome);
                ExitCode::from(outcome.exit_code)
            }
            Err(err) => fail(json, &err),
        }
    } else {
        run_session(&ctx, &cli, json)
    }
}

fn run_session(ctx: &AppContext, cli: &Cli, json: bool) -> ExitCode {
    let verb = Verb::Session(crate::cli::SessionArgs {
        action: crate::cli::SessionCommand::Start(crate::cli::StartArgs {
            name: if cli.resume.is_some() {
                None
            } else {
                Some(cli.session_name())
            },
            resume: None,
            model: None,
        }),
    });
    match commands::execute(ctx, &verb) {
        Ok(outcome) => {
            output::emit(json, &outcome);
            ExitCode::from(outcome.exit_code)
        }
        Err(err) => fail(json, &err),
    }
}

fn fail(json: bool, error: &ErrorOutcome) -> ExitCode {
    output::emit_error(json, error);
    ExitCode::from(error.code)
}
