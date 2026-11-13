//! Deterministic swarm workflow primitives: DAG simulator, journaled
//! parallel barrier, subagents-as-tools.
//!
//! Mechanics ported (taste: IDEA; chrome: ADAPT) from:
//! - grok `xai-workflow` (Rhai): phase/budget discipline — at most
//!   [`MAX_PHASES`] (64) phases, [`MAX_BUDGET`] (1024) total work units.
//! - freebuff `buildAgentToolSet()`: subagents become tool definitions
//!   (name + description + prompt) the orchestrator can offer to the model.
//!
//! Everything here is pure and deterministic (no IO, no threads, no RNG):
//! the same DAG + journal always replays to the same report, which is what
//! makes it a coordination TESTING simulator. Design: `KG-DESIGN` fail-open
//! does not apply — these are explicit simulations, errors propagate.
//!
//! ```rust,ignore
//! use memnoth_engine::workflow::{Dag, WorkflowJournal};
//! let mut dag = Dag::new();
//! dag.add("patch", vec![]); dag.add("tests", vec!["patch"]);
//! let report = dag.simulate_round_robin(4); // deterministic
//! ```

use serde::{Deserialize, Serialize};

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

/// Provenance of one proposed belief: the run, agent, turn, and source
/// location that produced it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    /// The run this belief was produced in.
    pub run_id: String,
    /// The agent that produced it.
    pub agent: String,
    /// The person the run acted for; a child never invents this.
    pub user: String,
    /// The turn within the run.
    pub turn: u64,
    /// The file the belief references or was recorded against.
    pub file: String,
    /// The line within the file.
    pub line: usize,
}

impl Provenance {
    /// Build provenance for a belief.
    pub fn new(run_id: &str, agent: &str, turn: u64, file: &str, line: usize) -> Self {
        Self {
            run_id: run_id.to_string(),
            agent: agent.to_string(),
            // A run with no named person is the local user; the field is
            // present so a shared vault can tell "me" from "a teammate".
            user: "local".to_string(),
            turn,
            file: file.to_string(),
            line,
        }
    }

    /// Name the person this run acts for.
    pub fn with_user(mut self, user: &str) -> Self {
        let user = user.trim();
        if !user.is_empty() {
            self.user = user.to_string();
        }
        self
    }

    /// Render as the JSON string stored on a vault row.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

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
}
