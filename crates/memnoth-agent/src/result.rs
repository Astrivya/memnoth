//! Result contract for a completed agent run.
//!
//! The parent receives an [`AgentResult`], not the child transcript. The
//! transcript lives in the session log; this struct carries the compressed
//! summary plus structured fields the parent can render without re-reading
//! the whole window.

use memnoth_capability::Receipt;

use crate::error::{AgentError, ErrorCode};

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
    /// The run was stopped by the budget cap *before* a request was sent.
    ///
    /// This is a distinct status (Wave 5 §5.7 / F-07): an over-budget run is
    /// neither a failure of the model nor a user cancellation, and a parent
    /// must be able to tell "we never asked" from "the ask was refused".
    BudgetRefused,
}

impl Exit {
    /// The distinct exit used when the ledger refuses a request.
    pub fn budget_refused() -> Self {
        Exit::BudgetRefused
    }

    /// Map a typed error to the budget-refused exit, when it is one.
    pub fn from_budget_error(error: &AgentError) -> Option<Self> {
        (error.code == ErrorCode::Budget).then_some(Exit::BudgetRefused)
    }
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

impl Finding {
    /// Whether this finding carries usable evidence.
    ///
    /// A `path:line` needs a non-empty path and a 1-based line; a command
    /// needs a non-empty argv. Anything else is an unevidenced claim and is
    /// dropped before the result reaches a parent (F-04).
    pub fn has_evidence(&self) -> bool {
        match &self.evidence {
            Evidence::Path { path, line } => !path.trim().is_empty() && *line >= 1,
            Evidence::Command { argv } => !argv.is_empty(),
        }
    }
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

impl AgentResult {
    /// Drop every unevidenced finding, returning one typed reason per drop.
    ///
    /// The parent is never given a claim without a `path:line` (or a command)
    /// behind it. Each drop is reported with `E_FINDING_UNEVIDENCED`, so a
    /// child that hallucinates a finding loses it rather than polluting the
    /// parent's window. Evidence that is present is kept verbatim and in
    /// order.
    pub fn drop_unevidenced_findings(&mut self) -> Vec<AgentError> {
        let mut dropped = Vec::new();
        let mut kept = Vec::with_capacity(self.findings.len());
        for finding in std::mem::take(&mut self.findings) {
            if finding.has_evidence() {
                kept.push(finding);
            } else {
                dropped.push(AgentError::new(
                    ErrorCode::FindingUnevidenced,
                    format!(
                        "finding `{}` has no path:line or command evidence",
                        finding.claim
                    ),
                ));
            }
        }
        self.findings = kept;
        dropped
    }

    /// The parent-facing view of this result: a summary and a reference.
    ///
    /// A parent never receives the child transcript inline. It receives the
    /// compressed summary and the path of the child's own JSONL, which a Trace
    /// pane can open on demand. `record` is a string so this type can cross a
    /// process boundary without depending on a path type.
    pub fn reference(
        &self,
        run_id: impl Into<String>,
        agent: impl Into<String>,
        record: impl Into<String>,
    ) -> ResultRef {
        ResultRef {
            run_id: run_id.into(),
            agent: agent.into(),
            summary: self.summary.clone(),
            exit: self.exit,
            record: record.into(),
        }
    }
}

/// A parent-facing reference to a child's full record.
///
/// The whole point of the result contract: three children produce three of
/// these in the parent's message list, not three transcripts. The transcript
/// stays in `record`, which the parent can open but does not inline.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResultRef {
    /// The child run this refers to.
    pub run_id: String,
    /// The agent spec the child ran as.
    pub agent: String,
    /// The compressed summary the parent renders.
    pub summary: String,
    /// The child's terminal exit status.
    pub exit: Exit,
    /// Path to the child's JSONL — a reference, never its content.
    pub record: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(claim: &str, evidence: Evidence) -> Finding {
        Finding {
            claim: claim.to_string(),
            evidence,
            confidence: 0.9,
        }
    }

    fn result(findings: Vec<Finding>) -> AgentResult {
        AgentResult {
            summary: "done".to_string(),
            findings,
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
    fn unevidenced_findings_are_dropped_with_the_stable_code() {
        let mut result = result(vec![
            finding(
                "located",
                Evidence::Path {
                    path: "src/lib.rs".to_string(),
                    line: 12,
                },
            ),
            finding(
                "guessed",
                Evidence::Path {
                    path: String::new(),
                    line: 0,
                },
            ),
            finding(
                "ran it",
                Evidence::Command {
                    argv: vec!["cargo".to_string(), "test".to_string()],
                },
            ),
        ]);
        let dropped = result.drop_unevidenced_findings();
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].code, ErrorCode::FindingUnevidenced);
        assert_eq!(dropped[0].code_str(), "E_FINDING_UNEVIDENCED");
        assert!(dropped[0].message.contains("guessed"));
        assert_eq!(result.findings.len(), 2);
        assert_eq!(result.findings[0].claim, "located");
        assert_eq!(result.findings[1].claim, "ran it");
    }

    #[test]
    fn the_parent_gets_a_reference_not_a_transcript() {
        let result = result(Vec::new());
        let reference = result.reference("r1", "scout", "/tmp/sessions/s1/r1.jsonl");
        let wire = serde_json::to_string(&reference).unwrap();
        assert!(wire.contains("done"));
        assert!(wire.contains("r1.jsonl"));
        assert!(
            !wire.contains("tool.use"),
            "a reference is not a transcript"
        );
        assert_eq!(reference.exit, Exit::Completed);

        let budget = AgentError::new(ErrorCode::Budget, "over budget");
        assert_eq!(Exit::from_budget_error(&budget), Some(Exit::BudgetRefused));
        assert_eq!(Exit::budget_refused(), Exit::BudgetRefused);
    }
}
