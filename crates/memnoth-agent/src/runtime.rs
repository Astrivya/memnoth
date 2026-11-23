//! Run registry — execution state kept apart from the session message list.
//!
//! A session's messages are the transcript. A run is a unit of execution (a
//! turn, a tool call, a delegated subagent task) with its own lifecycle:
//! pending → running → done / failed / cancelled. Runs are stored here, in
//! this registry, and never in the session's JSONL message list, so a run's
//! status can be queried and reported on without parsing the transcript.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
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

// ---------------------------------------------------------------------------
// AgentRun: the seven-phase state machine, journaled outside the message list
// ---------------------------------------------------------------------------

/// The seven phases of a delegated run (Wave 5 §5.3 / F-03).
///
/// This is the user-visible lifecycle. It is deliberately a different type
/// from [`RunStatus`]: the registry's four terminal labels are the row-level
/// projection a Deck renders, while these phases are the state machine whose
/// every edge is appended to a session journal. `waiting_approval` in
/// particular is a *live* phase — a run waiting on a person is not failed and
/// not idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    /// Admitted but not yet executing.
    Queued,
    /// Executing; tool boundaries are being crossed.
    Running,
    /// Paused on a permission or question answered at the root.
    WaitingApproval,
    /// Completed with a validated result.
    Completed,
    /// Failed with a typed reason.
    Failed,
    /// Cancelled by a person or by an approval denial.
    Cancelled,
    /// The recorded process identity could not be proven after a restart.
    Orphaned,
}

impl AgentPhase {
    /// The stable wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentPhase::Queued => "queued",
            AgentPhase::Running => "running",
            AgentPhase::WaitingApproval => "waiting_approval",
            AgentPhase::Completed => "completed",
            AgentPhase::Failed => "failed",
            AgentPhase::Cancelled => "cancelled",
            AgentPhase::Orphaned => "orphaned",
        }
    }

    /// Parse a wire spelling, falling back to [`AgentPhase::Queued`].
    pub fn from_label(label: &str) -> Self {
        match label {
            "running" => AgentPhase::Running,
            "waiting_approval" => AgentPhase::WaitingApproval,
            "completed" => AgentPhase::Completed,
            "failed" => AgentPhase::Failed,
            "cancelled" => AgentPhase::Cancelled,
            "orphaned" => AgentPhase::Orphaned,
            _ => AgentPhase::Queued,
        }
    }

    /// Whether this phase is terminal. No edge leaves a terminal phase.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AgentPhase::Completed
                | AgentPhase::Failed
                | AgentPhase::Cancelled
                | AgentPhase::Orphaned
        )
    }

    /// Whether `next` is a legal edge from `self`.
    ///
    /// The table is the invariant: there is no back edge from a terminal
    /// phase, approval only goes to `running` or `cancelled`, and any live
    /// phase can be orphaned by reconciliation (S-DLG-4).
    fn allows(self, next: Self) -> bool {
        if self.is_terminal() {
            return false;
        }
        match self {
            AgentPhase::Queued => matches!(
                next,
                AgentPhase::Running | AgentPhase::Cancelled | AgentPhase::Orphaned
            ),
            AgentPhase::Running => matches!(
                next,
                AgentPhase::WaitingApproval
                    | AgentPhase::Completed
                    | AgentPhase::Failed
                    | AgentPhase::Cancelled
                    | AgentPhase::Orphaned
            ),
            AgentPhase::WaitingApproval => matches!(
                next,
                AgentPhase::Running | AgentPhase::Cancelled | AgentPhase::Orphaned
            ),
            AgentPhase::Completed
            | AgentPhase::Failed
            | AgentPhase::Cancelled
            | AgentPhase::Orphaned => false,
        }
    }
}

/// One appended row in a session's run journal.
///
/// This is a *fact about a run*, not a session message. It lives in its own
/// JSONL beside the session, so compaction of the message list cannot drop an
/// in-flight run (F-05). The `detail` is a short reason or digest — never file
/// content, model output, or a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTransition {
    /// Schema version; `1` for every row this build writes.
    pub v: u32,
    /// Monotonically increasing row number within the session journal.
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub ts_ms: i64,
    /// The session the run belongs to, when one was named.
    pub session: Option<String>,
    /// The run this transition belongs to.
    pub run: String,
    /// The parent run, when this child was delegated.
    pub parent: Option<String>,
    /// The phase entered by this row.
    pub phase: AgentPhase,
    /// A short reason or digest, when the edge carries one.
    pub detail: Option<String>,
}

/// Schema version written into every [`RunTransition`].
pub const RUN_JOURNAL_VERSION: u32 = 1;

/// An append-only journal of run transitions, stored outside the message list.
///
/// A journal is opened for a path (conventionally
/// `.memnoth/sessions/<id>/runs.jsonl`). Opening resumes the sequence from the
/// highest row already on disk, so a restart appends instead of reusing ids.
#[derive(Debug)]
pub struct RunJournal {
    path: PathBuf,
    seq: u64,
}

impl RunJournal {
    /// Open (or create) the journal at `path`, resuming its sequence.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create run journal dir {}", parent.display()))?;
        }
        let seq = match fs::read_to_string(&path) {
            Ok(text) => text
                .lines()
                .filter_map(|line| serde_json::from_str::<RunTransition>(line).ok())
                .map(|row| row.seq)
                .max()
                .unwrap_or(0),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error).context("cannot read run journal"),
        };
        Ok(Self { path, seq })
    }

    /// The file this journal appends to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The sequence number of the last appended row.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Append one transition and return it.
    fn append(
        &mut self,
        run: &str,
        session: Option<&str>,
        parent: Option<&str>,
        phase: AgentPhase,
        detail: Option<&str>,
    ) -> Result<RunTransition> {
        self.seq += 1;
        let row = RunTransition {
            v: RUN_JOURNAL_VERSION,
            seq: self.seq,
            ts_ms: now_ms(),
            session: session.map(str::to_string),
            run: run.to_string(),
            parent: parent.map(str::to_string),
            phase,
            detail: detail.map(str::to_string),
        };
        let line = serde_json::to_string(&row)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("cannot open run journal {}", self.path.display()))?;
        writeln!(file, "{line}")?;
        Ok(row)
    }
}

/// Read every valid row of a run journal, in append order.
///
/// A malformed trailing line is skipped, exactly as for child logs: a run may
/// be appending while another reader looks on.
pub fn replay_run_journal(path: impl AsRef<Path>) -> Result<Vec<RunTransition>> {
    let path = path.as_ref();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("cannot read run journal"),
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<RunTransition>(line).ok())
        .collect())
}

/// Rebuild each run's last phase from a journal.
pub fn last_phases(path: impl AsRef<Path>) -> Result<BTreeMap<String, AgentPhase>> {
    let mut phases = BTreeMap::new();
    for row in replay_run_journal(path)? {
        phases.insert(row.run, row.phase);
    }
    Ok(phases)
}

/// Run ids whose last recorded phase is live at the point of a restart.
///
/// These are the runs recovery must either adopt (identity proven) or mark
/// orphaned (identity unproven). A run already orphaned is not a candidate.
pub fn orphan_candidates(path: impl AsRef<Path>) -> Result<Vec<String>> {
    Ok(last_phases(path)?
        .into_iter()
        .filter(|(_, phase)| !phase.is_terminal())
        .map(|(run, _)| run)
        .collect())
}

/// One run driven through the seven-phase state machine.
///
/// `AgentRun` owns its journal: every successful [`Self::start`],
/// [`Self::wait_for_approval`], [`Self::complete`] and so on appends a row
/// before returning, so a crash leaves a recoverable history rather than an
/// ambiguous "maybe it finished".
#[derive(Debug)]
pub struct AgentRun {
    /// Stable run id.
    pub id: String,
    /// The session that owns the run, when one was named.
    pub session_id: Option<String>,
    /// The parent run, when this run was delegated.
    pub parent: Option<String>,
    /// The agent spec name.
    pub agent: String,
    /// The current phase.
    pub phase: AgentPhase,
    /// When the current phase was entered.
    pub entered_ms: i64,
    /// The reason attached to the edge that entered this phase.
    pub detail: Option<String>,
    journal: RunJournal,
}

impl AgentRun {
    /// Enqueue a new run: append the first `queued` row and return the run.
    pub fn enqueue(
        id: impl Into<String>,
        session_id: Option<&str>,
        parent: Option<&str>,
        agent: &str,
        journal_path: impl AsRef<Path>,
    ) -> Result<Self> {
        let id = id.into();
        let mut journal = RunJournal::open(journal_path)?;
        let row = journal.append(
            &id,
            session_id,
            parent,
            AgentPhase::Queued,
            Some(&format!("agent {agent}")),
        )?;
        Ok(Self {
            id,
            session_id: session_id.map(str::to_string),
            parent: parent.map(str::to_string),
            agent: agent.to_string(),
            phase: AgentPhase::Queued,
            entered_ms: row.ts_ms,
            detail: row.detail,
            journal,
        })
    }

    /// Reattach to an existing journal for `id`, resuming from its last phase.
    ///
    /// This is the restart entry point: the caller can then call
    /// [`Self::orphan`] when reconciliation cannot prove the process identity.
    pub fn attach(id: impl Into<String>, journal_path: impl AsRef<Path>) -> Result<Self> {
        let id = id.into();
        let journal = RunJournal::open(journal_path)?;
        let phase = last_phases(journal.path())?
            .get(&id)
            .copied()
            .unwrap_or(AgentPhase::Queued);
        let detail = replay_run_journal(journal.path())?
            .into_iter()
            .rev()
            .find(|row| row.run == id)
            .and_then(|row| row.detail);
        let (session, parent, agent) = replay_run_journal(journal.path())?
            .into_iter()
            .find(|row| row.run == id)
            .map(|row| {
                (
                    row.session,
                    row.parent,
                    row.detail
                        .and_then(|d| d.strip_prefix("agent ").map(str::to_string)),
                )
            })
            .unwrap_or((None, None, None));
        Ok(Self {
            id,
            session_id: session,
            parent,
            agent: agent.unwrap_or_default(),
            phase,
            entered_ms: now_ms(),
            detail,
            journal,
        })
    }

    /// The sequence number of the last row this run appended.
    pub fn seq(&self) -> u64 {
        self.journal.seq()
    }

    /// The journal file this run appends to.
    pub fn journal_path(&self) -> &Path {
        self.journal.path()
    }

    fn move_to(&mut self, next: AgentPhase, detail: Option<&str>) -> Result<()> {
        if !self.phase.allows(next) {
            bail!(
                "run {} cannot move from {} to {}",
                self.id,
                self.phase.as_str(),
                next.as_str()
            );
        }
        let row = self.journal.append(
            &self.id,
            self.session_id.as_deref(),
            self.parent.as_deref(),
            next,
            detail,
        )?;
        self.phase = next;
        self.entered_ms = row.ts_ms;
        self.detail = row.detail;
        Ok(())
    }

    /// `queued → running`.
    pub fn start(&mut self) -> Result<()> {
        self.move_to(AgentPhase::Running, None)
    }

    /// `running → waiting_approval`, naming what the person must answer.
    pub fn wait_for_approval(&mut self, what: impl AsRef<str>) -> Result<()> {
        self.move_to(AgentPhase::WaitingApproval, Some(what.as_ref()))
    }

    /// `waiting_approval → running`, after the person allowed the call.
    pub fn approve(&mut self) -> Result<()> {
        self.move_to(AgentPhase::Running, Some("approval granted"))
    }

    /// `waiting_approval → cancelled`, after a denial or a timeout.
    pub fn deny(&mut self) -> Result<()> {
        self.move_to(AgentPhase::Cancelled, Some("approval denied"))
    }

    /// `running → completed`.
    pub fn complete(&mut self) -> Result<()> {
        self.move_to(AgentPhase::Completed, None)
    }

    /// `running → failed`, with the reason.
    pub fn fail(&mut self, reason: impl AsRef<str>) -> Result<()> {
        self.move_to(AgentPhase::Failed, Some(reason.as_ref()))
    }

    /// Live phase → `cancelled`.
    pub fn cancel(&mut self) -> Result<()> {
        self.move_to(AgentPhase::Cancelled, None)
    }

    /// Any live phase → `orphaned`, with the reconciliation reason.
    pub fn orphan(&mut self, reason: impl AsRef<str>) -> Result<()> {
        self.move_to(AgentPhase::Orphaned, Some(reason.as_ref()))
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

    fn journal(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "memnoth-agent-journal-{}-{name}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn every_transition_appends_a_row_and_no_edge_leaves_a_terminal_phase() {
        let path = journal("edges");
        let mut run = AgentRun::enqueue("r1", Some("s1"), None, "scout", &path).unwrap();
        assert_eq!(run.phase, AgentPhase::Queued);
        run.start().unwrap();
        run.wait_for_approval("write the file").unwrap();
        assert_eq!(run.phase, AgentPhase::WaitingApproval);
        run.approve().unwrap();
        run.complete().unwrap();
        assert_eq!(run.phase, AgentPhase::Completed);

        // A terminal run is frozen: every further edge is refused.
        assert!(run.start().is_err());
        assert!(run.cancel().is_err());
        assert!(run.orphan("late").is_err());

        let rows = replay_run_journal(&path).unwrap();
        let phases: Vec<AgentPhase> = rows.iter().map(|r| r.phase).collect();
        assert_eq!(
            phases,
            vec![
                AgentPhase::Queued,
                AgentPhase::Running,
                AgentPhase::WaitingApproval,
                AgentPhase::Running,
                AgentPhase::Completed,
            ]
        );
        assert_eq!(
            rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert!(
            rows.iter()
                .all(|r| r.v == RUN_JOURNAL_VERSION && r.run == "r1")
        );
        assert_eq!(
            last_phases(&path).unwrap().get("r1"),
            Some(&AgentPhase::Completed)
        );
    }

    #[test]
    fn restart_reports_only_live_runs_as_orphan_candidates() {
        let path = journal("candidates");
        let mut done = AgentRun::enqueue("done", None, None, "scout", &path).unwrap();
        done.start().unwrap();
        done.complete().unwrap();
        let mut live = AgentRun::enqueue("live", None, None, "worker", &path).unwrap();
        live.start().unwrap();
        let mut waiting = AgentRun::enqueue("waiting", None, None, "plan", &path).unwrap();
        waiting.start().unwrap();
        waiting.wait_for_approval("a question").unwrap();

        let candidates = orphan_candidates(&path).unwrap();
        assert_eq!(candidates, vec!["live", "waiting"]);
        assert!(!candidates.contains(&"done".to_string()));

        let mut attached = AgentRun::attach("live", &path).unwrap();
        assert_eq!(attached.phase, AgentPhase::Running);
        attached
            .orphan("process identity unproven at restart")
            .unwrap();
        assert_eq!(
            orphan_candidates(&path).unwrap(),
            vec!["waiting"],
            "only the still-live run remains a candidate"
        );
        assert_eq!(
            last_phases(&path).unwrap().get("live"),
            Some(&AgentPhase::Orphaned)
        );
    }

    #[test]
    fn an_illegal_edge_is_refused_without_appending() {
        let path = journal("illegal");
        let mut run = AgentRun::enqueue("r", None, None, "scout", &path).unwrap();
        assert!(run.complete().is_err(), "queued cannot jump to completed");
        assert_eq!(run.seq(), 1, "a refused edge writes no row");
        run.start().unwrap();
        assert!(run.wait_for_approval("x").is_ok());
        assert!(run.complete().is_err(), "approval must be resolved first");
        assert_eq!(run.seq(), 3);
    }
}
