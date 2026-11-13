//! Run registry — execution state kept apart from the session message list.
//!
//! A session's messages are the transcript. A run is a unit of execution (a
//! turn, a tool call, a delegated subagent task) with its own lifecycle:
//! pending → running → done / failed / cancelled. Runs are stored here, in
//! this registry, and never in the session's JSONL message list, so a run's
//! status can be queried and reported on without parsing the transcript.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Status of a run.
///
/// A run moves `Pending → Running → Done` on the happy path, or branches to
/// `Failed`, `Cancelled` or `Orphaned` at any point before it reaches a
/// terminal state. `Orphaned` means the child process vanished without a
/// terminal transition — its identity no longer matches the recorded
/// `(pid, boot_id, process_start_time)`. Once terminal, no further transition
/// is allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    /// The run has been created but not yet started.
    Pending,
    /// The run is currently executing.
    Running,
    /// The run completed successfully.
    Done,
    /// The run failed.
    Failed,
    /// The run was cancelled before completion.
    Cancelled,
    /// The child process disappeared mid-run; identity could not be proven.
    Orphaned,
}

impl RunStatus {
    /// The lowercase label used in serialised output.
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Pending => "pending",
            RunStatus::Running => "running",
            RunStatus::Done => "done",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Orphaned => "orphaned",
        }
    }

    /// Parse a label, falling back to [`RunStatus::Pending`] for an unknown one.
    pub fn from_label(s: &str) -> Self {
        match s {
            "running" => RunStatus::Running,
            "done" => RunStatus::Done,
            "failed" => RunStatus::Failed,
            "cancelled" => RunStatus::Cancelled,
            "orphaned" => RunStatus::Orphaned,
            _ => RunStatus::Pending,
        }
    }
}

/// Everything static about a run: who it belongs to and what it was asked to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    /// Run id, unique within this process and durable across restarts.
    pub id: String,
    /// The session this run belongs to, if any. A run need not be tied to a session.
    pub session_id: Option<String>,
    /// The agent surface that owns this run: `memnoth`, `ci`, or a named subagent.
    pub agent: String,
    /// The model asked to answer, when a model ran it.
    pub model: Option<String>,
    /// Human-readable description of what the run does.
    pub purpose: String,
    /// When the run was created.
    pub created_ms: i64,
}

/// One entry in the registry: the static run plus its live lifecycle state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEntry {
    /// The run as it was registered.
    pub run: Run,
    /// Where the run is in its lifecycle.
    pub status: RunStatus,
    /// When the run entered its current status.
    pub entered_ms: i64,
    /// When the run reached a terminal state, if it has.
    pub completed_ms: Option<i64>,
    /// A terminal failure reason, if the run failed.
    pub error: Option<String>,
}

/// The run registry: runs kept apart from the session message list.
///
/// Runs live in their own store keyed by run id. The session message list
/// (owned by [`memnoth_session::Session`]) knows nothing about runs, and this
/// registry knows nothing about messages — a run only references a session by
/// id, never by message.
#[derive(Debug)]
pub struct RunRegistry {
    runs: BTreeMap<String, RunEntry>,
}

impl RunRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self {
            runs: BTreeMap::new(),
        }
    }

    /// Register a new run in the pending state.
    ///
    /// Returns an error when a run with the same id already exists.
    pub fn register(&mut self, run: Run) -> Result<()> {
        if self.runs.contains_key(&run.id) {
            bail!("run {} already registered", run.id);
        }
        let now = now_ms();
        self.runs.insert(
            run.id.clone(),
            RunEntry {
                run,
                status: RunStatus::Pending,
                entered_ms: now,
                completed_ms: None,
                error: None,
            },
        );
        Ok(())
    }

    /// Mark a run as running.
    ///
    /// Returns an error when the run is unknown or already terminal.
    pub fn start(&mut self, id: &str) -> Result<()> {
        let entry = self
            .get_mut(id)
            .with_context(|| format!("no such run {id}"))?;
        if matches!(
            entry.status,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
        ) {
            bail!("run {id} is already {}", entry.status.as_str());
        }
        let now = now_ms();
        entry.status = RunStatus::Running;
        entry.entered_ms = now;
        Ok(())
    }

    /// Mark a run as done.
    ///
    /// Returns an error when the run is unknown or already terminal.
    pub fn complete(&mut self, id: &str) -> Result<()> {
        let entry = self
            .get_mut(id)
            .with_context(|| format!("no such run {id}"))?;
        if matches!(
            entry.status,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
        ) {
            bail!("run {id} is already {}", entry.status.as_str());
        }
        let now = now_ms();
        entry.status = RunStatus::Done;
        entry.entered_ms = now;
        entry.completed_ms = Some(now);
        Ok(())
    }

    /// Mark a run as failed with a reason.
    ///
    /// Returns an error when the run is unknown or already terminal.
    pub fn fail(&mut self, id: &str, reason: impl Into<String>) -> Result<()> {
        let entry = self
            .get_mut(id)
            .with_context(|| format!("no such run {id}"))?;
        if matches!(
            entry.status,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
        ) {
            bail!("run {id} is already {}", entry.status.as_str());
        }
        let now = now_ms();
        entry.status = RunStatus::Failed;
        entry.entered_ms = now;
        entry.completed_ms = Some(now);
        entry.error = Some(reason.into());
        Ok(())
    }

    /// Mark a run as cancelled.
    ///
    /// Returns an error when the run is unknown or already terminal.
    pub fn cancel(&mut self, id: &str) -> Result<()> {
        let entry = self
            .get_mut(id)
            .with_context(|| format!("no such run {id}"))?;
        if matches!(
            entry.status,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
        ) {
            bail!("run {id} is already {}", entry.status.as_str());
        }
        let now = now_ms();
        entry.status = RunStatus::Cancelled;
        entry.entered_ms = now;
        entry.completed_ms = Some(now);
        Ok(())
    }
    /// Revive a run: a cancelled, failed or orphaned run becomes pending again.
    ///
    /// Revive is how the Deck's `r` key works — a stopped child is resumed, not
    /// respawned, so its log and history survive. A run that is already live or
    /// done cannot be revived: resurrecting a finished run would rewrite the
    /// fact that it finished.
    pub fn revive(&mut self, id: &str) -> Result<()> {
        let entry = self
            .get_mut(id)
            .with_context(|| format!("no such run {id}"))?;
        match entry.status {
            RunStatus::Cancelled | RunStatus::Failed | RunStatus::Orphaned => {}
            status => bail!("run {id} is {status:?}; only a stopped run can be revived"),
        }
        let now = now_ms();
        entry.status = RunStatus::Pending;
        entry.entered_ms = now;
        entry.completed_ms = None;
        entry.error = None;
        Ok(())
    }

    /// Mark a run as orphaned: the child process vanished mid-run and its
    /// recorded identity can no longer be proven.
    ///
    /// Returns an error when the run is unknown or already terminal.
    pub fn orphan(&mut self, id: &str) -> Result<()> {
        let entry = self
            .get_mut(id)
            .with_context(|| format!("no such run {id}"))?;
        if matches!(
            entry.status,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Orphaned
        ) {
            bail!("run {id} is already {}", entry.status.as_str());
        }
        let now = now_ms();
        entry.status = RunStatus::Orphaned;
        entry.entered_ms = now;
        entry.completed_ms = Some(now);
        Ok(())
    }

    /// Look up a run entry by id.
    pub fn get(&self, id: &str) -> Option<&RunEntry> {
        self.runs.get(id)
    }

    /// Look up a run entry by id, mutably.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut RunEntry> {
        self.runs.get_mut(id)
    }

    /// All runs, newest first by creation time.
    pub fn list(&self) -> Vec<&RunEntry> {
        let mut out: Vec<&RunEntry> = self.runs.values().collect();
        out.sort_by_key(|e| Reverse(e.run.created_ms));
        out
    }

    /// All runs belonging to a session, newest first.
    pub fn list_for_session(&self, session_id: &str) -> Vec<&RunEntry> {
        self.list()
            .into_iter()
            .filter(|e| e.run.session_id.as_deref() == Some(session_id))
            .collect()
    }

    /// All runs in a given status, newest first.
    pub fn by_status(&self, status: RunStatus) -> Vec<&RunEntry> {
        self.list()
            .into_iter()
            .filter(|e| e.status == status)
            .collect()
    }

    /// Number of runs registered.
    pub fn len(&self) -> usize {
        self.runs.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

impl Default for RunRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_run(id: &str) -> Run {
        Run {
            id: id.to_string(),
            session_id: Some("sess-1".to_string()),
            agent: "memnoth".to_string(),
            model: Some("model-1".to_string()),
            purpose: "a test run".to_string(),
            created_ms: now_ms(),
        }
    }

    #[test]
    fn register_then_start_complete() {
        let mut reg = RunRegistry::new();
        let run = make_run("r1");
        reg.register(run).unwrap();
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("r1").unwrap().status, RunStatus::Pending);

        reg.start("r1").unwrap();
        assert_eq!(reg.get("r1").unwrap().status, RunStatus::Running);

        reg.complete("r1").unwrap();
        let entry = reg.get("r1").unwrap();
        assert_eq!(entry.status, RunStatus::Done);
        assert!(entry.completed_ms.is_some());
    }

    #[test]
    fn revive_returns_a_stopped_run_to_pending() {
        let mut reg = RunRegistry::new();
        reg.register(make_run("r1")).unwrap();
        reg.start("r1").unwrap();
        reg.cancel("r1").unwrap();
        reg.revive("r1").unwrap();
        let entry = reg.get("r1").unwrap();
        assert_eq!(entry.status, RunStatus::Pending);
        assert!(entry.completed_ms.is_none());
        assert!(entry.error.is_none());
        // A finished run is history, not resumable.
        reg.start("r1").unwrap();
        reg.complete("r1").unwrap();
        assert!(reg.revive("r1").is_err());
        assert!(reg.revive("nope").is_err());
    }

    #[test]
    fn terminal_transition_is_refused() {
        let mut reg = RunRegistry::new();
        reg.register(make_run("r1")).unwrap();
        reg.complete("r1").unwrap();
        assert!(reg.fail("r1", "boom").is_err());
        assert!(reg.cancel("r1").is_err());
        assert!(reg.start("r1").is_err());
    }

    #[test]
    fn unknown_run_is_refused() {
        let mut reg = RunRegistry::new();
        assert!(reg.start("nope").is_err());
        assert!(reg.complete("nope").is_err());
        assert!(reg.fail("nope", "x").is_err());
        assert!(reg.cancel("nope").is_err());
    }

    #[test]
    fn duplicate_id_is_refused() {
        let mut reg = RunRegistry::new();
        let run = make_run("r1");
        reg.register(run.clone()).unwrap();
        assert!(reg.register(run).is_err());
    }

    #[test]
    fn list_for_session_filters() {
        let mut reg = RunRegistry::new();
        reg.register(make_run("r1")).unwrap();
        let mut run = make_run("r2");
        run.session_id = Some("sess-2".to_string());
        reg.register(run).unwrap();

        assert_eq!(reg.list_for_session("sess-1").len(), 1);
        assert_eq!(reg.list_for_session("sess-2").len(), 1);
        assert_eq!(reg.list_for_session("sess-3").len(), 0);
    }

    #[test]
    fn by_status_filters() {
        let mut reg = RunRegistry::new();
        reg.register(make_run("r1")).unwrap();
        reg.register(make_run("r2")).unwrap();
        reg.start("r1").unwrap();
        assert_eq!(reg.by_status(RunStatus::Running).len(), 1);
        assert_eq!(reg.by_status(RunStatus::Pending).len(), 1);
    }

    #[test]
    fn newest_first_ordering() {
        let mut reg = RunRegistry::new();
        let _ = reg.register(make_run("older"));
        std::thread::sleep(std::time::Duration::from_millis(5));
        let _ = reg.register(make_run("newer"));
        let listed: Vec<String> = reg.list().into_iter().map(|e| e.run.id.clone()).collect();
        assert_eq!(listed, vec!["newer", "older"]);
    }
}
