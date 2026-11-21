//! Wiring-free harness for `store/belief_events.rs`.
//!
//! The engine module is intentionally not declared in `store.rs` yet: the
//! parent task owns that pending edit. Rust does not compile a stray `.rs`
//! file, so this test includes the source with `#[path]` and supplies the one
//! crate-internal name it uses (`audit::seal_hash` / `redact` / `redact_value`)
//! as a shim that re-exports the real `memnoth-engine` functions. That makes the
//! module's own `#[cfg(test)]` tests run, and adds one check that its seal is
//! byte-for-byte the engine's seal rather than a look-alike.
//!
//! Once `mod belief_events;` is added to `store.rs`, this file can be deleted;
//! the same tests will run inside `memnoth-engine`.

mod audit {
    pub use memnoth_engine::audit::{redact, redact_value, seal_hash};
}

#[path = "../crates/memnoth-engine/src/store/belief_events.rs"]
mod belief_events;

#[test]
fn the_ledger_seals_with_the_engine_hash_and_verifies() {
    use belief_events::{
        BeliefEventColumn, BeliefEventInput, append_belief_event, belief_events_where, chain_head,
        verify_belief_chain,
    };
    use serde_json::json;

    let conn = rusqlite::Connection::open_in_memory().expect("in-memory connection");
    conn.execute_batch(belief_events::BELIEF_EVENTS_SCHEMA_SQL)
        .expect("ledger schema");

    let tx = conn.unchecked_transaction().expect("transaction");
    let event = append_belief_event(
        &tx,
        &BeliefEventInput::new(
            "belief.approved",
            "agent-1",
            "acme",
            "applied",
            1_760_000_000_000,
            json!({"observation_created": 11}),
        )
        .entity_id(7)
        .observation_id(11)
        .proposal_id(3)
        .approved_by("priya")
        .approved_at(1_760_000_000_000)
        .reason("teammate report"),
    )
    .expect("append");
    // The event must not be visible before the caller commits: that is the
    // whole point of taking a `&Transaction`.
    assert_eq!(chain_head(&tx).unwrap().seq, 1);
    tx.commit().expect("commit");

    assert_eq!(
        event.hash,
        memnoth_engine::audit::seal_hash(
            event.seq,
            event.at,
            &event.action,
            &event.target,
            &event.outcome,
            &event.detail_json,
            &event.prev,
        )
    );
    assert!(event.hash_matches());
    assert!(event.fields_agree());

    let verdict = verify_belief_chain(&conn).expect("verify");
    assert!(verdict.ok, "{verdict:?}");
    assert_eq!(verdict.events, 1);

    let rows = belief_events_where(&conn, BeliefEventColumn::Observation, 11, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].hash, event.hash);

    // Every indexed filter variant must be reachable through the public API.
    for column in [
        BeliefEventColumn::Entity,
        BeliefEventColumn::Observation,
        BeliefEventColumn::Proposal,
    ] {
        let _ = belief_events_where(&conn, column, 1, 10).unwrap();
    }
}
