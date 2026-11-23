//! Deterministic swarm workflow primitives: DAG simulator, journaled
//! parallel barrier, subagents-as-tools.
//!
//! Mechanics ported (taste: IDEA; chrome: ADAPT) from:
//! - grok `xai-workflow` (Rhai): phase/budget discipline — at most
//!   [`MAX_PHASES`] (64) phases, [`MAX_BUDGET`] (1024) total work units.
//! - freebuff `buildAgentToolSet()`: subagents become tool definitions
//!   (name + description + prompt) the orchestrator can offer to the model.
//!
//! The coordination primitives are pure and deterministic (no IO, no threads,
//! no RNG): the same DAG + journal always replays to the same report, which is
//! what makes it a coordination TESTING simulator. `commit_slice` is the one
//! explicit DB-facing seam where a parent promotes a child's proposals.
//! Design: `KG-DESIGN` fail-open does not apply — errors propagate.
//!
//! ```rust,ignore
//! use memnoth_engine::workflow::{Dag, WorkflowJournal};
//! let mut dag = Dag::new();
//! dag.add("patch", vec![]); dag.add("tests", vec!["patch"]);
//! let report = dag.simulate_round_robin(4); // deterministic
//! ```

use serde::{Deserialize, Serialize};

use crate::store::proposals::Proposal;

/// Fixed provenance shape for every proposal on the shared memory bus.
///
/// The file lives at `src/provenance.rs`; the `#[path]` keeps `workflow` the
/// single public door (the crate root already re-exports `workflow::Provenance`).
#[path = "provenance.rs"]
pub mod provenance;
pub use provenance::{PROVENANCE_KEYS, Provenance, has_full_provenance};

/// Max phases per workflow (grok `xai-workflow` `agent_budget` discipline).
pub const MAX_PHASES: usize = 64;
/// Max total work units per workflow (grok `agent_budget 1-1024`).
pub const MAX_BUDGET: u64 = 1024;

/// One DAG node: a named unit of work with dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNode {
    /// Node id (unique within the DAG).
    pub id: String,
    /// Ids this node waits on.
    pub depends_on: Vec<String>,
    /// Work units consumed when this node runs (default 1).
    pub cost: u64,
}

/// A plan DAG: nodes + dependency edges.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Dag {
    /// Nodes in insertion order (edges are `depends_on`).
    pub nodes: Vec<DagNode>,
}

impl Dag {
    /// An empty plan.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node (idempotent by id — re-adding replaces deps/cost).
    pub fn add(&mut self, id: &str, depends_on: Vec<&str>, cost: u64) {
        let node = DagNode {
            id: id.to_string(),
            depends_on: depends_on.into_iter().map(str::to_string).collect(),
            cost: cost.max(1),
        };
        match self.nodes.iter_mut().find(|n| n.id == node.id) {
            Some(slot) => *slot = node,
            None => self.nodes.push(node),
        }
    }

    /// Deterministic round-robin simulation over `workers` workers.
    ///
    /// Each round, every idle worker takes the lexicographically smallest
    /// READY node (deps done); nodes complete at round end. Returns a
    /// [`SimReport`]. Errors on unknown deps or when the budget/phase caps
    /// would be exceeded (fail-closed: a bad plan is a loud error).
    pub fn simulate_round_robin(&self, workers: usize) -> anyhow::Result<SimReport> {
        let workers = workers.clamp(1, 64);
        let total_cost: u64 = self.nodes.iter().map(|n| n.cost).sum();
        if total_cost > MAX_BUDGET {
            anyhow::bail!("plan cost {total_cost} exceeds budget {MAX_BUDGET}");
        }
        let known: std::collections::HashSet<&str> =
            self.nodes.iter().map(|n| n.id.as_str()).collect();
        for n in &self.nodes {
            for d in &n.depends_on {
                if !known.contains(d.as_str()) {
                    anyhow::bail!("node {} depends on unknown {}", n.id, d);
                }
            }
        }
        let mut done: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut rounds: Vec<Vec<String>> = Vec::new();
        let mut order: Vec<String> = Vec::new();
        let mut remaining: Vec<&DagNode> = self.nodes.iter().collect();
        while !remaining.is_empty() {
            if rounds.len() >= MAX_PHASES {
                anyhow::bail!("plan exceeds {MAX_PHASES} phases (cycle or too deep)");
            }
            let mut ready: Vec<&DagNode> = remaining
                .iter()
                .filter(|n| n.depends_on.iter().all(|d| done.contains(d)))
                .copied()
                .collect();
            if ready.is_empty() {
                let stuck: Vec<&str> = remaining.iter().map(|n| n.id.as_str()).collect();
                anyhow::bail!("deadlock: no ready node, stuck on {stuck:?} (dependency cycle?)");
            }
            ready.sort_by(|a, b| a.id.cmp(&b.id));
            let take = ready.len().min(workers);
            let mut round: Vec<String> = Vec::new();
            for n in ready.into_iter().take(take) {
                round.push(n.id.clone());
                done.insert(n.id.clone());
                order.push(n.id.clone());
            }
            remaining.retain(|n| !done.contains(&n.id));
            rounds.push(round);
        }
        Ok(SimReport {
            rounds: rounds.len(),
            workers,
            total_cost,
            completion_order: order,
            rounds_detail: rounds,
        })
    }
}

/// Deterministic simulation outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimReport {
    /// Number of barrier rounds the simulation took.
    pub rounds: usize,
    /// Number of simulated workers.
    pub workers: usize,
    /// Sum of `cost` over every node that ran.
    pub total_cost: u64,
    /// Node ids in the order they completed.
    pub completion_order: Vec<String>,
    /// Node ids that ran in each round, in round order.
    pub rounds_detail: Vec<Vec<String>>,
}

/// Journaled parallel barrier (grok `xai-workflow` parallel() semantics).
///
/// Workers register, run, and check in; the barrier releases only when ALL
/// registered workers report (or are marked failed). Every transition is
/// appended to an in-memory journal so a crashed coordinator can replay the
/// exact sequence. Deterministic: iteration order is registration order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowJournal {
    entries: Vec<JournalEntry>,
    members: Vec<String>,
    checkins: std::collections::HashSet<String>,
    failed: std::collections::HashSet<String>,
}

/// One journaled barrier event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Zero-based append order (replay is by `seq`).
    pub seq: u64,
    /// Event verb (`registered` / `checked_in` / `failed`).
    pub event: String,
    /// Worker the event is about.
    pub worker: String,
}

impl WorkflowJournal {
    /// An empty journal: no members, nothing checked in, nothing failed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a worker at the barrier (idempotent).
    pub fn register(&mut self, worker: &str) {
        if !self.members.iter().any(|m| m == worker) {
            self.members.push(worker.to_string());
            self.append("registered", worker);
        }
    }

    /// Check in a registered worker. Returns true when the barrier releases
    /// (all members checked in or failed).
    pub fn check_in(&mut self, worker: &str) -> bool {
        if self.members.iter().any(|m| m == worker) {
            self.checkins.insert(worker.to_string());
            self.append("checked_in", worker);
        }
        self.released()
    }

    /// Mark a worker failed (counts toward release, records the failure).
    pub fn mark_failed(&mut self, worker: &str) -> bool {
        if self.members.iter().any(|m| m == worker) {
            self.failed.insert(worker.to_string());
            self.append("failed", worker);
        }
        self.released()
    }

    /// True when every member has checked in or failed (vacuous true when empty).
    pub fn released(&self) -> bool {
        self.members
            .iter()
            .all(|m| self.checkins.contains(m) || self.failed.contains(m))
    }

    /// Replay the journal in order (crash recovery / audit).
    pub fn replay(&self) -> Vec<JournalEntry> {
        self.entries.clone()
    }

    /// Registered workers that have neither checked in nor failed.
    pub fn pending(&self) -> Vec<String> {
        self.members
            .iter()
            .filter(|m| !self.checkins.contains(*m) && !self.failed.contains(*m))
            .cloned()
            .collect()
    }

    fn append(&mut self, event: &str, worker: &str) {
        let seq = self.entries.len() as u64;
        self.entries.push(JournalEntry {
            seq,
            event: event.to_string(),
            worker: worker.to_string(),
        });
    }
}

/// A subagent exposed as a tool definition (freebuff `buildAgentToolSet`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToolDef {
    /// Sanitized tool name (`[a-z0-9_]`).
    pub name: String,
    /// One-line description the model sees in the tool list.
    pub description: String,
    /// System prompt the subagent runs with.
    pub prompt: String,
}

/// Build tool definitions from subagent specs.
///
/// `(agent_name, role_description, system_prompt)` → tool defs the
/// orchestrator can register. Names are sanitized to `[a-z0-9_]`
/// (model tool-name grammar); empty prompts are rejected with `Err`.
pub fn build_agent_tool_set(specs: &[(&str, &str, &str)]) -> anyhow::Result<Vec<AgentToolDef>> {
    let mut out = Vec::with_capacity(specs.len());
    for (name, role, prompt) in specs {
        if prompt.trim().is_empty() {
            anyhow::bail!("agent {name} has an empty prompt");
        }
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let safe = safe.trim_matches('_').to_string();
        if safe.is_empty() {
            anyhow::bail!("agent name {name:?} sanitizes to empty");
        }
        out.push(AgentToolDef {
            name: format!("agent_{safe}"),
            description: format!("Delegate to the {name} subagent: {role}"),
            prompt: prompt.to_string(),
        });
    }
    Ok(out)
}
/// Where a workflow hook fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPoint {
    /// Immediately before a tool runs (`name` = tool name).
    PreToolUse,
    /// When a tool call returns, on every path (`name` = tool name).
    PostToolUse,
    /// On workflow/session stop (`name` = stop reason or session id).
    Stop,
    /// Before a subagent starts (`name` = subagent name).
    SubagentStart,
    /// When a subagent stops (`name` = subagent name).
    SubagentStop,
}

impl HookPoint {
    /// Wire name used to identify the hook point.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
            Self::Stop => "stop",
            Self::SubagentStart => "subagent_start",
            Self::SubagentStop => "subagent_stop",
        }
    }

    /// Parse a wire name (`pre_tool_use`, `post_tool_use`, `stop`,
    /// `subagent_start`, `subagent_stop`).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "pre_tool_use" => Some(Self::PreToolUse),
            "post_tool_use" => Some(Self::PostToolUse),
            "stop" => Some(Self::Stop),
            "subagent_start" => Some(Self::SubagentStart),
            "subagent_stop" => Some(Self::SubagentStop),
            _ => None,
        }
    }
}

/// A blocking lifecycle hook callback.
///
/// Returning `Ok(())` allows the action to proceed; returning `Err`
/// blocks it and the error propagates to the caller.
pub type HookCallback = Box<dyn FnMut(&str) -> anyhow::Result<()> + Send>;

/// One registered hook: `callback` fires when `point` matches `name`.
pub struct Hook {
    /// Where the hook fires.
    pub point: HookPoint,
    /// Match expression for the hook name: `*` or empty matches everything,
    /// otherwise a case-insensitive substring.
    pub matcher: String,
    /// The hook implementation; an `Err` blocks the action.
    pub callback: HookCallback,
}

impl std::fmt::Debug for Hook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hook")
            .field("point", &self.point)
            .field("matcher", &self.matcher)
            .field("callback", &"<hook>")
            .finish()
    }
}

impl Hook {
    /// Whether this hook fires for `name` at its registered point.
    fn matches(&self, name: &str) -> bool {
        let m = self.matcher.trim();
        if m.is_empty() || m == "*" {
            return true;
        }
        name.to_ascii_lowercase().contains(&m.to_ascii_lowercase())
    }
}

/// Registry of blocking lifecycle hooks.
///
/// Hooks are synchronous callbacks supplied by the outer layer, which keeps
/// the simulator pure (no process spawning inside the engine). Each matching
/// hook is invoked in registration order; the first `Err` blocks the action
/// and is returned — remaining hooks are not run.
#[derive(Default)]
pub struct Hooks {
    hooks: Vec<Hook>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.hooks.iter()).finish()
    }
}

impl Hooks {
    /// An empty hook registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a hook that fires at `point` for names matching `matcher`.
    pub fn add(&mut self, point: HookPoint, matcher: impl Into<String>, callback: HookCallback) {
        self.hooks.push(Hook {
            point,
            matcher: matcher.into(),
            callback,
        });
    }

    /// Hooks registered for `point` whose matcher matches `name`, in
    /// registration order.
    pub fn matching(&self, point: HookPoint, name: &str) -> Vec<&Hook> {
        self.hooks
            .iter()
            .filter(|h| h.point == point && h.matches(name))
            .collect()
    }

    /// Invoke every matching hook in registration order. The first `Err`
    /// blocks the action and is returned; later hooks are not run.
    pub fn dispatch(&mut self, point: HookPoint, name: &str) -> anyhow::Result<()> {
        let indices: Vec<usize> = self
            .hooks
            .iter()
            .enumerate()
            .filter(|(_, h)| h.point == point && h.matches(name))
            .map(|(i, _)| i)
            .collect();
        for i in indices {
            (self.hooks[i].callback)(name)?;
        }
        Ok(())
    }
}
// --------------------------------------------------------------- scoped vault slice
//
// # Scoped vault slices
//
// When the orchestrator spawns a child subagent, the child receives a scoped
// slice of the vault: it can propose beliefs but never write directly into
// `observations`. Every belief carries provenance
// `{run_id, agent, turn, file, line}`, so a reviewer can always trace a
// belief back to the spawn that produced it. Nothing here touches the
// database — slices are pure data the orchestrator hands to children and
// later collects for acceptance (pure and deterministic, like the rest of
// this module).

/// One belief a child proposed into its slice: not a vault row, not yet
/// accepted, just a card waiting for a parent to decide.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedBelief {
    /// The entity the belief is about.
    pub entity: String,
    /// The belief itself.
    pub content: String,
    /// Observation kind (`fact`, `decision`, `lesson`, `gotcha`, `correction`).
    pub kind: String,
    /// Confidence the child claimed, 0.0–1.0.
    pub confidence: f64,
    /// Provenance: {run_id, agent, turn, file, line}.
    pub provenance: Provenance,
}

/// A scoped slice of the vault handed to one spawned child.
///
/// Children propose beliefs into the slice; nothing touches the vault until
/// a parent accepts. The slice is scoped by `run_id` + `agent` + `turn`, so
/// two children of the same run never share a slice and every belief is
/// attributable to the spawn that produced it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSlice {
    /// The run this slice belongs to.
    pub run_id: String,
    /// The agent this slice is scoped to.
    pub agent: String,
    /// The person the run acts for.
    pub user: String,
    /// The turn this slice was created at.
    pub turn: u64,
    /// Beliefs proposed by this child, in proposal order.
    pub proposals: Vec<ProposedBelief>,
}

impl VaultSlice {
    /// Open a fresh slice for `agent` in `run_id` at `turn`.
    pub fn spawn(run_id: &str, agent: &str, turn: u64) -> Self {
        Self {
            run_id: run_id.to_string(),
            agent: agent.to_string(),
            user: "local".to_string(),
            turn,
            proposals: Vec::new(),
        }
    }

    /// Attribute every future proposal to `user`.
    pub fn with_user(mut self, user: &str) -> Self {
        let user = user.trim();
        if !user.is_empty() {
            self.user = user.to_string();
        }
        self
    }

    /// Propose a belief from this child. Returns the index it was appended at.
    pub fn propose(
        &mut self,
        entity: &str,
        content: &str,
        kind: &str,
        confidence: f64,
        file: &str,
        line: usize,
    ) -> usize {
        let proposal = ProposedBelief {
            entity: entity.to_string(),
            content: content.to_string(),
            kind: kind.to_string(),
            confidence: confidence.clamp(0.0, 1.0),
            provenance: Provenance::new(&self.run_id, &self.agent, self.turn, file, line)
                .with_user(&self.user),
        };
        self.proposals.push(proposal);
        self.proposals.len() - 1
    }

    /// All proposals in this slice, oldest first.
    pub fn proposals(&self) -> &[ProposedBelief] {
        &self.proposals
    }
}

// --------------------------------------------------------- parallel disagreement
//
// Two children running in parallel can read the same file and disagree. A
// disagreement is surfaced as a contradiction that names BOTH sources (run id,
// agent, file:line) and BOTH dates, so a reviewer never has to go back to two
// separate transcripts to see who said what and when.

/// One side of a disagreement between two children that ran in parallel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisagreementSide {
    /// The entity both children were writing about.
    pub entity: String,
    /// The belief this child proposed.
    pub content: String,
    /// Run that produced the belief (the source id).
    pub run_id: String,
    /// Agent that produced the belief.
    pub agent: String,
    /// Stable source label: `agent@run_id file:line`.
    pub source: String,
    /// Full provenance JSON for this side (`{run_id,agent,user,turn,file,line}`).
    pub provenance: String,
    /// Effective date of the belief, or proposal time when none was claimed.
    pub date: i64,
    /// When the proposal itself was staged (ms since epoch).
    pub proposed_at: i64,
    /// Turn within the run.
    pub turn: u64,
    /// File the belief references.
    pub file: String,
    /// Line within the file.
    pub line: usize,
}

impl DisagreementSide {
    fn from_proposal(proposal: &Proposal) -> Self {
        let prov: serde_json::Value =
            serde_json::from_str(&proposal.provenance).unwrap_or_else(|_| serde_json::json!({}));
        let text = |key: &str| -> String {
            prov.get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let number = |key: &str| -> u64 {
            prov.get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        let run_id = text("run_id");
        let agent = text("agent");
        let file = text("file");
        let line = number("line") as usize;
        let source = if agent.is_empty() && run_id.is_empty() {
            file.clone()
        } else {
            format!("{agent}@{run_id} {file}:{line}")
        };
        Self {
            entity: proposal.entity_name.clone(),
            content: proposal.content.clone(),
            run_id,
            agent,
            source,
            provenance: proposal.provenance.clone(),
            date: proposal.effective_at.unwrap_or(proposal.proposed_at),
            proposed_at: proposal.proposed_at,
            turn: number("turn"),
            file,
            line,
        }
    }
}

/// A contradiction surfaced from parallel proposals on the same entity.
///
/// Both sides are always present: the whole point of the gate is that a query
/// cannot show one child's finding and hide the other's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Disagreement {
    /// The name both children disagree about.
    pub entity: String,
    /// The earlier proposal.
    pub left: DisagreementSide,
    /// The later proposal.
    pub right: DisagreementSide,
    /// Explanation naming both sources, both contents and both dates.
    pub reason: String,
}

impl Disagreement {
    /// Render as the JSON row shape a `vault contradictions` consumer reads.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "entity": self.entity,
            "left": {
                "content": self.left.content,
                "source": self.left.source,
                "run_id": self.left.run_id,
                "date": self.left.date,
            },
            "right": {
                "content": self.right.content,
                "source": self.right.source,
                "run_id": self.right.run_id,
                "date": self.right.date,
            },
            "reason": self.reason,
        })
    }
}

/// Surface parallel disagreement as contradictions with BOTH sources and dates.
///
/// `proposals` is the open queue from
/// [`MemoryEngine::list_proposals`](crate::MemoryEngine::list_proposals).
/// Two still-open proposals for the same entity with different content cannot
/// both be true, so each such pair is reported with both children's run ids,
/// source locations and proposal dates. Identical content is deduped by the
/// vault before it gets here; ordering is deterministic (entity, then time/id).
pub fn disagreements_from_proposals(proposals: &[Proposal]) -> Vec<Disagreement> {
    use std::collections::BTreeMap;

    let mut groups: BTreeMap<String, Vec<&Proposal>> = BTreeMap::new();
    for proposal in proposals {
        if proposal.status != "proposed" {
            continue;
        }
        let key = proposal.entity_name.trim().to_ascii_lowercase();
        groups.entry(key).or_default().push(proposal);
    }

    let mut out = Vec::new();
    for mut items in groups.into_values() {
        if items.len() < 2 {
            continue;
        }
        items.sort_by_key(|proposal| (proposal.proposed_at, proposal.id));
        for (i, left) in items.iter().enumerate() {
            for right in items.iter().skip(i + 1) {
                if left.content.trim() == right.content.trim() {
                    continue;
                }
                let left_side = DisagreementSide::from_proposal(left);
                let right_side = DisagreementSide::from_proposal(right);
                let reason = format!(
                    "parallel disagreement about `{}`: {} (source {}, {}) vs {} (source {}, {})",
                    left_side.entity,
                    left_side.content,
                    left_side.source,
                    left_side.date,
                    right_side.content,
                    right_side.source,
                    right_side.date,
                );
                out.push(Disagreement {
                    entity: left_side.entity.clone(),
                    left: left_side,
                    right: right_side,
                    reason,
                });
            }
        }
    }
    out
}

// ------------------------------------------------------------------- commit seam
//
// Everything above is pure data. This is the single DB-facing bridge: the
// parent hands a child's slice here, and every staged belief becomes a proposal
// (never an observation). One call opens and closes exactly one episode, so an
// episode belongs to a RUN, not to a belief.

/// What the parent's commit of one child slice produced.
#[derive(Debug, Clone)]
pub struct CommitReport {
    /// The single episode that covers this run.
    pub episode_id: i64,
    /// `(proposal_id, created)` per staged belief. `created == false` means a
    /// sibling had already staged the same belief and this card was joined.
    pub proposal_ids: Vec<(i64, bool)>,
    /// Contradictions visible after the commit, each naming both sources.
    pub disagreements: Vec<Disagreement>,
}

/// Commit one child's scoped slice into the vault from the parent side.
///
/// Children never call this — they only have a [`VaultSlice`] and can `propose`
/// into it. This bridge is the only path from a slice to the database, and it
/// writes proposals (`status = 'proposed'`), so nothing a child staged becomes
/// a belief until a person accepts it. `actor` is recorded as the owner
/// accountable for that decision; a proposal with incomplete provenance is
/// refused rather than staged as an unattributable card.
pub fn commit_slice(
    engine: &crate::MemoryEngine,
    slice: &VaultSlice,
    actor: &str,
    at: i64,
) -> anyhow::Result<CommitReport> {
    let mut proposal_ids = Vec::with_capacity(slice.proposals().len());
    for belief in slice.proposals() {
        let json = belief.provenance.to_json();
        if !has_full_provenance(&json) {
            anyhow::bail!(
                "refusing unattributable proposal from {}: incomplete provenance",
                slice.agent
            );
        }
        // The committing parent owns the decision: the child proposed, but a
        // person is accountable for whether this card is ever accepted.
        let (id, created) = engine.propose_belief_with(
            &belief.entity,
            crate::EntityKind::Concept,
            &belief.content,
            &belief.kind,
            belief.confidence,
            &json,
            at,
            Some(actor),
            None,
        )?;
        proposal_ids.push((id, created));
    }

    let summary = format!(
        "run {} ({}) staged {} belief(s)",
        slice.run_id,
        slice.agent,
        slice.proposals().len()
    );
    let episode_id = engine.append_episode("delegate", Some(&slice.agent), Some(&summary), at)?;
    engine.close_episode(episode_id, at)?;

    let open = engine.list_proposals(Some("proposed"), at)?;
    let disagreements = disagreements_from_proposals(&open);
    Ok(CommitReport {
        episode_id,
        proposal_ids,
        disagreements,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dag_simulation_is_deterministic() {
        let mut dag = Dag::new();
        dag.add("reproduce", vec![], 1);
        dag.add("patch", vec!["reproduce"], 2);
        dag.add("tests", vec!["patch"], 1);
        dag.add("docs", vec!["patch"], 1);
        let r1 = dag.simulate_round_robin(2).unwrap();
        let r2 = dag.simulate_round_robin(2).unwrap();
        assert_eq!(r1.completion_order, r2.completion_order);
        assert_eq!(r1.rounds, 3);
        assert_eq!(r1.completion_order[0], "reproduce");
        assert!(r1.completion_order.contains(&"tests".to_string()));
        // Round 3 runs tests+docs in parallel (lexicographic).
        assert_eq!(
            r1.rounds_detail[2],
            vec!["docs".to_string(), "tests".to_string()]
        );
    }

    #[test]
    fn dag_rejects_cycles_and_unknown_deps() {
        let mut dag = Dag::new();
        dag.add("a", vec!["b"], 1);
        dag.add("b", vec!["a"], 1);
        assert!(dag.simulate_round_robin(2).is_err());
        let mut dag2 = Dag::new();
        dag2.add("a", vec!["ghost"], 1);
        assert!(dag2.simulate_round_robin(1).is_err());
    }

    #[test]
    fn barrier_releases_only_when_all_report() {
        let mut j = WorkflowJournal::new();
        j.register("w1");
        j.register("w2");
        assert!(!j.check_in("w1"));
        assert_eq!(j.pending(), vec!["w2".to_string()]);
        assert!(j.mark_failed("w2"));
        assert!(j.released());
        // Journal replays the exact sequence.
        let replayed = j.replay();
        let events: Vec<&str> = replayed.iter().map(|e| e.event.as_str()).collect();
        assert_eq!(
            events,
            vec!["registered", "registered", "checked_in", "failed"]
        );
        assert!(replayed.iter().enumerate().all(|(i, e)| e.seq == i as u64));
    }

    #[test]
    fn agent_tool_set_sanitizes_and_rejects_empty() {
        let defs = build_agent_tool_set(&[
            ("Code-Reviewer", "reviews diffs", "You review diffs."),
            ("planner", "plans work", "You plan."),
        ])
        .unwrap();
        assert_eq!(defs[0].name, "agent_code_reviewer");
        assert!(defs[0].description.contains("reviews diffs"));
        assert!(build_agent_tool_set(&[("x", "y", "   ")]).is_err());
    }
    #[test]
    fn proposed_belief_carries_full_provenance() {
        let prov = Provenance::new("run-1", "child-a", 3, "src/lib.rs", 42);
        let json = prov.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["run_id"], "run-1");
        assert_eq!(parsed["agent"], "child-a");
        assert_eq!(parsed["turn"], 3);
        assert_eq!(parsed["file"], "src/lib.rs");
        assert_eq!(parsed["line"], 42);
    }

    #[test]
    fn slice_proposes_beliefs_in_order_and_is_scoped() {
        let mut slice = VaultSlice::spawn("run-1", "child-a", 7);
        let i0 = slice.propose(
            "acme",
            "the deploy runs on fridays",
            "fact",
            0.8,
            "src/lib.rs",
            10,
        );
        let i1 = slice.propose(
            "acme",
            "sessions expire after 30 minutes",
            "decision",
            0.9,
            "src/bin.rs",
            5,
        );

        assert_eq!(i0, 0);
        assert_eq!(i1, 1);
        assert_eq!(slice.run_id, "run-1");
        assert_eq!(slice.agent, "child-a");
        assert_eq!(slice.turn, 7);
        assert_eq!(slice.proposals().len(), 2);
        assert_eq!(slice.proposals()[0].content, "the deploy runs on fridays");
        assert_eq!(slice.proposals()[0].provenance.run_id, "run-1");
        assert_eq!(slice.proposals()[0].provenance.agent, "child-a");
        assert_eq!(slice.proposals()[0].provenance.turn, 7);
        assert_eq!(slice.proposals()[0].provenance.file, "src/lib.rs");
        assert_eq!(slice.proposals()[0].provenance.line, 10);
        assert_eq!(
            slice.proposals()[1].content,
            "sessions expire after 30 minutes"
        );
        assert_eq!(slice.proposals()[1].provenance.file, "src/bin.rs");
        assert_eq!(slice.proposals()[1].provenance.line, 5);
        // Confidence is clamped into range.
        let i2 = slice.propose("acme", "clamped", "fact", 2.5, "src/x.rs", 1);
        assert_eq!(slice.proposals()[i2].confidence, 1.0);
    }

    #[test]
    fn slices_are_independent_per_spawn() {
        let mut a = VaultSlice::spawn("run-1", "child-1", 1);
        let mut b = VaultSlice::spawn("run-1", "child-2", 1);
        a.propose("acme", "belief a", "fact", 0.5, "src/a.rs", 1);
        b.propose("acme", "belief b", "fact", 0.5, "src/b.rs", 2);
        // Two children of the same run share nothing: each slice holds only
        // its own proposals.
        assert_eq!(a.proposals().len(), 1);
        assert_eq!(b.proposals().len(), 1);
        assert_eq!(a.proposals()[0].provenance.agent, "child-1");
        assert_eq!(b.proposals()[0].provenance.agent, "child-2");
        assert_ne!(
            a.proposals()[0].provenance.line,
            b.proposals()[0].provenance.line
        );
    }
    #[test]
    fn hook_dispatch_blocks_on_first_error() {
        let mut hooks = Hooks::new();
        hooks.add(HookPoint::PreToolUse, "*", Box::new(|_| anyhow::Ok(())));
        hooks.add(
            HookPoint::PreToolUse,
            "*",
            Box::new(|_| anyhow::bail!("denied by policy")),
        );
        hooks.add(HookPoint::PreToolUse, "*", Box::new(|_| anyhow::Ok(())));
        let err = hooks
            .dispatch(HookPoint::PreToolUse, "coord_claim")
            .unwrap_err();
        assert!(err.to_string().contains("denied by policy"));
        // The third hook never ran: the blocking error short-circuits.
    }

    #[test]
    fn hook_dispatch_allows_when_all_ok() {
        let mut hooks = Hooks::new();
        hooks.add(HookPoint::PostToolUse, "*", Box::new(|_| anyhow::Ok(())));
        hooks.add(HookPoint::Stop, "session-1", Box::new(|_| anyhow::Ok(())));
        assert!(hooks.dispatch(HookPoint::PostToolUse, "edit").is_ok());
        assert!(hooks.dispatch(HookPoint::Stop, "session-1").is_ok());
        // No hooks registered for SubagentStop: no-op success.
        assert!(hooks.dispatch(HookPoint::SubagentStop, "agent-a").is_ok());
    }

    #[test]
    fn hook_matcher_star_and_case_insensitive_substring() {
        let mut hooks = Hooks::new();
        hooks.add(
            HookPoint::SubagentStart,
            "coord_",
            Box::new(|_| anyhow::bail!("blocked")),
        );
        assert!(
            hooks
                .dispatch(HookPoint::SubagentStart, "coord_claim")
                .is_err()
        );
        assert!(
            hooks
                .dispatch(HookPoint::SubagentStart, "My_Coord_Review")
                .is_err()
        );
        assert!(hooks.dispatch(HookPoint::SubagentStart, "planner").is_ok());
    }

    #[test]
    fn hook_point_wire_names_parse_round_trip() {
        assert_eq!(HookPoint::PreToolUse.as_str(), "pre_tool_use");
        assert_eq!(HookPoint::PostToolUse.as_str(), "post_tool_use");
        assert_eq!(HookPoint::Stop.as_str(), "stop");
        assert_eq!(HookPoint::SubagentStart.as_str(), "subagent_start");
        assert_eq!(HookPoint::SubagentStop.as_str(), "subagent_stop");
        assert_eq!(
            HookPoint::parse("subagent_stop"),
            Some(HookPoint::SubagentStop)
        );
        assert_eq!(HookPoint::parse("unknown"), None);
    }

    #[test]
    fn agent_tool_set_names_the_children_that_get_slices() {
        // The delegation runtime builds its child tool list with this function;
        // each tool becomes a child run with its own scoped slice.
        let defs = build_agent_tool_set(&[
            ("reviewer", "reviews diffs", "You review diffs."),
            ("planner", "plans work", "You plan."),
        ])
        .unwrap();
        let slices: Vec<VaultSlice> = defs
            .iter()
            .enumerate()
            .map(|(i, def)| VaultSlice::spawn(&format!("run-{i}"), &def.name, 0))
            .collect();
        assert_eq!(slices[0].agent, "agent_reviewer");
        assert_eq!(slices[1].agent, "agent_planner");
        for (i, slice) in slices.iter().enumerate() {
            assert_eq!(slice.run_id, format!("run-{i}"));
            assert!(slice.proposals().is_empty(), "a fresh child stages nothing");
        }
    }
}
