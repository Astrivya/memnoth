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
}
