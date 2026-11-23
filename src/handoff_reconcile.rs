//! Declared `mn-handoff/1` session bundles and their reconciliation.
//!
//! P22-049 defines a versioned handoff shape that carries the state a second
//! machine needs to continue a task: goals, constraints, the repository
//! revision the work was done against, attempts, the *actual* results of the
//! checks that ran, and approvals awaiting re-check. P22-050 refuses to
//! continue when that recorded state no longer matches the workspace it is
//! being handed to: a different branch or revision, a dirty tree, or a check
//! recorded against an older revision.
//!
//! Two honesty rules shape this module:
//!
//! * **No hidden reasoning.** The shape has no field for it. [`strip_hidden_reasoning`]
//!   removes provider reasoning keys (`reasoning`, `thinking`, …) recursively
//!   before a bundle is written, so a model's private chain of thought cannot
//!   be exported as if it were a user-visible record.
//! * **No invented verification.** [`VerificationRecord`] holds only checks that
//!   actually ran, each stamped with the revision it ran against. A build with
//!   no verification store records an empty list; it never reports a pass it
//!   did not observe.
//!
//! Bundles are backward compatible: a workspace with no git revision produces
//! the legacy `{version, session, metadata}` shape, which [`crate::session_cmd::handoff`]
//! still reads unchanged. The declared shape is an additive `handoff` section
//! plus a `schema: "mn-handoff/1"` marker, written only when there is a
//! repository revision to reconcile against.

use std::path::Path;

use anyhow::{Context, Result};
use memnoth_session::{ApprovalRecord, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The declared bundle schema this build writes and reads. Distinct from
/// `mn-handoff/1`, which is the benchmark fixture schema (`ci/handoff-cases.json`).
pub const HANDOFF_SCHEMA: &str = "mn-session-handoff/1";

/// Top-level key under which the declared manifest is embedded in a bundle.
pub const HANDOFF_KEY: &str = "handoff";

/// Provider reasoning keys that must never survive into a handoff bundle.
/// Matching is case-insensitive so a gateway's `Reasoning` cannot slip through.
pub const HIDDEN_REASONING_KEYS: &[&str] = &[
    "reasoning",
    "reasoning_content",
    "reasoning_trace",
    "thinking",
    "thought",
    "thoughts",
    "chain_of_thought",
    "hidden_reasoning",
    "scratchpad",
    "internal_monologue",
    "delta_reasoning",
];

/// A completion condition carried across the handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalState {
    pub condition: String,
    /// Whether a person marked it met before the handoff.
    pub done: bool,
    pub set_ms: i64,
}

/// The repository identity a bundle was captured against.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryState {
    /// Branch at capture, when git could report one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Full HEAD object id at capture, when git could report one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Whether the worktree had uncommitted changes at capture.
    #[serde(default)]
    pub dirty: bool,
}

/// One recorded attempt at the task (one completed turn).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub label: String,
    pub at_ms: i64,
    /// `"completed"` or `"failed"` — the observed outcome, never a guess.
    pub outcome: String,
    pub detail: String,
}

/// An actual check result, stamped with the revision it ran against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRecord {
    pub name: String,
    pub ok: bool,
    pub at_ms: i64,
    /// The revision the check really ran against. `None` means freshness
    /// cannot be proven, not that the check is fresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

/// An approval decision carried to the receiving machine for re-check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub tool: String,
    pub subject: String,
    pub approved: bool,
    pub approved_by: String,
    pub at_ms: i64,
}

/// The declared `mn-handoff/1` shape. Deliberately has no reasoning field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffManifest {
    pub schema: String,
    pub session: String,
    pub captured_ms: i64,
    pub goals: Vec<GoalState>,
    pub constraints: Vec<String>,
    pub repository: RepositoryState,
    pub attempts: Vec<AttemptRecord>,
    pub verification: Vec<VerificationRecord>,
    pub pending_approvals: Vec<PendingApproval>,
}

impl HandoffManifest {
    /// An empty manifest for `session`, stamped now and declaring the schema.
    pub fn new(session: &str) -> Self {
        Self {
            schema: HANDOFF_SCHEMA.to_string(),
            session: session.to_string(),
            captured_ms: memnoth_session::now_ms(),
            goals: Vec::new(),
            constraints: Vec::new(),
            repository: RepositoryState::default(),
            attempts: Vec::new(),
            verification: Vec::new(),
            pending_approvals: Vec::new(),
        }
    }

    /// Serialize the manifest with hidden reasoning keys removed.
    pub fn to_value(&self) -> Result<Value> {
        let mut value = serde_json::to_value(self).context("serialize handoff manifest")?;
        strip_hidden_reasoning(&mut value);
        Ok(value)
    }
}

/// Why a handoff must not continue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReconcileReason {
    /// The recorded revision differs from the repository's current HEAD.
    RevisionChanged { recorded: String, current: String },
    /// The recorded branch differs from the branch currently checked out.
    BranchChanged { recorded: String, current: String },
    /// The worktree has uncommitted changes.
    DirtyWorktree,
    /// A recorded check ran against a different revision than HEAD.
    StaleVerification {
        name: String,
        recorded_revision: String,
        current_revision: String,
    },
}

impl ReconcileReason {
    /// A human-readable reason, used in the refusal message.
    pub fn message(&self) -> String {
        match self {
            Self::RevisionChanged { recorded, current } => {
                format!("revision changed: recorded {recorded}, current {current}")
            }
            Self::BranchChanged { recorded, current } => {
                format!("branch changed: recorded {recorded}, current {current}")
            }
            Self::DirtyWorktree => "worktree is dirty".to_string(),
            Self::StaleVerification {
                name,
                recorded_revision,
                current_revision,
            } => format!(
                "verification '{name}' is stale: ran against {recorded_revision}, current {current_revision}"
            ),
        }
    }
}

/// The result of reconciling a manifest against the current workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reconciliation {
    pub can_continue: bool,
    pub reasons: Vec<ReconcileReason>,
}

/// Read one git value in `root`, or `None` when git or the value is unavailable.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Capture the repository identity a bundle is being written against.
pub fn capture_repository(root: &Path) -> RepositoryState {
    RepositoryState {
        branch: git(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        revision: git(root, &["rev-parse", "HEAD"]),
        dirty: git(root, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty()),
    }
}

/// Build a declared manifest from a live session and the state around it.
///
/// Attempts come from the recorded turn receipts; pending approvals from the
/// session's approval ledger. Verification starts empty: this build has no
/// per-session verification store, and an invented pass would be a lie.
pub fn capture(
    root: &Path,
    session: &Session,
    goals: Vec<GoalState>,
    constraints: Vec<String>,
) -> HandoffManifest {
    let attempts = session
        .receipts
        .iter()
        .enumerate()
        .map(|(index, receipt)| AttemptRecord {
            label: format!("turn {}", receipt.turn_id.unwrap_or(index)),
            at_ms: receipt.at_ms,
            outcome: "completed".to_string(),
            detail: receipt.prefix_hash.clone(),
        })
        .collect();
    let pending_approvals = session
        .approvals
        .iter()
        .map(|approval| PendingApproval {
            tool: approval.tool.clone(),
            subject: approval.subject.clone(),
            approved: approval.approved,
            approved_by: approval.approved_by.clone(),
            at_ms: approval.at_ms,
        })
        .collect();
    HandoffManifest {
        schema: HANDOFF_SCHEMA.to_string(),
        session: session.id.clone(),
        captured_ms: memnoth_session::now_ms(),
        goals,
        constraints,
        repository: capture_repository(root),
        attempts,
        verification: Vec::new(),
        pending_approvals,
    }
}

/// Recursively remove hidden-reasoning keys from a JSON value.
pub fn strip_hidden_reasoning(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| {
                !HIDDEN_REASONING_KEYS
                    .iter()
                    .any(|hidden| key.eq_ignore_ascii_case(hidden))
            });
            for child in map.values_mut() {
                strip_hidden_reasoning(child);
            }
        }
        Value::Array(items) => {
            for child in items {
                strip_hidden_reasoning(child);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Whether a bundle declares the `mn-handoff/1` shape.
pub fn is_declared(bundle: &Value) -> bool {
    bundle.get("schema").and_then(Value::as_str) == Some(HANDOFF_SCHEMA)
        || bundle.get(HANDOFF_KEY).is_some()
}

/// Embed a manifest into a bundle, declaring the schema and stripping hidden
/// reasoning from the whole document before it is written.
pub fn embed(bundle: &mut Value, manifest: &HandoffManifest) -> Result<()> {
    let object = bundle
        .as_object_mut()
        .context("bundle is not a JSON object")?;
    object.insert("schema".to_string(), json!(HANDOFF_SCHEMA));
    object.insert(HANDOFF_KEY.to_string(), manifest.to_value()?);
    strip_hidden_reasoning(bundle);
    Ok(())
}

/// Read the declared manifest from a bundle, if it carries one.
pub fn extract(bundle: &Value) -> Option<HandoffManifest> {
    serde_json::from_value(bundle.get(HANDOFF_KEY)?.clone()).ok()
}

/// Reconcile a recorded manifest against the current workspace.
///
/// A legacy bundle has no manifest and is never passed here — that is the
/// backward-compatible path. A declared bundle must match on branch and
/// revision, arrive in a clean worktree, and carry no check recorded against
/// an older revision.
pub fn reconcile(manifest: &HandoffManifest, root: &Path) -> Reconciliation {
    let current = capture_repository(root);
    const UNAVAILABLE: &str = "<unavailable>";

    let mut reasons = Vec::new();

    match (&manifest.repository.revision, &current.revision) {
        (Some(recorded), Some(now)) if recorded != now => {
            reasons.push(ReconcileReason::RevisionChanged {
                recorded: recorded.clone(),
                current: now.clone(),
            });
        }
        (Some(recorded), None) => {
            reasons.push(ReconcileReason::RevisionChanged {
                recorded: recorded.clone(),
                current: UNAVAILABLE.to_string(),
            });
        }
        _ => {}
    }

    match (&manifest.repository.branch, &current.branch) {
        (Some(recorded), Some(now)) if recorded != now => {
            reasons.push(ReconcileReason::BranchChanged {
                recorded: recorded.clone(),
                current: now.clone(),
            });
        }
        (Some(recorded), None) => {
            reasons.push(ReconcileReason::BranchChanged {
                recorded: recorded.clone(),
                current: UNAVAILABLE.to_string(),
            });
        }
        _ => {}
    }

    if current.dirty {
        reasons.push(ReconcileReason::DirtyWorktree);
    }

    if let Some(current_revision) = &current.revision {
        for check in &manifest.verification {
            if let Some(recorded_revision) = &check.revision
                && recorded_revision != current_revision
            {
                reasons.push(ReconcileReason::StaleVerification {
                    name: check.name.clone(),
                    recorded_revision: recorded_revision.clone(),
                    current_revision: current_revision.clone(),
                });
            }
        }
    }

    Reconciliation {
        can_continue: reasons.is_empty(),
        reasons,
    }
}

/// Restore carried approval rows into a session on the receiving machine.
/// Only declared bundles carry these; legacy bundles keep their old behaviour.
pub fn restore_approvals(session: &mut Session, manifest: &HandoffManifest) {
    for approval in &manifest.pending_approvals {
        session.record_approval(ApprovalRecord {
            record: ApprovalRecord::KIND.to_string(),
            at_ms: approval.at_ms,
            tool: approval.tool.clone(),
            subject: approval.subject.clone(),
            approved: approval.approved,
            approved_by: approval.approved_by.clone(),
            rule: "handoff".to_string(),
            turn_id: None,
        });
    }
}
