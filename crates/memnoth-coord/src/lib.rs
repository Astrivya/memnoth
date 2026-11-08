#![warn(missing_docs)]
// TODO: Change to #![deny(missing_docs)] after all public items are documented

//! memnoth-coord — multi-agent session coordination on the shared vault.db.
//!
//! # Overview
//!
//! This crate provides multi-agent coordination capabilities:
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
//! let session_id = coord.join("my-agent", std::process::id(), "/path/to/project")?;
//!
//! // Claim a path
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
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize)]
/// Result of a path claim attempt.
pub enum ClaimResult {
    /// Claim was granted
    Granted {
        /// Unique identifier for this claim
        claim_id: i64,
    },
    /// Claim was refused because another session holds a conflicting path
    Refused {
        /// Session ID of the holder
        holder_session: String,
        /// Path glob held by the other session
        holder_glob: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Status of a task on the shared board.
pub enum TaskStatus {
    /// Task is waiting to be claimed
    Pending,
    /// Task is being worked on
    InProgress,
    /// Task is blocked by a dependency
    Blocked,
    /// Task is complete
    Done,
}

impl TaskStatus {
    /// Convert to a string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Done => "done",
        }
    }

    /// Parse from a string representation, falling back to [`TaskStatus::Pending`]
    /// for a label this build does not know.
    pub fn from_label(s: &str) -> Self {
        match s {
            "in_progress" => TaskStatus::InProgress,
            "blocked" => TaskStatus::Blocked,
            "done" => TaskStatus::Done,
            _ => TaskStatus::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Status of a session in the coordination room.
pub struct SessionStatus {
    /// Unique session identifier
    pub session_id: String,
    /// Display name for this session
    pub name: String,
    /// Process ID
    pub pid: u32,
    /// Working directory
    pub project_root: String,
    /// How long since the last heartbeat (ms)
    pub heartbeat_age_ms: i64,
    /// Whether this session is considered stale
    pub stale: bool,
    /// Path globs currently claimed by this session
    pub active_claims: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A task on the shared board.
pub struct TaskRow {
    /// Unique task identifier
    pub id: String,
    /// Task title
    pub title: String,
    /// Task description
    pub description: String,
    /// Current status
    pub status: TaskStatus,
    /// Session that claimed this task
    pub claimed_by: Option<String>,
    /// Task this task depends on
    pub depends_on: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
/// Status of a worker.
pub enum WorkerStatus {
    /// Worker is pending execution
    Pending,
    /// Worker is currently running
    Running,
    /// Worker completed successfully
    Done,
    /// Worker failed
    Failed,
    /// Worker was cancelled
    Cancelled,
}

impl WorkerStatus {
    /// Convert to a string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkerStatus::Pending => "pending",
            WorkerStatus::Running => "running",
            WorkerStatus::Done => "done",
            WorkerStatus::Failed => "failed",
            WorkerStatus::Cancelled => "cancelled",
        }
    }

    /// Parse from a string representation, falling back to [`WorkerStatus::Pending`]
    /// for a label this build does not know.
    pub fn from_label(s: &str) -> Self {
        match s {
            "running" => WorkerStatus::Running,
            "done" => WorkerStatus::Done,
            "failed" => WorkerStatus::Failed,
            "cancelled" => WorkerStatus::Cancelled,
            _ => WorkerStatus::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
/// A worker record.
pub struct WorkerRow {
    /// Unique worker identifier
    pub id: String,
    /// Worker name (user-assigned or auto-generated)
    pub name: String,
    /// Session that owns this worker
    pub session_id: String,
    /// Task description
    pub task: String,
    /// Current status
    pub status: WorkerStatus,
    /// When the worker started (ms since epoch)
    pub started_at: i64,
    /// When the worker completed (ms since epoch)
    pub completed_at: Option<i64>,
    /// Result output (truncated)
    pub result: Option<String>,
    /// Error message if failed
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A message in the coordination room.
pub struct MessageRow {
    /// Unique message identifier
    pub id: i64,
    /// Sender session ID
    pub from_session: String,
    /// Recipient session ID (None for broadcast)
    pub to_session: Option<String>,
    /// Message body
    pub body: String,
    /// When the message was created (ms since epoch)
    pub created_at: i64,
    /// Whether the message has been acknowledged
    pub acked: bool,
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
";

/// Literal prefix of a glob: everything before the first wildcard character.
/// Overlap check is prefix-based — `src/a/*` conflicts with `src/a/**` and
/// `src/ab` but not with `src/b/*`. Exact wildcards inside the path are not
/// expanded; this is deliberately conservative and cheap.
pub(crate) fn glob_prefix(glob: &str) -> String {
    let lower = glob.to_lowercase();
    match lower.find(['*', '?']) {
        Some(i) => lower[..i].to_string(),
        None => lower,
    }
}

fn globs_conflict(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let pa = glob_prefix(a);
    let pb = glob_prefix(b);
    if pa.is_empty() || pb.is_empty() {
        return false;
    }
    pa.starts_with(&pb) || pb.starts_with(&pa)
}

impl Coord {
    /// Open or create a coordination room at the given database path.
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self { conn })
    }

    /// Open an in-memory coordination room (for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self { conn })
    }

    fn new_id() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 + d.as_secs() * 1_000_000_000)
            .unwrap_or(0);
        format!("s-{nanos:x}-{}", std::process::id())
    }

    /// Join the coordination room.
    ///
    /// Returns a session ID that should be used for all other coord_* calls.
    pub fn join(&self, name: &str, pid: u32, project_root: &str) -> Result<String> {
        let id = Self::new_id();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO coord_sessions (id, name, pid, project_root, started_at, heartbeat_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, name, pid as i64, project_root, now],
        )?;
        Ok(id)
    }

    /// Join under a caller-chosen identity, resuming the row if it exists.
    ///
    /// [`Coord::join`] mints a new id on every call, which is why presence
    /// accumulated one row per invocation and why a message addressed to a
    /// colleague's live session could not be delivered. The durable id is the
    /// caller's: the first call creates the row, later calls refresh the
    /// heartbeat and pid, and `started_at` is preserved.
    pub fn join_stable(
        &self,
        session_id: &str,
        name: &str,
        pid: u32,
        project_root: &str,
    ) -> Result<String> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO coord_sessions (id, name, pid, project_root, started_at, heartbeat_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               pid = excluded.pid,
               project_root = excluded.project_root,
               heartbeat_at = excluded.heartbeat_at",
            params![session_id, name, pid as i64, project_root, now],
        )?;
        Ok(session_id.to_string())
    }

    /// Send a heartbeat to keep the session alive.
    pub fn heartbeat(&self, session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE coord_sessions SET heartbeat_at = ?1 WHERE id = ?2",
            params![now_ms(), session_id],
        )?;
        Ok(())
    }

    /// Leave the coordination room and release all claims.
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

    /// Claim a path glob before editing.
    ///
    /// First-writer-wins with TTL auto-release.
    pub fn claim(&self, session_id: &str, path_glob: &str, ttl_secs: u64) -> Result<ClaimResult> {
        let now = now_ms();
        let expires = now + (ttl_secs as i64) * 1000;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM coord_claims WHERE expires_at <= ?1",
            params![now],
        )?;
        let existing = {
            let mut stmt = tx.prepare("SELECT session_id, path_glob FROM coord_claims")?;

            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (holder, holder_glob) in existing {
            if holder != session_id && globs_conflict(path_glob, &holder_glob) {
                return Ok(ClaimResult::Refused {
                    holder_session: holder,
                    holder_glob,
                });
            }
        }
        tx.execute(
            "INSERT INTO coord_claims (session_id, path_glob, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, path_glob, now, expires],
        )?;
        let claim_id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(ClaimResult::Granted { claim_id })
    }

    /// Release a claimed path glob early.
    pub fn release(&self, session_id: &str, path_glob: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM coord_claims WHERE session_id = ?1 AND path_glob = ?2",
            params![session_id, path_glob],
        )?;
        Ok(())
    }

    /// Delete expired path claims and prune sessions whose heartbeats have
    /// gone stale. Returns `(expired_claims, pruned_sessions)`.
    pub fn cleanup_expired(&self) -> Result<(usize, usize)> {
        let now = now_ms();
        let expired = self.conn.execute(
            "DELETE FROM coord_claims WHERE expires_at <= ?1",
            params![now],
        )?;
        let cutoff = now - PRUNE_SESSION_MS;
        let stale: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM coord_sessions WHERE heartbeat_at < ?1")?;

            stmt.query_map(params![cutoff], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for sid in &stale {
            self.conn.execute(
                "UPDATE coord_tasks SET claimed_by = NULL WHERE claimed_by = ?1",
                params![sid],
            )?;
        }
        let pruned = self.conn.execute(
            "DELETE FROM coord_sessions WHERE heartbeat_at < ?1",
            params![cutoff],
        )?;
        Ok((expired, pruned))
    }

    /// Show room status with active sessions and claims.
    pub fn status(&self) -> Result<Vec<SessionStatus>> {
        let now = now_ms();
        let mut out = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT id, name, pid, project_root, heartbeat_at FROM coord_sessions ORDER BY started_at",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let sid: String = row.get(0)?;
            let hb: i64 = row.get(4)?;
            let mut claims_stmt = self.conn.prepare(
                "SELECT path_glob FROM coord_claims WHERE session_id = ?1 AND expires_at > ?2",
            )?;
            let claims = claims_stmt
                .query_map(params![sid, now], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            out.push(SessionStatus {
                heartbeat_age_ms: now - hb,
                stale: now - hb > STALE_HEARTBEAT_MS,
                active_claims: claims,
                session_id: sid,
                name: row.get(1)?,
                pid: row.get::<_, i64>(2)? as u32,
                project_root: row.get(3)?,
            });
        }
        Ok(out)
    }

    /// Create a new task on the shared board.
    pub fn task_create(
        &self,
        title: &str,
        description: &str,
        depends_on: Option<&str>,
    ) -> Result<String> {
        // ms-clock + per-process sequence: rapid same-ms creates (dag_expand,
        // parallel workers) must never collide on the PRIMARY KEY.
        static TASK_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = TASK_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("t-{:x}-{:x}", now_ms(), seq);
        self.conn.execute(
            "INSERT INTO coord_tasks (id, title, description, status, claimed_by, depends_on, updated_at)
             VALUES (?1, ?2, ?3, 'pending', NULL, ?4, ?5)",
            params![id, title, description, depends_on, now_ms()],
        )?;
        Ok(id)
    }

    /// Atomically claim a task so only one session owns it.
    pub fn task_claim(&self, task_id: &str, session_id: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE coord_tasks SET claimed_by = ?1, status = 'in_progress', updated_at = ?2
             WHERE id = ?3 AND claimed_by IS NULL",
            params![session_id, now_ms(), task_id],
        )?;
        if n == 0 {
            bail!("task {task_id} already claimed or missing");
        }
        Ok(())
    }

    /// Update a task's status.
    pub fn task_update_status(&self, task_id: &str, status: TaskStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE coord_tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), now_ms(), task_id],
        )?;
        Ok(())
    }

    /// List all tasks on the shared board.
    pub fn task_list(&self) -> Result<Vec<TaskRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, claimed_by, depends_on
             FROM coord_tasks ORDER BY updated_at DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(TaskRow {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    description: row.get(2)?,
                    status: TaskStatus::from_label(&row.get::<_, String>(3)?),
                    claimed_by: row.get(4)?,
                    depends_on: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Create a new worker.
    pub fn worker_create(&self, name: &str, session_id: &str, task: &str) -> Result<WorkerRow> {
        // ms-clock + per-process sequence, same contract as `task_create`: a
        // swarm fan-out creates several workers in the same millisecond, and a
        // bare ms id collides on the PRIMARY KEY (Lane C hit this live).
        static WORKER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = WORKER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("w-{:x}-{:x}", now_ms(), seq);
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

    /// Start a worker (transition from pending to running).
    pub fn worker_start(&self, worker_id: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE coord_workers SET status = 'running', started_at = ?1 WHERE id = ?2 AND status = 'pending'",
            params![now_ms(), worker_id],
        )?;
        if n == 0 {
            bail!("worker {worker_id} not found or already started");
        }
        Ok(())
    }

    /// Complete a worker with a result.
    pub fn worker_complete(&self, worker_id: &str, result: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE coord_workers SET status = 'done', completed_at = ?1, result = ?2 WHERE id = ?3",
            params![now_ms(), result, worker_id],
        )?;
        if n == 0 {
            bail!("worker {worker_id} not found");
        }
        Ok(())
    }

    /// Fail a worker with an error.
    pub fn worker_fail(&self, worker_id: &str, error: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE coord_workers SET status = 'failed', completed_at = ?1, error = ?2 WHERE id = ?3",
            params![now_ms(), error, worker_id],
        )?;
        if n == 0 {
            bail!("worker {worker_id} not found");
        }
        Ok(())
    }

    /// Cancel a worker.
    pub fn worker_cancel(&self, worker_id: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE coord_workers SET status = 'cancelled', completed_at = ?1 WHERE id = ?2 AND status IN ('pending','running')",
            params![now_ms(), worker_id],
        )?;
        if n == 0 {
            bail!("worker {worker_id} not found or already terminal");
        }
        Ok(())
    }

    /// List workers, optionally filtered by session.
    pub fn worker_list(&self, session_id: Option<&str>) -> Result<Vec<WorkerRow>> {
        let sql =
            "SELECT id, name, session_id, task, status, started_at, completed_at, result, error
                    FROM coord_workers ORDER BY started_at DESC";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], Self::worker_row)?;
        let mut all: Vec<WorkerRow> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        if let Some(sid) = session_id {
            all.retain(|w| w.session_id == sid);
        }
        Ok(all)
    }

    /// Clean up old terminal workers.
    pub fn worker_cleanup(&self, before_ms: i64) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM coord_workers WHERE status IN ('done','failed','cancelled') AND completed_at IS NOT NULL AND completed_at < ?1",
            params![before_ms],
        )?;
        Ok(n)
    }

    fn worker_row(row: &rusqlite::Row) -> rusqlite::Result<WorkerRow> {
        Ok(WorkerRow {
            id: row.get(0)?,
            name: row.get(1)?,
            session_id: row.get(2)?,
            task: row.get(3)?,
            status: WorkerStatus::from_label(&row.get::<_, String>(4)?),
            started_at: row.get(5)?,
            completed_at: row.get(6)?,
            result: row.get(7)?,
            error: row.get(8)?,
        })
    }

    /// Send a message to another session or broadcast to all.
    pub fn send_message(
        &self,
        from_session: &str,
        to_session: Option<&str>,
        body: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO coord_messages (from_session, to_session, body, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![from_session, to_session, body, now_ms()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Read messages in the inbox.
    pub fn inbox(&self, session_id: &str, include_broadcast: bool) -> Result<Vec<MessageRow>> {
        let sql = if include_broadcast {
            "SELECT id, from_session, to_session, body, created_at, acked_at
             FROM coord_messages WHERE to_session = ?1 OR to_session IS NULL ORDER BY id"
        } else {
            "SELECT id, from_session, to_session, body, created_at, acked_at
             FROM coord_messages WHERE to_session = ?1 ORDER BY id"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    from_session: row.get(1)?,
                    to_session: row.get(2)?,
                    body: row.get(3)?,
                    created_at: row.get(4)?,
                    acked: row.get::<_, Option<i64>>(5)?.is_some(),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Acknowledge a message.
    pub fn ack(&self, message_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE coord_messages SET acked_at = ?1 WHERE id = ?2",
            params![now_ms(), message_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        dir.join(format!(
            "memnoth-coord-test-{tag}-{}-{}.db",
            std::process::id(),
            now_ms()
        ))
    }

    #[test]
    fn overlapping_claim_refused() {
        let db = tmp_db("refused");
        let c = Coord::open(&db).unwrap();
        let a = c.join("alpha", 1, "/p").unwrap();
        let b = c.join("beta", 2, "/p").unwrap();
        assert!(matches!(
            c.claim(&a, "src/auth/*.rs", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));
        match c.claim(&b, "src/auth/login.rs", 1800).unwrap() {
            ClaimResult::Refused {
                holder_session,
                holder_glob,
            } => {
                assert_eq!(holder_session, a);
                assert_eq!(holder_glob, "src/auth/*.rs");
            }
            other => panic!("expected refused, got {other:?}"),
        }
    }

    #[test]
    fn join_stable_resumes_one_row_per_identity() {
        let db = tmp_db("stable");
        let c = Coord::open(&db).unwrap();
        let first = c.join_stable("u-amruth@abc", "amruth", 10, "/p").unwrap();
        let second = c.join_stable("u-amruth@abc", "amruth", 11, "/p").unwrap();
        assert_eq!(first, second, "the identity is the caller's, not a new id");
        let rows = c.status().unwrap();
        assert_eq!(rows.len(), 1, "two commands are one session, not two rows");
        assert_eq!(rows[0].pid, 11, "the newer process owns the row");
    }

    #[test]
    fn non_overlapping_claims_both_granted() {
        let db = tmp_db("nonoverlap");
        let c = Coord::open(&db).unwrap();
        let a = c.join("alpha", 1, "/p").unwrap();
        let b = c.join("beta", 2, "/p").unwrap();
        assert!(matches!(
            c.claim(&a, "src/a/*", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));
        assert!(matches!(
            c.claim(&b, "src/b/*", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));
    }

    #[test]
    fn release_then_reclaim() {
        let db = tmp_db("reclaim");
        let c = Coord::open(&db).unwrap();
        let a = c.join("alpha", 1, "/p").unwrap();
        let b = c.join("beta", 2, "/p").unwrap();
        c.claim(&a, "src/x/*", 1800).unwrap();
        c.release(&a, "src/x/*").unwrap();
        assert!(matches!(
            c.claim(&b, "src/x/*", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));
    }

    #[test]
    fn ttl_expiry_frees_claim() {
        let db = tmp_db("ttl");
        let c = Coord::open(&db).unwrap();
        let a = c.join("alpha", 1, "/p").unwrap();
        let b = c.join("beta", 2, "/p").unwrap();
        if let ClaimResult::Granted { claim_id } = c.claim(&a, "docs/*", 1800).unwrap() {
            c.conn
                .execute(
                    "UPDATE coord_claims SET expires_at = 1 WHERE id = ?1",
                    params![claim_id],
                )
                .unwrap();
        } else {
            panic!("first claim should be granted");
        }
        assert!(matches!(
            c.claim(&b, "docs/*", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));
    }

    #[test]
    fn broadcast_inbox_visibility() {
        let db = tmp_db("broadcast");
        let c = Coord::open(&db).unwrap();
        let a = c.join("alpha", 1, "/p").unwrap();
        let b = c.join("beta", 2, "/p").unwrap();
        c.send_message(&a, None, "hello all").unwrap();
        c.send_message(&a, Some(&b), "direct to beta").unwrap();
        let broad = c.inbox(&b, true).unwrap();
        assert_eq!(broad.len(), 2);
        let direct = c.inbox(&b, false).unwrap();
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].body, "direct to beta");
        c.ack(direct[0].id).unwrap();
        assert!(c.inbox(&b, false).unwrap()[0].acked);
    }

    #[test]
    fn task_claim_exclusive() {
        let db = tmp_db("taskexcl");
        let c = Coord::open(&db).unwrap();
        let a = c.join("alpha", 1, "/p").unwrap();
        let b = c.join("beta", 2, "/p").unwrap();
        let t = c.task_create("do thing", "desc", None).unwrap();
        c.task_claim(&t, &a).unwrap();
        assert!(c.task_claim(&t, &b).is_err());
        c.task_update_status(&t, TaskStatus::Done).unwrap();
        let tasks = c.task_list().unwrap();
        assert_eq!(tasks[0].status, TaskStatus::Done);
    }

    #[test]
    fn stale_status_and_prune() {
        let db = tmp_db("stale");
        let c = Coord::open(&db).unwrap();
        let a = c.join("old", 1, "/p").unwrap();
        let stale_ts = now_ms() - 20 * 60 * 1000;
        c.conn
            .execute(
                "UPDATE coord_sessions SET heartbeat_at = ?1 WHERE id = ?2",
                params![stale_ts, a],
            )
            .unwrap();
        let st = c.status().unwrap();
        assert_eq!(st.len(), 1);
        assert!(st[0].stale);
        let (_, pruned) = c.cleanup_expired().unwrap();
        assert_eq!(
            pruned, 0,
            "20min-old is stale but under the 60min prune cutoff"
        );
        let gone_ts = now_ms() - 61 * 60 * 1000;
        c.conn
            .execute(
                "UPDATE coord_sessions SET heartbeat_at = ?1 WHERE id = ?2",
                params![gone_ts, a],
            )
            .unwrap();
        let (_, pruned) = c.cleanup_expired().unwrap();
        assert_eq!(pruned, 1);
        assert!(c.status().unwrap().is_empty());
    }

    #[test]
    fn worker_create_ids_do_not_collide_within_a_millisecond() {
        let db = tmp_db("workerseq");
        let c = Coord::open(&db).unwrap();
        let session = c.join("main", 1, "/p").unwrap();

        // No sleeps: this is exactly the swarm fan-out shape that used to hit
        // "UNIQUE constraint failed: coord_workers.id".
        let ids: Vec<String> = (0..64)
            .map(|i| {
                c.worker_create(&format!("worker-{i}"), &session, "fan-out")
                    .unwrap()
                    .id
            })
            .collect();
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "worker ids collided: {ids:?}");
        assert_eq!(c.worker_list(None).unwrap().len(), 64);
    }

    #[test]
    fn worker_lifecycle() {
        let db = tmp_db("worker");
        let c = Coord::open(&db).unwrap();
        let session = c.join("main", 1, "/p").unwrap();

        let w = c
            .worker_create("worker-1", &session, "refactor auth")
            .unwrap();
        assert_eq!(w.status, WorkerStatus::Pending);
        assert_eq!(w.name, "worker-1");

        c.worker_start(&w.id).unwrap();
        let running = c.worker_list(None).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].status, WorkerStatus::Running);

        c.worker_complete(&w.id, "done: 3 files changed").unwrap();
        let done = c.worker_list(None).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].status, WorkerStatus::Done);
        assert_eq!(done[0].result.as_deref(), Some("done: 3 files changed"));

        let session_workers = c.worker_list(Some(&session)).unwrap();
        assert_eq!(session_workers.len(), 1);
    }

    #[test]
    fn worker_fail_and_cancel() {
        let db = tmp_db("workerfail");
        let c = Coord::open(&db).unwrap();
        let session = c.join("main", 1, "/p").unwrap();

        let w = c.worker_create("worker-2", &session, "fix bug").unwrap();
        c.worker_start(&w.id).unwrap();

        c.worker_fail(&w.id, "OOM").unwrap();
        let failed = c.worker_list(None).unwrap();
        assert_eq!(failed[0].status, WorkerStatus::Failed);
        assert_eq!(failed[0].error.as_deref(), Some("OOM"));

        let w2 = c.worker_create("worker-3", &session, "task 3").unwrap();
        c.worker_start(&w2.id).unwrap();
        c.worker_cancel(&w2.id).unwrap();
        let cancelled = c.worker_list(None).unwrap();
        assert!(
            cancelled
                .iter()
                .any(|w| w.status == WorkerStatus::Cancelled)
        );
    }
}
