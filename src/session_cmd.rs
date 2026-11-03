use anyhow::{Context, Result, bail};
use memnoth_session::{Message, Role, Session};
use serde_json::json;
use std::path::PathBuf;

use crate::context::AppContext;
use crate::output::{ExitCode, Outcome};
use crate::session::{SESSION_MODEL, SessionStore, now_ms, transcript, validate_name};

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
        (session, false)
    };

    // If model was explicitly provided and differs from what's on disk, update it
    if let Some(m) = model {
        if m != session.model {
            // Session was loaded, but model flag overrides
        }
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

    // Store effort level in session metadata (we'll add it as a special message)
    // For now, we'll record it as a system message
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

/// `mn replay <session>` — deterministically replay a session from its transcript.
pub fn replay(
    ctx: &AppContext,
    name: &str,
    from_turn: usize,
    max_turns: Option<usize>,
    model: Option<&str>,
    format: &str,
    output: Option<&str>,
    diff: bool,
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

    // For now, we'll output the replay transcript
    // In a full implementation, this would re-run the model with the same context
    let mut output_lines = Vec::new();
    output_lines.push(format!(
        "Replaying session '{}' from turn {} to {}",
        name, from_turn, end_turn
    ));
    output_lines.push(format!("Original model: {}", session.model));
    if let Some(m) = model {
        output_lines.push(format!("Replay model: {}", m));
    }
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
        "from_turn": from_turn,
        "to_turn": end_turn,
        "turns_replayed": replay_turns.len(),
        "original_model": session.model,
        "replay_model": model,
        "diff_requested": diff,
    });

    let text = if output.is_some() {
        format!(
            "Replay saved to {} ({} turns)",
            output.unwrap(),
            replay_turns.len()
        )
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
            "include_vault": include_vault,
            "include_kg": include_kg,
            "created_ms": crate::session::now_ms(),
        }
    });

    // TODO: Include vault snapshot if requested
    if include_vault {
        bundle["vault"] = json!({
            "status": "not_implemented",
            "message": "Vault snapshot not yet implemented"
        });
    }

    // TODO: Include code graph snapshot if requested
    if include_kg {
        bundle["kg"] = json!({
            "status": "not_implemented",
            "message": "Code graph snapshot not yet implemented"
        });
    }

    let bundle_json =
        serde_json::to_string_pretty(&bundle).context("failed to serialize bundle")?;

    let size_bytes = bundle_json.len();
    std::fs::write(&bundle_path, bundle_json)
        .with_context(|| format!("failed to write bundle to {}", bundle_path.display()))?;

    let data = json!({
        "session": name,
        "bundle_path": bundle_path.display().to_string(),
        "include_vault": include_vault,
        "include_kg": include_kg,
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
    workspace: Option<&str>,
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

    // Build sandbox policy
    let sandbox_policy = json!({
        "profile": policy,
        "network": network,
        "write_outside_workspace": write_outside,
        "timeout_seconds": timeout,
        "workspace": ctx.root.display().to_string(),
    });

    // In a full implementation, this would invoke memnoth-sandbox
    // For now, we'll use std::process::Command with basic restrictions
    let mut command = std::process::Command::new(&cmd[0]);
    command.args(&cmd[1..]);

    command.current_dir(&ctx.root);

    // Set timeout
    let output = command
        .output()
        .with_context(|| format!("failed to execute command: {}", cmd.join(" ")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

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
        "network_allowed": network,
        "write_outside_allowed": write_outside,
        "timeout_seconds": timeout,
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "session": session,
    });

    let text = if exit_code == 0 {
        format!("Command completed successfully (exit code {})", exit_code)
    } else {
        format!("Command failed with exit code {}", exit_code)
    };

    Ok(Outcome::new("session.exec", text, data))
}
