//! Mission Control feed: DAG + WORKERS + LOCKS + TIMELINE (§17.5).
//!
//! One call — [`Coord::mission_feed`] — returns everything Lane C needs to
//! render the Mission Control view, with zero TUI code on this side:
//!
//! ```text
//! PLAN auth-fix            WORKERS            LOCKS            TIMELINE
//! ✓ reproduce (done)       ● claude run 2m    🔒auth/* you    14:02 plan ok
//! ▸ patch (active)         ◆ codex wait 30s   🔒parser priya  14:09 tests ✓
//! ```
//!
//! - PLAN: tasks with `depends_on` edges + status (DAG ops `seed/expand/`
//!   `complete/inject` are the existing `task_create` / `task_update_status`
//!   / [`Coord::dag_inject`] below; all mutating actions should run under a
//!   `begin_or_replay` op key — idempotent actions only).
//! - WORKERS: lifecycle + TTL age (heartbeat age, claim expiry countdown).
//! - LOCKS: live path claims (`🔒 path holder`, expiry in seconds).
//! - TIMELINE: event ledger — recent messages + task updates, newest last.
//!
//! Bounded: workers/tasks/messages capped (see `MISSION_*` constants).
//! Read-only: never mutates the room.

use anyhow::Result;
use rusqlite::params;
use serde::Serialize;

use super::{Coord, TaskStatus};

/// Max workers in one feed.
pub const MISSION_MAX_WORKERS: usize = 32;
/// Max plan nodes in one feed.
pub const MISSION_MAX_TASKS: usize = 64;
/// Max timeline events in one feed.
pub const MISSION_MAX_EVENTS: usize = 50;

/// One plan node (DAG vertex).
#[derive(Debug, Clone, Serialize)]
pub struct PlanNode {
    /// Task id (stable, sortable — the feed is ordered by it).
    pub id: String,
    /// Human-readable task title.
    pub title: String,
    /// Task status verb (`todo` / `blocked` / `active` / `done`).
    pub status: String,
    /// Session id holding the claim, when claimed.
    pub claimed_by: Option<String>,
    /// Id of the single task this node waits on, when constrained.
    pub depends_on: Option<String>,
    /// True when every dependency is `done` (or there are none).
    pub ready: bool,
}

/// One worker row with liveness.
#[derive(Debug, Clone, Serialize)]
pub struct WorkerFeed {
    /// Worker id.
    pub id: String,
    /// Worker label shown in the WORKERS pane.
    pub name: String,
    /// Owning session id.
    pub session_id: String,
    /// Task description the worker was started with.
    pub task: String,
    /// Lifecycle status verb (`starting` / `running` / `completed` / `failed`).
    pub status: String,
    /// Milliseconds since the worker started (running) or ran (terminal).
    pub age_ms: i64,
}

/// One live lock (`🔒 path holder`).
#[derive(Debug, Clone, Serialize)]
pub struct LockFeed {
    /// Claimed path glob.
    pub path_glob: String,
    /// Session id holding the claim.
    pub holder_session: String,
    /// Seconds until auto-release.
    pub expires_in_secs: i64,
}

/// One timeline event (ledger grammar).
#[derive(Debug, Clone, Serialize)]
pub struct TimelineEvent {
    /// Event timestamp (ms since epoch).
    pub at_ms: i64,
    /// Event kind (`msg` for a broadcast, `task` for a status change).
    pub kind: String,
    /// Pre-rendered one-line event text.
    pub text: String,
}

/// The full Mission Control feed.
#[derive(Debug, Clone, Serialize)]
pub struct MissionFeed {
    /// PLAN pane: DAG nodes, ordered by id.
    pub plan: Vec<PlanNode>,
    /// `done/total` progress for the header bar.
    pub progress: (usize, usize),
    /// WORKERS pane: workers with liveness ages.
    pub workers: Vec<WorkerFeed>,
    /// LOCKS pane: live path claims with expiry countdowns.
    pub locks: Vec<LockFeed>,
    /// TIMELINE pane: recent events, oldest first.
    pub timeline: Vec<TimelineEvent>,
}

impl Coord {
    /// Inject a plan gap: create a `blocked` task depending on `depends_on`.
    ///
    /// This is the `[Inject gap]` action — the new node unblocks when its
    /// dependency completes. Returns the task id.
    pub fn dag_inject(&self, title: &str, description: &str, depends_on: &str) -> Result<String> {
        let id = self.task_create(title, description, Some(depends_on))?;
        self.task_update_status(&id, TaskStatus::Blocked)?;
        Ok(id)
    }

    /// Expand a plan node into subtasks (each depending on `parent_id`).
    /// Returns the created ids in order.
    pub fn dag_expand(&self, parent_id: &str, subtasks: &[(&str, &str)]) -> Result<Vec<String>> {
        let mut out = Vec::with_capacity(subtasks.len());
        for (title, desc) in subtasks {
            out.push(self.task_create(title, desc, Some(parent_id))?);
        }
        Ok(out)
    }

    /// Read-only Mission Control feed (PLAN + WORKERS + LOCKS + TIMELINE).
    pub fn mission_feed(&self) -> Result<MissionFeed> {
        let now = super::now_ms();
        let tasks = self.task_list()?;
        let done_ids: std::collections::HashSet<String> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .map(|t| t.id.clone())
            .collect();
        let mut plan: Vec<PlanNode> = tasks
            .into_iter()
            .take(MISSION_MAX_TASKS)
            .map(|t| {
                let ready = match &t.depends_on {
                    None => true,
                    Some(dep) => done_ids.contains(dep.as_str()),
                };
                PlanNode {
                    id: t.id,
                    title: t.title,
                    status: t.status.as_str().to_string(),
                    claimed_by: t.claimed_by,
                    depends_on: t.depends_on,
                    ready,
                }
            })
            .collect();
        let total = plan.len();
        let done = plan.iter().filter(|p| p.status == "done").count();
        plan.sort_by(|a, b| a.id.cmp(&b.id));

        let workers: Vec<WorkerFeed> = self
            .worker_list(None)?
            .into_iter()
            .take(MISSION_MAX_WORKERS)
            .map(|w| {
                let end = w.completed_at.unwrap_or(now);
                WorkerFeed {
                    id: w.id,
                    name: w.name,
                    session_id: w.session_id,
                    task: w.task,
                    status: w.status.as_str().to_string(),
                    age_ms: end.saturating_sub(w.started_at),
                }
            })
            .collect();

        let locks: Vec<LockFeed> = {
            let mut stmt = self.conn.prepare(
                "SELECT path_glob, session_id, expires_at FROM coord_claims
                 WHERE expires_at > ?1 ORDER BY expires_at LIMIT 64",
            )?;
            stmt.query_map(params![now], |r| {
                Ok(LockFeed {
                    path_glob: r.get(0)?,
                    holder_session: r.get(1)?,
                    expires_in_secs: (r.get::<_, i64>(2)? - now).max(0) / 1000,
                })
            })?
            .filter_map(|x| x.ok())
            .collect()
        };

        // Timeline: recent broadcasts + task updates, oldest first.
        let mut timeline: Vec<TimelineEvent> = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT from_session, body, created_at FROM coord_messages
                 WHERE to_session IS NULL ORDER BY id DESC LIMIT ?1",
            )?;
            let rows: Vec<(String, String, i64)> = stmt
                .query_map(params![MISSION_MAX_EVENTS as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .filter_map(|x| x.ok())
                .collect();
            for (from, body, at) in rows.into_iter().rev() {
                timeline.push(TimelineEvent {
                    at_ms: at,
                    kind: "msg".to_string(),
                    text: format!("{from}: {body}"),
                });
            }
        }
        {
            let mut stmt = self.conn.prepare(
                "SELECT id, status, updated_at FROM coord_tasks ORDER BY updated_at DESC LIMIT ?1",
            )?;
            let rows: Vec<(String, String, i64)> = stmt
                .query_map(params![MISSION_MAX_EVENTS as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .filter_map(|x| x.ok())
                .collect();
            for (id, status, at) in rows.into_iter().rev() {
                timeline.push(TimelineEvent {
                    at_ms: at,
                    kind: "task".to_string(),
                    text: format!("{id} → {status}"),
                });
            }
        }
        timeline.sort_by_key(|e| e.at_ms);
        timeline.truncate(MISSION_MAX_EVENTS);

        Ok(MissionFeed {
            plan,
            progress: (done, total),
            workers,
            locks,
            timeline,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Coord, TaskStatus};

    #[test]
    fn feed_shows_plan_workers_locks_timeline() {
        let c = Coord::open_in_memory().unwrap();
        let s = c.join("claude", 1, "/proj").unwrap();
        let t1 = c.task_create("reproduce", "repro it", None).unwrap();
        c.task_claim(&t1, &s).unwrap();
        c.task_update_status(&t1, TaskStatus::Done).unwrap();
        let gap = c.dag_inject("docs", "write docs", &t1).unwrap();
        let kids = c
            .dag_expand(&t1, &[("sub-a", "a"), ("sub-b", "b")])
            .unwrap();
        assert_eq!(kids.len(), 2);
        let w = c.worker_create("claude", &s, "patch auth").unwrap();
        c.worker_start(&w.id).unwrap();
        c.claim(&s, "auth/*", 1800).unwrap();
        c.send_message(&s, None, "plan ok").unwrap();

        let feed = c.mission_feed().unwrap();
        assert_eq!(feed.progress.0, 1); // one done
        let docs = feed.plan.iter().find(|p| p.id == gap).unwrap();
        assert_eq!(docs.status, "blocked");
        assert!(docs.ready, "dep is done so the gap is ready");
        let kid = feed.plan.iter().find(|p| p.id == kids[0]).unwrap();
        assert!(kid.ready);
        assert!(feed.workers.iter().any(|w| w.status == "running"));
        assert!(
            feed.locks
                .iter()
                .any(|l| l.path_glob == "auth/*" && l.expires_in_secs > 0)
        );
        assert!(feed.timeline.iter().any(|e| e.text.contains("plan ok")));
        assert!(feed.timeline.windows(2).all(|w| w[0].at_ms <= w[1].at_ms));
    }

    #[test]
    fn empty_room_feeds_empty_never_errors() {
        let c = Coord::open_in_memory().unwrap();
        let feed = c.mission_feed().unwrap();
        assert_eq!(feed.progress, (0, 0));
        assert!(feed.plan.is_empty() && feed.workers.is_empty());
        assert!(feed.locks.is_empty() && feed.timeline.is_empty());
    }
}
