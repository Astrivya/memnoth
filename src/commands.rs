use anyhow::Context;
use memnoth_capability::Profile;
use memnoth_skills::{execute_skill_command, SkillCommand as SkillsSkillCommand};

use crate::cli::{
    ActionArgs, ActionCommand, AutomationArgs, AutomationCommand, AuthCommand, BatchArgs, BatchCmdArgs, CiArgs, CiCommand,
    HookArgs, HookCommand, KgCommand, McpCommand, McpProfileCommand, ParallelArgs, TeamsCommand, VaultCommand, Verb,
    PrivacyArgs, PrivacyCommand, WorktreeArgs, WorktreeCommand,
    SkillCommand, SkillArgs, WatchArgs, ServeArgs, OpenArgs, DocsArgs,
    FeedbackArgs, FeedbackCommand, StatsArgs,
    ConnectArgs, ConnectCommand,
};
use crate::completion::{Shell, install, show_completions};
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};
use crate::{
    automation, cloud, config_cmd, doctor, docs, kg, mcp, models, session_cmd, setup, teams, update_cmd,
    usage, vault, team, billing, worktree, watch, serve, open,
    telemetry, sync, feedback, stats, connect,
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
            crate::cli::SessionCommand::Effort(effort) => session_cmd::effort(
                ctx,
                effort.name.as_deref(),
                &effort.level,
                effort.json,
            )
            .map_err(|e| ErrorOutcome::from_anyhow(&e)),
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
        Verb::Doctor(args) => doctor::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
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
            } => vault::remember(ctx, name, kind, *confidence, entity_kind, text),
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
            VaultCommand::Merge { from, into, reason } => {
                vault::merge(ctx, from, into, reason)
            }
            VaultCommand::Duplicates { at, json } => {
                vault::duplicates(ctx, at.as_deref(), *json)
            }
            VaultCommand::Top { limit, kind, at } => {
                vault::top(ctx, *limit, kind.as_deref(), at.as_deref())
            }
            VaultCommand::Timeline { limit, at, kind, include_observations } => {
                vault::timeline(ctx, *limit, at.as_deref(), kind.as_deref(), *include_observations)
            }
            VaultCommand::Audit { limit, kind, json } => {
                vault::audit(ctx, *limit, kind.as_deref(), *json)
            }
            VaultCommand::Explain { name, at } => {
                vault::explain(ctx, name, at.as_deref())
            }
            VaultCommand::Graph { name, depth, at, json } => {
                vault::graph(ctx, name, *depth, at.as_deref(), *json)
            }
            VaultCommand::Link { source, target, kind, confidence, reason } => {
                vault::link(ctx, source, target, kind, *confidence, reason)
            }
            VaultCommand::Unlink { source, target, kind, reason } => {
                vault::unlink(ctx, source, target, kind, reason)
            }
            VaultCommand::Search { query, limit, semantic, at } => {
                vault::search(ctx, query, *limit, *semantic, at.as_deref())
            }
            VaultCommand::Export { output, format, include_history, kind } => {
                vault::export(ctx, output.as_deref(), format, *include_history, kind.as_deref())
            }
            VaultCommand::Import { input, format, dry_run, on_conflict } => {
                vault::import(ctx, input.as_deref(), format, *dry_run, on_conflict)
            }
            VaultCommand::Gc { keep, apply } => {
                vault::gc(ctx, *keep, *apply)
            }
            VaultCommand::Prune { keep_days, apply } => {
                vault::prune(ctx, *keep_days, *apply)
            }
            VaultCommand::Reindex { embeddings, full } => {
                vault::reindex(ctx, *embeddings, *full)
            }
            VaultCommand::Stats { json, growth } => {
                vault::stats(ctx, *json, *growth)
            }
            VaultCommand::Backup { output, include_wal, compress } => {
                vault::backup(ctx, output, *include_wal, *compress)
            }
            VaultCommand::Restore { input, verify, force } => {
                vault::restore(ctx, input, *verify, *force)
            }
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
                let report = kg::callpath(ctx, from, to).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg callpath report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.callpath", kg::render_callpath(&report), data))
            }
            KgCommand::Outline { file } => {
                let report = kg::outline(ctx, file).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg outline report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.outline", kg::render_outline(&report), data))
            }
            KgCommand::Tree { dir } => {
                let report = kg::tree(ctx, dir).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg tree report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.tree", kg::render_tree(&report), data))
            }
            KgCommand::Review { staged } => {
                let report = kg::review(ctx, *staged).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg review report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.review", kg::render_review(&report), data))
            }
            KgCommand::TestsFor { symbol } => {
                let report = kg::tests_for(ctx, symbol).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize kg tests-for report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("kg.tests_for", kg::render_tests_for(&report), data))
            }
        },
        Verb::Mcp(args) => match &args.action {
            McpCommand::Serve { http, db, profile } => {
                let profile_opt = profile.as_ref().and_then(|p| Profile::from_name(p));
                mcp::serve(ctx, *http, db.clone(), profile_opt)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            McpCommand::Doctor => mcp::doctor(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e)),
            McpCommand::Add(args) => mcp::add_client(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
            McpCommand::Call(args) => mcp::call_tool(ctx, args)
                .map_err(|e| ErrorOutcome::from_anyhow(&e)),
            McpCommand::Selftest(args) => mcp::selftest(ctx, args)
                .map_err(|e| ErrorOutcome::from_anyhow(&e)),
            McpCommand::Clients(args) => mcp::list_clients(ctx, args)
                .map_err(|e| ErrorOutcome::from_anyhow(&e)),
            McpCommand::Token(args) => mcp::token(ctx, args)
                .map_err(|e| ErrorOutcome::from_anyhow(&e)),
            McpCommand::Logs(args) => {
                let report = mcp::logs(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize mcp logs report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("mcp.logs", mcp::render_logs(&report), data))
            }
            McpCommand::Profile(args) => match &args.action {
                McpProfileCommand::Show(args) => {
                    let report = mcp::profile_show(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                    let data = serde_json::to_value(&report)
                        .context("serialize mcp profile show report")
                        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                    Ok(Outcome::new("mcp.profile.show", mcp::render_profile_show(&report), data))
                }
                McpProfileCommand::Set(args) => {
                    let report = mcp::profile_set(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                    let data = serde_json::to_value(&report)
                        .context("serialize mcp profile set report")
                        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                    Ok(Outcome::new("mcp.profile.set", mcp::render_profile_set(&report), data))
                }
            },
            McpCommand::Exposes(args) => {
                let report = mcp::exposes(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize mcp exposes report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("mcp.exposes", mcp::render_exposes(&report), data))
            }
            McpCommand::Restart { server, force } => {
                let report = mcp::restart(ctx, server.clone(), *force)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                let data = serde_json::to_value(&report)
                    .context("serialize mcp restart report")
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                Ok(Outcome::new("mcp.restart", mcp::render_restart(&report), data))
            }
        },
        Verb::Models(args) => match &args.action {
            crate::cli::ModelsCommand::List(args) => models::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
            crate::cli::ModelsCommand::Use(args) => models::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        },
        Verb::Usage(args) => usage::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Stats(args) => stats::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
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
        Verb::Teams(args) => match &args.action {
            TeamsCommand::Join(args) => {
                teams::join(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
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
        },
        Verb::Update(args) => {
            update_cmd::execute_update(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
        },
        Verb::Install(args) => {
            update_cmd::execute_install(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e))
        },
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
                show_completions(shell).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
            crate::cli::CompletionCommand::Install(install_args) => {
                let shell = install_args
                    .shell
                    .as_deref()
                    .and_then(|s| match s {
                        "bash" => Some(Shell::Bash),
                        "zsh" => Some(Shell::Zsh),
                        "fish" => Some(Shell::Fish),
                        "powershell" => Some(Shell::PowerShell),
                        "elvish" => Some(Shell::Elvish),
                        _ => None,
                    });
                install(shell).map_err(|e| ErrorOutcome::from_anyhow(&e))
            }
        },
        Verb::Setup(args) => setup::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Telemetry(args) => telemetry::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Privacy(args) => crate::sync::run_privacy(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Billing(args) => billing::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Worktree(args) => worktree::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Batch(args) => worktree::run_batch_cmd(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Parallel(args) => worktree::run_parallel(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Skill(args) => {
            let skill_cmd = match &args.action {
                SkillCommand::Add(a) => SkillsSkillCommand::Add(memnoth_skills::commands::AddArgs {
                    path: a.path.clone(),
                    name: a.name.clone(),
                }),
                SkillCommand::List(a) => SkillsSkillCommand::List(memnoth_skills::commands::ListArgs {
                    detail: a.detail,
                    tag: a.tag.clone(),
                    json: a.json,
                }),
                SkillCommand::Remove(a) => SkillsSkillCommand::Remove(memnoth_skills::commands::RemoveArgs {
                    name: a.name.clone(),
                    force: a.force,
                }),
                SkillCommand::Run(a) => SkillsSkillCommand::Run(memnoth_skills::commands::RunArgs {
                    name: a.name.clone(),
                    args: a.args.clone(),
                    json: a.json,
                }),
                SkillCommand::Edit(a) => SkillsSkillCommand::Edit(memnoth_skills::commands::EditArgs {
                    name: a.name.clone(),
                    editor: a.editor.clone(),
                }),
            };
            let result = execute_skill_command(skill_cmd)
                .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
            Ok(Outcome::new("skill", result, serde_json::json!({})))
        },
        Verb::Watch(args) => watch::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Serve(args) => serve::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Open(args) => open::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Docs(args) => docs::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        Verb::Connect(args) => match &args.action {
            ConnectCommand::Add(args) => connect::connect_add(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
            ConnectCommand::List(args) => connect::connect_list(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
            ConnectCommand::Remove(args) => connect::connect_remove(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
            ConnectCommand::Test(args) => connect::connect_test(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
        },
        Verb::Feedback(args) => feedback::run(ctx, args).map_err(|e| ErrorOutcome::from_anyhow(&e)),
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
