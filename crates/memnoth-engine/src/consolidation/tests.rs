//! Extracted from the inline test module; see the parent module for context.
use super::*;
use crate::model::EntityKind;

fn engine() -> MemoryEngine {
    MemoryEngine::open_in_memory().unwrap()
}

#[test]
fn canonical_form_folds_case_and_punctuation() {
    assert_eq!(canonical_name("Postgres DB"), "postgres db");
    assert_eq!(canonical_name("postgres_db"), "postgres db");
    assert_eq!(canonical_name("POSTGRES-DB"), "postgres db");
    assert_eq!(canonical_name("  PostgreSQL  "), "postgresql");
    assert_eq!(canonical_name("C++"), "c");
}

/// The column existed and was never written; every lookup that believed it
/// was reading a canonical name was reading NULL.
#[test]
fn adding_an_entity_records_its_comparison_form() {
    let e = engine();
    let id = e
        .add_entity(EntityKind::Concept, "Postgres DB", 1.0, 0)
        .unwrap();
    let got: String = e
        .connection()
        .query_row(
            "SELECT canonical_name FROM entities WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(got, "postgres db");
    // …and it is now reachable under either spelling.
    assert_eq!(e.find_entities_by_name("postgres_db", 10).unwrap().len(), 1);
    assert_eq!(e.find_entities_by_name("Postgres DB", 10).unwrap().len(), 1);
}

#[test]
fn candidates_report_same_name_and_bounded_prefix_variants() {
    let e = engine();
    e.add_entity(EntityKind::Concept, "Postgres", 1.0, 0)
        .unwrap();
    e.add_entity(EntityKind::Concept, "postgres", 1.0, 0)
        .unwrap();
    e.add_entity(EntityKind::Concept, "PostgreSQL", 1.0, 0)
        .unwrap();
    e.add_entity(EntityKind::Concept, "Redis", 1.0, 0).unwrap();
    // Same spelling, different kind — not a duplicate.
    e.add_entity(EntityKind::Person, "redis", 1.0, 0).unwrap();

    let found = duplicate_candidates(&e, 10).unwrap();
    assert!(
        found
            .iter()
            .any(|c| c.reason == DuplicateReason::SameCanonical
                && c.left.name.eq_ignore_ascii_case("postgres")
                && c.right.name.eq_ignore_ascii_case("postgres")),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|c| c.reason == DuplicateReason::PrefixVariant
                && c.left.name.eq_ignore_ascii_case("postgres")
                && c.right.name.eq_ignore_ascii_case("postgresql")),
        "postgres/postgresql must be reported: {found:?}"
    );
    // Different kinds never pair, even with identical spelling.
    assert!(
        !found.iter().any(|c| c.left.kind != c.right.kind),
        "kinds must not mix: {found:?}"
    );
}

/// The whole point: the merge is total, explicable, and reversible in the
/// sense that history still shows two entities before it.
#[test]
fn merge_moves_everything_and_leaves_a_readable_trail() {
    let e = engine();
    let a = e
        .add_entity(EntityKind::Concept, "Postgres", 1.0, 0)
        .unwrap();
    let b = e
        .add_entity(EntityKind::Concept, "postgres", 1.0, 0)
        .unwrap();
    let c = e.add_entity(EntityKind::Concept, "App", 1.0, 0).unwrap();
    let d = e.add_entity(EntityKind::Concept, "Report", 1.0, 0).unwrap();
    e.add_relation(a, c, "uses", 1.0, 0).unwrap();
    e.add_relation(c, b, "connects", 1.0, 0).unwrap();
    e.add_relation(b, d, "reports", 1.0, 0).unwrap();
    e.add_observation(a, "version 16", "fact", 1.0, 0).unwrap();
    e.add_observation(b, "runs in prod", "fact", 1.0, 0)
        .unwrap();
    e.record_access(a, 5).unwrap();
    e.record_access(a, 7).unwrap();
    e.record_access(b, 9).unwrap();

    assert_eq!(duplicate_candidates(&e, 10).unwrap().len(), 1);

    let out = merge_entities(&e, b, a, "same database, alias", 20).unwrap();
    assert_eq!(out.into, a);
    assert_eq!(out.from, b);
    // Two of the three relations touch the loser: `c -> b` and `b -> d`.
    assert_eq!(out.relations_repointed, 2, "both directions move");
    assert_eq!(out.observations_moved, 1);
    assert_eq!(out.accesses_merged, 1);
    assert!(out.aliases_added.contains(&"postgres".to_string()));

    // Relations now reach the survivor, and none points at the loser.
    let at_loser: i64 = e
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM relations WHERE source_id = ?1 OR target_id = ?1",
            params![b],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(at_loser, 0);
    // `a -> c` survives and `b -> d` was repointed, so two edges leave `a`.
    assert_eq!(e.outgoing_relations(a, 30).unwrap().len(), 2);
    assert_eq!(e.observations_for(a, 30).unwrap().len(), 2);

    // Access history is additive, not dropped with the row.
    let (count, _last): (i64, i64) = e
        .connection()
        .query_row(
            "SELECT access_count, last_access FROM access_stats WHERE entity_id = ?1",
            params![a],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 3);

    // The old name still resolves — that is what the alias is for.
    let found = e.find_entities_by_name("postgres", 30).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, a);
    assert_eq!(aliases_of(&e, a).unwrap(), ["postgres"]);

    // The loser is closed, not deleted, with the reason on its history.
    let live = e.find_entities_by_name("Postgres", 30).unwrap();
    assert_eq!(live.len(), 1, "only the survivor is live");
    let reason: String = e
        .connection()
        .query_row(
            "SELECT change_reason FROM entity_versions WHERE entity_id = ?1",
            params![b],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reason, "same database, alias");
    // As-of *before* the merge, both still existed: the graph is honest
    // about what was believed at the time.
    assert_eq!(e.entities_as_of(10).unwrap().len(), 4);

    let history = merge_history(&e).unwrap();
    assert_eq!(history.len(), 5, "one action row per effect");
    assert!(history.iter().all(|h| h.job_id == out.job_id));
    assert!(history.iter().any(|h| h.action_type == "merge_entity"));
}

#[test]
fn a_double_merge_is_refused_rather_than_silently_repeated() {
    let e = engine();
    let a = e.add_entity(EntityKind::Concept, "X", 1.0, 0).unwrap();
    let b = e.add_entity(EntityKind::Concept, "x", 1.0, 0).unwrap();
    merge_entities(&e, b, a, "dupe", 10).unwrap();
    let err = merge_entities(&e, b, a, "dupe again", 20).unwrap_err();
    assert!(err.to_string().contains("not live"), "{err}");
    let err = merge_entities(&e, a, a, "self", 20).unwrap_err();
    assert!(err.to_string().contains("itself"), "{err}");
}

#[test]
fn merging_across_kinds_is_refused() {
    let e = engine();
    let a = e.add_entity(EntityKind::Concept, "Ada", 1.0, 0).unwrap();
    let b = e.add_entity(EntityKind::Person, "ada", 1.0, 0).unwrap();
    let err = merge_entities(&e, b, a, "guess", 10).unwrap_err();
    assert!(err.to_string().contains("kinds"), "{err}");
}

/// A merge that creates a self-loop or a duplicate edge must not leave one.
#[test]
fn merge_collapses_self_loops_and_duplicate_edges() {
    let e = engine();
    let a = e.add_entity(EntityKind::Concept, "Svc", 1.0, 0).unwrap();
    let b = e.add_entity(EntityKind::Concept, "svc", 1.0, 0).unwrap();
    let c = e.add_entity(EntityKind::Concept, "Db", 1.0, 0).unwrap();
    // a -> b would become a self-loop; a -> c and b -> c would collapse.
    e.add_relation(a, b, "peer", 1.0, 0).unwrap();
    e.add_relation(a, c, "uses", 1.0, 0).unwrap();
    e.add_relation(b, c, "uses", 1.0, 0).unwrap();

    let out = merge_entities(&e, b, a, "dupe", 10).unwrap();
    assert_eq!(out.relations_collapsed, 2, "{out:?}");
    let edges: Vec<(i64, i64, String)> = e
        .connection()
        .prepare("SELECT source_id, target_id, kind FROM relations ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .filter_map(|x| x.ok())
        .collect();
    assert_eq!(edges, [(a, c, "uses".to_string())], "{edges:?}");
}

// ------------------------------------------------ W1 triage + as-of suite
// Sprint-write shape: 12 update probes (4 dup, 4 contra, 4 neither) +
// 6 as-of queries. Rule-graded, no LLM judge. Reports triage accuracy,
// as-of precision, omission count (must be 0).

#[test]
fn w1_triage_accuracy_dup_contra_neither() {
    const T0: i64 = 1_700_000_000_000;
    const DAY: i64 = 86_400_000;
    let e = engine();
    // Seed: 4 canonical endpoints.
    e.add_entity(EntityKind::Concept, "CEO", 0.9, T0).unwrap();
    e.add_entity(EntityKind::Person, "Alice", 0.9, T0).unwrap();
    e.add_entity(EntityKind::Tool, "Postgres", 0.9, T0).unwrap();
    e.add_entity(EntityKind::Concept, "Launch", 0.9, T0)
        .unwrap();

    // 4 duplicates: same endpoint, same canonical (case/punct variants).
    let dups = [
        (EntityKind::Concept, "ceo", Some(""), "dup-1"),
        (EntityKind::Concept, "CEO!", Some(""), "dup-2"),
        (EntityKind::Person, "alice", Some(""), "dup-3"),
        (EntityKind::Tool, "postgres_db", None, "dup-4-neither?"),
    ];
    // Note: postgres_db canonicalizes to "postgres db" ≠ "postgres",
    // so it is NOT a dup — it grades as neither (prefix-variant needs W2).
    let mut correct = 0;
    // Identical re-ingest (same day) = dup (golden: identical=dup).
    let v = triage_update(
        &e,
        &EntityKind::Concept,
        "ceo",
        Some(""),
        T0 + 1,
        None,
        T0 + 1,
    )
    .unwrap();
    assert_eq!(v, TriageVerdict::Duplicate);
    correct += 1;
    let v = triage_update(
        &e,
        &EntityKind::Concept,
        "CEO!",
        Some(""),
        T0 + 1,
        None,
        T0 + 1,
    )
    .unwrap();
    assert_eq!(v, TriageVerdict::Duplicate);
    correct += 1;
    let v = triage_update(
        &e,
        &EntityKind::Person,
        "ALICE",
        Some(""),
        T0 + 1,
        None,
        T0 + 1,
    )
    .unwrap();
    assert_eq!(v, TriageVerdict::Duplicate);
    correct += 1;
    // 4 contradictions: same name, different content, overlapping window.
    e.add_entity(EntityKind::Concept, "Title", 0.9, T0).unwrap();
    let v = triage_update(
        &e,
        &EntityKind::Concept,
        "title",
        Some("different role"),
        T0 + DAY,
        None,
        T0 + DAY,
    )
    .unwrap();
    assert_eq!(v, TriageVerdict::Contradiction);
    correct += 1;
    let v = triage_update(
        &e,
        &EntityKind::Concept,
        "CEO",
        Some("someone else"),
        T0 + DAY,
        None,
        T0 + DAY,
    )
    .unwrap();
    assert_eq!(v, TriageVerdict::Contradiction);
    correct += 1;
    // 4 neither: novel names, no collision.
    for name in ["Offsite Q3", "Bistro-9", "Comet", "Ledger-review"] {
        let v = triage_update(
            &e,
            &EntityKind::Concept,
            name,
            Some("new event"),
            T0 + DAY,
            None,
            T0 + DAY,
        )
        .unwrap();
        assert_eq!(v, TriageVerdict::Neither, "{name}");
        correct += 1;
    }
    let _ = dups;
    // 3 dup + 2 contra + 4 neither = 9 graded here; full 12-probe sprint
    // fixture adds title-change + different-day-neither + identical-dup.
    assert!(correct >= 9, "triage accuracy 9/9 in this slice");
}

#[test]
fn w1_asof_day_double_return_regression() {
    const T0: i64 = 1_700_000_000_000;
    const DAY: i64 = 86_400_000;
    let e = engine();
    // Midnight T00:00:00Z supersede: half-open must never double-return.
    let midnight = (T0 / DAY) * DAY;
    let a = e
        .add_entity(EntityKind::Concept, "policy", 1.0, midnight - DAY)
        .unwrap();
    let b = e
        .supersede_entity(a, None, Some("policy-v2"), None, None, "update", midnight)
        .unwrap();
    for q in [
        midnight - 1,
        midnight,
        midnight + 1,
        midnight + DAY,
        midnight + 2 * DAY,
        midnight - DAY,
    ] {
        let old = e.entity_at(a, q).unwrap().is_some();
        let new = e.entity_at(b, q).unwrap().is_some();
        assert!(!(old && new), "double-return at {q}: old+new both visible");
        if q < midnight {
            assert!(old && !new, "before: old only @ {q}");
        } else {
            assert!(!old && new, "at/after: successor only @ {q}");
        }
    }
    // Omission count: 6/6 queries returned exactly one generation.
}

// ------------------------------------------------- W2 golden + runner

#[test]
fn w2_triage_golden_title_change_diff_day_identical() {
    const T0: i64 = 1_700_000_000_000;
    const DAY: i64 = 86_400_000;
    let e = engine();
    e.add_entity(EntityKind::Concept, "Title", 0.9, T0).unwrap();
    // title-change = contradiction (same name, different content).
    let v = triage_update(
        &e,
        &EntityKind::Concept,
        "title",
        Some("new title"),
        T0 + 1,
        None,
        T0 + 1,
    )
    .unwrap();
    assert_eq!(v, TriageVerdict::Contradiction, "title-change");
    // identical (same day, same content) = dup.
    let v = triage_update(
        &e,
        &EntityKind::Concept,
        "Title",
        None,
        T0 + 1,
        None,
        T0 + 1,
    )
    .unwrap();
    assert_eq!(v, TriageVerdict::Duplicate, "identical");
    // different-day (same content, next day) = neither (new event).
    let v = triage_update(
        &e,
        &EntityKind::Concept,
        "Title",
        None,
        T0 + DAY + 1,
        None,
        T0 + DAY + 1,
    )
    .unwrap();
    assert_eq!(v, TriageVerdict::Neither, "different-day");
}

#[test]
fn w2_trigram_cosine_orders_near_dup_above_unrelated() {
    let near = trigram_cosine(
        "the auth service issues jwt tokens",
        "the auth service issues jwt token",
    );
    let far = trigram_cosine(
        "the auth service issues jwt tokens",
        "completely unrelated ledger review",
    );
    assert!(near >= NEAR_DUP_COSINE, "near-dup must clear 0.97: {near}");
    assert!(far < near, "unrelated must score lower: {far} vs {near}");
}

#[test]
fn w2_validate_rejects_out_of_range_and_overlap() {
    let ok = TriageResponse {
        dup_idx: vec![0],
        contradicted_idx: vec![2],
    };
    assert!(validate_triage_response(&ok, 3).is_ok());
    let bad_range = TriageResponse {
        dup_idx: vec![7],
        contradicted_idx: vec![],
    };
    assert!(validate_triage_response(&bad_range, 3).is_err());
    let bad_overlap = TriageResponse {
        dup_idx: vec![1],
        contradicted_idx: vec![1],
    };
    assert!(validate_triage_response(&bad_overlap, 3).is_err());
}

#[test]
fn w2_batch_loader_caps_at_fifty() {
    const T0: i64 = 1_700_000_000_000;
    let e = engine();
    for i in 0..60 {
        e.add_entity(EntityKind::Concept, &format!("cap-{i:03}"), 0.5, T0 + i)
            .unwrap();
    }
    let wm = e.consolidation_watermark().unwrap();
    let batch = load_consolidation_batch(&e, wm, 50).unwrap();
    assert_eq!(batch.len(), 50, "batch loader caps at 50");
}

#[test]
fn w2_batch_loader_bounds_and_runner_is_idempotent() {
    const T0: i64 = 1_700_000_000_000;
    let e = engine();
    for i in 0..40 {
        e.add_entity(EntityKind::Concept, &format!("batch-{i:03}"), 0.5, T0 + i)
            .unwrap();
    }
    let wm = e.consolidation_watermark().unwrap();
    let batch = load_consolidation_batch(&e, wm, 50).unwrap();
    assert_eq!(batch.len(), 40, "all 40 fit one batch");
    // Runner with a neither-only LLM: creates episodes, marks consolidated,
    // second run finds nothing (idempotent, no dup rows).
    let total_0: i64 = e
        .connection()
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();
    let ep_0: i64 = e
        .connection()
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
        .unwrap();
    let noop_llm = |_req: &TriageRequest| -> anyhow::Result<TriageResponse> {
        Ok(TriageResponse {
            dup_idx: vec![],
            contradicted_idx: vec![],
        })
    };
    let r1 = run_consolidation_batch(&e, noop_llm, 50, 8, T0 + 100_000, "w2-test").unwrap();
    assert_eq!(r1.refined, 40);
    assert_eq!(r1.created, 0);
    let r2 = run_consolidation_batch(&e, noop_llm, 50, 8, T0 + 200_000, "w2-test").unwrap();
    assert_eq!(
        r2.refined + r2.created,
        0,
        "second run must be a no-op: {r2:?}"
    );
    let total_1: i64 = e
        .connection()
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        total_0, total_1,
        "neither-only run must not duplicate entities"
    );
    let ep_1: i64 = e
        .connection()
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
        .unwrap();
    assert!(ep_1 > ep_0, "journey episodes appended");
}

#[test]
fn w2_witness_crash_safety_and_stale_verify() {
    const T0: i64 = 1_700_000_000_000;
    let e = engine();
    let id = e.add_entity(EntityKind::Concept, "w", 0.5, T0).unwrap();
    // Crash simulation: begin a job, never finish → abandoned.
    let orphan = e.begin_job(T0).unwrap();
    assert!(
        e.abandoned_jobs()
            .unwrap()
            .iter()
            .any(|(j, _)| *j == orphan)
    );
    e.finish_job(orphan, &serde_json::json!({"recovered": true}), T0 + 1)
        .unwrap();
    assert!(
        !e.abandoned_jobs()
            .unwrap()
            .iter()
            .any(|(j, _)| *j == orphan)
    );
    // Stale-verify: unconsolidated needs work; marked does not.
    let wm = e.consolidation_watermark().unwrap();
    assert!(e.needs_consolidation(id, wm + 1).unwrap());
    e.mark_consolidated(id, wm + 1).unwrap();
    assert!(!e.needs_consolidation(id, wm + 1).unwrap());
}

#[test]
fn w2_trim_bounds_observation_history_without_deleting() {
    const T0: i64 = 1_700_000_000_000;
    let e = engine();
    let id = e.add_entity(EntityKind::Concept, "hist", 0.9, T0).unwrap();
    for i in 0..5 {
        e.add_observation(id, &format!("fact {i}"), "fact", 1.0, T0 + i)
            .unwrap();
    }
    let closed = e.trim_observations(id, 2, T0 + 100).unwrap();
    assert_eq!(closed.len(), 3);
    assert_eq!(e.observations_for(id, T0 + 101).unwrap().len(), 2);
    let total: i64 = e
        .connection()
        .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 5, "trim closes, never deletes");
}
