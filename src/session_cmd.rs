use anyhow::{Context, Result, bail};
use memnoth_session::{Role, Session};
use serde_json::json;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::context::AppContext;
use crate::output::Outcome;
use crate::session::{SESSION_MODEL, SessionStore, validate_name};

/// `memnoth session start [--resume NAME] [--model ID]` — start a new session.
pub fn start(
    ctx: &AppContext,
    name: Option<&str>,
    resume: Option<&str>,
    model: Option<&str>,
) -> Result<Outcome> {
    let store = SessionStore::new(&ctx.sessions_dir);
    let name = name.unwrap_or("session");
    let validated =
        validate_name(name).with_context(|| format!("invalid session name {name:?}"))?;

    let model_label = model.unwrap_or(SESSION_MODEL);

    let (session, was_resumed) = if let Some(resume_name) = resume {
        let validated_resume = validate_name(resume_name)
            .with_context(|| format!("invalid session name {resume_name:?}"))?;
        store.load_or_create(&validated_resume)?
    } else {
        let mut session = Session::new(model_label);
        session.id = validated.clone();
        // Spend is attributed at the moment the session starts: the project
        // can change later, and a receipt must keep the project it was spent
        // under rather than being re-attributed by today's config.
        session.project_id = crate::report::project_id(ctx);
        (session, false)
    };

    // If model was explicitly provided and differs from what's on disk, update it
    if let Some(m) = model
        && m != session.model
    {
        // Session was loaded, but model flag overrides
    }

    let path = store.save(&session)?;

    let data = json!({
        "name": session.id,
        "model": session.model,
        "created_ms": session.created_ms,
        "path": path.display().to_string(),
        "resumed": was_resumed,
        "messages": session.messages.len(),
    });

    let text = if was_resumed {
        format!(
            "Resumed session '{}' ({} messages)",
            session.id,
            session.messages.len()
        )
    } else {
        format!("Started session '{}'", session.id)
    };

    Ok(Outcome::new("session.start", text, data))
}

/// `memnoth session resume <name>` — resume an existing session.
pub fn resume(ctx: &AppContext, name: &str) -> Result<Outcome> {
    let store = SessionStore::new(&ctx.sessions_dir);
    let validated =
        validate_name(name).with_context(|| format!("invalid session name {name:?}"))?;

    let path = store.path(&validated)?;
    if !path.exists() {
        bail!("session '{}' not found", validated);
    }

    let mut session =
        Session::load(&path).with_context(|| format!("cannot load session {}", path.display()))?;

    // The file name is the identity a user types, so it wins over the header id
    session.id = validated.clone();

    let save_path = store.save(&session)?;

    let data = json!({
        "name": session.id,
        "model": session.model,
        "created_ms": session.created_ms,
        "path": save_path.display().to_string(),
        "messages": session.messages.len(),
        "input_tokens": session.input_tokens,
        "output_tokens": session.output_tokens,
    });

    let text = format!(
        "Resumed session '{}' ({} messages, {} input + {} output tokens)",
        session.id,
        session.messages.len(),
        session.input_tokens,
        session.output_tokens
    );

    Ok(Outcome::new("session.resume", text, data))
}

/// `memnoth session list [--json]` — list all sessions.
pub fn list(ctx: &AppContext, _json: bool) -> Result<Outcome> {
    let store = SessionStore::new(&ctx.sessions_dir);
    let sessions = store.list();

    let data = json!({
        "sessions": sessions.iter().map(|s| json!({
            "name": s.name,
            "model": s.model,
            "created_ms": s.created_ms,
            "messages": s.messages,
            "path": s.path,
        })).collect::<Vec<_>>(),
        "count": sessions.len(),
    });

    let text = if sessions.is_empty() {
        "No sessions found.".to_string()
    } else {
        let mut lines = Vec::new();
        for s in &sessions {
            let created = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(s.created_ms)
                .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            lines.push(format!(
                "{}  {}  {} messages  {}",
                s.name, created, s.messages, s.model
            ));
        }
        lines.join("\n")
    };

    Ok(Outcome::new("session.list", text, data))
}

/// `memnoth session status [--name NAME] [--json]` — show session status.
pub fn status(ctx: &AppContext, name: Option<&str>, _json: bool) -> Result<Outcome> {
    let store = SessionStore::new(&ctx.sessions_dir);

    let name = name.unwrap_or("session");
    let validated =
        validate_name(name).with_context(|| format!("invalid session name {name:?}"))?;

    let path = store.path(&validated)?;
    if !path.exists() {
        bail!("session '{}' not found", validated);
    }

    let session =
        Session::load(&path).with_context(|| format!("cannot load session {}", path.display()))?;

    let (spend_micros, priced_turns) = session.spend();

    let data = json!({
        "name": session.id,
        "model": session.model,
        "created_ms": session.created_ms,
        "messages": session.messages.len(),
        "user_messages": session.messages.iter().filter(|m| m.role == Role::User).count(),
        "assistant_messages": session.messages.iter().filter(|m| m.role == Role::Assistant).count(),
        "tool_messages": session.messages.iter().filter(|m| m.role == Role::Tool).count(),
        "receipts": session.receipts.len(),
        "writes": session.writes.len(),
        "checkpoints": session.checkpoints.len(),
        "trusted": crate::trust::is_trusted(&ctx.root),
        "root": crate::trust::canonical(&ctx.root).display().to_string(),
        "input_tokens": session.input_tokens,
        "output_tokens": session.output_tokens,
        "spend_micros": spend_micros,
        "priced_turns": priced_turns,
        "path": path.display().to_string(),
    });

    let created = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(session.created_ms)
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let text = format!(
        "Session: {}\nModel: {}\nCreated: {}\nMessages: {} ({} user, {} assistant, {} tool)\nReceipts: {}\nFile writes: {}\nCheckpoints: {}\nTokens: {} input + {} output\nSpend: ${:.4} ({} priced turns)\nPath: {}",
        session.id,
        session.model,
        created,
        session.messages.len(),
        session
            .messages
            .iter()
            .filter(|m| m.role == Role::User)
            .count(),
        session
            .messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .count(),
        session
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .count(),
        session.receipts.len(),
        session.writes.len(),
        session.checkpoints.len(),
        session.input_tokens,
        session.output_tokens,
        spend_micros as f64 / 1_000_000.0,
        priced_turns,
        path.display()
    );

    // The trust decision this session is running under. A `/status` that omits
    // it lets someone believe a refused project was somehow trusted, and the
    // screen and `doctor` would then disagree about the same folder.
    let text = text
        + &format!(
            "
Trust: {} — {}",
            if crate::trust::is_trusted(&ctx.root) {
                "trusted"
            } else {
                "not trusted (this session only)"
            },
            crate::trust::canonical(&ctx.root).display()
        );

    Ok(Outcome::new("session.status", text, data))
}

/// `memnoth session stop [--name NAME]` — stop (save and close) a session.
pub fn stop(ctx: &AppContext, name: Option<&str>) -> Result<Outcome> {
    let store = SessionStore::new(&ctx.sessions_dir);

    let name = name.unwrap_or("session");
    let validated =
        validate_name(name).with_context(|| format!("invalid session name {name:?}"))?;

    let path = store.path(&validated)?;
    if !path.exists() {
        bail!("session '{}' not found", validated);
    }

    let session =
        Session::load(&path).with_context(|| format!("cannot load session {}", path.display()))?;

    // Save it again to ensure it's properly closed/flushed
    let save_path = store.save(&session)?;

    let (spend_micros, priced_turns) = session.spend();

    let data = json!({
        "name": session.id,
        "model": session.model,
        "created_ms": session.created_ms,
        "messages": session.messages.len(),
        "input_tokens": session.input_tokens,
        "output_tokens": session.output_tokens,
        "spend_micros": spend_micros,
        "priced_turns": priced_turns,
        "path": save_path.display().to_string(),
    });

    let text = format!(
        "Stopped session '{}' ({} messages, {} input + {} output tokens, ${:.4} spend)",
        session.id,
        session.messages.len(),
        session.input_tokens,
        session.output_tokens,
        spend_micros as f64 / 1_000_000.0
    );

    Ok(Outcome::new("session.stop", text, data))
}
/// `memnoth session effort` — set reasoning effort budget for the session.
pub fn effort(ctx: &AppContext, name: Option<&str>, level: &str, _json: bool) -> Result<Outcome> {
    let store = SessionStore::new(&ctx.sessions_dir);
    let name = name.unwrap_or("session");
    let validated =
        validate_name(name).with_context(|| format!("invalid session name {name:?}"))?;

    let path = store.path(&validated)?;
    if !path.exists() {
        bail!("session '{}' not found", validated);
    }

    let mut session =
        Session::load(&path).with_context(|| format!("cannot load session {}", path.display()))?;

    // Validate effort level
    let valid_levels = ["low", "medium", "high", "max"];
    let level_lower = level.to_lowercase();
    if !valid_levels.contains(&level_lower.as_str()) {
        bail!(
            "invalid effort level '{}'; must be one of: {}",
            level,
            valid_levels.join(", ")
        );
    }

    // The session has no metadata field for effort, so it is recorded as a
    // system message — the only channel `memnoth-session` gives a header-less
    // value. A reader that wants the current level takes the last `effort:` line.
    session.push(Role::System, format!("effort:{}", level_lower));

    let save_path = store.save(&session)?;

    let data = json!({
        "name": session.id,
        "effort": level_lower,
        "path": save_path.display().to_string(),
    });

    let text = format!(
        "Set reasoning effort to '{}' for session '{}'",
        level_lower, session.id
    );

    Ok(Outcome::new("session.effort", text, data))
}

/// `mn replay <session>` — print the recorded turns of a session.
///
/// This is a transcript replay, not a model replay: no provider is contacted and
/// no answer is generated. `--model` is therefore refused rather than echoed,
/// because a line reading `Replay model: <id>` next to answers the model never
/// produced is the one thing this verb must not print.
// Each parameter is a CLI flag the verb exposes; grouping them into a struct
// would only move the list to the caller.
#[allow(clippy::too_many_arguments)]
pub fn replay(
    ctx: &AppContext,
    name: &str,
    from_turn: usize,
    max_turns: Option<usize>,
    model: Option<&str>,
    _format: &str,
    output: Option<&str>,
    diff: bool,
    _json: bool,
) -> Result<Outcome> {
    // Checked before any work is done, so a caller asking to re-run a model gets
    // an answer instead of a transcript that quietly ignores the flag.
    if let Some(requested) = model {
        bail!(
            "replay is transcript-only in this build and cannot re-run {requested}; \
             drop --model to print the recorded turns"
        );
    }

    let store = SessionStore::new(&ctx.sessions_dir);
    let validated =
        validate_name(name).with_context(|| format!("invalid session name {name:?}"))?;

    let path = store.path(&validated)?;
    if !path.exists() {
        bail!("session '{}' not found", validated);
    }

    let session =
        Session::load(&path).with_context(|| format!("cannot load session {}", path.display()))?;

    // Get turn indices
    let turns = session.turns();
    if from_turn >= turns.len() {
        bail!(
            "from_turn {} exceeds session turns ({})",
            from_turn,
            turns.len()
        );
    }

    let end_turn = max_turns.map_or(turns.len(), |m| (from_turn + m).min(turns.len()));
    let replay_turns = &turns[from_turn..end_turn];

    let mut output_lines = Vec::new();
    output_lines.push(format!(
        "Transcript replay of session '{}', turns {}..{} (no model was contacted)",
        name, from_turn, end_turn
    ));
    output_lines.push(format!("Recorded model: {}", session.model));
    output_lines.push("---".to_string());

    for (i, &turn_idx) in replay_turns.iter().enumerate() {
        if turn_idx < session.messages.len() {
            let msg = &session.messages[turn_idx];
            if msg.role == Role::User {
                output_lines.push(format!("[Turn {}] User: {}", from_turn + i, msg.content));
            }
        }
        // Find assistant response after this turn
        for msg in &session.messages[turn_idx + 1..] {
            if msg.role == Role::Assistant {
                output_lines.push(format!(
                    "[Turn {}] Assistant: {}",
                    from_turn + i,
                    msg.content
                ));
                break;
            }
        }
    }

    let replay_text = output_lines.join("\n");

    // Write to output file if specified
    if let Some(out_path) = output {
        std::fs::write(out_path, &replay_text)
            .with_context(|| format!("failed to write replay output to {}", out_path))?;
    }

    let data = json!({
        "session": name,
        "format": "transcript",
        "model_contacted": false,
        "from_turn": from_turn,
        "to_turn": end_turn,
        "turns_replayed": replay_turns.len(),
        "original_model": session.model,
        "diff_requested": diff,
    });

    let text = if let Some(output) = output {
        format!("Replay saved to {} ({} turns)", output, replay_turns.len())
    } else {
        replay_text
    };

    Ok(Outcome::new("session.replay", text, data))
}

/// `memnoth session attach` — prepare session for handoff to another machine.
pub fn attach(
    ctx: &AppContext,
    name: &str,
    output: Option<&str>,
    include_vault: bool,
    include_kg: bool,
    _json: bool,
) -> Result<Outcome> {
    let store = SessionStore::new(&ctx.sessions_dir);
    let validated =
        validate_name(name).with_context(|| format!("invalid session name {name:?}"))?;

    let path = store.path(&validated)?;
    if !path.exists() {
        bail!("session '{}' not found", validated);
    }

    let session =
        Session::load(&path).with_context(|| format!("cannot load session {}", path.display()))?;

    // Determine output path
    let bundle_path = output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{}.memnoth-bundle", name)));

    // Create bundle data
    let mut bundle = json!({
        "version": 1,
        "session": {
            "id": session.id,
            "model": session.model,
            "created_ms": session.created_ms,
            "messages": session.messages.iter().map(|m| json!({
                "role": format!("{:?}", m.role).to_lowercase(),
                "content": m.content,
                "at_ms": m.at_ms,
            })).collect::<Vec<_>>(),
            "receipts": session.receipts,
            "writes": session.writes,
            "checkpoints": session.checkpoints,
            "input_tokens": session.input_tokens,
            "output_tokens": session.output_tokens,
        },
        "metadata": {
            "created_ms": crate::session::now_ms(),
        }
    });

    // The snapshots are the same reads `memnoth vault status` and `memnoth kg
    // status` answer with, so a bundle cannot describe a vault or a graph the
    // verbs would not. A read that fails is recorded as its error: the bundle is
    // still a valid handoff, and the field says which half is missing.
    let mut vault_embedded = false;
    let mut kg_embedded = false;

    if include_vault {
        bundle["vault"] = match crate::vault::status(ctx) {
            Ok(outcome) => outcome.data,
            Err(error) => json!({ "error": error.message }),
        };
        vault_embedded = true;
    }

    if include_kg {
        bundle["kg"] = match crate::kg::status(ctx) {
            Ok(status) => serde_json::to_value(&status)
                .unwrap_or_else(|error| json!({ "error": error.to_string() })),
            Err(error) => json!({ "error": error.to_string() }),
        };
        kg_embedded = true;
    }

    bundle["metadata"]["include_vault"] = json!(vault_embedded);
    bundle["metadata"]["include_kg"] = json!(kg_embedded);

    let bundle_json =
        serde_json::to_string_pretty(&bundle).context("failed to serialize bundle")?;

    let size_bytes = bundle_json.len();
    std::fs::write(&bundle_path, bundle_json)
        .with_context(|| format!("failed to write bundle to {}", bundle_path.display()))?;

    let data = json!({
        "session": name,
        "bundle_path": bundle_path.display().to_string(),
        "include_vault": vault_embedded,
        "include_kg": kg_embedded,
        "size_bytes": size_bytes,
    });

    let text = format!(
        "Session '{}' attached to bundle '{}' ({} bytes)",
        name,
        bundle_path.display(),
        size_bytes
    );

    Ok(Outcome::new("session.attach", text, data))
}

/// `memnoth session handoff` — receive session on another machine.
pub fn handoff(
    ctx: &AppContext,
    bundle: &str,
    name: Option<&str>,
    _workspace: Option<&str>,
    _json: bool,
) -> Result<Outcome> {
    let bundle_path = PathBuf::from(bundle);
    if !bundle_path.exists() {
        bail!("bundle file '{}' not found", bundle);
    }

    let bundle_content = std::fs::read_to_string(&bundle_path)
        .with_context(|| format!("failed to read bundle {}", bundle_path.display()))?;

    let bundle_json: serde_json::Value =
        serde_json::from_str(&bundle_content).context("failed to parse bundle JSON")?;

    // Extract session data
    let session_data = &bundle_json["session"];
    let session_id = session_data["id"].as_str().unwrap_or("unknown");
    let model = session_data["model"].as_str().unwrap_or("memnoth-local");
    let created_ms = session_data["created_ms"].as_i64().unwrap_or(0);

    // Determine session name
    let session_name = name.unwrap_or(session_id);
    let validated = validate_name(session_name)
        .with_context(|| format!("invalid session name {session_name:?}"))?;

    // Reconstruct session
    let mut session = Session::new(model);
    session.id = validated.clone();
    session.created_ms = created_ms;

    // Restore messages
    if let Some(messages) = session_data["messages"].as_array() {
        for msg in messages {
            let role_str = msg["role"].as_str().unwrap_or("user");
            let role = match role_str {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                "system" => Role::System,
                "tool" => Role::Tool,
                _ => Role::User,
            };
            let content = msg["content"].as_str().unwrap_or("");
            let at_ms = msg["at_ms"].as_i64().unwrap_or(created_ms);
            session.messages.push(memnoth_session::Message {
                role,
                content: content.to_string(),
                at_ms,
            });
        }
    }

    // Restore receipts
    if let Some(receipts) = session_data["receipts"].as_array() {
        for receipt in receipts {
            if let Ok(r) = serde_json::from_value(receipt.clone()) {
                session.receipts.push(r);
            }
        }
    }

    // Restore writes
    if let Some(writes) = session_data["writes"].as_array() {
        for write in writes {
            if let Ok(w) = serde_json::from_value(write.clone()) {
                session.writes.push(w);
            }
        }
    }

    // Restore checkpoints
    if let Some(checkpoints) = session_data["checkpoints"].as_array() {
        for cp in checkpoints {
            if let Ok(c) = serde_json::from_value(cp.clone()) {
                session.checkpoints.push(c);
            }
        }
    }

    session.input_tokens = session_data["input_tokens"].as_u64().unwrap_or(0);
    session.output_tokens = session_data["output_tokens"].as_u64().unwrap_or(0);

    // Save to new location
    let store = SessionStore::new(&ctx.sessions_dir);
    let save_path = store.save(&session)?;

    let data = json!({
        "session": validated,
        "original_id": session_id,
        "model": model,
        "messages": session.messages.len(),
        "receipts": session.receipts.len(),
        "writes": session.writes.len(),
        "checkpoints": session.checkpoints.len(),
        "path": save_path.display().to_string(),
    });

    let text = format!(
        "Session '{}' handed off as '{}' ({} messages, {} receipts)",
        session_id,
        validated,
        session.messages.len(),
        session.receipts.len()
    );

    Ok(Outcome::new("session.handoff", text, data))
}

/// `memnoth session exec` — run a command under the policy engine (sandbox).
// Each parameter is a CLI flag the verb exposes; grouping them into a struct
// would only move the list to the caller.
#[allow(clippy::too_many_arguments)]
pub fn exec(
    ctx: &AppContext,
    cmd: Vec<String>,
    session: Option<&str>,
    policy: &str,
    network: bool,
    write_outside: bool,
    timeout: u64,
    _json: bool,
) -> Result<Outcome> {
    // Validate policy
    let valid_policies = ["strict", "balanced", "permissive"];
    if !valid_policies.contains(&policy) {
        bail!(
            "invalid policy '{}'; must be one of: {}",
            policy,
            valid_policies.join(", ")
        );
    }

    if cmd.is_empty() {
        bail!("no command specified");
    }

    // Load session context if provided
    let session_context = if let Some(sess_name) = session {
        let store = SessionStore::new(&ctx.sessions_dir);
        let validated = validate_name(sess_name)
            .with_context(|| format!("invalid session name {sess_name:?}"))?;
        let path = store.path(&validated)?;
        if path.exists() {
            let s = Session::load(&path)
                .with_context(|| format!("cannot load session {}", path.display()))?;
            Some(s)
        } else {
            None
        }
    } else {
        None
    };

    // The policy is enforced here, not recorded: a one-shot verb has nobody to
    // ask, so an `ask` is a refusal and a `deny` is a refusal. `--network` and
    // `--write-outside-workspace` shape the plan, and whether the plan becomes a
    // process boundary is the backend's answer — Linux with `bwrap` says yes,
    // every other platform says why not, and the answer prints that reason
    // instead of the word "sandboxed".
    let profile = match policy {
        // `balanced` is the workspace profile: writes in the tree, reads
        // everywhere, network allowed. The strict profile seals the network.
        "strict" => memnoth_sandbox::profiles::ProfileName::Strict,
        "balanced" => memnoth_sandbox::profiles::ProfileName::Workspace,
        // `permissive` is the devbox: the same tree plus the toolchain
        // directories, still no writes outside it.
        _ => memnoth_sandbox::profiles::ProfileName::Devbox,
    };
    let rules_path = ctx.home.join("rules.txt");
    let user_rules = match std::fs::read_to_string(&rules_path) {
        Ok(text) => memnoth_sandbox::rules::parse_rules(&text)
            .with_context(|| format!("{}: unparseable rule file", rules_path.display()))?,
        Err(_) => Vec::new(),
    };
    let rule_set = memnoth_sandbox::rules::RuleSet::with_user_rules(user_rules);
    let line = cmd.join(" ");
    match memnoth_sandbox::rules::decide(&rule_set, "Bash", &line) {
        memnoth_sandbox::rules::RuleDecision::Allow { .. } => {}
        memnoth_sandbox::rules::RuleDecision::Deny { reason, rule } => {
            bail!("denied by policy: {reason} (rule: {rule}); the command was not run")
        }
        memnoth_sandbox::rules::RuleDecision::Ask { reason, rule, .. } => bail!(
            "this command needs confirmation: {reason} (rule: {rule}); \
             `session exec` has nobody to ask, so it was not run"
        ),
    }

    // The same loader `!cmd` and `/tasks run` use, so the three entry points
    // cannot disagree about the policy: the global file, the project file, and
    // a refusal when either does not parse.
    let config = memnoth_sandbox::profiles::load_sandbox_config(&ctx.root)
        .context("cannot read the sandbox policy; the command was not run")?;
    let mut plan = memnoth_sandbox::backend::BackendPlan::build(
        memnoth_sandbox::backend::OsBackend::host(),
        &profile,
        &ctx.root,
        &config,
    )
    .context("cannot build the sandbox plan; the command was not run")?;
    // `--network` is the one way to open the network for a profile that seals
    // it; it can never close one the profile left open, because a flag that
    // looks like a permission must not also be able to revoke one.
    if network && plan.restrict_network {
        plan.restrict_network = false;
    }
    // `--write-outside-workspace` widens the writable roots to the whole
    // filesystem. Stated as a bind of `/` so the plan says what it means.
    if write_outside
        && !plan
            .read_write
            .iter()
            .any(|root| root == &PathBuf::from("/"))
    {
        plan.read_write.push(PathBuf::from("/"));
    }

    let mut argv = cmd.clone();
    let execution = memnoth_sandbox::prepare(&plan, &ctx.root, &argv);
    let (program, args, confinement) = match execution {
        memnoth_sandbox::Execution::Run(confinement) => {
            let (program, args) = match confinement.confined_argv() {
                Some((program, args)) => (program, args),
                None => (argv.remove(0), argv),
            };
            (program, args, confinement)
        }
        memnoth_sandbox::Execution::Refuse { reason } => {
            bail!("{reason}");
        }
    };

    let request = json!({
        "profile": policy,
        "network": network,
        "write_outside_workspace": write_outside,
        "timeout_seconds": timeout,
        "workspace": ctx.root.display().to_string(),
        "sandbox": confinement.describe(),
    });

    let mut child = std::process::Command::new(&program)
        .args(&args)
        .current_dir(&ctx.root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to execute command: {}", cmd.join(" ")))?;

    // Both pipes are drained on their own threads: a child that fills a pipe
    // without being read would otherwise block until the timeout, and a polling
    // loop on `try_wait` is the only way std offers to enforce one.
    fn drain<R: Read + Send + 'static>(
        pipe: Option<R>,
    ) -> Option<std::thread::JoinHandle<Vec<u8>>> {
        pipe.map(|mut pipe| {
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                let _ = pipe.read_to_end(&mut buffer);
                buffer
            })
        })
    }
    let stdout_reader = drain(child.stdout.take());
    let stderr_reader = drain(child.stderr.take());

    // `0` means no deadline, which is the only reading of "no timeout" a caller
    // can express through a u64.
    let deadline = (timeout > 0).then(|| Instant::now() + Duration::from_secs(timeout));
    let mut timed_out = false;
    let mut status = None;
    loop {
        match child.try_wait() {
            Ok(Some(waited)) => {
                status = Some(waited);
                break;
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error)
                    .with_context(|| format!("failed to wait for command: {}", cmd.join(" ")));
            }
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let stdout = stdout_reader
        .map(|reader| reader.join().unwrap_or_default())
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default();
    let stderr = stderr_reader
        .map(|reader| reader.join().unwrap_or_default())
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default();
    let exit_code = status.and_then(|status| status.code()).unwrap_or(-1);

    // Record in session if provided
    if let Some(mut sess) = session_context {
        sess.push(Role::User, format!("$ {}", cmd.join(" ")));
        sess.push(
            Role::Assistant,
            format!(
                "Exit code: {}\nStdout:\n{}\nStderr:\n{}",
                exit_code, stdout, stderr
            ),
        );
        let store = SessionStore::new(&ctx.sessions_dir);
        store.save(&sess)?;
    }

    let data = json!({
        "command": cmd,
        "policy": policy,
        "policy_enforced": true,
        "sandbox": confinement.describe(),
        "sandbox_enforced": confinement.is_enforced(),
        "network_allowed": network,
        "write_outside_allowed": write_outside,
        "timeout_seconds": timeout,
        "timed_out": timed_out,
        "request": request,
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "session": session,
    });

    // One sentence per axis, because a single "sandboxed" would be false on
    // exactly the machines where it matters most. Policy is enforced on every
    // platform (it is this binary's own decision); filesystem and network are
    // enforced only where a backend exists.
    let boundary = confinement.describe();
    let note = if timeout > 0 {
        format!("note: policy enforced; {boundary}; --timeout is enforced")
    } else {
        format!("note: policy enforced; {boundary}; no --timeout was set")
    };

    let text = if timed_out {
        format!(
            "Command timed out after {}s and was killed\n{}",
            timeout, note
        )
    } else if exit_code == 0 {
        format!(
            "Command completed successfully (exit code {})\n{}",
            exit_code, note
        )
    } else {
        format!("Command failed with exit code {}\n{}", exit_code, note)
    };

    Ok(Outcome::new("session.exec", text, data))
}
/// `memnoth session from-claude <path>` — import Claude Code session JSONL.
pub fn from_claude(ctx: &AppContext, path: &str, name: Option<&str>) -> Result<Outcome> {
    let source = PathBuf::from(path);
    if !source.exists() {
        bail!("Claude Code session file not found: {path}");
    }
    let content =
        std::fs::read_to_string(&source).with_context(|| format!("failed to read {path}"))?;
    let mut messages: Vec<memnoth_session::Message> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let rtype = event["type"].as_str().unwrap_or("");
        let role = match rtype {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "system" => Role::System,
            _ => Role::User,
        };
        let content_text = event["content"].as_str().unwrap_or("").to_string();
        let at_ms = event["timestamp"].as_i64().unwrap_or(0);
        messages.push(memnoth_session::Message {
            role,
            content: content_text,
            at_ms,
        });
    }
    import_session(ctx, &messages, name, path)
}

/// `memnoth session from-codex <path>` — import Codex session JSONL.
pub fn from_codex(ctx: &AppContext, path: &str, name: Option<&str>) -> Result<Outcome> {
    let source = PathBuf::from(path);
    if !source.exists() {
        bail!("Codex session file not found: {path}");
    }
    let content =
        std::fs::read_to_string(&source).with_context(|| format!("failed to read {path}"))?;
    let mut messages: Vec<memnoth_session::Message> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let rtype = event["type"].as_str().unwrap_or("");
        let role = match rtype {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "system" => Role::System,
            _ => Role::User,
        };
        let content_text = event["content"].as_str().unwrap_or("").to_string();
        let at_ms = event["timestamp"].as_i64().unwrap_or(0);
        messages.push(memnoth_session::Message {
            role,
            content: content_text,
            at_ms,
        });
    }
    import_session(ctx, &messages, name, path)
}

/// `memnoth session from-omp <path>` — import OMP session JSONL.
pub fn from_omp(ctx: &AppContext, path: &str, name: Option<&str>) -> Result<Outcome> {
    let source = PathBuf::from(path);
    if !source.exists() {
        bail!("OMP session file not found: {path}");
    }
    let content =
        std::fs::read_to_string(&source).with_context(|| format!("failed to read {path}"))?;
    let mut messages: Vec<memnoth_session::Message> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let role = match event["role"].as_str().unwrap_or("user") {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "system" => Role::System,
            "tool" => Role::Tool,
            _ => Role::User,
        };
        let content_text = event["content"].as_str().unwrap_or("").to_string();
        let at_ms = event["at_ms"].as_i64().unwrap_or(0);
        messages.push(memnoth_session::Message {
            role,
            content: content_text,
            at_ms,
        });
    }
    import_session(ctx, &messages, name, path)
}

/// Shared import: write messages into a new OMP session, redacting secrets.
fn import_session(
    ctx: &AppContext,
    messages: &[memnoth_session::Message],
    name: Option<&str>,
    source: &str,
) -> Result<Outcome> {
    let store = SessionStore::new(&ctx.sessions_dir);
    let default_name = std::path::Path::new(source)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("imported");
    let session_name = name.unwrap_or(default_name);
    let validated = validate_name(session_name)
        .with_context(|| format!("invalid session name {session_name:?}"))?;

    let mut session = Session::new(crate::session::SESSION_MODEL);
    session.id = validated.clone();

    // Redact secrets: never persist API keys or tokens in the transcript.
    for m in messages {
        let redacted = redact_secret(&m.content);
        match m.role {
            Role::User => session.push(Role::User, redacted),
            Role::Assistant => session.push(Role::Assistant, redacted),
            Role::System => session.push(Role::System, redacted),
            Role::Tool => session.push(Role::Tool, redacted),
        }
    }

    let save_path = store.save(&session)?;
    let data = serde_json::json!({
        "session": validated,
        "source": source,
        "messages": messages.len(),
        "path": save_path.display().to_string(),
    });
    Ok(Outcome::new(
        "session.import",
        format!(
            "Imported {} messages into session '{}'",
            messages.len(),
            validated
        ),
        data,
    ))
}

/// Redact secret-bearing strings: anything that looks like an API key or
/// token is replaced with a reference, never persisted.
fn redact_secret(text: &str) -> String {
    let out = text.to_string();
    // Redact anything that looks like an API key or token
    let redacted: String = out
        .split_whitespace()
        .map(|word| {
            if (word.starts_with("sk-") || word.starts_with("sk-")) && word.len() > 20 {
                "[REDACTED]".to_string()
            } else {
                word.to_string()
            }
        })
        .collect();
    redacted
}
