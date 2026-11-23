//! ALG18 acceptance tests for the event/snapshot split.
//!
//! Wiring-free harness, following `tests/belief_events.rs`: the parent task owns
//! the `pub mod event_sync;` line in `src/lib.rs`, so this test includes the
//! source directly. Rust does not compile a stray `.rs` file on its own. Once
//! the module is declared, replace this include with `use memnoth::event_sync::*`.

#[path = "../src/event_sync.rs"]
mod event_sync;

use std::collections::BTreeSet;

use event_sync::{
    AppendError, Claim, ClockOrder, Event, EventLog, Gap, Operation, Replica, VectorClock, replay,
};

fn log_of(events: &[Event]) -> EventLog {
    let mut log = EventLog::new();
    for event in events {
        log.append(event.clone()).expect("event appends cleanly");
    }
    log
}

#[test]
fn concurrent_events_from_two_replicas_converge() {
    let mut alice = Replica::new("alice");
    let mut bob = Replica::new("bob");

    // No `observe` calls: neither writer knows about the other, so their
    // events are concurrent and must be merged, not ordered by arrival.
    let alice_counter = alice.emit(
        "obj",
        100,
        Operation::CounterAdd {
            counter: "visits".into(),
            delta: 2,
        },
    );
    let alice_tag = alice.emit(
        "obj",
        101,
        Operation::SetAdd {
            set: "tags".into(),
            element: "alpha".into(),
        },
    );
    let bob_counter = bob.emit(
        "obj",
        200,
        Operation::CounterAdd {
            counter: "visits".into(),
            delta: 3,
        },
    );
    let bob_tag = bob.emit(
        "obj",
        201,
        Operation::SetAdd {
            set: "tags".into(),
            element: "beta".into(),
        },
    );

    assert_eq!(
        alice_counter.clock.compare(&bob_counter.clock),
        ClockOrder::Concurrent
    );

    // Deliver in opposite orders. Derived snapshots must be identical.
    let forward = log_of(&[
        alice_counter.clone(),
        alice_tag.clone(),
        bob_counter.clone(),
        bob_tag.clone(),
    ]);
    let reverse = log_of(&[
        bob_tag.clone(),
        bob_counter.clone(),
        alice_tag.clone(),
        alice_counter.clone(),
    ]);
    assert_eq!(forward.replay(), reverse.replay());

    let snapshot = forward.replay();
    assert_eq!(snapshot.counter("obj", "visits"), 5);
    assert_eq!(
        snapshot.set("obj", "tags"),
        BTreeSet::from(["alpha".to_string(), "beta".to_string()])
    );
    assert_eq!(
        snapshot.clock("obj"),
        VectorClock::merged(
            &VectorClock::from_ticks([("alice", 2)]),
            &VectorClock::from_ticks([("bob", 2)])
        )
    );

    // The log is a set of ids: re-merging the same events adds nothing.
    assert_eq!(forward.events().len(), 4);
    let mut merged = forward.clone();
    assert_eq!(merged.merge(&reverse).unwrap(), 0);
    assert_eq!(merged, forward);
    assert!(merged.gaps().is_empty());
}

#[test]
fn vector_clock_detects_gap() {
    let first = Event::new(
        "obj",
        "alice",
        1,
        VectorClock::from_ticks([("alice", 1)]),
        1,
        Operation::CounterAdd {
            counter: "c".into(),
            delta: 1,
        },
    );
    // Event 3 names a clock that includes alice:3, but alice:2 never arrived.
    let third = Event::new(
        "obj",
        "alice",
        3,
        VectorClock::from_ticks([("alice", 3)]),
        3,
        Operation::CounterAdd {
            counter: "c".into(),
            delta: 1,
        },
    );

    assert_eq!(third.dot(), ("alice".to_string(), 3));
    let complete = log_of(std::slice::from_ref(&first));
    assert!(complete.gaps().is_empty());

    let mut incomplete = log_of(&[first, third]);
    assert_eq!(
        incomplete.gaps(),
        vec![Gap {
            origin: "alice".into(),
            seq: 2,
        }]
    );

    // Filling the hole clears the gap and makes the dot sequence consistent.
    let second = Event::new(
        "obj",
        "alice",
        2,
        VectorClock::from_ticks([("alice", 2)]),
        2,
        Operation::CounterAdd {
            counter: "c".into(),
            delta: 1,
        },
    );
    incomplete.append(second).unwrap();
    assert!(incomplete.gaps().is_empty());
    assert_eq!(incomplete.replay().counter("obj", "c"), 3);

    // A second event with the same dot is refused rather than overwriting.
    let duplicate_dot = Event::new(
        "obj",
        "alice",
        2,
        VectorClock::from_ticks([("alice", 2)]),
        99,
        Operation::CounterAdd {
            counter: "other".into(),
            delta: 9,
        },
    );
    assert!(matches!(
        incomplete.append(duplicate_dot),
        Err(AppendError::DuplicateDot { .. })
    ));
}

#[test]
fn semantic_conflict_surfaces_for_review() {
    let mut alice = Replica::new("alice");
    let mut bob = Replica::new("bob");

    let left = alice.emit(
        "project",
        1,
        Operation::AssertBelief {
            key: "status".into(),
            claim: Claim::new("shipped", "alice/notes"),
        },
    );
    let right = bob.emit(
        "project",
        2,
        Operation::AssertBelief {
            key: "status".into(),
            claim: Claim::new("not shipped", "bob/notes"),
        },
    );

    // Neither clock dominates the other, so no causal rule can pick a winner.
    assert_eq!(left.clock.compare(&right.clock), ClockOrder::Concurrent);

    let snapshot = log_of(&[left.clone(), right.clone()]).replay();
    let view = snapshot.belief("project", "status").unwrap();
    assert!(view.needs_review());
    assert_eq!(view.claims.len(), 2);
    assert_eq!(view.current_text(), None);

    let review = snapshot.review_queue();
    assert_eq!(review.len(), 1);
    assert_eq!(review[0].object, "project");
    assert_eq!(review[0].key, "status");
    let texts: BTreeSet<&str> = review[0]
        .claims
        .iter()
        .map(|(_, claim)| claim.text.as_str())
        .collect();
    assert_eq!(texts, BTreeSet::from(["shipped", "not shipped"]));

    // Same text with different provenance is agreement, not a conflict.
    let mut carol = Replica::new("carol");
    let agreeing = carol.emit(
        "project",
        3,
        Operation::AssertBelief {
            key: "agreed".into(),
            claim: Claim::new("true", "carol/notes"),
        },
    );
    let dave_agreeing = Event::new(
        "project",
        "dave",
        1,
        VectorClock::from_ticks([("dave", 1)]),
        4,
        Operation::AssertBelief {
            key: "agreed".into(),
            claim: Claim::new("true", "dave/notes"),
        },
    );
    let agreed_snapshot = log_of(&[agreeing, dave_agreeing]).replay();
    assert!(
        !agreed_snapshot
            .belief("project", "agreed")
            .unwrap()
            .needs_review()
    );

    // CRDT data beside the conflict still converges; there is no global
    // "needs review" lock on the rest of the snapshot.
    assert!(
        snapshot
            .review_queue()
            .iter()
            .all(|item| item.key == "status")
    );
}

#[test]
fn replay_reproduces_a_snapshot_from_events() {
    let mut writer = Replica::new("writer");
    let add_x = writer.emit(
        "o",
        1,
        Operation::SetAdd {
            set: "s".into(),
            element: "x".into(),
        },
    );
    let add_y = writer.emit(
        "o",
        2,
        Operation::SetAdd {
            set: "s".into(),
            element: "y".into(),
        },
    );
    let remove_x = writer.emit(
        "o",
        3,
        Operation::SetRemove {
            set: "s".into(),
            element: "x".into(),
            observed_adds: vec![add_x.id.clone()],
        },
    );
    let plus = writer.emit(
        "o",
        4,
        Operation::CounterAdd {
            counter: "c".into(),
            delta: 7,
        },
    );
    let minus = writer.emit(
        "o",
        5,
        Operation::CounterAdd {
            counter: "c".into(),
            delta: -2,
        },
    );
    let belief = writer.emit(
        "o",
        6,
        Operation::AssertBelief {
            key: "k".into(),
            claim: Claim::new("settled", "writer"),
        },
    );

    let events = [
        add_x.clone(),
        add_y.clone(),
        remove_x.clone(),
        plus.clone(),
        minus.clone(),
        belief.clone(),
    ];
    let log = log_of(&events);
    let snapshot = log.replay();

    assert_eq!(snapshot.set("o", "s"), BTreeSet::from(["y".to_string()]));
    assert_eq!(snapshot.counter("o", "c"), 5);
    let view = snapshot.belief("o", "k").unwrap();
    assert!(!view.needs_review());
    assert_eq!(view.current_text(), Some("settled"));
    assert_eq!(view.claims.len(), 1);
    assert!(view.claims.contains_key(&belief.id));
    assert_eq!(snapshot.clock("o"), writer.clock("o"));

    // Folding in any order rebuilds the same snapshot, including the
    // observed-remove set (the remove names the add it saw by event id).
    let mut shuffled = events.clone();
    shuffled.reverse();
    assert_eq!(replay(&shuffled), snapshot);
    let rotated = [
        events[2].clone(),
        events[3].clone(),
        events[4].clone(),
        events[5].clone(),
        events[0].clone(),
        events[1].clone(),
    ];
    assert_eq!(replay(&rotated), snapshot);

    // The ids are derived from fields, not from arrival order or time of day.
    let rebuilt = Event::new(
        "o",
        "writer",
        add_x.seq,
        add_x.clock.clone(),
        add_x.at_ms,
        add_x.payload.clone(),
    );
    assert_eq!(rebuilt.id, add_x.id);
    for event in &events {
        assert!(event.id_matches(), "{} must verify", event.id);
    }
}

#[test]
fn a_belief_is_never_silently_overwritten() {
    let mut alice = Replica::new("alice");
    let first = alice.emit(
        "o",
        1,
        Operation::AssertBelief {
            key: "k".into(),
            claim: Claim::new("v1", "alice"),
        },
    );

    let mut bob = Replica::new("bob");
    // Bob observes Alice, so his later claim causally dominates hers. A
    // timestamp/LWW engine would overwrite; this one must still ask a human.
    bob.observe_event(&first);
    let later = bob.emit(
        "o",
        2,
        Operation::AssertBelief {
            key: "k".into(),
            claim: Claim::new("v2", "bob"),
        },
    );
    assert_eq!(first.clock.compare(&later.clock), ClockOrder::Before);

    let mut log = log_of(&[first.clone(), later.clone()]);
    let snapshot = log.replay();
    let view = snapshot.belief("o", "k").unwrap();
    assert_eq!(view.claims.len(), 2);
    assert!(view.claims.contains_key(&first.id));
    assert!(view.claims.contains_key(&later.id));
    assert!(view.needs_review());
    assert_eq!(view.current_text(), None);
    assert_eq!(snapshot.review_queue().len(), 1);

    // Only an explicit review event resolves the disagreement. The replaced
    // claim is retained in the log/view for audit, not deleted.
    let mut reviewer = Replica::new("reviewer");
    reviewer.observe_event(&first);
    reviewer.observe_event(&later);
    let resolution = reviewer.emit(
        "o",
        3,
        Operation::ResolveBelief {
            key: "k".into(),
            keep: later.id.clone(),
        },
    );
    log.append(resolution).unwrap();

    let resolved = log.replay();
    let view = resolved.belief("o", "k").unwrap();
    assert!(!view.needs_review());
    assert_eq!(view.current_text(), Some("v2"));
    assert_eq!(view.resolved_claim().unwrap().0, &later.id);
    assert!(
        view.claims.contains_key(&first.id),
        "v1 was explicitly superseded, never silently overwritten"
    );
    assert!(resolved.review_queue().is_empty());
}
