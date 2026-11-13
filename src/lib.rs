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

pub mod agents;
pub mod ask;
pub mod audit_cmd;
pub mod auth;
pub mod automation;
pub mod cli;
pub mod demo;
pub use context::{AppContext, Config, DEFAULT_MCP_URL, DEFAULT_PROFILE};
pub mod acp;
pub mod bench;
pub mod billing;
pub mod budget;
pub mod checkpoint;
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
pub mod extension;
pub mod extension_commands;
pub mod feedback;
pub mod goals;
pub mod keys;
pub mod kg;
pub mod mcp;
pub mod net_guard;
pub mod open;
pub mod org;
pub mod output;
pub mod projects;
pub mod provider;
pub mod provider_stream;
pub mod receipt;
pub mod report;
pub mod sandbox;

pub mod serve;
pub mod session;
pub mod session_cmd;
pub mod setup;
pub mod sync;
pub mod sync_crypto;
pub mod sync_engine;
pub mod team;
pub mod teams;
pub mod telemetry;
pub mod trust;
pub mod tui;
pub mod update_cmd;
pub mod vault;
pub mod watch;
pub mod worktree;
use anyhow::Context;
use clap::{CommandFactory, Parser};
use cli::{Cli, Verb};
use memnoth_command::build_p0_registry;
use memnoth_harness_api::{ApiEvent, ApiRequest, ClientFrame, ServerFrame};
use memnoth_transport::Stream;
use net_guard::strip_proxy_env;
use output::ErrorOutcome;
use std::collections::HashSet;
use std::io::IsTerminal;
use std::process::ExitCode;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
/// Parse the command line, run it, and turn the outcome into an exit code.
///
/// `main` is this call and nothing else: printing, JSON envelopes and exit codes
/// all live here so the TUI and the CLI cannot disagree about what a failure
/// looks like.
pub fn run() -> ExitCode {
    // Strip proxy environment variables before anything touches the network, so an
    // attacker-controlled proxy cannot steer an outbound request at an internal
    // address.
    strip_proxy_env();
    // `env_file::load()` used to run here. It now runs after the workspace
    // trust decision below: a cloned repository must not be able to set a
    // variable for this process merely by being the working directory.
    // Harness API wire: open the socket, say hello, and handle the reply.
    // The harness socket is optional: an editor or a driver may have one open,
    // and a plain `mn ask` has none. A refused connection is the normal case,
    // not a reason to stop before the verb runs.
    let _harness = tokio::runtime::Runtime::new()
        .context("harness runtime")
        .and_then(|rt| {
            rt.block_on(async {
                let stream = Stream::connect(memnoth_harness_api::api_socket_path())
                    .await
                    .context("connect harness socket")?;
                let (mut reader, mut writer) = stream.into_split();
                let frame = ClientFrame::new(
                    1,
                    ApiRequest::Hello {
                        min_version: memnoth_harness_api::API_VERSION_MAJOR,
                        max_version: memnoth_harness_api::API_VERSION_MAJOR,
                        client: "memnoth-cli".to_string(),
                    },
                );
                let payload = serde_json::to_string(&frame)?;
                writer.write_all(payload.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
                let mut line = String::new();
                reader.read_line(&mut line).await?;
                let reply: ServerFrame = serde_json::from_str(&line)?;
                match reply.event {
                    ApiEvent::HelloOk { .. } | ApiEvent::Ok => Ok(()),
                    ApiEvent::Error { code, message } => {
                        anyhow::bail!("harness hello failed: {code:?} {message}")
                    }
                    other => anyhow::bail!("unexpected reply to hello: {:?}", other),
                }
            })
        })
        .ok();

    let cli = Cli::parse();
    // Registry validation: every `Verb` variant clap parses must exist as a
    // row in the command registry, and every registry row must name a verb
    // clap parses. A mismatch exits non-zero before any command runs.
    let code = validate_registry(&cli);
    if code != ExitCode::SUCCESS {
        return code;
    }

    // Workspace trust comes before `.env`, the graph, `AGENTS.md`, the sandbox
    // policy, hooks or MCP configuration. `--trust-folder` allows this run and
    // writes nothing; without a terminal the answer is no.
    if needs_trust(cli.verb.as_ref()) {
        let root = cli
            .root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        if let Err(error) = trust::check(&root, cli.trust_folder) {
            return fail(cli.json, &ErrorOutcome::from_anyhow(&error));
        }
    }
    // After the trust decision, never before: this is the project's `.env`.
    env_file::load();

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

/// Whether this invocation reads the project before it can answer.
///
/// The rest — `doctor`, `trust`, `auth`, `version`, `completion`, `update`,
/// `config` — reads only `~/.memnoth` and may run in any folder.
fn needs_trust(verb: Option<&Verb>) -> bool {
    match verb {
        None => true,
        Some(verb) => matches!(
            verb,
            Verb::Session(_)
                | Verb::Ask(_)
                | Verb::Kg(_)
                | Verb::Mcp(_)
                | Verb::Open(_)
                | Verb::Serve(_)
                | Verb::Watch(_)
                | Verb::Batch(_)
                | Verb::Parallel(_)
                | Verb::Worktree(_)
                | Verb::Agents(_)
                | Verb::Automation(_)
                | Verb::Setup(_)
        ),
    }
}

fn fail(json: bool, error: &ErrorOutcome) -> ExitCode {
    output::emit_error(json, error);
    ExitCode::from(error.code)
}
/// Validate that every `Verb` variant clap parses has a row in the command
/// registry, and every registry row names a verb clap parses. Exit non-zero
/// on mismatch.
fn validate_registry(_cli: &Cli) -> ExitCode {
    let parsed: HashSet<String> = Cli::command()
        .get_subcommands()
        .map(|verb| verb.get_name().to_string())
        .collect();

    let registry = build_p0_registry().build();
    let registered: HashSet<String> = registry
        .all()
        .filter_map(|spec| spec.cli_path.first().cloned())
        .collect();

    let missing: Vec<&String> = parsed
        .iter()
        .filter(|verb| !registered.contains(*verb))
        .collect();
    let stray: Vec<&String> = registered
        .iter()
        .filter(|verb| !parsed.contains(*verb))
        .collect();

    if missing.is_empty() && stray.is_empty() {
        return ExitCode::SUCCESS;
    }

    if !missing.is_empty() {
        eprintln!("registry mismatch: these verbs parse but have no CommandSpec row: {missing:?}");
    }
    if !stray.is_empty() {
        eprintln!("registry mismatch: these registry rows name no verb in src/cli.rs: {stray:?}");
    }
    ExitCode::from(1)
}

pub mod lsp_diag;
