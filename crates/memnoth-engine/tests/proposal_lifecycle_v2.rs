//! MOAT-04 item 1 — local slice: proposal owner and effective time.
//!
//! These tests drive the public engine API only. They pin the four behaviours
//! the proposal queue already promised (invisible until accepted, fourteen-day
//! TTL with no auto-accept, dedupe into one card, both sides of a contradiction
//! kept) and add the two new additive fields to those same paths.

use memnoth_engine::{EntityKind, MemoryEngine, PROPOSAL_TTL_MS};

const T0: i64 = 1_760_000_000_000;
const DAY: i64 = 86_400_000;

fn propose(
    eng: &MemoryEngine,
    content: &str,
    owner: Option<&str>,
    effective_at: Option<i64>,
    at: i64,
) -> (i64, bool) {
    eng.propose_belief_with(
        "acme",
        EntityKind::Concept,
        content,
        "fact",
        0.8,
        r#"{"user":"agent-1","agent":"cli"}"#,
        at,
        owner,
        effective_at,
    )
    .unwrap()
}

fn observation_count(eng: &MemoryEngine) -> i64 {
    eng.connection()
        .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn owner_is_stored_and_surfaced_by_proposal_and_list() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let (id, created) = propose(&eng, "the runbook lives in docs", Some("dana"), None, T0);
    assert!(created);

    let one = eng.proposal(id).unwrap().unwrap();
    assert_eq!(one.owner.as_deref(), Some("dana"));
    assert_eq!(
        one.effective_at, None,
        "unset effective time stays defaulted"
    );

    let listed = eng.list_proposals(Some("proposed"), T0).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert_eq!(listed[0].owner.as_deref(), Some("dana"));
}

#[test]
fn effective_time_is_stored_and_surfaced_by_proposal_and_list() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let effective = T0 - 3 * DAY;
    let (id, created) = propose(
        &eng,
        "the incident started on monday",
        Some("dana"),
        Some(effective),
        T0,
    );
    assert!(created);

    let one = eng.proposal(id).unwrap().unwrap();
    assert_eq!(one.effective_at, Some(effective));
    assert_eq!(one.proposed_at, T0, "proposal time is not effective time");
    assert_eq!(
        one.expires_at,
        T0 + PROPOSAL_TTL_MS,
        "the TTL still runs from proposal time"
    );

    let listed = eng.list_proposals(None, T0).unwrap();
    assert_eq!(listed[0].effective_at, Some(effective));
}

#[test]
fn an_old_proposals_table_gains_the_new_columns_on_read() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    // Simulate a vault written before MOAT-04: the table exists without the
    // two new columns. A plain read must heal the schema, not fail.
    eng.connection()
        .execute_batch(
            "ALTER TABLE proposals DROP COLUMN owner;
             ALTER TABLE proposals DROP COLUMN effective_at;",
        )
        .unwrap();
    assert_eq!(eng.list_proposals(None, T0).unwrap().len(), 0);

    let (id, created) = propose(
        &eng,
        "a row in an old vault",
        Some("dana"),
        Some(T0 - DAY),
        T0,
    );
    assert!(created);
    let one = eng.proposal(id).unwrap().unwrap();
    assert_eq!(one.owner.as_deref(), Some("dana"));
    assert_eq!(one.effective_at, Some(T0 - DAY));
}

#[test]
fn legacy_propose_defaults_both_fields_to_none() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let (id, created) = eng
        .propose_belief(
            "acme",
            EntityKind::Concept,
            "a legacy proposal",
            "fact",
            0.7,
            r#"{"user":"agent-1","agent":"cli"}"#,
            T0,
        )
        .unwrap();
    assert!(created);
    let one = eng.proposal(id).unwrap().unwrap();
    assert_eq!(one.owner, None);
    assert_eq!(one.effective_at, None);
}

#[test]
fn proposal_is_invisible_to_recall_until_it_is_accepted() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let effective = T0 - 2 * DAY;
    let (id, _) = propose(
        &eng,
        "the deploy window changed",
        Some("dana"),
        Some(effective),
        T0,
    );

    // No observation exists, so neither recall nor FTS can see the belief,
    // even when the query is made at its effective time.
    assert!(
        eng.search_observations("deploy", 10, effective)
            .unwrap()
            .is_empty()
    );
    assert_eq!(observation_count(&eng), 0);

    let accepted = eng.accept_proposal(id, "priya", T0 + DAY).unwrap();
    assert_eq!(accepted.status, "accepted");
    assert_eq!(accepted.owner.as_deref(), Some("dana"));
    assert_eq!(accepted.effective_at, Some(effective));

    let observation_id = accepted.observation_id.unwrap();
    let (content, valid_from, recorded_at, provenance): (String, i64, Option<i64>, String) = eng
        .connection()
        .query_row(
            "SELECT content, valid_from, recorded_at, provenance FROM observations WHERE id = ?1",
            [observation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(content, "the deploy window changed");
    assert_eq!(valid_from, effective, "effective time becomes valid_from");
    assert_eq!(
        recorded_at,
        Some(T0 + DAY),
        "acceptance is when we learned it"
    );
    assert!(provenance.contains("\"owner\":\"dana\""));
    assert!(provenance.contains("\"accepted_by\":\"priya\""));

    assert_eq!(
        eng.search_observations("deploy", 10, effective)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn late_acceptance_is_refused_and_expiry_never_auto_accepts() {
    assert_eq!(
        PROPOSAL_TTL_MS,
        14 * DAY,
        "the fourteen-day gate stays pinned"
    );
    let eng = MemoryEngine::open_in_memory().unwrap();
    let (id, _) = propose(&eng, "the retry budget is two", Some("dana"), None, T0);

    let queued = eng.list_proposals(None, T0 + PROPOSAL_TTL_MS - 1).unwrap();
    assert_eq!(queued[0].status, "proposed");

    let queued = eng.list_proposals(None, T0 + PROPOSAL_TTL_MS).unwrap();
    assert_eq!(queued[0].status, "expired", "the window closed");

    let late = eng.accept_proposal(id, "priya", T0 + PROPOSAL_TTL_MS);
    assert!(late.is_err(), "an expired proposal is not late-acceptable");
    assert_eq!(observation_count(&eng), 0, "expiry is not acceptance");
}

#[test]
fn five_way_fanout_produces_one_card_with_the_first_owners_metadata() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let mut ids = Vec::new();
    for (n, owner) in ["dana", "priya", "sam", "lee", "kim"].iter().enumerate() {
        let (id, created) = propose(
            &eng,
            "the queue is redis",
            Some(owner),
            Some(T0 - (n as i64) * DAY),
            T0 + (n as i64) * 1000,
        );
        assert_eq!(created, n == 0, "only the first proposer creates the card");
        ids.push(id);
    }

    assert!(ids.iter().all(|id| *id == ids[0]), "one shared handle");
    let listed = eng.list_proposals(None, T0 + 5000).unwrap();
    assert_eq!(listed.len(), 1, "five reports, one approval card");
    assert_eq!(
        listed[0].owner.as_deref(),
        Some("dana"),
        "first writer wins"
    );
    assert_eq!(
        listed[0].effective_at,
        Some(T0),
        "and its effective time too"
    );
    assert!(listed[0].provenance.contains("agent-1"));
}

#[test]
fn a_contradictory_pair_is_visible_with_both_dates() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let old_effective = T0 - 10 * DAY;
    let new_effective = T0 - 2 * DAY;

    let (old_id, _) = propose(
        &eng,
        "the queue is redis",
        Some("dana"),
        Some(old_effective),
        T0,
    );
    let (new_id, _) = propose(
        &eng,
        "the queue is rabbitmq",
        Some("sam"),
        Some(new_effective),
        T0,
    );
    assert_ne!(old_id, new_id, "contradictions are different beliefs");

    let accepted_old = eng.accept_proposal(old_id, "priya", T0 + DAY).unwrap();
    let accepted_new = eng.accept_proposal(new_id, "priya", T0 + DAY).unwrap();
    assert_eq!(accepted_old.effective_at, Some(old_effective));
    assert_eq!(accepted_new.effective_at, Some(new_effective));

    let entity = eng.find_entities_by_name("acme", T0 + DAY).unwrap();
    let entity_id = entity[0].id;
    let facts = eng.observations_for(entity_id, T0 + DAY).unwrap();
    assert_eq!(
        facts.len(),
        2,
        "both sides survive; neither overwrites the other"
    );

    let by_content = |needle: &str| {
        facts
            .iter()
            .find(|fact| fact.content.contains(needle))
            .unwrap_or_else(|| panic!("missing fact {needle}: {facts:?}"))
    };
    assert_eq!(by_content("redis").valid_from, old_effective);
    assert_eq!(by_content("rabbitmq").valid_from, new_effective);

    assert_eq!(
        eng.search_observations("queue", 10, T0 + DAY)
            .unwrap()
            .len(),
        2
    );
}
