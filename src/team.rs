//! Team coordination commands using memnoth-coord crate.
//!
//! Commands:
//! - `mn team claim <task>` / `release` — claim/release tasks from shared board
//! - `mn team tasks` / `mn team inbox` — list tasks and messages
//! - `mn team presence` — show team presence with cloud sync
//! - `mn team lock <path>` — lock file paths for exclusive editing
//! - `mn team handoff` — hand off tasks to team members
//! - `mn team review` — request code reviews
//! - `mn audit` — audit team activity

use anyhow::{Context, Result};
use memnoth_cloud_client::CloudClient;
use memnoth_coord::{
    ClaimResult, Coord, SessionStatus, TaskRow, TaskStatus, WorkerRow,
};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::context::AppContext;
use crate::output::Outcome;
use memnoth_auth::get_credential_sync;

/// Path to the coordination database file.
const COORD_DB_NAME: &str = "coord.db";

/// Get the coordination database path.
fn coord_db_path(ctx: &AppContext) -> std::path::PathBuf {
    ctx.home.join(COORD_DB_NAME)
}

/// Open or create a Coord instance.
fn open_coord(ctx: &AppContext) -> Result<Coord> {
    let db_path = coord_db_path(ctx);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("cannot create coordination dir {}", parent.display())
        })?;
    }
    Coord::open(&db_path).context("failed to open coordination database")
}

/// Get or create a session ID for the current user.
fn get_session_id(coord: &Coord, ctx: &AppContext) -> Result<String> {
    // Generate a session ID based on current user/machine
    let name = current_user();
    let pid = std::process::id();
    let project_root = ctx.root.to_string_lossy().to_string();
    coord
        .join(&name, pid, &project_root)
        .context("failed to join coordination room")
}

/// The local account name, from the environment.
///
/// The CLI deliberately carries no user-account dependency: the name is only a
/// label on the coordination board, and an unset variable degrades to a stable
/// default rather than failing a `team` command.
fn current_user() -> String {
    ["MEMNOTH_USER", "USER", "LOGNAME", "USERNAME"]
        .iter()
        .find_map(|key| std::env::var(key).ok())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "memnoth".to_string())
}

/// A connection to the coordination database for the statements `memnoth-coord`
/// does not expose yet (releasing a task claim, and the audit reads).
fn coord_connection(ctx: &AppContext) -> Result<rusqlite::Connection> {
    rusqlite::Connection::open(coord_db_path(ctx))
        .context("failed to open coordination database")
}

/// Get the cloud client with auth token.
fn get_cloud_client(ctx: &AppContext) -> Result<CloudClient> {
    let base_url = get_base_url(ctx)?;
    let mut client = CloudClient::with_base_url(&base_url)
        .context("failed to create cloud client")?;

    let token = get_credential_sync("api_key")?
        .ok_or_else(|| anyhow::anyhow!("not authenticated; run `mn auth login` first"))?;

    client.set_auth_token(token);
    Ok(client)
}

/// Get the base URL for the cloud API.
fn get_base_url(_ctx: &AppContext) -> Result<String> {
    Ok(std::env::var("MEMNOTH_CLOUD_URL")
        .unwrap_or_else(|_| "https://memnoth.astrivya.ai/api/v1".to_string()))
}

/// Parse a time range string (e.g., "24h", "7d", "30d") into milliseconds.
fn parse_range(range: &str) -> Result<i64> {
    let range = range.trim().to_lowercase();
    let (num_str, unit) = if range.ends_with('h') {
        (&range[..range.len() - 1], "h")
    } else if range.ends_with('d') {
        (&range[..range.len() - 1], "d")
    } else if range.ends_with('m') {
        (&range[..range.len() - 1], "m")
    } else if range.ends_with('s') {
        (&range[..range.len() - 1], "s")
    } else {
        return Err(anyhow::anyhow!("invalid range format; use e.g. 24h, 7d, 30m"));
    };

    let num: i64 = num_str
        .parse()
        .context("invalid number in range")?;

    let ms = match unit {
        "h" => num * 60 * 60 * 1000,
        "d" => num * 24 * 60 * 60 * 1000,
        "m" => num * 60 * 1000,
        "s" => num * 1000,
        _ => return Err(anyhow::anyhow!("invalid unit in range")),
    };
    Ok(ms)
}

/// Claim a task from the shared board.
pub fn claim(ctx: &AppContext, args: &crate::cli::TeamClaimArgs) -> Result<Outcome> {
    let coord = open_coord(ctx)?;
    let session_id = get_session_id(&coord, ctx)?;

    coord
        .task_claim(&args.task_id, &session_id)
        .with_context(|| format!("failed to claim task {}", args.task_id))?;

    let text = format!("Claimed task {}", args.task_id);
    let data = json!({
        "task_id": args.task_id,
        "session_id": session_id,
        "action": "claim",
    });

    Ok(Outcome::new("team.claim", text, data))
}

/// Release a claimed task.
pub fn release(ctx: &AppContext, args: &crate::cli::TeamReleaseArgs) -> Result<Outcome> {
    let coord = open_coord(ctx)?;
    let session_id = get_session_id(&coord, ctx)?;

    // Update task status back to pending and clear claimed_by
    coord
        .task_update_status(&args.task_id, TaskStatus::Pending)
        .with_context(|| format!("failed to release task {}", args.task_id))?;
    // Also clear the claimed_by field
    let conn = coord_connection(ctx)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    conn.execute(
        "UPDATE coord_tasks SET claimed_by = NULL, updated_at = ?1 WHERE id = ?2 AND claimed_by = ?3",
        rusqlite::params![now, &args.task_id, &session_id],
    )?;

    let text = format!("Released task {}", args.task_id);
    let data = json!({
        "task_id": args.task_id,
        "session_id": session_id,
        "action": "release",
    });

    Ok(Outcome::new("team.release", text, data))
}

/// List tasks on the shared board.
pub fn tasks(ctx: &AppContext, args: &crate::cli::TeamTasksArgs) -> Result<Outcome> {
    let coord = open_coord(ctx)?;
    let all_tasks = coord.task_list().context("failed to list tasks")?;

    // Filter tasks based on flags
    let filtered: Vec<TaskRow> = all_tasks
        .into_iter()
        .filter(|t| {
            let matches_status = match t.status {
                TaskStatus::Pending => args.pending || !(args.in_progress || args.done),
                TaskStatus::InProgress => args.in_progress || !(args.pending || args.done),
                TaskStatus::Done => args.done || !(args.pending || args.in_progress),
                TaskStatus::Blocked => true, // always show blocked
            };
            matches_status
        })
        .collect();

    let text = if filtered.is_empty() {
        "No tasks found".to_string()
    } else {
        format!(
            "{} task(s):\n{}",
            filtered.len(),
            filtered
                .iter()
                .map(|t| format!(
                    "  {} [{}] {} {}",
                    t.id,
                    t.status.as_str(),
                    t.title,
                    t.claimed_by
                        .as_ref()
                        .map(|c| format!("(claimed by {})", c))
                        .unwrap_or_default()
                ))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let data = json!({
        "tasks": filtered.iter().map(|t| json!({
            "id": t.id,
            "title": t.title,
            "description": t.description,
            "status": t.status.as_str(),
            "claimed_by": t.claimed_by,
            "depends_on": t.depends_on,
        })).collect::<Vec<_>>(),
        "count": filtered.len(),
    });

    Ok(Outcome::new("team.tasks", text, data))
}

/// Show messages in your inbox.
pub fn inbox(ctx: &AppContext, args: &crate::cli::TeamInboxArgs) -> Result<Outcome> {
    let coord = open_coord(ctx)?;
    let session_id = get_session_id(&coord, ctx)?;

    let messages = coord
        .inbox(&session_id, args.broadcast)
        .context("failed to read inbox")?;

    if args.ack && !messages.is_empty() {
        for msg in &messages {
            coord.ack(msg.id).context("failed to acknowledge message")?;
        }
    }

    let text = if messages.is_empty() {
        "Inbox is empty".to_string()
    } else {
        format!(
            "{} message(s):\n{}",
            messages.len(),
            messages
                .iter()
                .map(|m| format!(
                    "  [{}] from {}: {} {}",
                    m.id,
                    m.from_session,
                    m.body,
                    if m.acked { "(acked)" } else { "" }
                ))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let data = json!({
        "messages": messages.iter().map(|m| json!({
            "id": m.id,
            "from_session": m.from_session,
            "to_session": m.to_session,
            "body": m.body,
            "created_at": m.created_at,
            "acked": m.acked,
        })).collect::<Vec<_>>(),
        "count": messages.len(),
    });

    Ok(Outcome::new("team.inbox", text, data))
}

/// Show team presence (active sessions).
pub fn presence(ctx: &AppContext, args: &crate::cli::TeamPresenceArgs) -> Result<Outcome> {
    let coord = open_coord(ctx)?;
    let session_id = get_session_id(&coord, ctx)?;

    // Clean up expired claims and stale sessions
    coord.cleanup_expired().context("failed to cleanup expired")?;

    let mut sessions = coord.status().context("failed to get status")?;

    // Filter stale if requested
    if args.stale {
        sessions.retain(|s| s.stale);
    }

    // Sync with cloud if requested
    let mut cloud_synced = false;
    let mut cloud_sessions: Vec<Value> = Vec::new();
    if args.sync {
        if let Ok(client) = get_cloud_client(ctx) {
            // The cloud API reports the authenticated account and its
            // organizations; it has no member roster yet, so the account itself
            // is the one presence entry that is known to be online.
            if let Ok(me) = client.me() {
                cloud_synced = true;
                let role = me
                    .organizations
                    .as_ref()
                    .and_then(|organizations| organizations.first())
                    .map(|organization| organization.role.clone())
                    .unwrap_or_else(|| "member".to_string());
                let name = me.name.clone().unwrap_or_else(|| me.email.clone());
                cloud_sessions = vec![json!({
                    "user_id": me.id,
                    "name": name,
                    "role": role,
                    "online": true,
                })];
            }
        }
    }

    let text = if sessions.is_empty() && cloud_sessions.is_empty() {
        "No active sessions".to_string()
    } else {
        let mut parts = Vec::new();
        if !sessions.is_empty() {
            parts.push(format!(
                "Local sessions ({}):\n{}",
                sessions.len(),
                sessions
                    .iter()
                    .map(|s| format!(
                        "  {} ({}) {} {} claim(s){}",
                        s.name,
                        s.session_id,
                        if s.stale { "[STALE]" } else { "" },
                        s.active_claims.len(),
                        s.active_claims
                            .iter()
                            .map(|c| format!("\n    - {}", c))
                            .collect::<Vec<_>>()
                            .join("")
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        if !cloud_sessions.is_empty() {
            parts.push(format!(
                "Cloud members ({}):\n{}",
                cloud_sessions.len(),
                cloud_sessions
                    .iter()
                    .map(|m| format!(
                        "  {} ({}) {}",
                        m.get("name").and_then(|v| v.as_str()).unwrap_or("unknown"),
                        m.get("user_id").and_then(|v| v.as_str()).unwrap_or(""),
                        if m.get("online").and_then(|v| v.as_bool()).unwrap_or(false) {
                            "🟢"
                        } else {
                            "⚪"
                        }
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        parts.join("\n\n")
    };

    let data = json!({
        "local_sessions": sessions.iter().map(|s| json!({
            "session_id": s.session_id,
            "name": s.name,
            "pid": s.pid,
            "project_root": s.project_root,
            "heartbeat_age_ms": s.heartbeat_age_ms,
            "stale": s.stale,
            "active_claims": s.active_claims,
        })).collect::<Vec<_>>(),
        "cloud_synced": cloud_synced,
        "cloud_members": cloud_sessions,
    });

    Ok(Outcome::new("team.presence", text, data))
}

/// Lock a file path for exclusive editing.
pub fn lock(ctx: &AppContext, args: &crate::cli::TeamLockArgs) -> Result<Outcome> {
    let coord = open_coord(ctx)?;
    let session_id = get_session_id(&coord, ctx)?;

    let result = coord
        .claim(&session_id, &args.path, args.ttl)
        .with_context(|| format!("failed to lock path {}", args.path))?;

    match result {
        ClaimResult::Granted { claim_id } => {
            let text = format!(
                "Locked {} for {} seconds (claim #{})",
                args.path, args.ttl, claim_id
            );
            let data = json!({
                "path": args.path,
                "claim_id": claim_id,
                "ttl_secs": args.ttl,
                "session_id": session_id,
                "action": "lock",
            });
            Ok(Outcome::new("team.lock", text, data))
        }
        ClaimResult::Refused {
            holder_session,
            holder_glob,
        } => {
            let text = format!(
                "Lock refused: {} is already locked by session {} (glob: {})",
                args.path, holder_session, holder_glob
            );
            let data = json!({
                "path": args.path,
                "holder_session": holder_session,
                "holder_glob": holder_glob,
                "action": "lock",
                "granted": false,
            });
            Ok(Outcome::new("team.lock", text, data))
        }
    }
}

/// Hand off a task to another team member.
pub fn handoff(ctx: &AppContext, args: &crate::cli::TeamHandoffArgs) -> Result<Outcome> {
    let coord = open_coord(ctx)?;
    let session_id = get_session_id(&coord, ctx)?;

    // Release our claim on the task
    coord
        .task_update_status(&args.task_id, TaskStatus::Pending)
        .with_context(|| format!("failed to release task {}", args.task_id))?;
    let conn = coord_connection(ctx)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    conn.execute(
        "UPDATE coord_tasks SET claimed_by = NULL, updated_at = ?1 WHERE id = ?2 AND claimed_by = ?3",
        rusqlite::params![now, &args.task_id, &session_id],
    )?;

    // Send a message to the target
    let message = args.message.clone().unwrap_or_else(|| {
        format!(
            "Task {} handed off to you from {}",
            args.task_id, session_id
        )
    });
    coord
        .send_message(&session_id, Some(&args.target), &message)
        .context("failed to send handoff message")?;

    let text = format!("Handed off task {} to {}", args.task_id, args.target);
    let data = json!({
        "task_id": args.task_id,
        "from_session": session_id,
        "to_session": args.target,
        "message": message,
        "action": "handoff",
    });

    Ok(Outcome::new("team.handoff", text, data))
}

/// Request a review for a task.
pub fn review(ctx: &AppContext, args: &crate::cli::TeamReviewArgs) -> Result<Outcome> {
    let coord = open_coord(ctx)?;
    let session_id = get_session_id(&coord, ctx)?;

    // Update task status to blocked (waiting for review)
    coord
        .task_update_status(&args.task_id, TaskStatus::Blocked)
        .with_context(|| format!("failed to update task {}", args.task_id))?;

    // Send a review request message to the reviewer
    let message = args.message.clone().unwrap_or_else(|| {
        format!(
            "Review requested for task {} by {}",
            args.task_id, session_id
        )
    });
    coord
        .send_message(&session_id, Some(&args.reviewer), &message)
        .context("failed to send review request")?;

    let text = format!("Requested review for task {} from {}", args.task_id, args.reviewer);
    let data = json!({
        "task_id": args.task_id,
        "requester": session_id,
        "reviewer": args.reviewer,
        "message": message,
        "action": "review",
    });

    Ok(Outcome::new("team.review", text, data))
}

/// Run an audit of team activity.
pub fn audit(ctx: &AppContext, args: &crate::cli::TeamAuditArgs) -> Result<Outcome> {
    let coord = open_coord(ctx)?;

    let range_ms = parse_range(&args.range)
        .with_context(|| format!("invalid range: {}", args.range))?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let since = now - range_ms;

    // Get all sessions (including stale)
    let all_sessions = coord.status().context("failed to get sessions")?;

    // Filter by session if requested
    let sessions: Vec<SessionStatus> = if let Some(ref sid) = args.session {
        all_sessions
            .into_iter()
            .filter(|s| s.session_id == *sid)
            .collect()
    } else {
        all_sessions
    };

    // Get all tasks
    let all_tasks = coord.task_list().context("failed to list tasks")?;

    // Get all workers
    let all_workers = coord.worker_list(args.session.as_deref()).context("failed to list workers")?;

    // Filter workers by time range
    let recent_workers: Vec<WorkerRow> = all_workers
        .into_iter()
        .filter(|w| w.started_at >= since)
        .collect();

    // Get all claims
    let conn = coord_connection(ctx)?;
    let mut stmt = conn.prepare(
        "SELECT id, session_id, path_glob, created_at, expires_at
         FROM coord_claims WHERE created_at >= ?1 ORDER BY created_at DESC",
    )?;
    let claims: Vec<Value> = stmt
        .query_map(rusqlite::params![since], |row| {
            Ok(json!({
                "id": row.get::<_, i64>(0)?,
                "session_id": row.get::<_, String>(1)?,
                "path_glob": row.get::<_, String>(2)?,
                "created_at": row.get::<_, i64>(3)?,
                "expires_at": row.get::<_, i64>(4)?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // Get messages in range
    let mut stmt = conn.prepare(
        "SELECT id, from_session, to_session, body, created_at, acked_at
         FROM coord_messages WHERE created_at >= ?1 ORDER BY created_at DESC",
    )?;
    let messages: Vec<Value> = stmt
        .query_map(rusqlite::params![since], |row| {
            Ok(json!({
                "id": row.get::<_, i64>(0)?,
                "from_session": row.get::<_, String>(1)?,
                "to_session": row.get::<_, Option<String>>(2)?,
                "body": row.get::<_, String>(3)?,
                "created_at": row.get::<_, i64>(4)?,
                "acked_at": row.get::<_, Option<i64>>(5)?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let text = format!(
        "Team audit for last {} ({} ms):\n\
         Sessions: {}\n\
         Tasks: {}\n\
         Workers started: {}\n\
         Claims: {}\n\
         Messages: {}",
        args.range, range_ms, sessions.len(), all_tasks.len(), recent_workers.len(), claims.len(), messages.len()
    );

    let data = json!({
        "range": args.range,
        "since_ms": since,
        "sessions": sessions.iter().map(|s| json!({
            "session_id": s.session_id,
            "name": s.name,
            "pid": s.pid,
            "project_root": s.project_root,
            "heartbeat_age_ms": s.heartbeat_age_ms,
            "stale": s.stale,
            "active_claims": s.active_claims,
        })).collect::<Vec<_>>(),
        "tasks": all_tasks.iter().map(|t| json!({
            "id": t.id,
            "title": t.title,
            "status": t.status.as_str(),
            "claimed_by": t.claimed_by,
        })).collect::<Vec<_>>(),
        "workers": recent_workers.iter().map(|w| json!({
            "id": w.id,
            "name": w.name,
            "session_id": w.session_id,
            "task": w.task,
            "status": w.status.as_str(),
            "started_at": w.started_at,
            "completed_at": w.completed_at,
            "result": w.result,
            "error": w.error,
        })).collect::<Vec<_>>(),
        "claims": claims,
        "messages": messages,
    });

    Ok(Outcome::new("team.audit", text, data))
}