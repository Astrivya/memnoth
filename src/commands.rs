use anyhow::Context;
use memnoth_capability::Profile;
use memnoth_skills::{SkillCommand as SkillsSkillCommand, execute_skill_command};

use crate::auth::{auth_status, login, logout, whoami};
use crate::cli::{
    AuthCommand, ConnectCommand, KgCommand, McpCommand, McpProfileCommand, SkillCommand,
    TeamsCommand, VaultCommand, Verb,
};
use crate::completion::{Shell, install, render_completions, uninstall};
use crate::context::AppContext;
use crate::output::{ErrorOutcome, ExitCode, Outcome, confirm};
use serde_json::json;

use crate::{
    acp, agents, audit_cmd, automation, bench, billing, budget, cloud, config_cmd, connect, demo,
    docs, doctor, extension, feedback, keys, kg, mcp, open, org, projects, receipt, sandbox, serve,
    session_cmd, setup, team, teams, telemetry, trust, update_cmd, vault, watch, worktree,
};
pub fn execute(ctx: &AppContext, verb: &Verb) -> Result<Outcome, ErrorOutcome> {
    match verb {
        Verb::Session(args) => match &args.action {
            crate::cli::SessionCommand::Start(start) => session_cmd::start(
                ctx,
                start.name.as_deref(),
                start.resume.as_deref(),
                start.model.as_deref(),
            )
            .map_err(|e| ErrorOutcome::from_anyhow(&e)),
            crate::cli::SessionCommand::Resume(resume) => {
                session_cmd::resume(ctx, &resume.name).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            crate::cli::SessionCommand::List(list) => {
                session_cmd::list(ctx, list.json).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            crate::cli::SessionCommand::Status(status) => {
                session_cmd::status(ctx, status.name.as_deref(), status.json)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            crate::cli::SessionCommand::Stop(stop) => session_cmd::stop(ctx, stop.name.as_deref())
                .map_err(|e| ErrorOutcome::from_anyhow(&e)),
            crate::cli::SessionCommand::Effort(effort) => {
                session_cmd::effort(ctx, effort.name.as_deref(), &effort.level, effort.json)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            crate::cli::SessionCommand::Replay(replay) => session_cmd::replay(
                ctx,
                &replay.name,
                replay.from_turn,
                replay.max_turns,
                replay.model.as_deref(),
                &replay.format,
                replay.output.as_deref(),
                replay.diff,
                ctx.json,
            )
            .map_err(|e| ErrorOutcome::from_anyhow(&e)),
            crate::cli::SessionCommand::Attach(attach) => session_cmd::attach(
                ctx,
                &attach.name,
                attach.output.as_deref(),
                attach.include_vault,
                attach.include_kg,
                attach.json,
            )
            .map_err(|e| ErrorOutcome::from_anyhow(&e)),
            crate::cli::SessionCommand::Handoff(handoff) => session_cmd::handoff(
                ctx,
                &handoff.bundle,
                handoff.name.as_deref(),
                handoff.workspace.as_deref(),
                handoff.json,
            )
            .map_err(|e| ErrorOutcome::from_anyhow(&e)),
            crate::cli::SessionCommand::FromClaude(import) => {
                session_cmd::from_claude(ctx, &import.path, import.name.as_deref())
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            crate::cli::SessionCommand::FromCodex(import) => {
                session_cmd::from_codex(ctx, &import.path, import.name.as_deref())
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            crate::cli::SessionCommand::FromOmp(import) => {
                session_cmd::from_omp(ctx, &import.path, import.name.as_deref())
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            crate::cli::SessionCommand::Exec(exec) => session_cmd::exec(
                ctx,
                exec.cmd.clone(),
                exec.session.as_deref(),
                &exec.policy,
                exec.network,
                exec.write_outside,
                exec.timeout,
                exec.json,
            )
            .map_err(|e| ErrorOutcome::from_anyhow(&e)),
        },
        Verb::Ask(args) => crate::ask::run(ctx, args),
        Verb::Doctor(args) => doctor::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Receipt(args) => receipt::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Audit(args) => audit_cmd::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Vault(args) => match &args.action {
            VaultCommand::Init => vault::init(ctx),
            VaultCommand::Status => vault::status(ctx),
            VaultCommand::AsOf { at, name, limit } => {
                vault::asof(ctx, at.as_deref(), name.as_deref(), *limit)
            }
            VaultCommand::History { name } => vault::history(ctx, name),
            VaultCommand::Contradictions { name } => vault::contradictions(ctx, name.as_deref()),
            VaultCommand::Remember {
                name,
                kind,
                confidence,
                entity_kind,
                text,
                author,
            } => vault::remember(
                ctx,
                name,
                kind,
                *confidence,
                entity_kind,
                text,
                author.as_deref(),
            ),
            VaultCommand::Propose {
                name,
                kind,
                confidence,
                entity_kind,
                text,
                author,
            } => vault::propose(
                ctx,
                name,
                kind,
                *confidence,
                entity_kind,
                text,
                author.as_deref(),
            ),
            VaultCommand::Proposals { status, json } => {
                vault::proposals(ctx, status.as_deref(), *json)
            }
            VaultCommand::Approve { id, by } => vault::approve(ctx, *id, by.as_deref()),
            VaultCommand::Reject { id, reason, by } => {
                vault::reject(ctx, *id, reason, by.as_deref())
            }
            VaultCommand::Recall { query, limit, at } => {
                vault::recall(ctx, query, *limit, at.as_deref())
            }
            VaultCommand::Forget {
                name,
                reason,
                apply,
            } => vault::forget(ctx, name, reason, *apply),
            VaultCommand::Correct {
                name,
                new_name,
                summary,
                confidence,
                reason,
            } => vault::correct(
                ctx,
                name,
                new_name.as_deref(),
                summary.as_deref(),
                *confidence,
                reason,
            ),
            VaultCommand::Link {
                source,
                target,
                kind,
                confidence,
                reason,
            } => vault::link(ctx, source, target, kind, *confidence, reason),
            VaultCommand::Unlink {
                source,
                target,
                kind,
                reason,
            } => vault::unlink(ctx, source, target, kind, reason),
            VaultCommand::Search {
                query,
                limit,
                semantic,
                at,
            } => vault::search(ctx, query, *limit, *semantic, at.as_deref()),
            VaultCommand::Export {
                output,
                format,
                include_history,
                kind,
            } => vault::export(
                ctx,
                output.as_deref(),
                format,
                *include_history,
                kind.as_deref(),
            ),
            VaultCommand::Import {
                input,
                format,
                dry_run,
                on_conflict,
            } => vault::import(ctx, input.as_deref(), format, *dry_run, on_conflict),
            VaultCommand::Gc { keep, apply } => vault::gc(ctx, *keep, *apply),
            VaultCommand::Prune { keep_days, apply } => vault::prune(ctx, *keep_days, *apply),
            VaultCommand::Reindex { embeddings, full } => vault::reindex(ctx, *embeddings, *full),
            VaultCommand::Stats { json, growth } => vault::stats(ctx, *json, *growth),
            VaultCommand::Backup {
                output,
                include_wal,
                compress,
            } => vault::backup(ctx, output, *include_wal, *compress),
            VaultCommand::Restore {
                input,
                verify,
                force,
            } => vault::restore(ctx, input, *verify, *force),
            VaultCommand::Audit { limit, kind, json } => {
                vault::audit(ctx, *limit, kind.as_deref(), *json)
            }
            VaultCommand::Merge { from, into, reason } => vault::merge(ctx, from, into, reason),
            VaultCommand::Duplicates { at, json } => vault::duplicates(ctx, at.as_deref(), *json),
            VaultCommand::Top { limit, kind, at } => {
                vault::top(ctx, *limit, kind.as_deref(), at.as_deref())
            }
            VaultCommand::Timeline {
                limit,
                at,
                kind,
                include_observations,
            } => vault::timeline(
                ctx,
                *limit,
                at.as_deref(),
                kind.as_deref(),
                *include_observations,
            ),
            VaultCommand::Explain { name, at } => vault::explain(ctx, name, at.as_deref()),
            VaultCommand::Graph {
                name,
                depth,
                at,
                json,
            } => vault::graph(ctx, name, *depth, at.as_deref(), *json),
        },
        Verb::Kg(args) => match &args.action {
            KgCommand::Index { full, no_embed } => {
                let mut progress = |line: &str| {
                    if !ctx.quiet && !ctx.json {
                        eprintln!("{line}");
                    }
                };
                let report = kg::index(ctx, *full, *no_embed, &mut progress)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg index report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.index", kg::render_index(&report), data))
            }
            KgCommand::Status => {
                let status = kg::status(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&status)
                    .context("serialize kg status")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.status", kg::render_status(&status), data))
            }
            KgCommand::Impact { symbol, depth } => {
                let report =
                    kg::impact(ctx, symbol, *depth).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg impact report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.impact", kg::render_impact(&report), data))
            }
            KgCommand::Search { query, limit } => {
                let hits =
                    kg::search(ctx, query, *limit).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::json!({
                    "query": query.join(" "),
                    "hits": hits,
                    "count": hits.len(),
                });
                Ok(Outcome::new("kg.search", kg::render_search(&hits), data))
            }
            KgCommand::Explore { symbol } => {
                let report = kg::explore(ctx, symbol).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg explore report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "kg.explore",
                    kg::render_explore(&report),
                    data,
                ))
            }
            KgCommand::Refs { symbol, limit } => {
                let report =
                    kg::refs(ctx, symbol, *limit).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg refs report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.refs", kg::render_refs(&report), data))
            }
            KgCommand::Callers { symbol, limit } => {
                let report =
                    kg::callers(ctx, symbol, *limit).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg callers report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "kg.callers",
                    kg::render_callers(&report),
                    data,
                ))
            }
            KgCommand::Callees { symbol, limit } => {
                let report =
                    kg::callees(ctx, symbol, *limit).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg callees report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "kg.callees",
                    kg::render_callees(&report),
                    data,
                ))
            }
            KgCommand::Def { symbol } => {
                let report = kg::def(ctx, symbol).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg def report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.def", kg::render_def(&report), data))
            }
            KgCommand::DiffImpact { staged } => {
                let report =
                    kg::diff_impact(ctx, *staged).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg diff-impact report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "kg.diff_impact",
                    kg::render_diff_impact(&report),
                    data,
                ))
            }
            KgCommand::Callpath { from, to } => {
                let report =
                    kg::callpath(ctx, from, to).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg callpath report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "kg.callpath",
                    kg::render_callpath(&report),
                    data,
                ))
            }
            KgCommand::Outline { file } => {
                let report = kg::outline(ctx, file).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg outline report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "kg.outline",
                    kg::render_outline(&report),
                    data,
                ))
            }
            KgCommand::Tree { dir } => {
                let report = kg::tree(ctx, dir).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg tree report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.tree", kg::render_tree(&report), data))
            }
            KgCommand::Review { staged, base } => {
                let report = kg::review(ctx, *staged, base.as_deref())
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg review report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.review", kg::render_review(&report), data))
            }
            KgCommand::TestsFor { symbol } => {
                let report =
                    kg::tests_for(ctx, symbol).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg tests-for report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "kg.tests_for",
                    kg::render_tests_for(&report),
                    data,
                ))
            }
            KgCommand::Owners { path } => {
                let report = kg::owners(ctx, path).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg owners report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.owners", kg::render_owners(&report), data))
            }
            KgCommand::DeadCode => {
                let report = kg::dead_code(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg dead-code report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "kg.dead_code",
                    kg::render_dead_code(&report),
                    data,
                ))
            }
            KgCommand::Cycles => {
                let report = kg::cycles(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg cycles report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.cycles", kg::render_cycles(&report), data))
            }
            KgCommand::Hotspots => {
                let report = kg::hotspots(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg hotspots report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "kg.hotspots",
                    kg::render_hotspots(&report),
                    data,
                ))
            }
            KgCommand::UnusedExports => {
                let report = kg::unused_exports(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg unused-exports report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "kg.unused_exports",
                    kg::render_unused_exports(&report),
                    data,
                ))
            }
            KgCommand::Deps { file } => {
                let report = kg::deps(ctx, file).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg deps report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.deps", kg::render_deps(&report), data))
            }
            KgCommand::Query { expr } => {
                let report = kg::query(ctx, expr).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg query report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.query", kg::render_query(&report), data))
            }
            // Both of these are the same long-running processes `memnoth watch`
            // and `memnoth serve` run; delegating is what makes `kg serve`
            // actually bind the port it prints instead of reporting one it never
            // opened. The `kg` verb takes no flags of its own, so the defaults
            // are the ones a reader of `memnoth kg serve --help` expects.
            KgCommand::Watch => watch::run(
                ctx,
                &crate::cli::WatchArgs {
                    port: None,
                    debounce: 500,
                    vault_changes: false,
                    once: false,
                    json: ctx.json,
                },
            ),
            KgCommand::Serve { port } => {
                let runtime = tokio::runtime::Runtime::new()
                    .map_err(|e| ErrorOutcome::from_anyhow(&anyhow::Error::from(e)))?;
                runtime.block_on(serve::run(
                    ctx,
                    &crate::cli::ServeArgs {
                        port: *port,
                        host: "127.0.0.1".to_string(),
                        cors: false,
                        token: None,
                        graph_path: None,
                        json: ctx.json,
                    },
                ))
            }
        },
        Verb::Mcp(args) => match &args.action {
            McpCommand::Serve { http, db, profile } => {
                let profile_opt = mcp_profile(profile)?;
                mcp::serve(ctx, *http, db.clone(), profile_opt)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))
                    .map(|()| Outcome::inline("mcp.serve", "MCP server stopped."))
            }
            McpCommand::Doctor => {
                let report = mcp::doctor(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "mcp.doctor",
                    mcp::render_doctor(&report),
                    serde_json::to_value(&report).unwrap_or_default(),
                ))
            }
            McpCommand::Add {
                clients,
                profile,
                url,
                token,
                local,
                db,
                team,
                allow_writes,
                dry_run,
            } => {
                let profile_opt = mcp_profile(profile)?;
                let report = mcp::add(
                    ctx,
                    mcp::AddOptions {
                        client_names: clients,
                        profile: profile_opt,
                        url: url.as_deref(),
                        token: token.as_deref(),
                        local: *local,
                        db: db.clone(),
                        team: team.as_deref(),
                        allow_writes: *allow_writes,
                        dry_run: *dry_run,
                    },
                )
                .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize mcp add report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("mcp.add", mcp::render_add(&report), data))
            }
            McpCommand::Call {
                tool,
                args: args_json,
                profile,
            } => {
                let profile_opt = mcp_profile(profile)?.unwrap_or(Profile::Full);
                let value = mcp::call(ctx, tool, args_json, profile_opt)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "mcp.call",
                    mcp::render_call(tool, &value),
                    value,
                ))
            }
            McpCommand::Selftest => {
                let report = mcp::selftest_inner(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize mcp selftest report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "mcp.selftest",
                    mcp::render_selftest(&report),
                    data,
                ))
            }
            McpCommand::Remove {
                clients,
                token,
                dry_run,
            } => {
                let report = mcp::remove(ctx, clients, *token, *dry_run)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize mcp remove report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "mcp.remove",
                    mcp::render_remove(&report),
                    data,
                ))
            }
            McpCommand::List => {
                let report = mcp::list(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize mcp list report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("mcp.list", mcp::render_list(&report), data))
            }
            McpCommand::Test { tool, profile } => {
                let profile_opt = mcp_profile(profile)?;
                let report = mcp::test(ctx, tool.as_deref(), profile_opt)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize mcp test report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let mut outcome = Outcome::new("mcp.test", mcp::render_test(&report), data);
                // A test that prints `failed` and still exits 0 cannot gate a
                // script or a CI step.
                if !report.tools_list_ok || !report.call_ok {
                    outcome.exit_code = ExitCode::Unexpected as u8;
                }
                Ok(outcome)
            }
            McpCommand::Token { json: _ } => {
                let report = mcp::token(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize mcp token report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("mcp.token", mcp::render_token(&report), data))
            }
            McpCommand::Logs {
                session,
                tail,
                json: _,
            } => {
                let report = mcp::logs_impl(ctx, session.clone(), *tail, ctx.json)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize mcp logs report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("mcp.logs", mcp::render_logs(&report), data))
            }
            McpCommand::Profile { action } => match action {
                McpProfileCommand::Show { json: _ } => {
                    let report =
                        mcp::profile_show(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                    let data = serde_json::to_value(&report)
                        .context("serialize mcp profile show report")
                        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                    Ok(Outcome::new(
                        "mcp.profile.show",
                        mcp::render_profile_show(&report),
                        data,
                    ))
                }
                McpProfileCommand::Set { profile } => {
                    let report = mcp::profile_set(ctx, profile)
                        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                    let data = serde_json::to_value(&report)
                        .context("serialize mcp profile set report")
                        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                    Ok(Outcome::new(
                        "mcp.profile.set",
                        mcp::render_profile_set(&report),
                        data,
                    ))
                }
            },
            McpCommand::Exposes { json: _ } => {
                let report = mcp::exposes(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize mcp exposes report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "mcp.exposes",
                    mcp::render_exposes(&report),
                    data,
                ))
            }
            McpCommand::Restart { server, force } => {
                let report = mcp::restart_impl(ctx, server.clone(), *force)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize mcp restart report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "mcp.restart",
                    mcp::render_restart(&report),
                    data,
                ))
            }
        },
        Verb::Auth(args) => match &args.action {
            AuthCommand::Login(args) => login(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
            AuthCommand::Logout(args) => {
                logout(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            AuthCommand::Status(args) => {
                auth_status(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            AuthCommand::Whoami(args) => {
                whoami(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
        },
        Verb::Sync(args) => {
            crate::sync::run_sync_top(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
        }
        Verb::Teams(args) => match &args.action {
            TeamsCommand::Join(args) => {
                teams::join(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::List(args) => {
                teams::list(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Create(args) => {
                teams::create(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Members(args) => {
                teams::members(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Claim(args) => {
                team::claim(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Release(args) => {
                team::release(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Tasks(args) => {
                team::tasks(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Inbox(args) => {
                team::inbox(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Presence(args) => {
                team::presence(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Lock(args) => {
                team::lock(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Handoff(args) => {
                team::handoff(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Review(args) => {
                team::review(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Audit(args) => {
                team::audit(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Namespace(args) => team::team_namespace(ctx, args.action.as_deref())
                .map_err(|e| ErrorOutcome::from_anyhow(&e)),
            TeamsCommand::Provenance(args) => {
                team::team_provenance(ctx, args.limit).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Disputed(_args) => {
                team::team_disputed(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::Enforce(_args) => {
                team::enforce(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            TeamsCommand::ImportExport(args) => {
                let output = if let Some(path) = args.from_claude.as_deref() {
                    format!("Importing Claude session from {path}")
                } else if let Some(path) = args.from_codex.as_deref() {
                    format!("Importing Codex session from {path}")
                } else if let Some(path) = args.from_omp.as_deref() {
                    format!("Importing OMP session from {path}")
                } else if let Some(path) = args.export.as_deref() {
                    format!("Exporting session to {path} as {}", args.format)
                } else {
                    "Specify --from-claude, --from-codex, --from-omp, or --export".to_string()
                };
                Ok(Outcome::new("team.import_export", output, json!({})))
            }
        },
        Verb::Cloud(args) => cloud::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Config(args) => match &args.action {
            crate::cli::ConfigCommand::Get(args) => {
                config_cmd::get(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            crate::cli::ConfigCommand::Set(args) => {
                config_cmd::set(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            crate::cli::ConfigCommand::List(args) => {
                config_cmd::list(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            crate::cli::ConfigCommand::Edit(args) => {
                config_cmd::edit(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            crate::cli::ConfigCommand::Explain(args) => config_cmd::explain(ctx, args),
        },
        Verb::Update(args) => update_cmd::execute_update(ctx, args),
        Verb::Install(args) => update_cmd::execute_install(ctx, args),
        Verb::Uninstall(args) => update_cmd::execute_uninstall(ctx, args),
        Verb::SetTheme(theme) => {
            let mut config = ctx.config.clone();
            config.ui.get_or_insert_with(Default::default).theme = Some(theme.as_str().to_string());
            config
                .save(&ctx.config_path)
                .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
            Ok(Outcome::new(
                "set-theme",
                format!(
                    "Theme set to {}.\nNext: `mn session` to see it.",
                    theme.as_str()
                ),
                serde_json::json!({
                    "theme": theme.as_str(),
                    "config": ctx.config_path.display().to_string(),
                }),
            ))
        }
        Verb::SetScreenReader(mode) => {
            let mut config = ctx.config.clone();
            config.ui.get_or_insert_with(Default::default).screen_reader =
                Some(mode.as_str().to_string());
            config
                .save(&ctx.config_path)
                .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
            Ok(Outcome::new(
                "set-screen-reader",
                format!(
                    "Screen reader mode set to {}.\nNext: `mn session` to see it.",
                    mode.as_str()
                ),
                serde_json::json!({
                    "screen_reader": mode.as_str(),
                    "config": ctx.config_path.display().to_string(),
                }),
            ))
        }
        Verb::Sandbox(args) => sandbox::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Completion(args) => match &args.action {
            crate::cli::CompletionCommand::Show(show_args) => {
                let shell = match show_args.shell.as_str() {
                    "bash" => Shell::Bash,
                    "zsh" => Shell::Zsh,
                    "fish" => Shell::Fish,
                    "powershell" => Shell::PowerShell,
                    "elvish" => Shell::Elvish,
                    _ => Shell::Bash,
                };
                let bytes = render_completions(shell).map_err(|e| ErrorOutcome::from_anyhow(&e))?;

                if let Some(path) = &show_args.output {
                    if show_args.check {
                        let existing = match std::fs::read(path) {
                            Ok(existing) => existing,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                return Err(ErrorOutcome::not_found(format!(
                                    "{} does not exist, so there is nothing to compare",
                                    path.display()
                                ))
                                .with_fix(format!(
                                    "run `mn completion show --shell {} --output {}`",
                                    show_args.shell,
                                    path.display()
                                )));
                            }
                            Err(error) => {
                                return Err(ErrorOutcome::new(
                                    ExitCode::Unexpected,
                                    format!("cannot read {}: {error}", path.display()),
                                ));
                            }
                        };
                        if existing != bytes {
                            return Err(ErrorOutcome::new(
                                ExitCode::Unexpected,
                                format!(
                                    "{} is stale: the command surface and the completion disagree",
                                    path.display()
                                ),
                            )
                            .with_fix(format!(
                                "run `mn completion show --shell {} --output {}` and review the diff",
                                show_args.shell,
                                path.display()
                            )));
                        }
                        return Ok(Outcome::new(
                            "completion.show",
                            format!(
                                "{} matches the generated {shell:?} completion",
                                path.display()
                            ),
                            serde_json::json!({
                                "output": path.display().to_string(),
                                "shell": format!("{shell:?}"),
                                "check": true,
                                "matches": true,
                            }),
                        ));
                    }

                    std::fs::write(path, &bytes).map_err(|error| {
                        ErrorOutcome::new(
                            ExitCode::Unexpected,
                            format!("cannot write {}: {error}", path.display()),
                        )
                    })?;
                    return Ok(Outcome::new(
                        "completion.show",
                        format!("Wrote {shell:?} completions to {}", path.display()),
                        serde_json::json!({
                            "output": path.display().to_string(),
                            "shell": format!("{shell:?}"),
                        }),
                    ));
                }

                // The script is the answer. Returning it as the outcome text
                // keeps `emit` from appending a prose line to a file a shell
                // will source.
                Ok(Outcome::new(
                    "completion.show",
                    String::from_utf8_lossy(&bytes).into_owned(),
                    serde_json::json!({ "shell": format!("{shell:?}") }),
                ))
            }
            crate::cli::CompletionCommand::Install(install_args) => {
                let shell = install_args.shell.as_deref().and_then(|s| match s {
                    "bash" => Some(Shell::Bash),
                    "zsh" => Some(Shell::Zsh),
                    "fish" => Some(Shell::Fish),
                    "powershell" => Some(Shell::PowerShell),
                    "elvish" => Some(Shell::Elvish),
                    _ => None,
                });
                let label = shell.map(|s| format!("{s:?}"));
                install(shell).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "completion.install",
                    match &label {
                        Some(name) => format!("Installed {name} completions."),
                        None => "Installed completions for the detected shell.".to_string(),
                    },
                    serde_json::json!({ "shell": label }),
                ))
            }
            crate::cli::CompletionCommand::Uninstall(uninstall_args) => {
                let shell = uninstall_args.shell.as_deref().and_then(|s| match s {
                    "bash" => Some(Shell::Bash),
                    "zsh" => Some(Shell::Zsh),
                    "fish" => Some(Shell::Fish),
                    "powershell" => Some(Shell::PowerShell),
                    "elvish" => Some(Shell::Elvish),
                    _ => None,
                });
                uninstall(shell).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new(
                    "completion.uninstall",
                    "Uninstalled completions.".to_string(),
                    serde_json::json!({ "shell": shell.map(|s| format!("{s:?}")) }),
                ))
            }
        },
        Verb::Setup(args) => setup::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Telemetry(args) => {
            telemetry::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
        }
        Verb::Privacy(args) => {
            crate::sync::run_privacy(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
        }
        Verb::Billing(args) => billing::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Projects(args) => projects::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Keys(args) => keys::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Org(args) => org::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Skills(args) => {
            // `skill remove` deletes a file tree. Ask here, in front of the
            // skill crate's own `--force` rule, so a piped run refuses with the
            // documented exit 2 and names `--yes` instead of surfacing a
            // lower-level "use --force" error.
            if let SkillCommand::Remove(remove) = &args.action
                && !remove.dry_run
                && !confirm(
                    &format!("Remove skill '{}'?", remove.name),
                    remove.yes || remove.force,
                )?
            {
                return Ok(Outcome::new(
                    "skills.remove",
                    "Cancelled; nothing was removed.
",
                    serde_json::json!({ "cancelled": true, "name": remove.name }),
                ));
            }
            // Which verb answered is part of the answer: `skill list --json`
            // and `skill run --json` both return a document, and a script has
            // to be able to tell them apart without re-reading the command.
            let action = match &args.action {
                SkillCommand::Add(_) => "add",
                SkillCommand::List(_) => "list",
                SkillCommand::Remove(_) => "remove",
                SkillCommand::Run(_) => "run",
                SkillCommand::Edit(_) => "edit",
            };
            let skill_cmd = match &args.action {
                SkillCommand::Add(a) => {
                    SkillsSkillCommand::Add(memnoth_skills::commands::AddArgs {
                        path: a.path.clone(),
                        name: a.name.clone(),
                    })
                }
                SkillCommand::List(a) => {
                    SkillsSkillCommand::List(memnoth_skills::commands::ListArgs {
                        detail: a.detail,
                        tag: a.tag.clone(),
                        json: a.json,
                    })
                }
                SkillCommand::Remove(a) => {
                    SkillsSkillCommand::Remove(memnoth_skills::commands::RemoveArgs {
                        name: a.name.clone(),
                        yes: a.yes,
                        dry_run: a.dry_run,
                        force: a.force,
                    })
                }
                SkillCommand::Run(a) => {
                    SkillsSkillCommand::Run(memnoth_skills::commands::RunArgs {
                        name: a.name.clone(),
                        args: a.args.clone(),
                        json: a.json,
                    })
                }
                SkillCommand::Edit(a) => {
                    SkillsSkillCommand::Edit(memnoth_skills::commands::EditArgs {
                        name: a.name.clone(),
                        editor: a.editor.clone(),
                    })
                }
            };
            let result =
                execute_skill_command(skill_cmd).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
            // `--json` already makes the skill crate print a document, so lift
            // that document instead of handing the pipe an empty object. A
            // plain-text answer is carried as the message it printed.
            let mut data = match serde_json::from_str::<serde_json::Value>(&result) {
                Ok(value @ serde_json::Value::Object(_)) => value,
                Ok(value @ serde_json::Value::Array(_)) => serde_json::json!({ "result": value }),
                _ => serde_json::json!({ "message": result.clone() }),
            };
            data["action"] = serde_json::json!(action);
            Ok(Outcome::new("skill", result, data))
        }
        Verb::Open(args) => open::run(ctx, args),
        Verb::Connect(args) => match &args.action {
            ConnectCommand::Add(args) => {
                connect::connect_add(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            ConnectCommand::List(args) => {
                connect::connect_list(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            ConnectCommand::Remove(args) => {
                connect::connect_remove(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            ConnectCommand::Test(args) => {
                connect::connect_test(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
        },
        Verb::Feedback(args) => feedback::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Agents(args) => agents::run(ctx, args),
        Verb::Automation(args) => {
            automation::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
        }
        Verb::Worktree(args) => worktree::run(ctx, args),
        Verb::Batch(args) => worktree::run_batch_cmd(ctx, args),
        Verb::Parallel(args) => worktree::run_parallel(ctx, args),
        Verb::Docs(args) => docs::run(ctx, args),
        Verb::Version(args) => {
            let data = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "target": format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
                "commit": option_env!("MEMNOTH_BUILD_COMMIT").unwrap_or("unknown"),
            });
            let text = if args.json {
                serde_json::to_string_pretty(&data)
                    .map_err(|e| ErrorOutcome::from_anyhow(&anyhow::Error::from(e)))?
            } else {
                format!("memnoth {}", env!("CARGO_PKG_VERSION"))
            };
            Ok(Outcome::new("version", text, data))
        }
        Verb::Bench(args) => bench::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Help(args) => {
            use clap::CommandFactory;

            let mut root = crate::cli::Cli::command();
            let text = if args.all {
                let registry = memnoth_command::build_p0_registry().build();
                let mut rows: Vec<(String, bool, String)> = registry
                    .all()
                    .map(|spec| {
                        (
                            spec.cli_path.join(" "),
                            spec.slash.hidden,
                            spec.slash.description.clone(),
                        )
                    })
                    .collect();
                rows.sort_by(|a, b| a.0.cmp(&b.0));
                let mut text = format!("Every verb this build accepts ({}):\n\n", registry.len());
                for (path, hidden, description) in rows {
                    text.push_str(&format!(
                        "  memnoth {path}{}\n      {description}\n",
                        if hidden { "  (hidden)" } else { "" }
                    ));
                }
                text
            } else if let Some(name) = &args.verb {
                match root.find_subcommand_mut(name) {
                    Some(command) => command.render_long_help().to_string(),
                    None => {
                        return Err(ErrorOutcome::not_found(format!(
                            "no verb named {name:?}"
                        ))
                        .with_fix("run `memnoth help --all` to list every verb, including hidden ones"));
                    }
                }
            } else {
                root.render_help().to_string()
            };
            Ok(Outcome::new(
                "help",
                text,
                serde_json::json!({ "all": args.all, "verb": args.verb }),
            ))
        }
        Verb::Schema(args) => {
            let registry = memnoth_command::build_p0_registry().build();
            let commands: Vec<_> = registry.all().collect();
            let document = serde_json::json!({
                "schema": "memnoth.commands/1",
                "command_count": registry.len(),
                "commands": commands,
            });
            let text = serde_json::to_string_pretty(&document)
                .map_err(|e| ErrorOutcome::from_anyhow(&anyhow::Error::from(e)))?;

            if args.check {
                let Some(path) = &args.output else {
                    return Err(ErrorOutcome::usage(
                        "`schema --check` needs --output: there is no file to compare against",
                    )
                    .with_fix("run `mn schema --check --output docs/cli/schema.json`"));
                };
                let existing = match std::fs::read_to_string(path) {
                    Ok(existing) => existing,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Err(ErrorOutcome::not_found(format!(
                            "{} does not exist, so there is nothing to compare",
                            path.display()
                        ))
                        .with_fix(format!("run `mn schema --output {}`", path.display())));
                    }
                    Err(error) => {
                        return Err(ErrorOutcome::new(
                            ExitCode::Unexpected,
                            format!("cannot read {}: {error}", path.display()),
                        ));
                    }
                };
                if existing != text {
                    return Err(ErrorOutcome::new(
                        ExitCode::Unexpected,
                        format!(
                            "{} is stale: the registry and the file disagree",
                            path.display()
                        ),
                    )
                    .with_fix(format!(
                        "run `mn schema --output {}` and review the diff",
                        path.display()
                    )));
                }
                return Ok(Outcome::new(
                    "schema",
                    format!("{} matches the registry", path.display()),
                    serde_json::json!({
                        "output": path.display().to_string(),
                        "check": true,
                        "matches": true,
                    }),
                ));
            }

            match &args.output {
                Some(path) => {
                    std::fs::write(path, &text).map_err(|error| {
                        ErrorOutcome::new(
                            ExitCode::Unexpected,
                            format!("cannot write {}: {error}", path.display()),
                        )
                    })?;
                    Ok(Outcome::new(
                        "schema",
                        format!("Registry schema written to {}", path.display()),
                        serde_json::json!({ "output": path.display().to_string() }),
                    ))
                }
                None => Ok(Outcome::new(
                    "schema",
                    text,
                    serde_json::json!({ "command_count": registry.len() }),
                )),
            }
        }
        Verb::Checkpoint(args) => crate::tui::commands::checkpoint::run(ctx, args.name.as_deref()),
        Verb::Undo => crate::tui::commands::undo::run(ctx),
        Verb::Redo => crate::tui::commands::redo::run(ctx),
        Verb::Delegate(args) => crate::tui::commands::delegate::run(ctx, &args.name, &args.task),
        Verb::Merge(args) => crate::tui::commands::merge::run(ctx, &args.branch),
        Verb::Handoff(args) => crate::tui::commands::handoff::run(ctx, args.path.as_deref()),
        Verb::Compact(args) => crate::tui::commands::compact::run(ctx, args.focus.as_deref()),
        Verb::Btw(args) => crate::tui::commands::btw::run(ctx, &args.question),
        Verb::Recap => crate::tui::commands::recap::run(ctx),
        Verb::Export(args) => crate::tui::commands::export::run(ctx, args.path.as_deref()),
        Verb::Rename(args) => crate::tui::commands::rename::run(ctx, &args.name),
        Verb::Keymap(args) => crate::tui::commands::keymap::run(ctx, args),
        Verb::Onboarding => crate::tui::commands::onboarding::run(ctx),
        Verb::Extension(args) => extension::run(ctx, &args.action),
        Verb::Acp(args) => acp::run(&acp::AcpArgs {
            host: args.host.clone(),
            port: args.port,
        })
        .map(|()| {
            Outcome::new(
                "acp",
                "acp server exited",
                serde_json::json!({ "host": args.host, "port": args.port }),
            )
        })
        .map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Watch(args) => watch::run(ctx, args),
        Verb::Serve(args) => {
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|e| ErrorOutcome::from_anyhow(&anyhow::Error::from(e)))?;
            runtime.block_on(serve::run(ctx, args))
        }
        Verb::Drift(args) => crate::tui::commands::drift::run(ctx, args),
        Verb::Simulate(args) => crate::tui::commands::simulate::run(ctx, args),
        Verb::Lineage(args) => {
            crate::tui::commands::lineage::run(ctx, &args.entity, args.depth, None, ctx.json)
        }
        Verb::Why(args) => crate::tui::commands::why::run(ctx, &args.symbol, args.at.as_deref()),
        Verb::Impact(args) => crate::tui::commands::impact::run(ctx, args),
        Verb::Plan(args) => {
            crate::tui::commands::plan::run(ctx, args.name.as_deref(), args.step.as_deref())
        }
        Verb::Goal(args) => crate::tui::commands::goal::run(ctx, args.condition.as_deref()),
        Verb::Budget(args) => budget::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Trust(args) => trust::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Demo => demo::run(ctx),
        Verb::Brief => crate::tui::commands::brief::run(ctx),
    }
}

/// The one-line skill summary the session shows when a `/skill` line is wrong.
///
/// It lives beside the dispatcher because this is the function that turns a
/// skill command into an answer; the TUI quotes it rather than keeping a second
/// copy that could drift.
pub const SKILL_USAGE: &str =
    "usage: /skill add <path> | list | remove <name> | run <name> [args] | edit <name>";

/// A `--profile` value, or the reason it is not one.
///
/// A typo must fail *here*, before a server is spawned or a client's config is
/// written: silently treating `codde` as "no profile" registered the default
/// surface and told the caller nothing.
fn mcp_profile(profile: &Option<String>) -> Result<Option<Profile>, ErrorOutcome> {
    match profile {
        None => Ok(None),
        Some(name) => match Profile::from_name(name) {
            Some(profile) => Ok(Some(profile)),
            None => Err(ErrorOutcome::new(
                crate::output::ExitCode::Usage,
                format!("unknown tool profile: {name}"),
            )
            .with_fix("use micro, memory, coord, code, full or agent")),
        },
    }
}

/// Parse a profile name, naming the valid set on a typo.
pub fn parse_profile(name: &str) -> anyhow::Result<Profile> {
    Profile::from_name(name).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown profile '{}'. Valid: micro, memory, coord, code, full, agent",
            name
        )
    })
}
