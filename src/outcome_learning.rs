//! Outcome learning (ALG16): turn verified outcomes into *reviewed* procedure
//! candidates.
//!
//! A raw success signal ([`ObservedOutcome::score`]) is evidence, not authority.
//! A perfect score never promotes a procedure into active use: [`learn`] always
//! writes a [`CandidateStatus::PendingReview`] candidate and only an explicit
//! human [`AcceptanceRecord`] can move it to [`CandidateStatus::Accepted`].
//!
//! That is the reward-hacking guard. An agent can manufacture a flattering score
//! by touching the grader, a credential or another protected path; [`learn`]
//! refuses such an outcome outright, however good the number looks. Everything
//! else observed on the way is preserved on the candidate: the failures, the
//! scope it applies to, and an expiry so a once-useful procedure cannot live
//! forever.
//!
//! The module is pure: no files, no network, no model. Callers own persistence
//! (an in-memory [`LearningStore`] is provided) and the human approval UI.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Unix seconds. An integer keeps stored documents independent of a chrono
/// serde feature and lets tests use fixed, deterministic times.
pub type Timestamp = i64;

/// Schema tag for stored candidate documents.
pub const SCHEMA: &str = "mn-outcome-learning/1";

/// Protected path fragments that are refused regardless of policy. A substring
/// match is deliberate: `repo/.git/config`, `.env.local` and `id_rsa.pem` must
/// all be caught. They are the common ways to fake a good score rather than
/// earn it.
const ALWAYS_FORBIDDEN: &[&str] = &[
    ".git/",
    ".env",
    "id_rsa",
    "private_key",
    "credential",
    "secret",
];

/// Current time as unix seconds. Kept separate so the core functions take an
/// explicit `now` and stay deterministic under test.
pub fn now() -> Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Where a procedure may be applied. A candidate matches a query when the
/// project agrees and every tag the candidate requires is present in the query.
/// This is what keeps a procedure learned in one project out of another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub project: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Scope {
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            tags: Vec::new(),
        }
    }

    /// Whether a candidate carrying this scope is applicable to `query`.
    pub fn matches(&self, query: &Scope) -> bool {
        self.project == query.project
            && self
                .tags
                .iter()
                .all(|tag| query.tags.iter().any(|q| q == tag))
    }
}

/// The reusable steps learned from an outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Procedure {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<String>,
}

/// A failure observed while producing the outcome.
///
/// Failures are kept even when the final score is high: a procedure that only
/// worked after three retries is not the same procedure as one that worked first
/// try, and a reviewer needs to see that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub step: String,
    pub reason: String,
    pub recovered: bool,
}

/// Provenance of the outcome a candidate was learned from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeProvenance {
    pub session: String,
    pub task: String,
    pub actor: String,
    pub source: String,
    pub recorded_at: Timestamp,
}

/// Evidence that the outcome was actually verified, not merely claimed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verification {
    pub verifier: String,
    #[serde(default)]
    pub checks: Vec<String>,
    pub at: Timestamp,
}

/// An observed outcome, before it is verified.
///
/// `score` is the raw success signal. It is recorded for the reviewer and never
/// used to advance the candidate's status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservedOutcome {
    pub id: String,
    pub score: f64,
    pub procedure: Procedure,
    #[serde(default)]
    pub touched_paths: Vec<String>,
    #[serde(default)]
    pub failures: Vec<Failure>,
    pub provenance: OutcomeProvenance,
    pub scope: Scope,
}

/// An outcome with its verification attached. Only verified outcomes can become
/// procedure candidates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifiedOutcome {
    pub outcome: ObservedOutcome,
    pub verification: Verification,
}

/// The human acceptance record that makes a candidate applicable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceRecord {
    /// Identity of the person who accepted. Must not be empty: an acceptance
    /// with no acceptor is not a human record.
    pub accepted_by: String,
    pub accepted_at: Timestamp,
    #[serde(default)]
    pub note: Option<String>,
}

/// A rejection, with the reason preserved verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub rejected_by: String,
    pub rejected_at: Timestamp,
    pub reason: String,
}

/// Lifecycle of a procedure candidate. Nothing starts accepted; acceptance and
/// rejection both require an explicit record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CandidateStatus {
    /// Written by [`learn`]. A candidate in this state is never applied.
    PendingReview,
    Accepted(AcceptanceRecord),
    Rejected(Rejection),
}

/// A reviewed procedure candidate stored for human decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcedureCandidate {
    pub id: String,
    pub schema: String,
    pub procedure: Procedure,
    /// Provenance of the originating outcome, preserved unchanged.
    pub provenance: OutcomeProvenance,
    /// The verification that promoted an observation into a candidate.
    pub verification: Verification,
    pub scope: Scope,
    pub status: CandidateStatus,
    /// Failures from the originating outcome, preserved unchanged.
    #[serde(default)]
    pub failures: Vec<Failure>,
    /// Raw score, kept as evidence for the reviewer. It has no effect on status.
    pub score: f64,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
}

impl ProcedureCandidate {
    pub fn is_pending(&self) -> bool {
        matches!(self.status, CandidateStatus::PendingReview)
    }

    pub fn is_accepted(&self) -> bool {
        matches!(self.status, CandidateStatus::Accepted(_))
    }

    pub fn is_rejected(&self) -> bool {
        matches!(self.status, CandidateStatus::Rejected(_))
    }

    /// The preserved rejection reason, if this candidate was rejected.
    pub fn rejection_reason(&self) -> Option<&str> {
        match &self.status {
            CandidateStatus::Rejected(rejection) => Some(&rejection.reason),
            _ => None,
        }
    }

    pub fn is_expired(&self, at: Timestamp) -> bool {
        at >= self.expires_at
    }

    /// Whether this candidate is usable: accepted by a human, not expired, and
    /// in scope. A pending candidate is never usable, no matter its score.
    pub fn applies_to(&self, scope: &Scope, at: Timestamp) -> bool {
        self.is_accepted() && !self.is_expired(at) && self.scope.matches(scope)
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }

    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }
}

/// Rules applied when learning from a verified outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearningPolicy {
    /// Additional protected paths (exact file or directory prefix). Touching one
    /// is treated as reward hacking and refuses the candidate entirely.
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
    /// Lifetime of a new candidate, in seconds.
    pub ttl_secs: Timestamp,
}

impl Default for LearningPolicy {
    fn default() -> Self {
        Self {
            forbidden_paths: Vec::new(),
            ttl_secs: 90 * 24 * 60 * 60,
        }
    }
}

/// Why a learning or review transition was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LearningError {
    /// The outcome touched a protected path. No candidate is created.
    ForbiddenPath { path: String, rule: String },
    /// An acceptance must name a human acceptor.
    EmptyAcceptance,
    /// A rejection must carry a reason.
    EmptyRejection,
    /// Only a pending candidate can be accepted or rejected.
    NotPendingReview,
}

impl std::fmt::Display for LearningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ForbiddenPath { path, rule } => {
                write!(f, "outcome touched forbidden path `{path}` (rule `{rule}`)")
            }
            Self::EmptyAcceptance => write!(f, "acceptance must name a human acceptor"),
            Self::EmptyRejection => write!(f, "rejection must carry a reason"),
            Self::NotPendingReview => write!(f, "candidate is no longer pending review"),
        }
    }
}

impl std::error::Error for LearningError {}

/// Find the first protected rule a touched path violates.
fn forbidden_rule(path: &str, policy: &LearningPolicy) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    for fragment in ALWAYS_FORBIDDEN {
        if lower.contains(*fragment) {
            return Some((*fragment).to_string());
        }
    }
    for rule in &policy.forbidden_paths {
        let rule = rule.trim_end_matches('/');
        let prefix = format!("{rule}/");
        if path == rule || path.starts_with(prefix.as_str()) {
            return Some(rule.to_string());
        }
    }
    None
}

/// Turn a verified outcome into a pending procedure candidate.
///
/// Invariants, regardless of `score`:
/// * a protected path refuses the candidate outright ([`LearningError::ForbiddenPath`]);
/// * the new candidate is always [`CandidateStatus::PendingReview`] — a raw
///   success signal is never auto-promoted to applied;
/// * failures, scope and provenance are carried across unchanged;
/// * `expires_at` is fixed at `now + policy.ttl_secs`.
pub fn learn(
    verified: VerifiedOutcome,
    policy: &LearningPolicy,
    now: Timestamp,
) -> Result<ProcedureCandidate, LearningError> {
    let outcome = verified.outcome;
    for path in &outcome.touched_paths {
        if let Some(rule) = forbidden_rule(path, policy) {
            return Err(LearningError::ForbiddenPath {
                path: path.clone(),
                rule,
            });
        }
    }

    let id = outcome.id.clone();
    Ok(ProcedureCandidate {
        id,
        schema: SCHEMA.to_string(),
        procedure: outcome.procedure,
        provenance: outcome.provenance,
        verification: verified.verification,
        scope: outcome.scope,
        status: CandidateStatus::PendingReview,
        failures: outcome.failures,
        score: outcome.score,
        created_at: now,
        expires_at: now.saturating_add(policy.ttl_secs),
    })
}

/// Record a human acceptance. Only a pending candidate can be accepted, and the
/// record must name an acceptor.
pub fn accept(
    candidate: &mut ProcedureCandidate,
    record: AcceptanceRecord,
) -> Result<(), LearningError> {
    if !candidate.is_pending() {
        return Err(LearningError::NotPendingReview);
    }
    if record.accepted_by.trim().is_empty() {
        return Err(LearningError::EmptyAcceptance);
    }
    candidate.status = CandidateStatus::Accepted(record);
    Ok(())
}

/// Record a rejection. The reason is preserved verbatim on the candidate.
pub fn reject(
    candidate: &mut ProcedureCandidate,
    rejection: Rejection,
) -> Result<(), LearningError> {
    if !candidate.is_pending() {
        return Err(LearningError::NotPendingReview);
    }
    if rejection.reason.trim().is_empty() {
        return Err(LearningError::EmptyRejection);
    }
    candidate.status = CandidateStatus::Rejected(rejection);
    Ok(())
}

/// A small in-memory store, useful for tests and a single-process caller. It
/// keeps provenance with each candidate and can list the candidates for a scope.
#[derive(Debug, Default, Clone)]
pub struct LearningStore {
    candidates: HashMap<String, ProcedureCandidate>,
}

impl LearningStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&mut self, candidate: ProcedureCandidate) {
        self.candidates.insert(candidate.id.clone(), candidate);
    }

    pub fn get(&self, id: &str) -> Option<&ProcedureCandidate> {
        self.candidates.get(id)
    }

    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Candidates whose scope is applicable to `query` and that are still live.
    /// Pending and rejected candidates are retained in the store but are not
    /// returned here: storing a candidate is not applying it.
    pub fn applicable_to(&self, query: &Scope, at: Timestamp) -> Vec<&ProcedureCandidate> {
        self.candidates
            .values()
            .filter(|c| c.applies_to(query, at))
            .collect()
    }
}
