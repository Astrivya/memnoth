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

pub mod auth;
pub mod cli;

pub use context::Config;
pub mod cloud;
pub mod commands;
pub mod completion;
pub mod config_cmd;
pub mod context;
pub mod doctor;
pub mod edit;
pub mod kg;
pub mod mcp;
pub mod models;
pub mod output;
pub mod provider;
pub mod session;
pub mod session_cmd;
pub mod setup;
pub mod sync;
pub mod teams;
pub mod team;
pub mod telemetry;
pub mod billing;
pub mod tui;
pub mod update_cmd;
pub mod usage;
pub mod vault;
pub mod open;
pub mod feedback;
pub mod stats;
pub mod connect;
use clap::Parser;
use cli::{Cli, Verb};
use context::{AppContext, DEFAULT_MCP_URL, DEFAULT_PROFILE};
use std::process::ExitCode;
use output::ErrorOutcome;
/// Parse the command line, run it, and turn the outcome into an exit code.
///
/// `main` is this call and nothing else: printing, JSON envelopes and exit codes
/// all live here so the TUI and the CLI cannot disagree about what a failure
/// looks like.
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let ctx = match AppContext::new(&cli) {
        Ok(ctx) => ctx,
        Err(err) => return fail(json, &ErrorOutcome::from_anyhow(&err)),
    };

    if let Some(verb) = cli.verb {
        match commands::execute(&ctx, &verb) {
            Ok(outcome) => {
                if json {
                    println!("{}", serde_json::to_string(&outcome).unwrap());
                } else {
                    print!("{}", outcome.text);
                }
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
            name: if cli.resume.is_some() { None } else { Some(cli.session_name()) },
            resume: None,
            model: None,
        }),
    });
    match commands::execute(ctx, &verb) {
        Ok(outcome) => {
            if json {
                println!("{}", serde_json::to_string(&outcome).unwrap());
            } else {
                print!("{}", outcome.text);
            }
            ExitCode::from(outcome.exit_code)
        }
        Err(err) => fail(json, &err),
    }
}

fn fail(json: bool, error: &ErrorOutcome) -> ExitCode {
    if json {
        eprintln!("{}", serde_json::to_string(error).unwrap());
    } else {
        eprintln!("{error}");
    }
    ExitCode::from(error.code)
}