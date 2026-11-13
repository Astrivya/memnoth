//! Result contract for a completed agent run.
//!
//! The parent receives an [`AgentResult`], not the child transcript. The
//! transcript lives in the session log; this struct carries the compressed
//! summary plus structured fields the parent can render without re-reading
//! the whole window.

use memnoth_capability::Receipt;

/// Terminal outcome of an agent run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exit {
    /// The run finished successfully.
    Completed,
    /// The run hit an error.
    Failed,
    /// The run was cancelled by the user.
    Cancelled,
    /// The run was interrupted and left orphaned.
    Orphaned,
}

/// Cost of a run: provider-reported tokens and USD.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Cost {
    /// Estimated tokens consumed.
    pub tokens: u64,
    /// Estimated USD spent.
    pub usd: f64,
}

/// One command executed during the run.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommandRun {
    /// The command and its arguments.
    pub argv: Vec<String>,
    /// Exit code of the command.
    pub exit: i32,
    /// Tail of the command's output (last ~20 lines).
    pub tail: String,
}

/// Evidence backing a finding: either a source location or a command.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// A `path:line` reference into a source file.
    Path {
        /// The file path.
        path: String,
        /// The line number.
        line: usize,
    },
    /// A command that produced the evidence.
    Command {
        /// The command and its arguments.
        argv: Vec<String>,
    },
}

/// One finding from the run. Every finding must carry evidence.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Finding {
    /// The claim being made.
    pub claim: String,
    /// Evidence backing the claim (`path:line` or `command`).
    pub evidence: Evidence,
    /// Confidence in the claim, 0.0–1.0.
    pub confidence: f64,
}

/// One belief proposed by the run, pending parent approval.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProposedBelief {
    /// Handle the parent uses to approve or reject the proposal.
    pub proposal_id: String,
}

/// The compressed result of an agent run, sent from child to parent.
///
/// The parent gets the summary plus structured fields — never the whole
/// transcript inline. The full record lives in the session log.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentResult {
    /// Human-readable summary: outcome, what changed, what failed, next step.
    pub summary: String,
    /// Findings, each carrying evidence (`path:line` or `command`).
    pub findings: Vec<Finding>,
    /// Files changed by the run, sorted by path.
    pub changed_files: Vec<String>,
    /// Commands executed during the run, in execution order.
    pub commands_run: Vec<CommandRun>,
    /// Tool call receipts measured during the run.
    pub receipts: Vec<Receipt>,
    /// Beliefs proposed by the run, pending parent approval.
    pub proposed_beliefs: Vec<ProposedBelief>,
    /// Cost of the run (tokens, USD).
    pub cost: Cost,
    /// Number of turns taken.
    pub turns: u32,
    /// Terminal exit status.
    pub exit: Exit,
}
