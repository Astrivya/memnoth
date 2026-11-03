//! Worker pool: isolated worktree handles sharing one coordination room.
//!
//! A [`Worker`] is a single handle bound to one isolated worktree, tied 1:1 to
//! a coordination-room session used for heartbeats and path claims. A
//! [`WorkerPool`] holds many workers in one room; claims act as file locks so
//! at most one worker holds a non-overlapping path glob at a time.
//!
//! Use [`join_workspace`] to open (or create) a room for a workspace and get a
//! pool.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::{ClaimResult, Coord, STALE_HEARTBEAT_MS, WorkerStatus};

/// A single worker handle, bound to one isolated worktree.
///
/// The `session_id` is the coordination-room session used for heartbeats and
/// path claims. It is tied 1:1 to this worker.
#[derive(Clone, Debug)]
pub struct Worker {
    /// Unique worker identifier
    pub id: String,
    /// Isolated worktree path (`<workspace>/.memnoth/worktrees/<id>/`)
    pub worktree: PathBuf,
    /// Current lifecycle state
    pub state: WorkerStatus,
    /// Coord session id (used for heartbeat and claims)
    pub session_id: String,
}
/// A pool of workers sharing one coordination room.
///
/// All workers in a pool share the same `Coord` room database. Each worker
/// has its own session and isolated worktree. Claims act as file locks: at
/// most one worker can hold a non-overlapping path glob at a time.
pub struct WorkerPool {
    coord: Coord,
    workspace: PathBuf,
    workers: HashMap<String, Worker>,
}

/// Open (or create) a coordination room for `workspace` and return a worker pool.
///
/// The room database lives at `<workspace>/.memnoth/coord.db`. Worker worktrees
/// are created under `<workspace>/.memnoth/worktrees/<worker-id>/`.
pub fn join_workspace(workspace: &Path) -> Result<WorkerPool> {
    let memnoth_dir = workspace.join(".memnoth");
    std::fs::create_dir_all(&memnoth_dir)?;
    let db_path = memnoth_dir.join("coord.db");
    let coord = Coord::open(&db_path)?;
    Ok(WorkerPool {
        coord,
        workspace: workspace.to_path_buf(),
        workers: HashMap::new(),
    })
}

impl WorkerPool {
    /// Spawn a new worker: create a coord session, a `coord_workers` row, and an
    /// isolated worktree directory. The worker starts in `Running` state.
    pub fn spawn(&mut self, name: &str, task: &str) -> Result<Worker> {
        let session_id = self.coord.join(
            name,
            std::process::id(),
            self.workspace.to_str().unwrap_or(""),
        )?;
        let worker_row = self.coord.worker_create(name, &session_id, task)?;
        self.coord.worker_start(&worker_row.id)?;
        let worktree = self
            .workspace
            .join(".memnoth")
            .join("worktrees")
            .join(&worker_row.id);
        std::fs::create_dir_all(&worktree)?;
        let worker = Worker {
            id: worker_row.id.clone(),
            worktree,
            state: WorkerStatus::Running,
            session_id: session_id.clone(),
        };
        self.workers.insert(worker_row.id, worker.clone());
        Ok(worker)
    }

    /// Claim a path glob on behalf of `worker_id`. Acts as a file lock:
    /// first-writer-wins with TTL auto-release. Returns `Granted` or `Refused`.
    ///
    /// Only a tracked worker may claim; unknown worker ids return an error.
    pub fn claim_task(
        &mut self,
        worker_id: &str,
        path_glob: &str,
        ttl_secs: u64,
    ) -> Result<ClaimResult> {
        let worker = match self.workers.get(worker_id) {
            Some(w) => w,
            None => bail!("worker {worker_id} not found"),
        };
        self.coord.claim(&worker.session_id, path_glob, ttl_secs)
    }

    /// Release a claimed path glob early.
    ///
    /// Only a tracked worker may release; unknown worker ids return an error.
    pub fn release(&mut self, worker_id: &str, path_glob: &str) -> Result<()> {
        let worker = match self.workers.get(worker_id) {
            Some(w) => w,
            None => bail!("worker {worker_id} not found"),
        };
        self.coord.release(&worker.session_id, path_glob)
    }

    /// Heartbeat a single worker to keep its session alive.
    ///
    /// Intended to be called from the caller's tick loop (TUI redraw tick or
    /// CLI loop), well within the [`STALE_HEARTBEAT_MS`] window.
    ///
    /// Only a tracked worker may heartbeat; unknown worker ids return an error.
    pub fn heartbeat(&mut self, worker_id: &str) -> Result<()> {
        let worker = match self.workers.get(worker_id) {
            Some(w) => w,
            None => bail!("worker {worker_id} not found"),
        };
        self.coord.heartbeat(&worker.session_id)
    }

    /// Heartbeat every worker in the pool.
    ///
    /// Call this on each tick while workers are alive. Returns an error if any
    /// individual heartbeat fails (the session row may have been deleted).
    pub fn heartbeat_all(&mut self) -> Result<()> {
        for worker in self.workers.values() {
            self.coord.heartbeat(&worker.session_id)?;
        }
        Ok(())
    }

    /// Fail-closed: release all claims of workers whose heartbeats have gone
    /// stale (older than [`STALE_HEARTBEAT_MS`]) and mark them `Failed`.
    ///
    /// Returns the number of claims released. After this call, stale workers
    /// are no longer tracked by the pool and their path globs are available
    /// to other workers.
    pub fn fail_closed(&mut self) -> Result<usize> {
        let status = self.coord.status()?;

        let stale: Vec<(String, Vec<String>)> = status
            .into_iter()
            .filter(|s| s.heartbeat_age_ms > STALE_HEARTBEAT_MS)
            .map(|s| (s.session_id, s.active_claims))
            .collect();

        let mut released = 0usize;
        for (session_id, globs) in &stale {
            for glob in globs {
                self.coord.release(session_id, glob)?;
                released += 1;
            }
            let failed: Vec<String> = self
                .workers
                .iter()
                .filter(|(_, w)| w.session_id == *session_id)
                .map(|(id, _)| id.clone())
                .collect();
            for wid in &failed {
                self.coord.worker_fail(wid, "missed heartbeat")?;
                self.workers.remove(wid);
            }
        }
        Ok(released)
    }

    /// Shut down the pool: leave every worker session (releasing all claims
    /// and deleting the session) and mark all workers `Cancelled`.
    ///
    /// After shutdown the pool tracks no workers.
    pub fn shutdown(&mut self) -> Result<()> {
        let worker_ids: Vec<String> = self.workers.keys().cloned().collect();
        for worker_id in &worker_ids {
            let worker = self.workers.get(worker_id).unwrap();
            self.coord.leave(&worker.session_id)?;
            let _ = self.coord.worker_cancel(worker_id);
        }
        self.workers.clear();
        Ok(())
    }

    /// Return the worker with `id`, if it is still tracked by this pool.
    pub fn worker(&self, worker_id: &str) -> Option<&Worker> {
        self.workers.get(worker_id)
    }

    /// Return the number of tracked workers.
    pub fn len(&self) -> usize {
        self.workers.len()
    }

    /// Return `true` if the pool tracks no workers.
    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }

    /// Iterate over all tracked workers.
    pub fn iter(&self) -> impl Iterator<Item = &Worker> {
        self.workers.values()
    }

    /// Access the underlying coordination room.
    pub fn coord(&self) -> &Coord {
        &self.coord
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::now_ms;

    fn tmp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir();
        dir.join(format!(
            "memnoth-worker-test-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ))
    }

    #[test]
    fn spawn_creates_worker_with_isolated_worktree() {
        let workspace = tmp_workspace("spawn");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut pool = join_workspace(&workspace).unwrap();

        let w1 = pool.spawn("alpha", "task-1").unwrap();
        let w2 = pool.spawn("beta", "task-2").unwrap();

        assert_eq!(w1.state, WorkerStatus::Running);
        assert_eq!(w2.state, WorkerStatus::Running);
        assert_ne!(w1.id, w2.id);
        assert_ne!(w1.session_id, w2.session_id);
        assert_ne!(w1.worktree, w2.worktree);
        assert!(
            w1.worktree
                .starts_with(workspace.join(".memnoth/worktrees"))
        );
        assert!(
            w2.worktree
                .starts_with(workspace.join(".memnoth/worktrees"))
        );
        assert!(w1.worktree.exists());
        assert!(w2.worktree.exists());
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn claim_first_writer_wins() {
        let workspace = tmp_workspace("claim");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut pool = join_workspace(&workspace).unwrap();

        let w1 = pool.spawn("alpha", "task-1").unwrap();
        let w2 = pool.spawn("beta", "task-2").unwrap();

        assert!(matches!(
            pool.claim_task(&w1.id, "src/auth/*.rs", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));
        match pool.claim_task(&w2.id, "src/auth/login.rs", 1800).unwrap() {
            ClaimResult::Refused {
                holder_session,
                holder_glob,
            } => {
                assert_eq!(holder_session, w1.session_id);
                assert_eq!(holder_glob, "src/auth/*.rs");
            }
            other => panic!("expected refused, got {other:?}"),
        }
    }

    #[test]
    fn non_overlapping_claims_both_granted() {
        let workspace = tmp_workspace("nonoverlap");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut pool = join_workspace(&workspace).unwrap();

        let w1 = pool.spawn("alpha", "task-1").unwrap();
        let w2 = pool.spawn("beta", "task-2").unwrap();

        assert!(matches!(
            pool.claim_task(&w1.id, "src/a/*", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));
        assert!(matches!(
            pool.claim_task(&w2.id, "src/b/*", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));
    }

    #[test]
    fn release_then_reclaim() {
        let workspace = tmp_workspace("release");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut pool = join_workspace(&workspace).unwrap();

        let w1 = pool.spawn("alpha", "task-1").unwrap();
        let w2 = pool.spawn("beta", "task-2").unwrap();

        pool.claim_task(&w1.id, "src/x/*", 1800).unwrap();
        pool.release(&w1.id, "src/x/*").unwrap();
        assert!(matches!(
            pool.claim_task(&w2.id, "src/x/*", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));
    }

    #[test]
    fn heartbeat_keeps_worker_alive() {
        let workspace = tmp_workspace("heartbeat");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut pool = join_workspace(&workspace).unwrap();

        let w = pool.spawn("alpha", "task-1").unwrap();

        pool.heartbeat(&w.id).unwrap();
        let status = pool.coord.status().unwrap();
        let session = status
            .iter()
            .find(|s| s.session_id == w.session_id)
            .expect("session should exist");
        assert!(!session.stale);
    }

    #[test]
    fn heartbeat_all_keeps_all_workers_alive() {
        let workspace = tmp_workspace("heartbeat_all");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut pool = join_workspace(&workspace).unwrap();

        let w1 = pool.spawn("alpha", "task-1").unwrap();
        let w2 = pool.spawn("beta", "task-2").unwrap();

        pool.heartbeat_all().unwrap();

        let status = pool.coord.status().unwrap();
        for s in &status {
            if s.session_id == w1.session_id || s.session_id == w2.session_id {
                assert!(!s.stale, "session {} should not be stale", s.session_id);
            }
        }
    }

    #[test]
    fn fail_closed_releases_claims_on_stale_worker() {
        let workspace = tmp_workspace("failclosed");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut pool = join_workspace(&workspace).unwrap();

        let w1 = pool.spawn("alpha", "task-1").unwrap();
        let w2 = pool.spawn("beta", "task-2").unwrap();

        // w1 claims a path
        assert!(matches!(
            pool.claim_task(&w1.id, "src/secret/*", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));

        // Backdate w1's heartbeat to make it stale
        let stale_ts = now_ms() - 2 * STALE_HEARTBEAT_MS as i64;
        pool.coord
            .conn
            .execute(
                "UPDATE coord_sessions SET heartbeat_at = ?1 WHERE id = ?2",
                rusqlite::params![stale_ts, &w1.session_id],
            )
            .unwrap();

        // Run fail-closed: w1's claim should be released
        let released = pool.fail_closed().unwrap();
        assert_eq!(released, 1);

        // w2 can now claim the same path (fail-closed: w1 no longer holds it)
        assert!(matches!(
            pool.claim_task(&w2.id, "src/secret/*", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));

        // w1 is no longer tracked by the pool
        assert!(pool.worker(&w1.id).is_none());
    }

    #[test]
    fn fail_closed_no_op_when_all_heartbeating() {
        let workspace = tmp_workspace("failclosed_noop");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut pool = join_workspace(&workspace).unwrap();

        let _w = pool.spawn("alpha", "task-1").unwrap();
        pool.heartbeat_all().unwrap();

        let released = pool.fail_closed().unwrap();
        assert_eq!(released, 0);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn shutdown_releases_all_claims() {
        let workspace = tmp_workspace("shutdown");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut pool = join_workspace(&workspace).unwrap();

        let w1 = pool.spawn("alpha", "task-1").unwrap();
        let w2 = pool.spawn("beta", "task-2").unwrap();

        pool.claim_task(&w1.id, "src/a/*", 1800).unwrap();
        pool.claim_task(&w2.id, "src/b/*", 1800).unwrap();

        pool.shutdown().unwrap();

        assert!(pool.is_empty());

        // A new worker can claim the same paths (old claims released)
        let w3 = pool.spawn("gamma", "task-3").unwrap();
        assert!(matches!(
            pool.claim_task(&w3.id, "src/a/*", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));
        assert!(matches!(
            pool.claim_task(&w3.id, "src/b/*", 1800).unwrap(),
            ClaimResult::Granted { .. }
        ));
    }

    #[test]
    fn unknown_worker_errors() {
        let workspace = tmp_workspace("unknown");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut pool = join_workspace(&workspace).unwrap();

        let err = pool
            .claim_task("no-such-worker", "src/*", 1800)
            .unwrap_err();
        assert!(err.to_string().contains("not found"));

        let err = pool.heartbeat("no-such-worker").unwrap_err();
        assert!(err.to_string().contains("not found"));

        let err = pool.release("no-such-worker", "src/*").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn worker_lifecycle_via_pool() {
        let workspace = tmp_workspace("lifecycle");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut pool = join_workspace(&workspace).unwrap();

        let w = pool.spawn("gamma", "refactor auth").unwrap();
        assert_eq!(w.state, WorkerStatus::Running);
        assert_eq!(pool.len(), 1);

        let found = pool.worker(&w.id).unwrap();
        assert_eq!(found.id, w.id);
        assert_eq!(found.session_id, w.session_id);

        pool.heartbeat(&w.id).unwrap();
        assert!(pool.worker(&w.id).is_some());

        pool.shutdown().unwrap();
        assert!(pool.worker(&w.id).is_none());
    }
}
