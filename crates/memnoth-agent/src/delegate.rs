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

pub mod event_replay;
pub mod event_schema;

use event_schema::{
    ATTENTION_QUEUED, EVENT_SCHEMA_VERSION, EventState, RUN_CANCELLED, RUN_DONE, RUN_FAILED,
    RUN_REVIVE, RUN_SPAWN, RUN_START, RUN_STOP, SOURCE_DELEGATOR, STEER, TOOL_USE,
};

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::error::{AgentError, ErrorCode};
use crate::result::AgentResult;
use crate::runtime::{Run, RunRegistry};
use crate::spec::SpawnScope;

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

/// A cooperative cancellation token, checked at tool boundaries only.
///
/// This is the "never kill a thread" rule (F-06): cancelling sets a flag, and
/// the child observes it on its next boundary. A tool already in flight is
/// allowed to finish and report; the next boundary refuses. Cloning a token
/// clones the handle to the same flag, so a parent can cancel a child from a
/// `/kill` while the executor holds the other handle.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// A token that has not been cancelled.
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation. Idempotent; every clone sees it immediately.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

static ATTENTION_REVISION: AtomicU64 = AtomicU64::new(1);

fn next_attention_revision() -> u64 {
    ATTENTION_REVISION.fetch_add(1, Ordering::Relaxed)
}

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
///
/// The first four fields are the original wire format and stay mandatory so
/// old files load unchanged. Everything after `detail` is additive (schema
/// v1) and carries a serde default: an old line parses with `seq == 0`, an
/// empty `id`, no ids, no source, `EventState::Unknown` and `version == 0`.
/// A missing `state` or an unknown future state degrades to `Unknown` rather
/// than failing the line, so a reader never mistakes a new event for a
/// terminal state just because it does not recognise the kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildEvent {
    /// Event kind: `run.spawn`, `run.start`, `tool.use`, `run.done`,
    /// `run.failed`, `run.cancelled`, `run.stop`, `run.revive`, `steer`, or
    /// `attention.queued`. Old logs also contain only these original kinds.
    pub event: String,
    /// The child this line belongs to (legacy field, equal to `run_id`).
    pub agent_id: String,
    /// Milliseconds since the epoch.
    pub at_ms: i64,
    /// Event detail: the tool name, the result summary, the reason.
    ///
    /// For `tool.use` this is only the tool name observed at a boundary. It
    /// is never a tool result and never reasoning text; neither is stored in
    /// this log.
    pub detail: String,

    /// Stable event id, `<run-id>:<seq>`. Empty on legacy lines.
    #[serde(default)]
    pub id: String,
    /// Monotonic, per-run sequence starting at 1. Zero on legacy lines.
    #[serde(default)]
    pub seq: u64,
    /// Run this event belongs to, denormalised with `agent_id` for new lines.
    #[serde(default)]
    pub run_id: String,
    /// Session that owns the run, when one was recorded.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Turn within the run, when a caller supplies one.
    #[serde(default)]
    pub turn_id: Option<String>,
    /// Parent run id, when this child was delegated.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Producer label (`delegator` for rows written here). Empty on old lines.
    #[serde(default)]
    pub source: String,
    /// Typed lifecycle state, projected from the kind.
    #[serde(default, deserialize_with = "event_schema::deserialize_state")]
    pub state: EventState,
    /// Schema version; 0 for legacy lines, 1 for the current schema.
    #[serde(default)]
    pub version: u32,
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
    /// Per-run counters behind each event's `seq` and `<run-id>:<seq>` id.
    /// Keyed by run id so two children both start at 1 in their own logs.
    event_seq: BTreeMap<String, u64>,
    /// O(1) change token for attention inbox readers. Globally advanced so a
    /// replacement Delegator cannot reuse another instance's cache key.
    attention_revision: u64,
    /// Per-run cooperative cancellation handles, armed by the parent.
    cancel_tokens: BTreeMap<String, CancelToken>,
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
            event_seq: BTreeMap::new(),
            attention_revision: next_attention_revision(),
            cancel_tokens: BTreeMap::new(),
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
        // Spawning is not execution, so it writes no `run.start`. The `node`
        // above *is* the discoverable spawn record: it carries the parent,
        // root, session, task, spawn order and timestamp a parent needs to
        // reconstruct the tree. Only `begin`, which an executor calls when it
        // actually picks the run up, emits `run.start`. Callers that want an
        // explicit persisted spawn transition can call `record_spawn`, which
        // writes the additive `run.spawn` kind (schema v1) without claiming
        // the child has started.
        Ok(node)
    }

    /// Arm a cooperative cancellation token for `id`.
    ///
    /// The parent keeps the returned handle (or a clone) and calls
    /// [`CancelToken::cancel`] to stop the child. Nothing is killed: the next
    /// tool boundary observes the flag and refuses.
    pub fn arm_cancellation(&mut self, id: &str, token: CancelToken) -> Result<CancelToken> {
        if !self.nodes.contains_key(id) {
            bail!("no such run {id}: cannot arm cancellation");
        }
        self.cancel_tokens.insert(id.to_string(), token.clone());
        Ok(token)
    }

    /// The cancellation handle armed for `id`, when one was.
    pub fn cancellation_token(&self, id: &str) -> Option<CancelToken> {
        self.cancel_tokens.get(id).cloned()
    }

    /// Whether spawning `agent` under `parent` would repeat an agent already
    /// on the active chain — a run-time spawn cycle.
    ///
    /// Static cycles are rejected at spec load; this catches a cycle formed by
    /// the call graph, e.g. a `review` child asking for another `review`.
    pub fn would_cycle(&self, parent: &str, agent: &str) -> bool {
        self.path_to_root(parent)
            .iter()
            .any(|node| node.agent == agent)
    }

    /// Spawn only when the parent's `Agent(...)` whitelist names the child and
    /// the spawn would not cycle.
    ///
    /// This is the run-time half of Wave 5 §5.2: a spec's tool whitelist is
    /// enforced at load, and this keeps a caller from bypassing it by naming an
    /// agent directly at spawn time.
    pub fn spawn_allowed(
        &mut self,
        parent: &str,
        session_id: Option<&str>,
        agent: &str,
        task: &str,
        scope: &SpawnScope,
    ) -> Result<ChildRun> {
        if !scope.allows(agent) {
            return Err(AgentError::new(
                ErrorCode::Tools,
                format!("agent `{agent}` is not in the parent's spawn whitelist"),
            )
            .into());
        }
        if self.would_cycle(parent, agent) {
            return Err(AgentError::new(
                ErrorCode::Cycle,
                format!("spawn cycle: `{agent}` is already on the active chain"),
            )
            .into());
        }
        self.spawn(Some(parent), session_id, agent, task)
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
        self.log(id, STEER, note)
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
                    RUN_STOP,
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
            RUN_REVIVE,
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
        self.log(run_id, ATTENTION_QUEUED, &request.summary)?;
        self.attention.push(request.clone());
        self.attention_revision = next_attention_revision();
        Ok(request)
    }

    /// The requests waiting for the root, oldest first.
    pub fn attention(&self) -> &[AttentionRequest] {
        &self.attention
    }

    /// A cheap cache key that changes whenever the attention inbox changes.
    pub fn attention_revision(&self) -> u64 {
        self.attention_revision
    }

    /// Take one request out of the inbox, by id. `None` when there is no such
    /// request — answering an already-answered row is not an error, it is
    /// the row the person just closed.
    pub fn take_attention(&mut self, id: &str) -> Option<AttentionRequest> {
        let index = self.attention.iter().position(|r| r.id == id)?;
        let request = self.attention.remove(index);
        self.attention_revision = next_attention_revision();
        Some(request)
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

    /// Persist an explicit spawn transition for `id`, using the additive
    /// `run.spawn` kind (schema v1).
    ///
    /// `spawn()` does not call this automatically: a spawn record already
    /// exists as the [`ChildRun`] node, and the default log must still start
    /// with `run.start` once an executor calls [`Self::begin`]. A caller that
    /// maintains a parent-side roster may call this to get a durable,
    /// discoverable "child was created" row. `run.spawn` is deliberately not
    /// `run.start`: the first means a node exists, the second means code is
    /// executing.
    pub fn record_spawn(&mut self, id: &str) -> Result<()> {
        let agent = self
            .nodes
            .get(id)
            .with_context(|| format!("no such child {id}: cannot record a spawn"))?
            .agent
            .clone();
        self.log(id, RUN_SPAWN, &format!("spawned {agent}"))
    }

    /// Mark a child as running: the executor has picked it up.
    ///
    /// This is the only place `run.start` is emitted. Calling `begin` on a
    /// run that is already running is a no-op for the log, so an actually
    /// started child has exactly one `run.start` no matter how many times an
    /// executor reports the transition.
    pub fn begin(&mut self, id: &str) -> Result<()> {
        let already_running = self
            .registry
            .get(id)
            .is_some_and(|entry| entry.status == crate::runtime::RunStatus::Running);
        self.registry.start(id)?;
        if !already_running {
            let task = self
                .nodes
                .get(id)
                .with_context(|| format!("no such run {id}"))?
                .task
                .clone();
            self.log(id, RUN_START, &task)?;
        }
        Ok(())
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

    /// Check cooperative cancellation at a boundary, without claiming a tool
    /// ran.
    ///
    /// A tool that started before the cancel is allowed to finish; the next
    /// boundary is where the child stops. A fired token retires the whole
    /// subtree so descendants observe the cancel on their own next boundary.
    pub fn check_cancellation(&mut self, id: &str) -> Result<()> {
        if !self.nodes.contains_key(id) {
            bail!("no such run {id}");
        }
        let token_fired = self
            .cancel_tokens
            .get(id)
            .is_some_and(CancelToken::is_cancelled);
        if self.is_cancelled(id) || token_fired {
            if !self.is_cancelled(id) {
                let _ = self.registry.cancel(id);
            }
            self.log(id, RUN_CANCELLED, "cancelled before tool boundary")?;
            bail!("run {id} is cancelled; refusing the next tool");
        }
        Ok(())
    }

    /// A tool boundary: record the tool, or refuse if the chain is cancelled.
    ///
    /// Cancellation is checked before the `tool.use` row is written, so a
    /// cancelled or over-budget step never leaves a row claiming it used a
    /// tool it did not run.
    pub fn tool_boundary(&mut self, id: &str, tool: &str) -> Result<()> {
        self.check_cancellation(id)?;
        // `tool.use` is a typed fact about tool *use* at a boundary, with a
        // source label. It carries only the tool name in `detail`: never the
        // result, never reasoning text. Readers must not render it as either.
        self.log(id, TOOL_USE, tool)
    }

    /// Finish a child with the result it returned to its parent.
    pub fn finish(&mut self, id: &str, result: &AgentResult) -> Result<()> {
        let summary = serde_json::to_string(result).unwrap_or_else(|_| "{}".to_string());
        self.log(id, RUN_DONE, &summary)?;
        self.registry.complete(id)
    }

    /// Fail a child.
    pub fn fail(&mut self, id: &str, reason: &str) -> Result<()> {
        self.log(id, RUN_FAILED, reason)?;
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
            let _ = self.log(&target, RUN_CANCELLED, "cancelled by parent");
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

    fn log(&mut self, id: &str, event: &str, detail: &str) -> Result<()> {
        let (session_id, parent_id, log_path) = {
            let node = self
                .nodes
                .get(id)
                .with_context(|| format!("no such run {id}"))?;
            (
                node.session_id.clone(),
                node.parent_id.clone(),
                node.log.clone(),
            )
        };

        // Continue an existing log instead of reusing ids from a previous
        // Delegator instance. Legacy lines have `seq == 0`, so they never
        // collide with the v1 ids written here.
        let resume_from = if self.event_seq.contains_key(id) {
            0
        } else {
            Self::max_seq_in_log(&log_path)
        };
        let counter = self.event_seq.entry(id.to_string()).or_insert(resume_from);
        *counter += 1;
        let seq = *counter;

        let line = serde_json::to_string(&ChildEvent {
            event: event.to_string(),
            agent_id: id.to_string(),
            at_ms: now_ms(),
            detail: detail.to_string(),
            id: format!("{id}:{seq}"),
            seq,
            run_id: id.to_string(),
            session_id,
            turn_id: None,
            parent_id,
            source: SOURCE_DELEGATOR.to_string(),
            state: event_schema::state_for_kind(event),
            version: EVENT_SCHEMA_VERSION,
        })?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("cannot open child log {}", log_path.display()))?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Highest `seq` already present in `log_path`, or 0 for a new/unreadable
    /// file. Malformed trailing lines are ignored, as in [`Self::read_log`].
    fn max_seq_in_log(log_path: &Path) -> u64 {
        match std::fs::read_to_string(log_path) {
            Ok(text) => text
                .lines()
                .filter_map(|line| serde_json::from_str::<ChildEvent>(line).ok())
                .map(|event| event.seq)
                .max()
                .unwrap_or(0),
            Err(_) => 0,
        }
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
        // `run.start` is emitted when the child actually starts, not at spawn.
        delegator.begin(&child.agent_id).unwrap();
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
            delegator.begin(&child.agent_id).unwrap();
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

#[cfg(test)]
mod event_log_tests {
    use super::*;
    use crate::result::{Cost, Exit};
    use std::collections::BTreeSet;

    fn dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "memnoth-delegate-events-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        path
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
    fn event_ids_are_stable_and_unique() {
        let mut d = Delegator::new(dir("ids"));
        let root = d.root(Some("s1"), "scout", "look").unwrap();
        d.begin(&root.agent_id).unwrap();
        d.tool_boundary(&root.agent_id, "read_file").unwrap();
        d.tool_boundary(&root.agent_id, "grep").unwrap();
        d.finish(&root.agent_id, &result_ok()).unwrap();

        let first = d.read_log(&root.agent_id).unwrap();
        let ids: Vec<&str> = first.iter().map(|e| e.id.as_str()).collect();
        let expected: Vec<String> = (1..=4).map(|n| format!("{}:{n}", root.agent_id)).collect();
        let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
        assert_eq!(ids, expected, "id is <run-id>:<seq>");

        let mut seen = BTreeSet::new();
        for id in &ids {
            assert!(seen.insert(*id), "duplicate event id {id}");
        }
        // A second read returns byte-for-byte the same ids: they are stable,
        // not regenerated on read.
        let again = d.read_log(&root.agent_id).unwrap();
        assert_eq!(again.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ids);
    }

    #[test]
    fn seq_is_monotonic_per_run() {
        let mut d = Delegator::new(dir("seq"));
        let a = d.root(None, "scout", "a").unwrap();
        let b = d.spawn(Some(&a.agent_id), None, "worker", "b").unwrap();
        d.begin(&a.agent_id).unwrap();
        d.tool_boundary(&a.agent_id, "read_file").unwrap();
        d.begin(&b.agent_id).unwrap();
        d.tool_boundary(&b.agent_id, "grep").unwrap();
        d.tool_boundary(&b.agent_id, "write_file").unwrap();

        for id in [&a.agent_id, &b.agent_id] {
            let events = d.read_log(id).unwrap();
            assert!(!events.is_empty());
            assert_eq!(events[0].seq, 1, "each run starts its own seq at 1");
            assert!(
                events.windows(2).all(|w| w[0].seq + 1 == w[1].seq),
                "seq must be strictly monotonic with no gaps"
            );
            assert!(
                events.iter().all(|e| e.id == format!("{id}:{}", e.seq)),
                "id must always match run id and seq"
            );
        }
    }

    #[test]
    fn a_spawn_only_child_never_emits_run_start() {
        let mut d = Delegator::new(dir("spawn-only"));
        let root = d.root(Some("s1"), "scout", "look").unwrap();
        let events = d.read_log(&root.agent_id).unwrap();
        assert!(events.is_empty(), "spawn writes no lifecycle event");
        assert!(!events.iter().any(|e| e.event == RUN_START));
        assert_eq!(
            d.registry().get(&root.agent_id).unwrap().status,
            crate::runtime::RunStatus::Pending,
            "a spawned child is pending, not running"
        );
    }

    #[test]
    fn an_actually_started_child_emits_exactly_one_run_start() {
        let mut d = Delegator::new(dir("one-start"));
        let root = d.root(None, "scout", "look").unwrap();
        d.begin(&root.agent_id).unwrap();
        d.begin(&root.agent_id).unwrap(); // a repeated report is a no-op
        let events = d.read_log(&root.agent_id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, RUN_START);
        assert_eq!(events[0].state, EventState::Started);
        assert_eq!(events.iter().filter(|e| e.event == RUN_START).count(), 1);
    }

    #[test]
    fn a_spawn_record_is_distinct_from_a_start() {
        let mut d = Delegator::new(dir("spawn-vs-start"));
        let root = d.root(None, "scout", "look").unwrap();
        d.record_spawn(&root.agent_id).unwrap();
        d.begin(&root.agent_id).unwrap();
        let events = d.read_log(&root.agent_id).unwrap();
        assert_eq!(events[0].event, RUN_SPAWN);
        assert_eq!(events[0].state, EventState::Spawned);
        assert_eq!(events[0].version, EVENT_SCHEMA_VERSION);
        assert_eq!(events[1].event, RUN_START);
        assert_eq!(events[1].state, EventState::Started);
        assert_eq!(events.iter().filter(|e| e.event == RUN_START).count(), 1);
        assert!(d.record_spawn("nope").is_err());
    }

    #[test]
    fn tool_use_is_a_typed_fact_not_a_result_or_reasoning() {
        let mut d = Delegator::new(dir("tool-fact"));
        let root = d.root(None, "worker", "edit").unwrap();
        d.begin(&root.agent_id).unwrap();
        d.tool_boundary(&root.agent_id, "read_file").unwrap();
        let events = d.read_log(&root.agent_id).unwrap();
        let tool = events.iter().find(|e| e.event == TOOL_USE).unwrap();
        assert_eq!(tool.detail, "read_file", "only the tool name is recorded");
        assert_eq!(tool.source, SOURCE_DELEGATOR);
        assert_eq!(tool.state, EventState::ToolUse);
        assert_eq!(tool.version, EVENT_SCHEMA_VERSION);
        assert!(tool.turn_id.is_none(), "no turn context was supplied");
        assert_ne!(tool.state, EventState::Completed, "use is not a result");
    }

    #[test]
    fn old_log_lines_still_parse_alongside_new_ones() {
        let mut d = Delegator::new(dir("legacy"));
        let root = d.root(Some("s1"), "scout", "look").unwrap();
        d.begin(&root.agent_id).unwrap();
        {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&root.log)
                .unwrap();
            writeln!(
                file,
                r#"{{"event":"tool.use","agent_id":"{}","at_ms":1,"detail":"legacy_tool"}}"#,
                root.agent_id
            )
            .unwrap();
        }
        let events = d.read_log(&root.agent_id).unwrap();
        assert_eq!(events.len(), 2, "the old line is not dropped");
        assert_eq!(events[0].event, RUN_START, "new lines carry full schema");
        assert!(events[0].seq > 0);
        assert_eq!(events[1].event, "tool.use");
        assert_eq!(events[1].seq, 0, "missing seq defaults to zero");
        assert_eq!(events[1].id, "");
        assert_eq!(events[1].run_id, "");
        assert_eq!(events[1].session_id, None);
        assert_eq!(events[1].source, "");
        assert_eq!(events[1].state, EventState::Unknown);
        assert_eq!(events[1].version, 0, "missing version means legacy");
    }
}

#[cfg(test)]
mod loop_guards {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "memnoth-delegate-loop-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn a_cancelled_token_is_checked_at_the_next_boundary_only() {
        let mut tree = Delegator::with_cap(dir("token"), 2);
        let root = tree.root(None, "scout", "map").unwrap();
        let child = tree
            .spawn(Some(&root.agent_id), None, "worker", "edit")
            .unwrap();
        let token = tree
            .arm_cancellation(&child.agent_id, CancelToken::new())
            .unwrap();

        tree.tool_boundary(&child.agent_id, "first_read").unwrap();
        token.cancel();
        // The first tool already crossed its boundary; the *next* one refuses.
        let error = tree
            .tool_boundary(&child.agent_id, "write_file")
            .expect_err("the token must stop the next tool");
        assert!(error.to_string().contains("cancelled"), "{error}");
        assert!(tree.is_cancelled(&child.agent_id));
        assert!(
            !tree.is_cancelled(&root.agent_id),
            "the parent is not cancelled by a child token"
        );
        let log = tree.read_log(&child.agent_id).unwrap();
        assert!(log.iter().any(|event| event.event == RUN_CANCELLED));
    }

    #[test]
    fn a_spawn_outside_the_whitelist_is_refused() {
        let mut tree = Delegator::new(dir("whitelist"));
        let root = tree.root(None, "scout", "map").unwrap();
        let scope = SpawnScope::Only(vec!["worker".to_string()]);
        let ok = tree.spawn_allowed(&root.agent_id, None, "worker", "edit", &scope);
        assert!(ok.is_ok(), "a named child is allowed");
        let refused = tree.spawn_allowed(&root.agent_id, None, "critic", "review", &scope);
        let message = refused.expect_err("not in the whitelist").to_string();
        assert!(message.contains("spawn whitelist"), "{message}");
        assert!(message.contains("E_AGENT_TOOLS"), "{message}");
    }

    #[test]
    fn a_spawn_that_repeats_an_agent_on_the_chain_is_refused() {
        let mut tree = Delegator::with_cap(dir("cycle"), 3);
        let root = tree.root(None, "worker", "root").unwrap();
        let scope = SpawnScope::Only(vec!["worker".to_string(), "checker".to_string()]);
        let child = tree
            .spawn_allowed(&root.agent_id, None, "checker", "check", &scope)
            .unwrap();
        let refused = tree.spawn_allowed(&child.agent_id, None, "worker", "again", &scope);
        let message = refused.expect_err("cycle").to_string();
        assert!(message.contains("spawn cycle"), "{message}");
        assert!(message.contains("E_AGENT_CYCLE"), "{message}");
    }
}
