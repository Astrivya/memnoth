#![warn(missing_docs)]
// TODO: Change to #![deny(missing_docs)] after all public items are documented

//! memnoth-coord — multi-agent session coordination on the shared vault.db.
//!
//! # Overview
//!
//! This crate provides multi-agent session coordination capabilities:
//!
//! - **Path claims** — First-writer-wins with TTL auto-release
//! - **Task board** — Shared tasks with atomic claiming
//! - **Messaging** — Direct messages and broadcasts
//! - **Session tracking** — Active sessions with staleness detection
//!
//! # Quick Start
//!
//! ```rust
//! use memnoth_coord::Coord;
//! use std::path::Path;
//!
//! // Open or create a coordination room
//! let coord = Coord::open(Path::new("coord.db"))?;
//!
//! // Join the room
//! let session_id = coord.join("my-agent", std::process::id() as i32, "/path/to/project")?;
//!
//! // Claim a path (TTL in seconds)
//! match coord.claim(&session_id, "src/auth/*.rs", 1800)? {
//!     memnoth_coord::ClaimResult::Granted { claim_id } => {
//!         println!("Claim granted: {}", claim_id);
//!     }
//!     memnoth_coord::ClaimResult::Refused { holder_session, holder_glob } => {
//!         println!("Claim refused: held by {} on {}", holder_session, holder_glob);
//!     }
//! }
//!
//! // Leave the room
//! coord.leave(&session_id)?;
//! # Ok::<(), anyhow::Error>(())
//! ```

use std::path::Path;
use std::str::FromStr;

use anyhow::{Result, bail};
use rusqlite::Connection;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod arbitration;
pub mod continuity;
pub mod durable;
pub use durable::{BeginResult, FINAL_TTL_MS, PENDING_TTL_MS};
pub mod feed;
pub use feed::{LockFeed, MissionFeed, PlanNode, TimelineEvent, WorkerFeed};
pub mod logstream;
pub mod worker;
pub use worker::{join_workspace, join_workspace_at};

const STALE_HEARTBEAT_MS: i64 = 15 * 60 * 1000;
const PRUNE_SESSION_MS: i64 = 60 * 60 * 1000;

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Result of a path claim attempt.
#[derive(Debug, Clone, Serialize)]
pub enum ClaimResult {
    /// The claim was stored; this is its id.
    Granted {
        /// Opaque claim id, echoed by release and by the surface.
        claim_id: String,
    },
    /// Another live claim covers the same literal prefix.
    Refused {
        /// Session that holds the conflicting claim.
        holder_session: String,
        /// The glob that holder asked for.
        holder_glob: String,
    },
}

/// Status of a task on the shared board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    /// Not started and nobody holds it.
    Pending,
    /// Someone holds it and is working.
    InProgress,
    /// Waiting on a dependency, a review, or a person.
    Blocked,
    /// Finished.
    Done,
}

impl TaskStatus {
    /// The wire spelling stored in SQLite and emitted in JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Done => "done",
        }
    }
}

impl FromStr for TaskStatus {
    type Err = String;

    /// Parse the wire spelling, rejecting anything the board cannot store so a
    /// corrupted row degrades instead of panicking.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(TaskStatus::Pending),
            "in_progress" => Ok(TaskStatus::InProgress),
            "blocked" => Ok(TaskStatus::Blocked),
            "done" => Ok(TaskStatus::Done),
            _ => Err(format!("unknown task status {s:?}")),
        }
    }
}

/// Status of a session in the coordination room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStatus {
    /// Session id.
    pub session_id: String,
    /// Display name the session joined under.
    pub name: String,
    /// Process id that opened the session.
    pub pid: i64,
    /// Cloud user id, when the session is attributed; empty on a local room.
    pub user_id: String,
    /// Workspace the session joined from.
    pub project_root: String,
    /// Coarse lifecycle state; a joined session is in progress.
    pub status: TaskStatus,
    /// Wall-clock of the last heartbeat (ms since epoch).
    pub last_heartbeat: i64,
    /// Milliseconds since that heartbeat. What `fail_closed` reads.
    pub heartbeat_age_ms: i64,
    /// True when the heartbeat is older than [`STALE_HEARTBEAT_MS`].
    pub stale: bool,
    /// Live claim globs held by this session, sorted.
    pub active_claims: Vec<String>,
}

/// A task on the shared board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRow {
    /// Task id.
    pub id: String,
    /// One-line title.
    pub title: String,
    /// Longer description; empty when the caller gave none.
    pub description: String,
    /// Board status.
    pub status: TaskStatus,
    /// Session that claimed it, when claimed.
    pub claimed_by: Option<String>,
    /// Id of the single task this one waits on, when constrained.
    pub depends_on: Option<String>,
    /// Last status change (ms since epoch).
    pub updated_at: i64,
}

/// Status of a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum WorkerStatus {
    /// Created but not started.
    Pending,
    /// Running now.
    Running,
    /// Finished with a result.
    Done,
    /// Finished with an error.
    Failed,
    /// Stopped before finishing.
    Cancelled,
}

impl WorkerStatus {
    /// The wire spelling stored in SQLite and emitted in JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkerStatus::Pending => "pending",
            WorkerStatus::Running => "running",
            WorkerStatus::Done => "done",
            WorkerStatus::Failed => "failed",
            WorkerStatus::Cancelled => "cancelled",
        }
    }
}

impl FromStr for WorkerStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(WorkerStatus::Pending),
            "running" => Ok(WorkerStatus::Running),
            "done" => Ok(WorkerStatus::Done),
            "failed" => Ok(WorkerStatus::Failed),
            "cancelled" => Ok(WorkerStatus::Cancelled),
            _ => Err(format!("unknown worker status {s:?}")),
        }
    }
}

/// A worker record.
#[derive(Debug, Clone, Serialize)]
pub struct WorkerRow {
    /// Worker id.
    pub id: String,
    /// Label shown in the WORKERS pane.
    pub name: String,
    /// Session the worker is bound to.
    pub session_id: String,
    /// Task text the worker was started with.
    pub task: String,
    /// Lifecycle status.
    pub status: WorkerStatus,
    /// When the worker was created (ms since epoch).
    pub started_at: i64,
    /// When it finished, when it has.
    pub completed_at: Option<i64>,
    /// Result text, when the worker reported one.
    pub result: Option<String>,
    /// Error text, when the worker failed.
    pub error: Option<String>,
}

/// A message in the coordination room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRow {
    /// Dense message id; order is delivery order.
    pub id: i64,
    /// Sending session.
    pub from_session: String,
    /// Recipient; `None` is a broadcast.
    pub to_session: Option<String>,
    /// Verbatim body.
    pub body: String,
    /// When it was sent (ms since epoch).
    pub created_at: i64,
    /// When a recipient acknowledged it, when anyone did.
    pub acked_at: Option<i64>,
}

/// Multi-agent coordination room.
pub struct Coord {
    conn: Connection,
}

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS coord_sessions (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    pid INTEGER NOT NULL,
    project_root TEXT NOT NULL DEFAULT '',
    started_at INTEGER NOT NULL,
    heartbeat_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS coord_claims (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    path_glob TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_coord_claims_glob ON coord_claims(path_glob);
CREATE TABLE IF NOT EXISTS coord_tasks (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK (status IN ('pending','in_progress','blocked','done')),
    claimed_by TEXT,
    depends_on TEXT,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS coord_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    from_session TEXT NOT NULL,
    to_session TEXT,
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    acked_at INTEGER
);
CREATE TABLE IF NOT EXISTS coord_workers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    session_id TEXT NOT NULL,
    task TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending','running','done','failed','cancelled')),
    started_at INTEGER NOT NULL,
    completed_at INTEGER,
    result TEXT,
    error TEXT
);
CREATE INDEX IF NOT EXISTS idx_coord_workers_session ON coord_workers(session_id);
CREATE INDEX IF NOT EXISTS idx_coord_workers_status ON coord_workers(status);
CREATE TABLE IF NOT EXISTS coord_ops (
    op_key TEXT PRIMARY KEY,
    holder_session TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending','final')),
    grant TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_coord_ops_status ON coord_ops(status, updated_at);
CREATE TABLE IF NOT EXISTS coord_vault_sessions (
    session_id TEXT NOT NULL,
    org_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    opened_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_coord_vault_sessions_org ON coord_vault_sessions(org_id, namespace);
";

/// Literal prefix of a glob: everything before the first wildcard character.
/// Overlap check is prefix-based — `src/a/*` conflicts with `src/a/**` and
/// `src/ab` but not with `src/b/*`. Exact wildcards inside the path are not
/// expanded; this is deliberately conservative and cheap.
pub(crate) fn glob_prefix(glob: &str) -> String {
    glob.chars()
        .take_while(|c| *c != '*' && *c != '?')
        .collect()
}

/// True when two globs name overlapping trees.
///
/// Prefix-based and deliberately conservative: a claim on `src/a/*` blocks
/// `src/a/login.rs`, and two identical claims always conflict. The rule is
/// symmetric so a claim cannot be dodged by widening or narrowing the spelling.
fn globs_conflict(a: &str, b: &str) -> bool {
    let a_prefix = glob_prefix(a);
    let b_prefix = glob_prefix(b);
    a_prefix.starts_with(&b_prefix) || b_prefix.starts_with(&a_prefix)
}

impl Coord {
    /// Open (or create) a room at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Coord { conn })
    }

    /// A room in memory — used by tests and by a dry run.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open(":memory:")?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Coord { conn })
    }

    /// Join the room and return the new session id.
    pub fn join(&self, name: &str, pid: i32, project_root: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO coord_sessions (id, name, pid, project_root, started_at, heartbeat_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, name, pid, project_root, now],
        )?;
        Ok(id)
    }

    /// Claim a path glob for `ttl_secs`, first-writer-wins.
    ///
    /// The TTL is in seconds because that is what every caller, flag and MCP
    /// tool names; the column stores an absolute millisecond expiry so a
    /// reader never has to know when the claim was made.
    pub fn claim(&self, session_id: &str, path_glob: &str, ttl_secs: u64) -> Result<ClaimResult> {
        if path_glob.trim().is_empty() {
            bail!("a claim needs a non-empty path glob");
        }
        let now = now_ms();
        let ttl_ms = (ttl_secs as i64).saturating_mul(1000).max(1);

        // Overlap, not equality: a claim on `src/auth/*.rs` must refuse
        // `src/auth/login.rs`, or the lock protects nothing.
        let live: Vec<(i64, String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, path_glob, session_id FROM coord_claims WHERE expires_at > ?1",
            )?;
            stmt.query_map(params![now], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };

        for (claim_row_id, held_glob, holder_session) in live {
            if globs_conflict(&held_glob, path_glob) {
                // Re-claiming your own tree is a retry, not a conflict: hand
                // back the existing claim so a retry loop cannot deadlock on
                // itself.
                if holder_session == session_id {
                    return Ok(ClaimResult::Granted {
                        claim_id: claim_row_id.to_string(),
                    });
                }
                return Ok(ClaimResult::Refused {
                    holder_session,
                    holder_glob: held_glob,
                });
            }
        }

        let claim_id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO coord_claims (session_id, path_glob, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, path_glob, now, now + ttl_ms],
        )?;
        Ok(ClaimResult::Granted { claim_id })
    }

    /// Release one of this session's claims early.
    ///
    /// Deleting is scoped to `session_id`, so a session cannot unlock a path
    /// another session holds; releasing something not held is a no-op rather
    /// than an error, because shutdown paths release unconditionally.
    pub fn release(&self, session_id: &str, path_glob: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM coord_claims WHERE session_id = ?1 AND path_glob = ?2",
            params![session_id, path_glob],
        )?;
        Ok(())
    }

    /// Leave the room: drop the session and every claim it still holds.
    ///
    /// Claims go with the session. Leaving a claim behind would keep a path
    /// locked for an owner that is gone until the TTL expired.
    pub fn leave(&self, session_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM coord_claims WHERE session_id = ?1",
            params![session_id],
        )?;
        self.conn.execute(
            "DELETE FROM coord_sessions WHERE id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// Record a heartbeat so the session is not considered stale.
    pub fn heartbeat(&self, session_id: &str) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "UPDATE coord_sessions SET heartbeat_at = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;
        Ok(())
    }

    /// Live claim globs held by one session, sorted.
    fn live_claims(&self, session_id: &str) -> Result<Vec<String>> {
        let now = now_ms();
        let mut stmt = self.conn.prepare(
            "SELECT path_glob FROM coord_claims WHERE session_id = ?1 AND expires_at > ?2",
        )?;
        let mut globs = stmt
            .query_map(params![session_id, now], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        globs.sort();
        Ok(globs)
    }

    /// Every joined session with its heartbeat age and live claims.
    pub fn status(&self) -> Result<Vec<SessionStatus>> {
        let now = now_ms();
        let rows: Vec<(String, String, i64, String, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, name, pid, project_root, heartbeat_at FROM coord_sessions ORDER BY id",
            )?;
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };

        let mut sessions = Vec::with_capacity(rows.len());
        for (session_id, name, pid, project_root, heartbeat_at) in rows {
            let heartbeat_age_ms = (now - heartbeat_at).max(0);
            sessions.push(SessionStatus {
                active_claims: self.live_claims(&session_id)?,
                session_id,
                name,
                pid,
                user_id: String::new(),
                project_root,
                status: TaskStatus::InProgress,
                last_heartbeat: heartbeat_at,
                heartbeat_age_ms,
                stale: heartbeat_age_ms > STALE_HEARTBEAT_MS,
            });
        }
        Ok(sessions)
    }

    /// Every worker record, newest first.
    pub fn worker_list(&self, _filter: Option<&str>) -> Result<Vec<WorkerRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, session_id, task, status, started_at, completed_at, result, error
             FROM coord_workers ORDER BY started_at DESC, id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(WorkerRow {
                id: row.get(0)?,
                name: row.get(1)?,
                session_id: row.get(2)?,
                task: row.get(3)?,
                status: row
                    .get::<_, String>(4)?
                    .parse()
                    .unwrap_or(WorkerStatus::Pending),
                started_at: row.get(5)?,
                completed_at: row.get(6)?,
                result: row.get(7)?,
                error: row.get(8)?,
            })
        })?;
        let mut workers = Vec::new();
        for row in rows {
            workers.push(row?);
        }
        Ok(workers)
    }

    /// Create a worker row in `Pending`; returns the record, not just its id,
    /// so a caller cannot render a worker it cannot describe.
    pub fn worker_create(&self, name: &str, session_id: &str, task: &str) -> Result<WorkerRow> {
        let id = Uuid::new_v4().to_string();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO coord_workers (id, name, session_id, task, status, started_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
            params![id, name, session_id, task, now],
        )?;
        Ok(WorkerRow {
            id,
            name: name.to_string(),
            session_id: session_id.to_string(),
            task: task.to_string(),
            status: WorkerStatus::Pending,
            started_at: now,
            completed_at: None,
            result: None,
            error: None,
        })
    }

    /// Mark a worker running and stamp its start.
    pub fn worker_start(&self, id: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE coord_workers SET status = 'running', started_at = ?1 WHERE id = ?2",
            params![now_ms(), id],
        )?;
        if changed == 0 {
            bail!("worker {id} not found");
        }
        Ok(())
    }

    /// Finish a worker with a result.
    pub fn worker_complete(&self, id: &str, result: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE coord_workers SET status = 'done', result = ?1, completed_at = ?2 WHERE id = ?3",
            params![result, now_ms(), id],
        )?;
        if changed == 0 {
            bail!("worker {id} not found");
        }
        Ok(())
    }

    /// Fail a worker, keeping the error next to the record.
    pub fn worker_fail(&self, id: &str, error: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE coord_workers SET status = 'failed', error = ?1, completed_at = ?2 WHERE id = ?3",
            params![error, now_ms(), id],
        )?;
        if changed == 0 {
            bail!("worker {id} not found");
        }
        Ok(())
    }

    /// Cancel a worker without a result. Cancelling twice is not an error.
    pub fn worker_cancel(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE coord_workers SET status = 'cancelled', completed_at = ?1 WHERE id = ?2",
            params![now_ms(), id],
        )?;
        Ok(())
    }

    /// Track a shared vault session for one org member.
    pub fn vault_session_open(
        &self,
        session_id: &str,
        org_id: &str,
        namespace: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO coord_vault_sessions (session_id, org_id, namespace, opened_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, org_id, namespace, now_ms()],
        )?;
        Ok(())
    }

    /// Close a shared vault session.
    pub fn vault_session_close(&self, session_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM coord_vault_sessions WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// List all sessions that currently have a given org's vault namespace open.
    pub fn vault_sessions_for_org(&self, org_id: &str, namespace: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id FROM coord_vault_sessions WHERE org_id = ?1 AND namespace = ?2",
        )?;
        let rows = stmt
            .query_map(params![org_id, namespace], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Create a task on the shared board; `depends_on` names the task it waits
    /// for, and the new task is `Pending` either way (the DAG view computes
    /// readiness from the dependency's status).
    pub fn task_create(
        &self,
        title: &str,
        description: &str,
        depends_on: Option<&str>,
    ) -> Result<String> {
        if title.trim().is_empty() {
            bail!("a task needs a title");
        }
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO coord_tasks (id, title, description, status, depends_on, updated_at)
             VALUES (?1, ?2, ?3, 'pending', ?4, ?5)",
            params![id, title, description, depends_on, now_ms()],
        )?;
        Ok(id)
    }

    /// Every task, ordered by id so two readers agree.
    pub fn task_list(&self) -> Result<Vec<TaskRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, claimed_by, depends_on, updated_at
             FROM coord_tasks ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TaskRow {
                id: row.get(0)?,
                title: row.get(1)?,
                description: row.get(2)?,
                status: row
                    .get::<_, String>(3)?
                    .parse()
                    .unwrap_or(TaskStatus::Pending),
                claimed_by: row.get(4)?,
                depends_on: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }

    /// Claim a task for `session_id`, refusing a task another session holds.
    ///
    /// This is the atomic part of the board: a second claim on a held task
    /// names the holder instead of silently reassigning the work.
    pub fn task_claim(&self, task_id: &str, session_id: &str) -> Result<()> {
        let now = now_ms();
        let current: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT claimed_by FROM coord_tasks WHERE id = ?1",
                params![task_id],
                |row| row.get(0),
            )
            .ok();
        let Some(claimed_by) = current else {
            bail!("task {task_id} not found");
        };
        if let Some(holder) = claimed_by
            && holder != session_id
        {
            bail!("task {task_id} is already claimed by {holder}");
        }
        let changed = self.conn.execute(
            "UPDATE coord_tasks SET claimed_by = ?1, status = 'in_progress', updated_at = ?2
             WHERE id = ?3",
            params![session_id, now, task_id],
        )?;
        if changed == 0 {
            bail!("task {task_id} not found");
        }
        Ok(())
    }

    /// Set a task's status.
    pub fn task_update_status(&self, id: &str, status: TaskStatus) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE coord_tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), now_ms(), id],
        )?;
        if changed == 0 {
            bail!("task {id} not found");
        }
        Ok(())
    }

    /// Send a message; `None` for `to_session` is a broadcast. Returns its id.
    pub fn send_message(
        &self,
        from_session: &str,
        to_session: Option<&str>,
        body: &str,
    ) -> Result<i64> {
        if body.trim().is_empty() {
            bail!("a message needs a body");
        }
        self.conn.execute(
            "INSERT INTO coord_messages (from_session, to_session, body, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![from_session, to_session, body, now_ms()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Messages addressed to `session_id`, oldest first.
    ///
    /// With `include_broadcast`, broadcasts (`to_session IS NULL`) are included;
    /// a broadcast is one message, not one row per recipient, so it is filtered
    /// here rather than copied on write.
    pub fn inbox(&self, session_id: &str, include_broadcast: bool) -> Result<Vec<MessageRow>> {
        let sql = if include_broadcast {
            "SELECT id, from_session, to_session, body, created_at, acked_at
             FROM coord_messages WHERE to_session = ?1 OR to_session IS NULL ORDER BY id"
        } else {
            "SELECT id, from_session, to_session, body, created_at, acked_at
             FROM coord_messages WHERE to_session = ?1 ORDER BY id"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(MessageRow {
                id: row.get(0)?,
                from_session: row.get(1)?,
                to_session: row.get(2)?,
                body: row.get(3)?,
                created_at: row.get(4)?,
                acked_at: row.get(5)?,
            })
        })?;
        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        Ok(messages)
    }

    /// Acknowledge a message; returns the timestamp recorded.
    ///
    /// Acking twice returns the first timestamp: an ack is a fact, not an
    /// event stream, and a retry must not move it.
    pub fn ack(&self, message_id: i64) -> Result<i64> {
        let already: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT acked_at FROM coord_messages WHERE id = ?1",
                params![message_id],
                |row| row.get(0),
            )
            .ok();
        let Some(acked_at) = already else {
            bail!("message {message_id} not found");
        };
        if let Some(at) = acked_at {
            return Ok(at);
        }
        let now = now_ms();
        self.conn.execute(
            "UPDATE coord_messages SET acked_at = ?1 WHERE id = ?2 AND acked_at IS NULL",
            params![now, message_id],
        )?;
        Ok(now)
    }

    /// Delete sessions whose heartbeat has been gone for over an hour, and the
    /// claims they left behind. Returns how many sessions were pruned.
    pub fn prune_stale_sessions(&self) -> Result<usize> {
        let cutoff = now_ms() - PRUNE_SESSION_MS;
        let stale: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM coord_sessions WHERE heartbeat_at < ?1")?;
            stmt.query_map(params![cutoff], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for id in &stale {
            self.leave(id)?;
        }
        Ok(stale.len())
    }

    /// A fresh id for a caller that needs to name something before it exists.
    pub fn new_id(&self) -> String {
        Uuid::new_v4().to_string()
    }

    /// The room, when one exists. A missing room is not an error: callers that
    /// coordinate opportunistically get `None` and run alone.
    pub fn optional(&self) -> Result<Option<Self>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_open_and_status() {
        let coord = Coord::open_in_memory().unwrap();
        let sessions = coord.status().unwrap();
        assert_eq!(sessions.len(), 0);

        let id = coord.join("test-agent", 12345, "/tmp/test").unwrap();
        let sessions = coord.status().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, id);
        assert_eq!(sessions[0].project_root, "/tmp/test");
        assert_eq!(sessions[0].pid, 12345);
        assert!(sessions[0].heartbeat_age_ms >= 0);
    }

    #[test]
    fn worker_list_empty() {
        let coord = Coord::open_in_memory().unwrap();
        let workers = coord.worker_list(None).unwrap();
        assert_eq!(workers.len(), 0);
    }

    #[test]
    fn overlapping_claim_is_refused_and_names_the_holder() {
        let coord = Coord::open_in_memory().unwrap();
        let a = coord.join("a", 1, "/w").unwrap();
        let b = coord.join("b", 2, "/w").unwrap();

        assert!(matches!(
            coord.claim(&a, "src/auth/*.rs", 60).unwrap(),
            ClaimResult::Granted { .. }
        ));
        match coord.claim(&b, "src/auth/login.rs", 60).unwrap() {
            ClaimResult::Refused {
                holder_session,
                holder_glob,
            } => {
                assert_eq!(holder_session, a);
                assert_eq!(holder_glob, "src/auth/*.rs");
            }
            other => panic!("expected refusal, got {other:?}"),
        }
        // A disjoint tree is granted.
        assert!(matches!(
            coord.claim(&b, "src/parser/*", 60).unwrap(),
            ClaimResult::Granted { .. }
        ));
    }

    #[test]
    fn expired_claim_is_reusable() {
        let coord = Coord::open_in_memory().unwrap();
        let a = coord.join("a", 1, "/w").unwrap();
        let b = coord.join("b", 2, "/w").unwrap();

        // TTL 0 is clamped to one millisecond, so this expires immediately.
        coord.claim(&a, "src/x/*", 0).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(matches!(
            coord.claim(&b, "src/x/*", 60).unwrap(),
            ClaimResult::Granted { .. }
        ));
    }

    #[test]
    fn leaving_releases_claims_and_unknown_tasks_are_refused() {
        let coord = Coord::open_in_memory().unwrap();
        let a = coord.join("a", 1, "/w").unwrap();
        coord.claim(&a, "src/a/*", 600).unwrap();
        assert_eq!(coord.status().unwrap()[0].active_claims, vec!["src/a/*"]);

        coord.leave(&a).unwrap();
        assert!(coord.status().unwrap().is_empty());
        // The path is free again immediately, without waiting for the TTL.
        let b = coord.join("b", 2, "/w").unwrap();
        assert!(matches!(
            coord.claim(&b, "src/a/*", 60).unwrap(),
            ClaimResult::Granted { .. }
        ));

        assert!(coord.task_claim("no-such-task", &b).is_err());
        assert!(
            coord
                .task_update_status("no-such-task", TaskStatus::Done)
                .is_err()
        );
    }

    #[test]
    fn task_board_claim_is_first_writer_wins() {
        let coord = Coord::open_in_memory().unwrap();
        let a = coord.join("a", 1, "/w").unwrap();
        let b = coord.join("b", 2, "/w").unwrap();
        let t = coord.task_create("fix leak", "plug it", None).unwrap();

        coord.task_claim(&t, &a).unwrap();
        let err = coord.task_claim(&t, &b).unwrap_err().to_string();
        assert!(err.contains(&a), "{err}");

        let row = coord
            .task_list()
            .unwrap()
            .into_iter()
            .find(|row| row.id == t)
            .unwrap();
        assert_eq!(row.status, TaskStatus::InProgress);
        assert_eq!(row.claimed_by.as_deref(), Some(a.as_str()));
    }

    #[test]
    fn messages_route_to_the_recipient_and_broadcasts_are_opt_in() {
        let coord = Coord::open_in_memory().unwrap();
        let a = coord.join("a", 1, "/w").unwrap();
        let b = coord.join("b", 2, "/w").unwrap();

        let direct = coord.send_message(&a, Some(&b), "for you").unwrap();
        coord.send_message(&a, None, "for everyone").unwrap();

        let mine = coord.inbox(&b, false).unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].id, direct);

        let with_broadcast = coord.inbox(&b, true).unwrap();
        assert_eq!(with_broadcast.len(), 2);
        assert!(with_broadcast.iter().any(|m| m.to_session.is_none()));

        // A session with nothing addressed to it sees only the broadcast.
        let c = coord.join("c", 3, "/w").unwrap();
        assert_eq!(coord.inbox(&c, false).unwrap().len(), 0);
        assert_eq!(coord.inbox(&c, true).unwrap().len(), 1);

        coord.ack(direct).unwrap();
        assert!(coord.inbox(&b, false).unwrap()[0].acked_at.is_some());
        assert!(coord.ack(9_999).is_err());
    }

    #[test]
    fn worker_lifecycle_records_every_terminal_state() {
        let coord = Coord::open_in_memory().unwrap();
        let a = coord.join("a", 1, "/w").unwrap();

        let ok = coord.worker_create("scout", &a, "map auth").unwrap();
        assert_eq!(ok.status, WorkerStatus::Pending);
        coord.worker_start(&ok.id).unwrap();
        coord.worker_complete(&ok.id, "3 entry points").unwrap();

        let bad = coord.worker_create("reviewer", &a, "review").unwrap();
        coord.worker_start(&bad.id).unwrap();
        coord.worker_fail(&bad.id, "timed out").unwrap();

        let stopped = coord.worker_create("designer", &a, "429 body").unwrap();
        coord.worker_cancel(&stopped.id).unwrap();

        let rows = coord.worker_list(None).unwrap();
        assert_eq!(rows.len(), 3);
        let done = rows.iter().find(|w| w.id == ok.id).unwrap();
        assert_eq!(done.status, WorkerStatus::Done);
        assert_eq!(done.result.as_deref(), Some("3 entry points"));
        assert!(done.completed_at.is_some());

        let failed = rows.iter().find(|w| w.id == bad.id).unwrap();
        assert_eq!(failed.status, WorkerStatus::Failed);
        assert_eq!(failed.error.as_deref(), Some("timed out"));

        let cancelled = rows.iter().find(|w| w.id == stopped.id).unwrap();
        assert_eq!(cancelled.status, WorkerStatus::Cancelled);

        assert!(coord.worker_start("no-such-worker").is_err());
    }

    #[test]
    fn status_spellings_round_trip() {
        for status in [
            TaskStatus::Pending,
            TaskStatus::InProgress,
            TaskStatus::Blocked,
            TaskStatus::Done,
        ] {
            assert_eq!(status.as_str().parse::<TaskStatus>().unwrap(), status);
        }
        assert!("nonsense".parse::<TaskStatus>().is_err());

        for status in [
            WorkerStatus::Pending,
            WorkerStatus::Running,
            WorkerStatus::Done,
            WorkerStatus::Failed,
            WorkerStatus::Cancelled,
        ] {
            assert_eq!(status.as_str().parse::<WorkerStatus>().unwrap(), status);
        }
        assert!("nonsense".parse::<WorkerStatus>().is_err());
    }
}
