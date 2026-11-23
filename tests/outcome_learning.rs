//! Focused tests for outcome learning (ALG16).
//!
//! The module is included by path so this suite compiles and runs before the
//! CLI wiring lands (another change owns `lib.rs`). Once the module is exported
//! from the library, this file can switch to `use memnoth::outcome_learning::…`
//! without changing any test body.

#[path = "../src/outcome_learning.rs"]
mod outcome_learning;

use outcome_learning::{
    AcceptanceRecord, CandidateStatus, Failure, LearningError, LearningPolicy, LearningStore,
    ObservedOutcome, OutcomeProvenance, Procedure, Rejection, Scope, Verification, VerifiedOutcome,
    accept, learn, now, reject,
};

const T0: i64 = 1_000;

fn policy() -> LearningPolicy {
    LearningPolicy {
        forbidden_paths: vec!["eval/grader.rs".to_string(), "qa/scoreboard".to_string()],
        ttl_secs: 100,
    }
}

fn verified(score: f64, paths: &[&str], failures: Vec<Failure>, scope: Scope) -> VerifiedOutcome {
    VerifiedOutcome {
        outcome: ObservedOutcome {
            id: "cand-1".to_string(),
            score,
            procedure: Procedure {
                name: "rotate-token".to_string(),
                steps: vec!["open settings".to_string(), "rotate".to_string()],
            },
            touched_paths: paths.iter().map(|p| (*p).to_string()).collect(),
            failures,
            provenance: OutcomeProvenance {
                session: "s-42".to_string(),
                task: "t-7".to_string(),
                actor: "agent-1".to_string(),
                source: "session-transcript".to_string(),
                recorded_at: 90,
            },
            scope,
        },
        verification: Verification {
            verifier: "ci".to_string(),
            checks: vec!["cargo test".to_string(), "cargo clippy".to_string()],
            at: 95,
        },
    }
}

fn failures() -> Vec<Failure> {
    vec![
        Failure {
            step: "open settings".to_string(),
            reason: "dialog was slow".to_string(),
            recovered: true,
        },
        Failure {
            step: "rotate".to_string(),
            reason: "permission denied once".to_string(),
            recovered: false,
        },
    ]
}

fn scope() -> Scope {
    let mut s = Scope::new("alpha");
    s.tags.push("rust".to_string());
    s
}

/// A verified outcome produces a pending candidate, never an applied one. The
/// failures, scope, provenance and expiry all survive the transition.
#[test]
fn verified_outcome_creates_candidate_not_applied() {
    let observed = verified(0.82, &["src/app.rs"], failures(), scope());
    let expected_provenance = observed.outcome.provenance.clone();
    let expected_verification = observed.verification.clone();
    let expected_failures = observed.outcome.failures.clone();
    let expected_scope = observed.outcome.scope.clone();

    let candidate = learn(observed, &policy(), T0).unwrap();

    assert_eq!(candidate.id, "cand-1");
    assert_eq!(candidate.schema, "mn-outcome-learning/1");
    assert!(candidate.is_pending());
    assert_eq!(candidate.status, CandidateStatus::PendingReview);
    assert!(!candidate.applies_to(&scope(), T0 + 1));

    assert_eq!(candidate.score, 0.82);
    assert_eq!(
        candidate.failures, expected_failures,
        "failures are preserved"
    );
    assert_eq!(candidate.scope, expected_scope, "scope is preserved");
    assert_eq!(
        candidate.provenance, expected_provenance,
        "provenance is stored"
    );
    assert_eq!(candidate.verification, expected_verification);
    assert_eq!(candidate.created_at, T0);
    assert_eq!(
        candidate.expires_at,
        T0 + 100,
        "expiry is fixed at learn time"
    );

    let mut store = LearningStore::new();
    assert!(store.is_empty());
    store.put(candidate);
    assert_eq!(store.len(), 1);
    assert!(!store.is_empty());
    assert!(store.get("cand-1").is_some());
    assert!(
        store.applicable_to(&scope(), T0 + 1).is_empty(),
        "storing is not applying"
    );
}

/// A perfect raw success signal is still only a pending candidate. This is the
/// core reward-hacking guard: score is evidence for the reviewer, not authority.
#[test]
fn a_perfect_score_never_auto_promotes() {
    // `now` is a real clock helper; the core takes an explicit time so tests
    // stay deterministic, but the helper must still be usable by callers.
    assert!(now() > 0);
    let candidate = learn(
        verified(1.0, &["src/app.rs"], Vec::new(), scope()),
        &policy(),
        T0,
    )
    .unwrap();

    assert!(candidate.is_pending());
    assert!(!candidate.is_accepted());
    assert!(!candidate.applies_to(&scope(), T0 + 1));
    assert!(
        candidate.to_json().unwrap().contains("PendingReview"),
        "the stored document records the pending status"
    );
}

/// Acceptance by a human is required before a candidate applies.
#[test]
fn acceptance_is_required_before_a_candidate_applies() {
    let mut candidate = learn(
        verified(0.9, &["src/app.rs"], Vec::new(), scope()),
        &policy(),
        T0,
    )
    .unwrap();
    assert!(!candidate.applies_to(&scope(), T0 + 1));

    // An "acceptance" with no acceptor is not a human record.
    let empty = AcceptanceRecord {
        accepted_by: "  ".to_string(),
        accepted_at: T0 + 1,
        note: None,
    };
    assert_eq!(
        accept(&mut candidate, empty),
        Err(LearningError::EmptyAcceptance)
    );
    assert!(candidate.is_pending());

    let record = AcceptanceRecord {
        accepted_by: "alice".to_string(),
        accepted_at: T0 + 2,
        note: Some("checked the verification".to_string()),
    };
    accept(&mut candidate, record.clone()).unwrap();
    assert!(candidate.is_accepted());
    assert_eq!(candidate.status, CandidateStatus::Accepted(record));
    assert!(candidate.applies_to(&scope(), T0 + 2));

    // Acceptance is a one-way, single transition.
    assert_eq!(
        accept(
            &mut candidate,
            AcceptanceRecord {
                accepted_by: "bob".to_string(),
                accepted_at: T0 + 3,
                note: None,
            }
        ),
        Err(LearningError::NotPendingReview)
    );
}

/// Rejection records the reason verbatim and the candidate never applies.
#[test]
fn rejection_preserves_reason() {
    let mut candidate = learn(
        verified(0.8, &["src/app.rs"], Vec::new(), scope()),
        &policy(),
        T0,
    )
    .unwrap();

    let rejection = Rejection {
        rejected_by: "alice".to_string(),
        rejected_at: T0 + 4,
        reason: "score came from a stale cache, not the change".to_string(),
    };
    let expected = rejection.reason.clone();
    reject(&mut candidate, rejection.clone()).unwrap();

    assert!(candidate.is_rejected());
    assert_eq!(candidate.status, CandidateStatus::Rejected(rejection));
    assert_eq!(candidate.rejection_reason(), Some(expected.as_str()));
    assert!(!candidate.applies_to(&scope(), T0 + 5));

    // The reason survives storage and retrieval.
    let restored = OutcomeCandidateRoundTrip::restore(&candidate);
    assert_eq!(restored.rejection_reason(), Some(expected.as_str()));

    // A rejected candidate cannot be accepted, and needs a reason to reject.
    assert_eq!(
        accept(
            &mut candidate,
            AcceptanceRecord {
                accepted_by: "bob".to_string(),
                accepted_at: T0 + 6,
                note: None,
            }
        ),
        Err(LearningError::NotPendingReview)
    );
    let mut fresh = learn(
        verified(0.7, &["src/app.rs"], Vec::new(), scope()),
        &policy(),
        T0,
    )
    .unwrap();
    assert_eq!(
        reject(
            &mut fresh,
            Rejection {
                rejected_by: "alice".to_string(),
                rejected_at: T0 + 7,
                reason: String::new(),
            }
        ),
        Err(LearningError::EmptyRejection)
    );
}

/// Tiny helper that makes the JSON round-trip assertion read clearly.
struct OutcomeCandidateRoundTrip;

impl OutcomeCandidateRoundTrip {
    fn restore(
        candidate: &outcome_learning::ProcedureCandidate,
    ) -> outcome_learning::ProcedureCandidate {
        outcome_learning::ProcedureCandidate::from_json(&candidate.to_json().unwrap()).unwrap()
    }
}

/// An accepted candidate stops applying once it expires.
#[test]
fn expiry_stops_application() {
    let mut candidate = learn(
        verified(0.9, &["src/app.rs"], Vec::new(), scope()),
        &policy(),
        T0,
    )
    .unwrap();
    accept(
        &mut candidate,
        AcceptanceRecord {
            accepted_by: "alice".to_string(),
            accepted_at: T0 + 1,
            note: None,
        },
    )
    .unwrap();

    assert!(!candidate.is_expired(T0 + 99));
    assert!(candidate.applies_to(&scope(), T0 + 99));
    // At the boundary the candidate is expired and no longer applies.
    assert!(candidate.is_expired(T0 + 100));
    assert!(!candidate.applies_to(&scope(), T0 + 100));
    assert!(!candidate.applies_to(&scope(), T0 + 1_000));
}

/// A candidate learned for one scope does not apply to another, including when
/// a store holds candidates for several scopes.
#[test]
fn scope_isolation() {
    let mut candidate = learn(
        verified(0.9, &["src/app.rs"], Vec::new(), scope()),
        &policy(),
        T0,
    )
    .unwrap();
    accept(
        &mut candidate,
        AcceptanceRecord {
            accepted_by: "alice".to_string(),
            accepted_at: T0 + 1,
            note: None,
        },
    )
    .unwrap();

    assert!(candidate.applies_to(&scope(), T0 + 1));

    let mut without_tag = scope();
    without_tag.tags.clear();
    assert!(
        !candidate.applies_to(&without_tag, T0 + 1),
        "required scope tags must be present"
    );

    assert!(
        !candidate.applies_to(&Scope::new("beta"), T0 + 1),
        "a different project is isolated"
    );

    let mut beta = verified(0.95, &["src/app.rs"], Vec::new(), Scope::new("beta"));
    beta.outcome.id = "cand-2".to_string();
    let mut beta = learn(beta, &policy(), T0).unwrap();
    accept(
        &mut beta,
        AcceptanceRecord {
            accepted_by: "bob".to_string(),
            accepted_at: T0 + 1,
            note: None,
        },
    )
    .unwrap();

    let mut store = LearningStore::new();
    store.put(candidate);
    store.put(beta);
    assert_eq!(store.len(), 2);
    let alpha_ids: Vec<_> = store
        .applicable_to(&scope(), T0 + 1)
        .iter()
        .map(|c| c.id.clone())
        .collect();
    assert_eq!(alpha_ids, vec!["cand-1"]);
    let beta_ids: Vec<_> = store
        .applicable_to(&Scope::new("beta"), T0 + 1)
        .iter()
        .map(|c| c.id.clone())
        .collect();
    assert_eq!(beta_ids, vec!["cand-2"]);
}

/// Reward-hack fixture: a high score earned by touching a forbidden path is
/// refused outright. Policy rules and built-in protected paths both apply.
#[test]
fn good_score_but_forbidden_path_is_refused() {
    let good_but_gamed = verified(1.0, &["eval/grader.rs"], Vec::new(), scope());
    assert_eq!(
        learn(good_but_gamed, &policy(), T0),
        Err(LearningError::ForbiddenPath {
            path: "eval/grader.rs".to_string(),
            rule: "eval/grader.rs".to_string(),
        })
    );

    let built_in = verified(0.99, &["repo/.git/config"], Vec::new(), scope());
    assert!(matches!(
        learn(built_in, &policy(), T0),
        Err(LearningError::ForbiddenPath { .. })
    ));

    let nested = verified(0.98, &["qa/scoreboard/index.js"], Vec::new(), scope());
    assert!(matches!(
        learn(nested, &policy(), T0),
        Err(LearningError::ForbiddenPath { .. })
    ));

    // A clean outcome with the same high score is accepted for review.
    let clean = learn(
        verified(1.0, &["src/app.rs"], Vec::new(), scope()),
        &policy(),
        T0,
    )
    .unwrap();
    assert!(clean.is_pending());
}
