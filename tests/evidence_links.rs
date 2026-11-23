//! ALG11 tests — typed evidence links and the review queue.
//!
//! Self-contained: the module under test is included by path so these tests run
//! before `lib.rs` gains its `pub mod evidence_links;` line. Once wired, the
//! path include can be replaced by `use memnoth::evidence_links::*`.
//!
//! All four required properties are covered:
//! 1. a link is created with the source identity;
//! 2. a source revision change enqueues a review item;
//! 3. the accepted fact text is unchanged by that enqueue;
//! 4. an unchanged revision produces no review item.

use std::sync::{Arc, Mutex};

use memnoth_engine::{
    AuditEvent, AuditLedger, CallbackSink, EntityKind, MemoryEngine, open_in_memory,
};
use serde_json::json;

#[path = "../src/evidence_links.rs"]
mod evidence_links;

use evidence_links::{
    EVIDENCE_FOR, NEEDS_REVIEW, SourceIdentity, link_evidence_for, record_source_revision,
    review_queue,
};

const BASE: i64 = 1_700_000_000_000;
const FACT: &str = "Rate limiting uses a token bucket";

/// Build an in-memory vault with an attached ledger whose events we can inspect.
fn setup() -> (MemoryEngine, Arc<Mutex<Vec<AuditEvent>>>) {
    let engine = open_in_memory().expect("in-memory vault");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let mut ledger = AuditLedger::new();
    ledger.add_sink(Box::new(CallbackSink::new(move |event| {
        sink.lock().expect("audit mutex").push(event.clone());
        Ok(())
    })));
    engine.attach_audit(ledger);
    (engine, captured)
}

fn source(revision: &str) -> SourceIdentity {
    SourceIdentity {
        symbol: "rate_limit".to_string(),
        file: "src/rate_limit.rs".to_string(),
        revision: revision.to_string(),
    }
}

/// A decision entity carrying one already-accepted fact.
fn decision_with_fact(engine: &MemoryEngine, at: i64) -> i64 {
    let id = engine
        .add_entity(EntityKind::Decision, "decide-rate-limit", 0.9, at)
        .expect("decision entity");
    engine
        .add_observation(id, FACT, "decision", 0.9, at)
        .expect("accepted fact");
    id
}

/// 1. The typed link is created and carries the full source identity.
#[test]
fn link_created_carries_source_identity() {
    let (engine, captured) = setup();
    let decision = decision_with_fact(&engine, BASE);
    let src = source("r1");

    let link = link_evidence_for(&engine, decision, &src, BASE).expect("evidence_for link");

    assert_eq!(link.source, src);
    assert_eq!(link.decision_entity_id, decision);

    let rows = engine.memory_links_for(decision).expect("links");
    let stored = rows
        .iter()
        .find(|row| row.0 == link.link_id)
        .expect("link row present");
    assert_eq!(stored.3, EVIDENCE_FOR);
    let reason: SourceIdentity = serde_json::from_str(stored.4.as_deref().expect("reason"))
        .expect("source identity is JSON");
    assert_eq!(reason, src);

    let events = captured.lock().expect("audit mutex");
    assert_eq!(
        events
            .iter()
            .filter(|e| e.action == "evidence.link")
            .count(),
        1
    );
    assert!(events.iter().all(|e| e.outcome == "applied"));
}

/// 2. A new source revision enqueues a review item for the impacted decision.
#[test]
fn source_revision_change_enqueues_review() {
    let (engine, captured) = setup();
    let decision = decision_with_fact(&engine, BASE);
    let src = source("r1");
    let link = link_evidence_for(&engine, decision, &src, BASE).expect("evidence_for link");

    record_source_revision(&engine, &src, "r2", BASE + 100).expect("source revision supersede");

    let events = captured.lock().expect("audit mutex").clone();
    let queue = review_queue(&engine, &events, BASE + 200).expect("review queue");

    assert_eq!(queue.len(), 1, "one changed source, one review item");
    let item = &queue[0];
    assert_eq!(item.decision_entity_id, decision);
    assert_eq!(item.recorded_revision, "r1");
    assert_eq!(item.observed_revision, "r2");
    assert_eq!(item.fact_text, FACT);
    assert!(
        item.feed == evidence_links::RevisionFeed::EntityVersions
            || item.feed == evidence_links::RevisionFeed::AuditLedger
    );

    // The mark is a NEW ADD-only link, and it did not remove the evidence link.
    let rows = engine.memory_links_for(decision).expect("links");
    assert!(
        rows.iter()
            .any(|row| row.0 == link.link_id && row.3 == EVIDENCE_FOR)
    );
    let marks: Vec<_> = rows.iter().filter(|row| row.3 == NEEDS_REVIEW).collect();
    assert_eq!(marks.len(), 1, "exactly one needs_review mark");
    assert_eq!(marks[0].0, item.mark_link_id);
    let mark: serde_json::Value =
        serde_json::from_str(marks[0].4.as_deref().expect("mark reason")).expect("mark is JSON");
    assert_eq!(mark["observed_revision"], json!("r2"));
    assert_eq!(mark["recorded_revision"], json!("r1"));

    // The audit trail records the review, not a rewrite.
    let events = captured.lock().expect("audit mutex");
    assert!(
        events
            .iter()
            .any(|event| event.action == "evidence.needs_review")
    );
}

/// 3. Enqueuing the review leaves the accepted fact text byte-for-byte intact.
#[test]
fn fact_text_is_not_rewritten_by_review() {
    let (engine, captured) = setup();
    let decision = decision_with_fact(&engine, BASE);
    let src = source("r1");
    link_evidence_for(&engine, decision, &src, BASE).expect("evidence_for link");

    let before = memnoth_engine::vault_history::current_facts(&engine, decision, BASE + 50)
        .expect("facts before");
    assert_eq!(before.len(), 1);
    assert_eq!(before[0]["fact"], json!(FACT));

    record_source_revision(&engine, &src, "r3", BASE + 100).expect("source revision supersede");
    let events = captured.lock().expect("audit mutex").clone();
    review_queue(&engine, &events, BASE + 200).expect("review queue");

    let after = memnoth_engine::vault_history::current_facts(&engine, decision, BASE + 250)
        .expect("facts after");
    assert_eq!(after.len(), 1, "no fact was added or removed");
    assert_eq!(after[0]["fact"], json!(FACT), "the fact text is unchanged");
    assert_eq!(after[0]["kind"], json!("decision"));
}

/// 4. No revision change means no review item and no `needs_review` mark.
#[test]
fn unchanged_source_produces_no_review() {
    let (engine, captured) = setup();
    let decision = decision_with_fact(&engine, BASE);
    let src = source("r1");
    link_evidence_for(&engine, decision, &src, BASE).expect("evidence_for link");

    // Only the link event exists: the source is still at r1.
    let events = captured.lock().expect("audit mutex").clone();
    let queue = review_queue(&engine, &events, BASE + 200).expect("review queue");

    assert!(queue.is_empty(), "no change, no review");
    let rows = engine.memory_links_for(decision).expect("links");
    assert!(
        !rows.iter().any(|row| row.3 == NEEDS_REVIEW),
        "no needs_review mark was added"
    );
}

/// The queue is idempotent: a source change is marked once even if the audit
/// event slice changes between runs (the entity_versions feed still sees it).
#[test]
fn enqueue_is_idempotent() {
    let (engine, captured) = setup();
    let decision = decision_with_fact(&engine, BASE);
    let src = source("r1");
    link_evidence_for(&engine, decision, &src, BASE).expect("evidence_for link");
    record_source_revision(&engine, &src, "r2", BASE + 100).expect("source revision supersede");

    let events = captured.lock().expect("audit mutex").clone();
    let first = review_queue(&engine, &events, BASE + 200).expect("first queue");
    assert_eq!(first.len(), 1);

    // Re-running with the same (or an empty) event slice must not add a second
    // mark, because the entity_versions feed still sees r2.
    let second = review_queue(&engine, &[], BASE + 300).expect("second queue");
    assert!(second.is_empty(), "enqueue is idempotent");
    let rows = engine.memory_links_for(decision).expect("links");
    assert_eq!(
        rows.iter().filter(|row| row.3 == NEEDS_REVIEW).count(),
        1,
        "one mark only"
    );
}
