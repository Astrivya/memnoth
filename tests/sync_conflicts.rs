//! ALG18 strategy acceptance tests for encrypted-sync conflict handling.
//!
//! Wiring-free harness, following `tests/belief_events.rs` and
//! `tests/event_sync.rs`: the parent task owns the `pub mod sync_conflicts;`
//! line in `src/lib.rs`, so this test includes the pure source directly. Rust
//! does not compile a stray `.rs` file on its own. Once the module is declared,
//! replace the `#[path]` include with `use memnoth::sync_conflicts::*`.
//!
//! Every case is local and deterministic: two in-memory replicas, no network,
//! no clock reads other than explicit `at_ms` values supplied by the test.

#[path = "../src/sync_conflicts.rs"]
mod sync_conflicts;

use sync_conflicts::{
    Change, ClockOrder, ConflictLog, ConflictReason, MissingEvent, Replica, ReviewDecision,
    Validity,
};

fn correct(key: &str, text: &str, from: i64, to: Option<i64>, provenance: &str) -> Change {
    Change::Correct {
        key: key.to_string(),
        text: text.to_string(),
        validity: Validity::new(from, to),
        provenance: provenance.to_string(),
    }
}

fn log_of(events: &[sync_conflicts::Event]) -> ConflictLog {
    let mut log = ConflictLog::new();
    for event in events {
        log.append(event.clone()).expect("event appends cleanly");
    }
    log
}

#[test]
fn two_replicas_edit_same_belief_both_kept() {
    let mut alice = Replica::new("alice");
    let mut bob = Replica::new("bob");

    // No `observe`: the two corrections are concurrent. Neither timestamp nor
    // origin name may pick a winner.
    let a = alice.emit(100, correct("budget", "cap is 50", 10, None, "alice:mail"));
    let b = bob.emit(900, correct("budget", "cap is 80", 10, None, "bob:meeting"));
    let log = log_of(&[b.clone(), a.clone()]); // arrival order reversed on purpose

    let state = log.replay();
    let view = state.belief("budget").expect("belief exists");
    assert_eq!(view.corrections.len(), 2, "both corrections are kept");
    assert!(view.corrections.contains_key(&a.id));
    assert!(view.corrections.contains_key(&b.id));
    assert_eq!(
        view.distinct_texts(),
        ["cap is 50", "cap is 80"].into_iter().collect()
    );

    // Replay is order-independent: reversing delivery derives the same state.
    assert_eq!(log_of(&[a.clone(), b.clone()]).replay(), state);

    // A conflict is surfaced with both sides and concrete reasons.
    let queue = log.review_queue();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].key, "budget");
    assert_eq!(queue[0].sides.len(), 2);
    let side_ids: Vec<_> = queue[0]
        .sides
        .iter()
        .map(|side| side.event_id.clone())
        .collect();
    assert!(side_ids.contains(&a.id) && side_ids.contains(&b.id));
    assert!(queue[0].reasons.contains(&ConflictReason::DivergentText));
    assert!(queue[0].reasons.contains(&ConflictReason::Concurrent));
    assert!(
        queue[0]
            .reasons
            .contains(&ConflictReason::OverlappingValidity)
    );

    // No silent winner.
    assert_eq!(log.current("budget"), None);
}

#[test]
fn disjoint_edits_merge() {
    let mut alice = Replica::new("alice");
    let mut bob = Replica::new("bob");
    let a = alice.emit(1, correct("alpha", "first", 0, None, "a"));
    let b = bob.emit(2, correct("beta", "second", 0, None, "b"));

    let mut left = log_of(&[a]);
    let right = log_of(&[b]);
    assert_eq!(left.merge(&right).expect("merge"), 1);
    assert_eq!(left.merge(&right).expect("idempotent merge"), 0);

    let state = left.replay();
    assert_eq!(state.current("alpha"), Some("first"));
    assert_eq!(state.current("beta"), Some("second"));
    assert!(
        left.review_queue().is_empty(),
        "disjoint edits need no review"
    );
    assert_eq!(left.len(), 2);
}

#[test]
fn vector_clock_detects_missing_event() {
    let mut alice = Replica::new("alice");
    let a1 = alice.emit(1, correct("k", "from alice", 0, None, "a"));

    let mut bob = Replica::new("bob");
    bob.observe_event(&a1); // bob's next clock references alice:1
    let b1 = bob.emit(2, correct("k", "from bob", 0, None, "b"));
    assert_eq!(b1.clock.get("alice"), 1, "the clock names alice's event");
    assert_eq!(a1.clock.compare(&b1.clock), ClockOrder::Before);

    // Only bob's event arrives. The referenced alice:1 is absent.
    let only_bob = vec![b1.clone()];
    let incomplete = log_of(&only_bob);
    assert_eq!(
        incomplete.detect_missing_events(),
        vec![MissingEvent {
            origin: "alice".to_string(),
            seq: 1,
        }]
    );
    // The gap is also surfaced as a conflict reason, once there is a real
    // disagreement to review. A single correction is not a conflict.
    let mut carol = Replica::new("carol");
    let c1 = carol.emit(3, correct("k", "from carol", 0, None, "c"));
    let with_conflict = log_of(&[b1.clone(), c1.clone()]);
    assert_eq!(with_conflict.review_queue().len(), 1);
    assert!(
        with_conflict.review_queue()[0]
            .reasons
            .contains(&ConflictReason::MissingEvent(MissingEvent {
                origin: "alice".to_string(),
                seq: 1,
            }))
    );

    // Delivering the referenced event closes the gap.
    let complete = log_of(&[a1, b1, c1]);
    assert!(complete.detect_missing_events().is_empty());
}

#[test]
fn conflict_requires_review_before_accepting() {
    let mut alice = Replica::new("alice");
    let mut bob = Replica::new("bob");
    let a = alice.emit(1, correct("k", "keep me", 5, Some(15), "a"));
    let b = bob.emit(2, correct("k", "also me", 5, Some(15), "b"));
    let mut log = log_of(&[a.clone(), b.clone()]);

    // Pending: there is no current value and the queue holds both sides.
    assert_eq!(log.current("k"), None);
    let items = log.review_queue();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].sides.len(), 2);

    // Validation: a decision must name a real correction and a real reviewer.
    assert!(matches!(
        log.accept(ReviewDecision::new("k", "no-such-id", "human:p", "why", 3)),
        Err(sync_conflicts::AcceptError::UnknownCorrection { .. })
    ));
    assert!(matches!(
        log.accept(ReviewDecision::new("k", a.id.clone(), " ", "why", 3)),
        Err(sync_conflicts::AcceptError::ReviewerRequired)
    ));
    assert!(matches!(
        log.accept(ReviewDecision::new("k", a.id.clone(), "human:p", " ", 3)),
        Err(sync_conflicts::AcceptError::ReasonRequired)
    ));

    // An explicit, attributed decision is the only way to pick a value.
    let resolution = log
        .accept(ReviewDecision::new(
            "k",
            a.id.clone(),
            "human:priya",
            "mail thread is authoritative",
            3,
        ))
        .expect("reviewed acceptance");
    assert!(resolution.id_matches());
    assert_eq!(log.current("k").as_deref(), Some("keep me"));
    assert!(log.review_queue().is_empty(), "the review is closed");

    // The resolution is recorded in the log, not substituted for the history.
    let record = log
        .replay()
        .belief("k")
        .unwrap()
        .resolution()
        .unwrap()
        .clone();
    assert_eq!(record.keep, a.id);
    assert_eq!(record.reviewer, "human:priya");
    assert_eq!(record.reason, "mail thread is authoritative");
}

#[test]
fn no_belief_is_silently_overwritten() {
    let mut alice = Replica::new("alice");
    let mut bob = Replica::new("bob");

    let a1 = alice.emit(1, correct("k", "A", 0, None, "a"));
    let b1 = bob.emit(2, correct("k", "B", 0, None, "b"));

    // Alice now *observes* bob and emits a causally later correction with a
    // much later wall clock. It must not become the winner by recency.
    alice.observe_event(&b1);
    let a2 = alice.emit(9_999, correct("k", "C", 0, None, "a-later"));
    assert_eq!(a2.clock.compare(&b1.clock), ClockOrder::After);

    let mut log = log_of(&[a1.clone(), b1.clone(), a2.clone()]);
    assert_eq!(
        log.current("k"),
        None,
        "a later event is still just a correction"
    );
    let view = log.replay().belief("k").unwrap().clone();
    assert_eq!(view.corrections.len(), 3, "all three corrections retained");
    assert_eq!(view.distinct_texts().len(), 3);
    assert_eq!(log.review_queue()[0].sides.len(), 3);

    // Only a review closes it, and the kept id is still present for audit.
    log.accept(ReviewDecision::new(
        "k",
        b1.id.clone(),
        "policy:senior-source",
        "bob owns this field",
        10_000,
    ))
    .expect("acceptance");
    assert_eq!(log.current("k").as_deref(), Some("B"));
    assert_eq!(log.replay().belief("k").unwrap().corrections.len(), 3);

    // A divergent correction arriving after acceptance re-opens the review;
    // it cannot ride on the earlier decision.
    let mut carol = Replica::new("carol");
    let c1 = carol.emit(3, correct("k", "D", 0, None, "c"));
    log.append(c1.clone()).expect("late correction");
    assert_eq!(log.current("k"), None);
    assert_eq!(log.review_queue().len(), 1);
}

#[test]
fn acceptance_is_refused_while_history_is_incomplete() {
    let mut alice = Replica::new("alice");
    let a1 = alice.emit(1, correct("k", "A", 0, None, "a"));
    let mut bob = Replica::new("bob");
    bob.observe_event(&a1);
    let b1 = bob.emit(2, correct("k", "B", 0, None, "b"));

    let mut carol = Replica::new("carol");
    let c1 = carol.emit(3, correct("k", "C", 0, None, "c"));
    let mut incomplete = log_of(&[b1.clone(), c1]);
    let verdict = incomplete.accept(ReviewDecision::new("k", b1.id.clone(), "human:p", "why", 3));
    assert!(matches!(
        verdict,
        Err(sync_conflicts::AcceptError::IncompleteHistory { .. })
    ));
    assert!(
        incomplete.review_queue()[0]
            .reasons
            .iter()
            .any(|reason| matches!(reason, ConflictReason::MissingEvent(_)))
    );
}

#[test]
fn public_surface_is_usable_by_wired_callers() {
    use sync_conflicts::VectorClock;

    let open = Validity::unbounded(7);
    assert!(open.contains(7));
    assert!(open.contains(i64::MAX - 1));
    assert!(!open.contains(6));

    let mut alice = Replica::new("alice");
    let event = alice.emit(1, correct("k", "A", 0, None, "a"));
    assert_eq!(event.key(), "k");
    assert_eq!(event.dot(), ("alice".to_string(), 1));
    let joined = VectorClock::merged(&alice.clock(), &event.clock);
    assert_eq!(joined.get("alice"), 1);

    let log = log_of(&[event]);
    assert!(!log.is_empty());
    assert_eq!(log.events().len(), 1);
    assert_eq!(log.timeline("k").len(), 1);
    assert!(ConflictLog::new().is_empty());
}
