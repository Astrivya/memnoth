//! Delegation: the spawn tree, child logs, depth caps, and cancellation.
//!
//! The registry in [`crate::runtime`] answers "what state is this run in". This
//! module answers the questions a Deck and a Trace ask: who spawned whom, how
//! deep the tree is, what did one child do, and is anything above me cancelled.
//!
//! A child is not a thread and not a second `Session`: it is a registry run
//! plus a tree edge plus its own JSONL file, so the parent's transcript stays
//! its own. Every event a child produces is appended to
//! `<dir>/<agent_id>.jsonl`, which is the file the Trace pane tails.
//!
//! Cancellation is checked at a tool boundary, never mid-tool: a tool that has
//! started is allowed to finish and report, and the *next* boundary refuses.
//! That is the only place a cancellation is cheap and safe.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::result::AgentResult;
use crate::runtime::{Run, RunRegistry};

/// How deep a delegation chain may go before a spawn is refused.
///
/// A cap exists because "spawn another agent" is the one operation whose cost
/// is unbounded in a way the budget ledger cannot see in advance: each child
/// can spawn children. Three is enough for scout → worker → checker.
pub const MAX_DEPTH: usize = 3;

/// The depth a spawn gets when nobody says otherwise.
///
/// One: a child is a leaf unless the caller raises the cap deliberately. The
/// previous default was the hard ceiling, which meant an agent could fan out
/// three levels deep without anyone asking for it.
pub const DEFAULT_DEPTH: usize = 1;

/// How many children may be live at the same time.
///
/// "Live" is a registry state, not a count of files: a child that finished,
/// failed or was cancelled frees its slot, so this bounds concurrency rather
/// than history.
pub const MAX_LIVE_CHILDREN: usize = 16;

/// How many spawns one session may make in total, live or not.
///
/// Concurrency is not the only runaway: a parent that spawns, waits, and spawns
/// again never exceeds the live cap while still producing an unbounded tree.
pub const MAX_SPAWNS_PER_SESSION: usize = 64;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A permission or question a child raised, routed to the root's inbox.
///
/// A child may not block on a person inside its own surface: it queues one of
/// these and the root renders it. `Enter` on the row opens the child at the
/// prompt that answers it. `agent` and `root_id` are denormalised so the inbox
/// renders without walking the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionRequest {
    /// Stable id (`attn-1`, …), unique within the delegator.
    pub id: String,
    /// The child that raised it.
    pub run_id: String,
    /// The root of the tree the child belongs to.
    pub root_id: String,
    /// The agent name the child runs as.
    pub agent: String,
    /// What kind of answer is wanted (`permission`, `question`, …).
    pub kind: String,
    /// One-line summary of what is being asked.
    pub summary: String,
    /// When the request was queued, epoch milliseconds.
    pub at_ms: i64,
}

/// One node in the spawn tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildRun {
    /// The registry run id this child owns.
    pub agent_id: String,
    /// The run that spawned it; `None` at the root.
    pub parent_id: Option<String>,
    /// The run at the top of this tree: the same id for every descendant, so a
    /// deck can group by root without walking `parent_id` to the top.
    pub root_id: String,
    /// The parent session that asked for the tree, when one did.
    pub session_id: Option<String>,
    /// 1 for the first spawn of the session, 2 for the next, and so on. Order
    /// of arrival, which `created_ms` ties cannot express.
    pub spawn_seq: u64,
    /// 0 for a run the person started, 1 for its child, and so on.
    pub depth: usize,
    /// The agent spec name (`scout`, `worker`, …).
    pub agent: String,
    /// What it was asked to do, as typed.
    pub task: String,
    /// The model the child runs on, once an executor picks it up.
    pub model: Option<String>,
    /// The worktree it writes in, when it was given one.
    pub worktree: Option<String>,
    /// Its own JSONL, one event per line.
    pub log: PathBuf,
    /// When the node was created.
    pub created_ms: i64,
}

/// One line in a child's JSONL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildEvent {
    /// `run.start`, `tool.use`, `run.done`, `run.failed`, `run.cancelled`.
    pub event: String,
    /// The child this line belongs to.
    pub agent_id: String,
    /// Milliseconds since the epoch.
    pub at_ms: i64,
    /// Event detail: the tool name, the result summary, the reason.
    pub detail: String,
}

/// The spawn tree over a run registry.
#[derive(Debug)]
pub struct Delegator {
    dir: PathBuf,
    /// Depth cap: how many edges from the root a spawn may sit at.
    cap: usize,
    /// How many children may be live at once.
    live_cap: usize,
    /// How many spawns this session may make in total.
    spawn_cap: usize,
    registry: RunRegistry,
    nodes: BTreeMap<String, ChildRun>,
    /// How many spawns this delegator has made, which is the `spawn_seq` of the
    /// next one.
    spawn_seq: u64,
    /// Permission/question requests children raised, awaiting the root.
    attention: Vec<AttentionRequest>,
    /// Monotonic counter behind the `attn-N` ids.
    attention_seq: u64,
}

impl Delegator {
    /// A delegator whose child logs live in `dir` and whose cap is
    /// [`MAX_DEPTH`].
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self::with_caps(
            dir,
            DEFAULT_DEPTH,
            MAX_LIVE_CHILDREN,
            MAX_SPAWNS_PER_SESSION,
        )
    }

    /// A delegator with an explicit cap, for tests and for a project that
    /// wants a shallower tree.
    pub fn with_cap(dir: impl Into<PathBuf>, cap: usize) -> Self {
        Self::with_caps(dir, cap, MAX_LIVE_CHILDREN, MAX_SPAWNS_PER_SESSION)
    }

    /// A delegator with every limit explicit, for tests and for a project that
    /// wants a smaller tree than the defaults allow.
    pub fn with_caps(
        dir: impl Into<PathBuf>,
        cap: usize,
        live_cap: usize,
        spawn_cap: usize,
    ) -> Self {
        Self {
            dir: dir.into(),
            cap,
            live_cap,
            spawn_cap,
            registry: RunRegistry::new(),
            nodes: BTreeMap::new(),
            spawn_seq: 0,
            attention: Vec::new(),
            attention_seq: 0,
        }
    }

    /// The tree root for a session: depth 0, no parent.
    pub fn root(&mut self, session_id: Option<&str>, agent: &str, task: &str) -> Result<ChildRun> {
        self.spawn(None, session_id, agent, task)
    }

    /// Spawn a child of `parent`, refusing anything past the cap.
    ///
    /// The refusal names the depth and the cap, because "too deep" alone leaves
    /// the caller guessing whether to raise the cap or restructure the task.
    pub fn spawn(
        &mut self,
        parent: Option<&str>,
        session_id: Option<&str>,
        agent: &str,
        task: &str,
    ) -> Result<ChildRun> {
        let (parent_id, depth) = match parent {
            Some(id) => {
                let node = self
                    .nodes
                    .get(id)
                    .with_context(|| format!("no such parent run {id}"))?
                    .clone();
                if self.is_cancelled(id) {
                    bail!("parent run {id} is cancelled; refusing to spawn {agent}");
                }
                (Some(id.to_string()), node.depth + 1)
            }
            None => (None, 0),
        };

        if depth > self.cap {
            bail!(
                "refusing to spawn {agent} at depth {depth}: the cap is {} (set MAX_DEPTH or flatten the task)",
                self.cap
            );
        }

        // Two more limits, and each names itself: a refusal that says "limit
        // reached" leaves the caller guessing which one to raise or wait for.
        if self.nodes.len() >= self.spawn_cap {
            bail!(
                "refusing to spawn {agent}: this session has made {} spawns, the cap is {}",
                self.nodes.len(),
                self.spawn_cap
            );
        }
        let live = self.live_children();
        if live >= self.live_cap {
            bail!(
                "refusing to spawn {agent}: {live} children are live, the cap is {} (cancel one or wait for it to finish)",
                self.live_cap
            );
        }

        let run_id = uuid::Uuid::new_v4().to_string();
        let purpose = if parent_id.is_some() {
            format!("delegate: {task}")
        } else {
            format!("run: {task}")
        };
        self.registry.register(Run {
            id: run_id.clone(),
            session_id: session_id.map(str::to_string),
            agent: agent.to_string(),
            model: None,
            purpose,
            created_ms: now_ms(),
        })?;

        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("cannot create child log dir {}", self.dir.display()))?;
        let log = self.dir.join(format!("{run_id}.jsonl"));

        self.spawn_seq += 1;
        let spawn_seq = self.spawn_seq;
        // Every descendant carries the root's id, so grouping by root never
        // walks the tree. The root is its own root.
        let root_id = match parent_id.as_deref() {
            Some(id) => self
                .nodes
                .get(id)
                .map(|parent| parent.root_id.clone())
                .unwrap_or_else(|| run_id.clone()),
            None => run_id.clone(),
        };
        let node = ChildRun {
            agent_id: run_id.clone(),
            parent_id,
            root_id,
            session_id: session_id.map(str::to_string),
            spawn_seq,
            depth,
            agent: agent.to_string(),
            task: task.to_string(),
            model: None,
            worktree: None,
            log,
            created_ms: now_ms(),
        };
        self.nodes.insert(run_id.clone(), node.clone());
        self.log(&run_id, "run.start", task)?;
        // Spawning registers the run; it does not start it. A child with no
        // executor must not claim to be running: `begin` is what an executor
        // calls when it actually picks the run up.
        Ok(node)
    }

    /// The node for a run id.
    pub fn node(&self, id: &str) -> Option<&ChildRun> {
        self.nodes.get(id)
    }

    /// The run registry, for the status column in a Deck.
    pub fn registry(&self) -> &RunRegistry {
        &self.registry
    }

    /// Record the model a child is running as, after the fact.
    ///
    /// The spawn records `None` because the model is chosen by whoever builds
    /// the run, not by the tree. Once it is known — the TUI sets it when the
    /// child starts — the Deck and the card show it in the same place.
    pub fn set_model(&mut self, id: &str, model: Option<String>) -> Result<()> {
        self.node_mut(id)?.model = model;
        Ok(())
    }

    /// Record the worktree a child is working in, after the fact.
    ///
    /// A child that edits files owns a worktree (`memnoth-worktree`); the
    /// spawn names no path because the worktree is created by the caller.
    /// Recording it here is what lets `/merge` and the patch card find it.
    pub fn set_worktree(&mut self, id: &str, worktree: String) -> Result<()> {
        self.node_mut(id)?.worktree = Some(worktree);
        Ok(())
    }

    /// Steer a child: a note from the person, recorded in its log.
    ///
    /// The Deck's `s` key calls this; the note lands in the child's JSONL so
    /// the trace shows when the direction changed and the child reads it on
    /// its next step. An unknown child is refused rather than noted nowhere.
    pub fn steer(&mut self, id: &str, note: &str) -> Result<()> {
        if !self.nodes.contains_key(id) {
            bail!("no such child {id}: cannot steer what does not exist");
        }
        self.log(id, "steer", note)
    }

    /// Stop one child without touching its parent or siblings.
    ///
    /// `cancel` retires the whole subtree, which is right for teardown and
    /// wrong for focus: stopping one child in the Deck must not kill its
    /// parent or the children beside it. Returns `false` when the child was
    /// already in a terminal state, so the Deck can say so instead of lying.
    pub fn stop_one(&mut self, id: &str) -> Result<bool> {
        if !self.nodes.contains_key(id) {
            bail!("no such child {id}: cannot stop what does not exist");
        }
        match self.registry.cancel(id) {
            Ok(()) => {
                self.log(
                    id,
                    "run.stop",
                    "stopped from the deck; parent and siblings untouched",
                )?;
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }

    /// Revive a stopped child: it becomes pending again, log intact.
    ///
    /// The Deck's `r` key calls this. Resuming reuses the run — the same id,
    /// the same JSONL, the same worktree — rather than spawning a fresh
    /// child, because the whole point is to continue the work that stopped.
    pub fn revive(&mut self, id: &str) -> Result<()> {
        if !self.nodes.contains_key(id) {
            bail!("no such child {id}: cannot revive what does not exist");
        }
        self.registry.revive(id)?;
        self.log(
            id,
            "run.revive",
            "revived from the deck; log and history kept",
        )
    }

    /// Queue a permission/question request from a child for the root inbox.
    ///
    /// Returns the request as queued; the root drains it with
    /// [`Self::take_attention`] once the person answers.
    pub fn queue_attention(
        &mut self,
        run_id: &str,
        kind: &str,
        summary: &str,
    ) -> Result<AttentionRequest> {
        let node = self
            .nodes
            .get(run_id)
            .with_context(|| {
                format!("no such child {run_id}: cannot queue attention for what does not exist")
            })?
            .clone();
        self.attention_seq += 1;
        let request = AttentionRequest {
            id: format!("attn-{}", self.attention_seq),
            run_id: run_id.to_string(),
            root_id: node.root_id.clone(),
            agent: node.agent.clone(),
            kind: kind.to_string(),
            summary: summary.to_string(),
            at_ms: now_ms(),
        };
        self.log(run_id, "attention.queued", &request.summary)?;
        self.attention.push(request.clone());
        Ok(request)
    }

    /// The requests waiting for the root, oldest first.
    pub fn attention(&self) -> &[AttentionRequest] {
        &self.attention
    }

    /// Take one request out of the inbox, by id. `None` when there is no such
    /// request — answering an already-answered row is not an error, it is
    /// the row the person just closed.
    pub fn take_attention(&mut self, id: &str) -> Option<AttentionRequest> {
        let index = self.attention.iter().position(|r| r.id == id)?;
        Some(self.attention.remove(index))
    }

    fn node_mut(&mut self, id: &str) -> Result<&mut ChildRun> {
        self.nodes
            .get_mut(id)
            .with_context(|| format!("no such child {id}"))
    }

    /// Every node in the tree, shallowest first. This is the roster a Deck
    /// draws; `path_to_root` is the active path a Trace highlights.
    pub fn nodes(&self) -> Vec<&ChildRun> {
        let mut out: Vec<&ChildRun> = self.nodes.values().collect();
        out.sort_by(|a, b| a.depth.cmp(&b.depth).then(a.created_ms.cmp(&b.created_ms)));
        out
    }

    /// Mark a child as running: the executor has picked it up.
    pub fn begin(&mut self, id: &str) -> Result<()> {
        self.registry.start(id)
    }

    /// Direct children of `id`, oldest first.
    /// How many children are still live.
    ///
    /// A run frees its slot when the registry says it is terminal, so this is a
    /// concurrency cap and not a history: a session may spawn a thousand
    /// children as long as no more than [`MAX_LIVE_CHILDREN`] are running.
    pub fn live_children(&self) -> usize {
        self.nodes
            .values()
            .filter(|node| {
                node.parent_id.is_some()
                    && self.registry.get(&node.agent_id).is_some_and(|entry| {
                        entry.completed_ms.is_none()
                            && !matches!(
                                entry.status,
                                crate::runtime::RunStatus::Cancelled
                                    | crate::runtime::RunStatus::Failed
                            )
                    })
            })
            .count()
    }

    /// The direct children of `id`: one edge down, not the whole subtree.
    pub fn children(&self, id: &str) -> Vec<&ChildRun> {
        let mut out: Vec<&ChildRun> = self
            .nodes
            .values()
            .filter(|node| node.parent_id.as_deref() == Some(id))
            .collect();
        out.sort_by_key(|node| node.created_ms);
        out
    }

    /// The chain from the root to `id`, root first. This is the Trace's active
    /// path: a child is only legible next to the chain that asked for it.
    pub fn path_to_root(&self, id: &str) -> Vec<&ChildRun> {
        let mut chain = Vec::new();
        let mut cursor = self.nodes.get(id);
        while let Some(node) = cursor {
            chain.push(node);
            cursor = node.parent_id.as_deref().and_then(|p| self.nodes.get(p));
        }
        chain.reverse();
        chain
    }

    /// Every descendant of `id`, breadth first.
    pub fn descendants(&self, id: &str) -> Vec<&ChildRun> {
        let mut out = Vec::new();
        let mut frontier = vec![id.to_string()];
        while let Some(current) = frontier.pop() {
            for child in self.children(&current) {
                out.push(child);
                frontier.push(child.agent_id.clone());
            }
        }
        out
    }

    /// Whether `id` — or anything above it — has been cancelled.
    ///
    /// Cancelling a parent must stop work below it without hunting for each
    /// child, so the question is asked of the chain, not of the node.
    pub fn is_cancelled(&self, id: &str) -> bool {
        let mut cursor = self.nodes.get(id);
        while let Some(node) = cursor {
            if self
                .registry
                .get(&node.agent_id)
                .is_some_and(|entry| entry.status == crate::runtime::RunStatus::Cancelled)
            {
                return true;
            }
            cursor = node
                .parent_id
                .as_deref()
                .and_then(|parent| self.nodes.get(parent));
        }
        false
    }

    /// A tool boundary: record the tool, or refuse if the chain is cancelled.
    ///
    /// This is the cancellation point. A tool that started before the cancel is
    /// allowed to finish — its result is already half-applied — and the next
    /// boundary is where the child stops.
    pub fn tool_boundary(&mut self, id: &str, tool: &str) -> Result<()> {
        if !self.nodes.contains_key(id) {
            bail!("no such run {id}");
        }
        if self.is_cancelled(id) {
            self.log(id, "run.cancelled", "cancelled before tool boundary")?;
            bail!("run {id} is cancelled; refusing {tool}");
        }
        self.log(id, "tool.use", tool)
    }

    /// Finish a child with the result it returned to its parent.
    pub fn finish(&mut self, id: &str, result: &AgentResult) -> Result<()> {
        let summary = serde_json::to_string(result).unwrap_or_else(|_| "{}".to_string());
        self.log(id, "run.done", &summary)?;
        self.registry.complete(id)
    }

    /// Fail a child.
    pub fn fail(&mut self, id: &str, reason: &str) -> Result<()> {
        self.log(id, "run.failed", reason)?;
        self.registry.fail(id, reason)
    }

    /// Cancel a child and everything below it, returning how many stops.
    ///
    /// Children already terminal are left alone: rewriting a done run as
    /// cancelled would erase the fact that it finished.
    pub fn cancel(&mut self, id: &str) -> Result<usize> {
        let targets: Vec<String> = std::iter::once(id.to_string())
            .chain(self.descendants(id).into_iter().map(|n| n.agent_id.clone()))
            .collect();
        let mut stopped = 0;
        for target in targets {
            let terminal = self
                .registry
                .get(&target)
                .is_some_and(|entry| entry.completed_ms.is_some());
            if terminal {
                continue;
            }
            // Log first: a cancelled run whose log says nothing is a mystery.
            let _ = self.log(&target, "run.cancelled", "cancelled by parent");
            self.registry.cancel(&target)?;
            stopped += 1;
        }
        Ok(stopped)
    }

    /// One child's log, parsed line by line. A malformed line is skipped rather
    /// than failing the pane: a half-written trailing line is expected while a
    /// child is running.
    pub fn read_log(&self, id: &str) -> Result<Vec<ChildEvent>> {
        let node = self
            .nodes
            .get(id)
            .with_context(|| format!("no such run {id}"))?;
        let text = match std::fs::read_to_string(&node.log) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("cannot read child log"),
        };
        Ok(text
            .lines()
            .filter_map(|line| serde_json::from_str::<ChildEvent>(line).ok())
            .collect())
    }

    fn log(&self, id: &str, event: &str, detail: &str) -> Result<()> {
        let node = self
            .nodes
            .get(id)
            .with_context(|| format!("no such run {id}"))?;
        let line = serde_json::to_string(&ChildEvent {
            event: event.to_string(),
            agent_id: id.to_string(),
            at_ms: now_ms(),
            detail: detail.to_string(),
        })?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&node.log)
            .with_context(|| format!("cannot open child log {}", node.log.display()))?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// The directory child logs live in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{Cost, Exit};

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    fn result_ok() -> AgentResult {
        AgentResult {
            summary: "done".to_string(),
            findings: Vec::new(),
            changed_files: Vec::new(),
            commands_run: Vec::new(),
            receipts: Vec::new(),
            proposed_beliefs: Vec::new(),
            cost: Cost {
                tokens: 0,
                usd: 0.0,
            },
            turns: 1,
            exit: Exit::Completed,
        }
    }

    #[test]
    fn a_child_knows_its_parent_and_depth() {
        let dir = temp();
        let mut delegator = Delegator::new(dir.path());
        let root = delegator
            .root(Some("sess-1"), "memnoth", "fix the login")
            .unwrap();
        let child = delegator
            .spawn(
                Some(&root.agent_id),
                Some("sess-1"),
                "scout",
                "find the redirect",
            )
            .unwrap();
        assert_eq!(child.depth, 1);
        assert_eq!(child.parent_id.as_deref(), Some(root.agent_id.as_str()));
        assert_eq!(delegator.children(&root.agent_id).len(), 1);
        assert_eq!(delegator.path_to_root(&child.agent_id).len(), 2);
        assert_eq!(delegator.registry().len(), 2);
    }

    #[test]
    fn the_cap_refuses_a_spawn_and_names_the_reason() {
        let dir = temp();
        let mut delegator = Delegator::with_cap(dir.path(), 1);
        let root = delegator.root(None, "memnoth", "root").unwrap();
        let child = delegator
            .spawn(Some(&root.agent_id), None, "worker", "work")
            .unwrap();
        let error = delegator
            .spawn(Some(&child.agent_id), None, "checker", "check")
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("depth 2"), "{text}");
        assert!(text.contains("cap is 1"), "{text}");
    }

    #[test]
    fn every_child_writes_its_own_jsonl() {
        let dir = temp();
        let mut delegator = Delegator::new(dir.path());
        let child = delegator.root(Some("s1"), "scout", "look around").unwrap();
        delegator
            .tool_boundary(&child.agent_id, "read_file")
            .unwrap();
        delegator.finish(&child.agent_id, &result_ok()).unwrap();

        let events = delegator.read_log(&child.agent_id).unwrap();
        let names: Vec<&str> = events.iter().map(|e| e.event.as_str()).collect();
        assert_eq!(names, vec!["run.start", "tool.use", "run.done"]);
        assert_eq!(events[1].detail, "read_file");
        assert!(child.log.is_file(), "{}", child.log.display());
    }

    #[test]
    fn cancelling_a_parent_stops_every_descendant() {
        let dir = temp();
        let mut delegator = Delegator::with_cap(dir.path(), MAX_DEPTH);
        let root = delegator.root(None, "memnoth", "root").unwrap();
        let child = delegator
            .spawn(Some(&root.agent_id), None, "worker", "work")
            .unwrap();
        let grandchild = delegator
            .spawn(Some(&child.agent_id), None, "checker", "check")
            .unwrap();

        let stopped = delegator.cancel(&root.agent_id).unwrap();
        assert_eq!(stopped, 3, "root, child and grandchild");
        assert!(delegator.is_cancelled(&grandchild.agent_id));
        let error = delegator
            .tool_boundary(&grandchild.agent_id, "write_file")
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
    }

    #[test]
    fn a_finished_child_is_not_rewritten_as_cancelled() {
        let dir = temp();
        let mut delegator = Delegator::new(dir.path());
        let root = delegator.root(None, "memnoth", "root").unwrap();
        let done = delegator
            .spawn(Some(&root.agent_id), None, "scout", "done")
            .unwrap();
        delegator.finish(&done.agent_id, &result_ok()).unwrap();

        let stopped = delegator.cancel(&root.agent_id).unwrap();
        assert_eq!(stopped, 1, "only the root was still running");
        assert_eq!(
            delegator.registry().get(&done.agent_id).unwrap().status,
            crate::runtime::RunStatus::Done
        );
    }

    #[test]
    fn a_cancelled_parent_refuses_a_new_child() {
        let dir = temp();
        let mut delegator = Delegator::new(dir.path());
        let root = delegator.root(None, "memnoth", "root").unwrap();
        delegator.cancel(&root.agent_id).unwrap();
        let error = delegator
            .spawn(Some(&root.agent_id), None, "scout", "late")
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
    }

    #[test]
    fn the_log_survives_a_restart() {
        let dir = temp();
        let (id, path) = {
            let mut delegator = Delegator::new(dir.path());
            let child = delegator.root(None, "scout", "look").unwrap();
            delegator.tool_boundary(&child.agent_id, "grep").unwrap();
            (child.agent_id, child.log)
        };
        assert!(path.is_file());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(&id), "the child's own id is in its log");
        assert_eq!(text.lines().count(), 2);
    }
}

#[cfg(test)]
mod caps {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memnoth-delegate-caps-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_child_is_a_leaf_by_default() {
        // W8-06's "depth 1 default": an agent cannot quietly fan out three
        // levels deep because that used to be the ceiling.
        let mut tree = Delegator::new(scratch("default-depth"));
        let root = tree.root(None, "scout", "map the repo").unwrap();
        let child = tree
            .spawn(Some(&root.agent_id), None, "worker", "edit one file")
            .unwrap();
        assert_eq!(child.depth, 1);

        let refusal = tree
            .spawn(Some(&child.agent_id), None, "checker", "review it")
            .expect_err("a grandchild is past the default depth");
        let message = refusal.to_string();
        assert!(message.contains("depth 2"), "{message}");
        assert!(message.contains("cap is 1"), "{message}");
    }

    #[test]
    fn raising_the_depth_cap_is_explicit() {
        let mut tree = Delegator::with_cap(scratch("raised"), 2);
        let root = tree.root(None, "scout", "map").unwrap();
        let child = tree
            .spawn(Some(&root.agent_id), None, "worker", "edit")
            .unwrap();
        let grandchild = tree.spawn(Some(&child.agent_id), None, "checker", "review");
        assert!(grandchild.is_ok(), "a cap of 2 must allow depth 2");
    }

    #[test]
    fn sixteen_live_children_is_the_concurrency_limit() {
        let mut tree = Delegator::with_caps(scratch("live"), 1, 2, 64);
        let root = tree.root(None, "scout", "map").unwrap();
        let _first = tree
            .spawn(Some(&root.agent_id), None, "worker", "one")
            .unwrap();
        let _second = tree
            .spawn(Some(&root.agent_id), None, "worker", "two")
            .unwrap();
        assert_eq!(tree.live_children(), 2, "two spawned children are live");

        let refusal = tree
            .spawn(Some(&root.agent_id), None, "worker", "three")
            .expect_err("the live cap is two children");
        let message = refusal.to_string();
        assert!(message.contains("live"), "{message}");
        assert!(message.contains("cap is 2"), "{message}");
    }

    #[test]
    fn cancelling_a_child_frees_its_slot() {
        let mut tree = Delegator::with_caps(scratch("freed"), 1, 1, 64);
        let root = tree.root(None, "scout", "map").unwrap();
        let first = tree
            .spawn(Some(&root.agent_id), None, "worker", "one")
            .unwrap();
        assert!(
            tree.spawn(Some(&root.agent_id), None, "worker", "two")
                .is_err(),
            "one live child is the cap here"
        );
        tree.cancel(&first.agent_id).unwrap();
        assert_eq!(tree.live_children(), 0, "a cancelled child is not live");
        assert!(
            tree.spawn(Some(&root.agent_id), None, "worker", "three")
                .is_ok(),
            "a cancelled child must not hold its slot"
        );
    }

    #[test]
    fn sixty_four_spawns_is_the_session_limit() {
        let mut tree = Delegator::with_caps(scratch("spawns"), 1, 16, 3);
        let root = tree.root(None, "scout", "map").unwrap();
        let _a = tree
            .spawn(Some(&root.agent_id), None, "worker", "a")
            .unwrap();
        let _b = tree
            .spawn(Some(&root.agent_id), None, "worker", "b")
            .unwrap();

        let refusal = tree
            .spawn(Some(&root.agent_id), None, "worker", "c")
            .expect_err("three nodes is the session cap here");
        let message = refusal.to_string();
        assert!(message.contains("spawns"), "{message}");
        assert!(message.contains("cap is 3"), "{message}");
    }
}

#[cfg(test)]
mod session_model_tests {
    use super::*;

    #[test]
    fn a_descendant_carries_the_root_and_its_own_spawn_order() {
        let dir = tempfile::tempdir().expect("dir");
        let mut delegator = Delegator::with_cap(dir.path(), 3);
        let root = delegator
            .root(Some("sess-1"), "lead", "ship it")
            .expect("root");
        let child = delegator
            .spawn(Some(&root.agent_id), Some("sess-1"), "worker", "work")
            .expect("child");
        let grand = delegator
            .spawn(Some(&child.agent_id), Some("sess-1"), "checker", "check")
            .expect("grandchild");
        assert_eq!(root.root_id, root.agent_id);
        assert_eq!(child.root_id, root.agent_id);
        assert_eq!(
            grand.root_id, root.agent_id,
            "every descendant names the root without walking parent_id"
        );
        assert_eq!(
            (root.spawn_seq, child.spawn_seq, grand.spawn_seq),
            (1, 2, 3)
        );
        assert_eq!(grand.session_id.as_deref(), Some("sess-1"));
        assert_eq!(grand.depth, 2);
        assert!(
            grand.model.is_none() && grand.worktree.is_none(),
            "an executor fills those, not the delegator"
        );
    }

    #[test]
    fn model_and_worktree_are_recorded_after_the_fact() {
        let dir = tempfile::tempdir().expect("dir");
        let mut delegator = Delegator::new(dir.path());
        let root = delegator
            .root(Some("sess-1"), "lead", "ship it")
            .expect("root");
        let wt = dir.path().join("wt-lead").to_string_lossy().to_string();
        delegator
            .set_model(&root.agent_id, Some("model-1".to_string()))
            .expect("model");
        delegator
            .set_worktree(&root.agent_id, wt.clone())
            .expect("worktree");
        let node = delegator.node(&root.agent_id).expect("node");
        assert_eq!(node.model.as_deref(), Some("model-1"));
        assert_eq!(node.worktree.as_ref(), Some(&wt));
        assert!(delegator.set_model("nope", None).is_err());
        assert!(delegator.set_worktree("nope", wt).is_err());
    }

    #[test]
    fn stop_one_kills_only_that_child() {
        let dir = tempfile::tempdir().expect("dir");
        let mut delegator = Delegator::new(dir.path());
        let root = delegator.root(None, "lead", "root").expect("root");
        let a = delegator
            .spawn(Some(&root.agent_id), None, "worker", "a")
            .expect("a");
        let b = delegator
            .spawn(Some(&root.agent_id), None, "worker", "b")
            .expect("b");
        assert!(delegator.stop_one(&a.agent_id).expect("stop"));
        // Parent and sibling are untouched: the focus case stops one child.
        assert_eq!(
            delegator.registry().get(&root.agent_id).unwrap().status,
            crate::runtime::RunStatus::Pending
        );
        assert_eq!(
            delegator.registry().get(&b.agent_id).unwrap().status,
            crate::runtime::RunStatus::Pending
        );
        assert_eq!(
            delegator.registry().get(&a.agent_id).unwrap().status,
            crate::runtime::RunStatus::Cancelled
        );
        // A second stop reports already-stopped instead of lying.
        assert!(!delegator.stop_one(&a.agent_id).expect("stop again"));
        assert!(delegator.stop_one("nope").is_err());
    }

    #[test]
    fn revive_returns_a_stopped_child_to_pending_in_place() {
        let dir = tempfile::tempdir().expect("dir");
        let mut delegator = Delegator::new(dir.path());
        let root = delegator.root(None, "lead", "root").expect("root");
        assert!(delegator.stop_one(&root.agent_id).expect("stop"));
        delegator.revive(&root.agent_id).expect("revive");
        assert_eq!(
            delegator.registry().get(&root.agent_id).unwrap().status,
            crate::runtime::RunStatus::Pending
        );
        // A done run cannot be revived: that would rewrite that it finished.
        let mut delegator = Delegator::new(dir.path());
        let root = delegator.root(None, "lead", "root").expect("root");
        delegator.registry.complete(&root.agent_id).unwrap();
        assert!(delegator.revive(&root.agent_id).is_err());
    }

    #[test]
    fn steer_is_logged_and_unknown_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let mut delegator = Delegator::new(dir.path());
        let root = delegator.root(None, "lead", "root").expect("root");
        delegator
            .steer(&root.agent_id, "focus on the parser")
            .expect("steer");
        let events = delegator.read_log(&root.agent_id).expect("log");
        assert!(events.iter().any(|e| e.event == "steer"));
        assert!(delegator.steer("nope", "x").is_err());
    }

    #[test]
    fn attention_requests_route_to_the_root_inbox() {
        let dir = tempfile::tempdir().expect("dir");
        let mut delegator = Delegator::new(dir.path());
        let root = delegator.root(None, "lead", "root").expect("root");
        let child = delegator
            .spawn(Some(&root.agent_id), None, "worker", "work")
            .expect("child");
        let request = delegator
            .queue_attention(&child.agent_id, "permission", "run git write")
            .expect("queue");
        assert_eq!(request.id, "attn-1");
        assert_eq!(request.root_id, root.agent_id);
        assert_eq!(request.agent, "worker");
        assert_eq!(delegator.attention().len(), 1);
        // The child log carries the request too, so the trace shows who asked.
        let events = delegator.read_log(&child.agent_id).expect("log");
        assert!(events.iter().any(|e| e.event == "attention.queued"));
        let taken = delegator.take_attention(&request.id).expect("take");
        assert_eq!(taken.run_id, child.agent_id);
        assert!(delegator.attention().is_empty());
        assert!(delegator.take_attention("attn-99").is_none());
        assert!(
            delegator
                .queue_attention("nope", "permission", "x")
                .is_err()
        );
    }
}
