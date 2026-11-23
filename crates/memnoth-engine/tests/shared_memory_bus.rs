//! G11 / M5 gate: the shared memory bus for spawned subagents.
//!
//! A child gets a scoped vault slice and can only `propose` into it. It has no
//! path to the database: the parent commits the slice through
//! [`commit_slice`], which stages proposals (never observations), opens one
//! episode per run, and surfaces parallel disagreement with both sources and
//! both dates. Nothing a child staged reaches recall before a person accepts it.

use memnoth_engine::workflow::{
    VaultSlice, build_agent_tool_set, commit_slice, disagreements_from_proposals,
    provenance::has_full_provenance,
};
use memnoth_engine::{MemoryEngine, open_in_memory};

const T0: i64 = 1_760_000_000_000;
const DAY: i64 = 86_400_000;

fn observation_count(engine: &MemoryEngine) -> i64 {
    engine
        .connection()
        .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
        .unwrap()
}

fn episode_count(engine: &MemoryEngine) -> i64 {
    engine
        .connection()
        .query_row("SELECT COUNT(*) FROM episodes", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn a_child_cannot_write_directly_only_propose_into_its_slice() {
    let engine = open_in_memory().unwrap();
    let mut slice = VaultSlice::spawn("run-1", "child-a", 0).with_user("priya");
    slice.propose(
        "queue-config",
        "the queue is rabbitmq",
        "fact",
        0.9,
        "src/queue.rs",
        12,
    );

    // The child staged a belief in memory. It reached neither the proposal
    // queue nor the observation table: a VaultSlice holds no database handle.
    assert_eq!(slice.proposals().len(), 1);
    assert!(engine.list_proposals(None, T0).unwrap().is_empty());
    assert_eq!(observation_count(&engine), 0);

    // The parent is the only writer, and even it stages a proposal, not a
    // belief. Recall still sees nothing.
    let report = commit_slice(&engine, &slice, "parent", T0).unwrap();
    assert_eq!(report.proposal_ids.len(), 1);
    assert!(report.proposal_ids[0].1, "a new proposal card was created");
    let queue = engine.list_proposals(Some("proposed"), T0).unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].status, "proposed");
    assert_eq!(queue[0].owner.as_deref(), Some("parent"));
    assert_eq!(observation_count(&engine), 0);
    assert!(
        engine
            .search_observations("rabbitmq", 10, T0 + DAY)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn five_way_fan_out_produces_one_approval_card_and_one_episode_per_run() {
    let engine = open_in_memory().unwrap();
    // The delegation runtime names its children with build_agent_tool_set;
    // each child tool becomes one run with its own scoped slice.
    let defs = build_agent_tool_set(&[
        ("scout-one", "reads the cache code", "Read."),
        ("scout-two", "reads the cache code", "Read."),
        ("scout-three", "reads the cache code", "Read."),
        ("scout-four", "reads the cache code", "Read."),
        ("scout-five", "reads the cache code", "Read."),
    ])
    .unwrap();
    assert_eq!(defs.len(), 5);

    let mut created = Vec::new();
    for (index, def) in defs.iter().enumerate() {
        let mut slice = VaultSlice::spawn(&format!("run-{index}"), &def.name, index as u64);
        slice.propose(
            "cache-config",
            "the cache is redis",
            "fact",
            0.8,
            "src/cache.rs",
            index + 1,
        );
        let report = commit_slice(&engine, &slice, "parent", T0 + index as i64).unwrap();
        assert_eq!(report.proposal_ids.len(), 1);
        created.push(report.proposal_ids[0].1);
        assert!(report.episode_id > 0, "each run gets a real episode row");
    }

    // Five children, one belief: five commits join the same open card.
    assert_eq!(
        created,
        vec![true, false, false, false, false],
        "only the first proposer creates the card"
    );
    let queue = engine.list_proposals(Some("proposed"), T0 + 4).unwrap();
    assert_eq!(queue.len(), 1, "five-way fan-out is one approval card");
    assert_eq!(
        episode_count(&engine),
        5,
        "one episode per run, not per belief"
    );

    // The first proposer keeps attribution; joining does not re-author the card.
    let provenance: serde_json::Value = serde_json::from_str(&queue[0].provenance).unwrap();
    assert_eq!(provenance["run_id"], "run-0");
    assert_eq!(provenance["agent"], "agent_scout_one");
    assert!(has_full_provenance(&queue[0].provenance));
}

#[test]
fn a_contradictory_pair_shows_both_sources_and_both_dates() {
    let engine = open_in_memory().unwrap();

    let mut left = VaultSlice::spawn("run-left", "alpha", 0);
    left.propose(
        "deploy-window",
        "the deploy window is tuesday",
        "fact",
        0.9,
        "src/deploy.rs",
        10,
    );
    commit_slice(&engine, &left, "parent", T0).unwrap();

    let mut right = VaultSlice::spawn("run-right", "beta", 2);
    right.propose(
        "deploy-window",
        "the deploy window is thursday",
        "fact",
        0.9,
        "src/schedule.rs",
        22,
    );
    commit_slice(&engine, &right, "parent", T0 + DAY).unwrap();

    let queue = engine.list_proposals(Some("proposed"), T0 + DAY).unwrap();
    assert_eq!(queue.len(), 2, "different content stays two cards");
    let disagreements = disagreements_from_proposals(&queue);
    assert_eq!(
        disagreements.len(),
        1,
        "the disagreement is one contradiction"
    );

    let contradiction = &disagreements[0];
    assert_eq!(contradiction.entity, "deploy-window");
    assert_eq!(contradiction.left.run_id, "run-left");
    assert_eq!(contradiction.right.run_id, "run-right");
    assert_eq!(contradiction.left.agent, "alpha");
    assert_eq!(contradiction.right.agent, "beta");
    assert_eq!(contradiction.left.date, T0);
    assert_eq!(contradiction.right.date, T0 + DAY);
    assert_eq!(contradiction.left.proposed_at, T0);
    assert_eq!(contradiction.right.proposed_at, T0 + DAY);
    assert_eq!(contradiction.left.file, "src/deploy.rs");
    assert_eq!(contradiction.right.file, "src/schedule.rs");
    assert!(contradiction.left.source.contains("run-left"));
    assert!(contradiction.right.source.contains("run-right"));
    assert!(has_full_provenance(&contradiction.left.provenance));
    assert!(has_full_provenance(&contradiction.right.provenance));

    // One query, one reason: both claims, both sources, both dates.
    for needle in [
        "run-left",
        "run-right",
        "the deploy window is tuesday",
        "the deploy window is thursday",
    ] {
        assert!(
            contradiction.reason.contains(needle),
            "the contradiction must name {needle}: {}",
            contradiction.reason
        );
    }
    let row = contradiction.to_json();
    assert_eq!(row["left"]["run_id"], "run-left");
    assert_eq!(row["right"]["run_id"], "run-right");
    assert_eq!(row["left"]["date"], T0);
    assert_eq!(row["right"]["date"], T0 + DAY);

    // The report returned by the later commit already carried it: surfacing
    // does not require a second, special-purpose query.
    let mut third = VaultSlice::spawn("run-third", "gamma", 1);
    third.propose("unrelated", "the cache is redis", "fact", 0.5, "x.rs", 1);
    let report = commit_slice(&engine, &third, "parent", T0 + 2 * DAY).unwrap();
    assert_eq!(report.disagreements.len(), 1);
    assert_eq!(report.disagreements[0].right.run_id, "run-right");
}

#[test]
fn no_belief_reaches_recall_without_full_provenance() {
    let engine = open_in_memory().unwrap();
    let mut slice = VaultSlice::spawn("run-1", "child-a", 3);
    slice.propose(
        "retry-policy",
        "the retry budget is five",
        "fact",
        0.8,
        "src/retry.rs",
        7,
    );
    let report = commit_slice(&engine, &slice, "parent", T0).unwrap();

    // An unaccepted proposal is invisible to recall: there is no observation.
    assert!(
        engine
            .search_observations("retry", 10, T0 + DAY)
            .unwrap()
            .is_empty()
    );

    let id = report.proposal_ids[0].0;
    engine.accept_proposal(id, "priya", T0 + DAY).unwrap();

    let (provenance, content): (String, String) = engine
        .connection()
        .query_row(
            "SELECT provenance, content FROM observations LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(content, "the retry budget is five");
    assert!(
        has_full_provenance(&provenance),
        "a recalled belief must carry all six provenance fields: {provenance}"
    );
    let parsed: serde_json::Value = serde_json::from_str(&provenance).unwrap();
    assert_eq!(parsed["run_id"], "run-1");
    assert_eq!(parsed["agent"], "child-a");
    assert_eq!(parsed["user"], "local");
    assert_eq!(parsed["turn"], 3);
    assert_eq!(parsed["file"], "src/retry.rs");
    assert_eq!(parsed["line"], 7);
    assert_eq!(parsed["accepted_by"], "priya");

    // Now, and only now, recall finds the belief.
    assert_eq!(
        engine
            .search_observations("retry", 10, T0 + DAY)
            .unwrap()
            .len(),
        1
    );

    // Nothing without full provenance exists in the observation table at all.
    let mut stmt = engine
        .connection()
        .prepare("SELECT provenance FROM observations")
        .unwrap();
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|json| has_full_provenance(json)));
}
