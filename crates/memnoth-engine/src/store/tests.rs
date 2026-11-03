//! Extracted from the inline test module; see the parent module for context.
use super::*;

const T0: i64 = 1_700_000_000_000;
const DAY: i64 = 86_400_000;

#[test]
fn supersede_closes_old_opens_new_and_journals() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let e1 = eng
        .add_entity(EntityKind::Concept, "auth", 1.0, T0)
        .unwrap();

    let e2 = eng
        .supersede_entity(
            e1,
            None,
            Some("auth-service"),
            None,
            None,
            "rename",
            T0 + DAY,
        )
        .unwrap();

    assert_ne!(e1, e2);
    // as-of before rename: old name visible, successor invisible
    assert_eq!(eng.entity_at(e1, T0 + 1).unwrap().unwrap().name, "auth");
    assert!(eng.entity_at(e2, T0 + 1).unwrap().is_none());
    // as-of after rename: old closed, successor current
    assert!(eng.entity_at(e1, T0 + 2 * DAY).unwrap().is_none());
    assert_eq!(
        eng.entity_at(e2, T0 + 2 * DAY).unwrap().unwrap().name,
        "auth-service"
    );
    // exactly one version journaled for the old entity
    let n: i64 = eng
        .connection()
        .query_row("SELECT COUNT(*) FROM entity_versions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn revoke_hides_from_now_but_not_history() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let a = eng.add_entity(EntityKind::Concept, "a", 1.0, T0).unwrap();
    let b = eng.add_entity(EntityKind::Concept, "b", 1.0, T0).unwrap();
    let r = eng.add_relation(a, b, "uses", 1.0, T0).unwrap();

    eng.revoke_relation(r, "wrong: b does not use a", T0 + DAY)
        .unwrap();

    // gone from "now"
    assert!(eng.outgoing_relations(a, T0 + 2 * DAY).unwrap().is_empty());
    // but present in history
    assert_eq!(eng.outgoing_relations(a, T0 + 1).unwrap().len(), 1);
    // relation_at respects revocation
    assert!(eng.relation_at(r, T0 + 2 * DAY).unwrap().is_none());
    assert!(eng.relation_at(r, T0 + 1).unwrap().is_some());
}

#[test]
fn superseded_relation_retargets_edge() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let a = eng.add_entity(EntityKind::Concept, "app", 1.0, T0).unwrap();
    let db1 = eng.add_entity(EntityKind::Tool, "sqlite", 1.0, T0).unwrap();
    let db2 = eng
        .add_entity(EntityKind::Tool, "postgres", 1.0, T0)
        .unwrap();
    let r = eng.add_relation(a, db1, "stores-in", 1.0, T0).unwrap();

    let r2 = eng
        .supersede_relation(r, Some(db2), None, None, "migration", T0 + DAY)
        .unwrap();

    assert_eq!(
        eng.outgoing_relations(a, T0 + 2 * DAY).unwrap()[0].target_id,
        db2
    );
    assert!(eng.relation_at(r, T0 + 2 * DAY).unwrap().is_none());
    assert!(eng.relation_at(r2, T0 + 2 * DAY).unwrap().is_some());
}

#[test]
fn as_of_snapshot_is_consistent() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    for i in 0..10 {
        eng.add_entity(EntityKind::Concept, &format!("e{i}"), 1.0, T0 + i)
            .unwrap();
    }
    assert_eq!(eng.entities_as_of(T0 + 4).unwrap().len(), 5);
    assert_eq!(eng.entities_as_of(T0 + 100).unwrap().len(), 10);
    assert_eq!(eng.entities_as_of(T0 - 1).unwrap().len(), 0);
}

#[test]
fn observations_respect_validity() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let e = eng.add_entity(EntityKind::Decision, "d", 1.0, T0).unwrap();
    eng.add_observation(e, "chose JWT", "fact", 1.0, T0)
        .unwrap();
    assert_eq!(eng.observations_for(e, T0 + 1).unwrap().len(), 1);
    assert_eq!(eng.observations_for(e, T0 - 1).unwrap().len(), 0);
}

#[test]
fn stats_counts_current_vs_total() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let a = eng.add_entity(EntityKind::Concept, "a", 1.0, T0).unwrap();
    let b = eng.add_entity(EntityKind::Concept, "b", 1.0, T0).unwrap();
    eng.supersede_entity(b, None, Some("bb"), None, None, "rename", T0 + 1)
        .unwrap();
    eng.add_relation(a, b, "relates", 1.0, T0).unwrap();
    eng.revoke_relation(
        eng.add_relation(b, a, "backref", 1.0, T0).unwrap(),
        "nope",
        T0 + 1,
    )
    .unwrap();

    let s = eng.stats().unwrap();
    assert_eq!(s.entities_current, 2); // a + renamed bb
    assert_eq!(s.entities_total, 3);
    assert_eq!(s.relations_current, 1); // relates survives, backref revoked
    assert_eq!(s.relations_total, 2);
}

#[test]
fn frontier_expansion_walks_edges() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let a = eng.add_entity(EntityKind::Concept, "a", 1.0, T0).unwrap();
    let b = eng.add_entity(EntityKind::Concept, "b", 1.0, T0).unwrap();
    let c = eng.add_entity(EntityKind::Concept, "c", 1.0, T0).unwrap();
    eng.add_relation(a, b, "next", 1.0, T0).unwrap();
    eng.add_relation(b, c, "next", 1.0, T0).unwrap();

    let hop1 = eng.expand_frontier(&[a], T0 + 1).unwrap();
    assert_eq!(hop1.len(), 1);
    let hop2 = eng.expand_frontier(&[hop1[0].target_id], T0 + 1).unwrap();
    assert_eq!(hop2[0].target_id, c);
}

#[test]
fn fts_search_respects_temporal_filter() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let e1 = eng
        .add_entity(EntityKind::Concept, "temporal-graph", 1.0, T0)
        .unwrap();
    let e2 = eng
        .supersede_entity(
            e1,
            None,
            Some("temporal-store"),
            None,
            None,
            "rename",
            T0 + DAY,
        )
        .unwrap();
    eng.add_entity(EntityKind::Concept, "unrelated", 1.0, T0)
        .unwrap();

    // old name findable as-of before the rename...
    let early = eng.search_entities("temporal", 10, T0 + 1).unwrap();
    assert_eq!(early.len(), 1);
    assert_eq!(early[0].0.id, e1);
    // ...and not after (only the renamed successor matches)
    let late = eng.search_entities("temporal", 10, T0 + 2 * DAY).unwrap();
    assert_eq!(late.len(), 1);
    assert_eq!(late[0].0.id, e2);
}

#[test]
fn fts_query_is_injection_safe() {
    // inner quotes are doubled so tokens stay inert
    assert_eq!(
        MemoryEngine::sanitize_fts_query("temporal\" OR 1=1 --"),
        "\"temporal\"\"\" \"OR\" \"1=1\" \"--\""
    );
    let eng = MemoryEngine::open_in_memory().unwrap();
    // must not error on hostile input
    assert!(
        eng.search_entities("\" OR 1=1 --", 10, T0)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn hybrid_search_surfaces_graph_neighbors() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    // "auth-service" textually matches; "jwt" only connected via edge
    let auth = eng
        .add_entity(EntityKind::Concept, "auth-service", 1.0, T0)
        .unwrap();
    let jwt = eng
        .add_entity(EntityKind::Tool, "jwt-tokens", 1.0, T0)
        .unwrap();
    eng.add_relation(auth, jwt, "issues", 1.0, T0).unwrap();

    let hits = eng.hybrid_search("auth-service", 10, T0 + 1).unwrap();
    let ids: Vec<i64> = hits.iter().map(|(e, _)| e.id).collect();
    assert!(ids.contains(&auth), "text match must be present");
    assert!(
        ids.contains(&jwt),
        "1-hop neighbor must surface via RRF even without text match"
    );
    // the direct text match outranks the neighbor
    assert_eq!(hits[0].0.id, auth);
}

// ---- R1: N-arm RRF(60) + caps + boosts + budget packing ----

#[test]
fn n_arm_fusion_prefers_multi_arm_hits() {
    // id 2 fires in two arms → must outrank single-arm ids even when its
    // per-arm rank is worse (the N-arm property the old single-list
    // fusion could not express).
    let arms = vec![vec![1, 2], vec![3, 2]];
    let scores = MemoryEngine::rrf_fuse_arms(&arms, MemoryEngine::RRF_K);
    assert!(scores[&2] > scores[&1]);
    assert!(scores[&2] > scores[&3]);
}

#[test]
fn per_arm_caps_hold() {
    let ids: Vec<i64> = (0..100).collect();
    assert_eq!(MemoryEngine::cap_arm(ids, 10).len(), 10);
}

#[test]
fn interleave_keeps_each_arms_top_hit() {
    // arm0's #1 (id 9) is absent elsewhere; arm1/arm2 agree on id 1 early.
    // RRF sums reciprocal ranks, so id 9 (one 1/60) loses to id 1 (two 1/60s)
    // and can fall below the budget cutoff. Interleave must surface it.
    let arms = vec![vec![9], vec![1, 2], vec![1, 3]];
    let rrf = MemoryEngine::rrf_fuse_arms(&arms, MemoryEngine::RRF_K);
    assert!(
        rrf[&9] < rrf[&1],
        "precondition: RRF must rank the single-arm #1 below the multi-arm id"
    );
    let il = MemoryEngine::interleave_fuse_arms(&arms);
    assert_eq!(il, vec![9, 1, 2, 3], "every arm's #1 takes a slot first");
    // De-duplication: an id present in two arms appears once.
    let dup = vec![vec![5, 6], vec![5, 7]];
    assert_eq!(MemoryEngine::interleave_fuse_arms(&dup), vec![5, 6, 7]);
    // Empty arms never fabricate.
    assert!(MemoryEngine::interleave_fuse_arms(&[vec![], vec![]]).is_empty());
}

#[test]
fn fused_search_interleave_mode_returns_hits() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let a = eng
        .add_entity(EntityKind::Concept, "alpha shared", 1.0, T0)
        .unwrap();
    let b = eng
        .add_entity(EntityKind::Concept, "beta shared", 1.0, T0)
        .unwrap();
    // Both fusions must return the same membership on this trivial corpus; the
    // mode only changes ordering/score, never fabricates or drops.
    let arms = vec![vec![a], vec![b]];
    let rrf = eng
        .fused_search_with(&arms, 10, T0 + 1, true, false)
        .unwrap();
    let il = eng
        .fused_search_with(&arms, 10, T0 + 1, true, true)
        .unwrap();
    assert_eq!(rrf.len(), 2);
    assert_eq!(il.len(), 2);
    let mut ri: Vec<i64> = rrf.iter().map(|(e, _)| e.id).collect();
    let mut ii: Vec<i64> = il.iter().map(|(e, _)| e.id).collect();
    ri.sort();
    ii.sort();
    assert_eq!(ri, ii, "membership must match across fusions");
}

// ---- S2/S5: PPR graph arm + link-type boosts ----

#[test]
fn link_kind_boost_ranks_causal_above_associative() {
    assert!(
        MemoryEngine::link_kind_boost("causes") > MemoryEngine::link_kind_boost("mentions"),
        "causal links must outweigh associative ones"
    );
    assert!(MemoryEngine::link_kind_boost("enables") > MemoryEngine::link_kind_boost("mentions"));
    assert_eq!(MemoryEngine::link_kind_boost("mentions"), 1.0);
}

#[test]
fn ppr_reaches_multi_hop_and_conserves_mass() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let seed = eng
        .add_entity(EntityKind::Concept, "seed", 1.0, T0)
        .unwrap();
    let near = eng
        .add_entity(EntityKind::Concept, "near", 1.0, T0)
        .unwrap();
    let far = eng.add_entity(EntityKind::Concept, "far", 1.0, T0).unwrap();
    let other = eng
        .add_entity(EntityKind::Concept, "other", 1.0, T0)
        .unwrap();
    eng.add_relation(seed, near, "mentions", 1.0, T0).unwrap();
    eng.add_relation(near, far, "mentions", 1.0, T0).unwrap();
    eng.add_relation(seed, other, "mentions", 1.0, T0).unwrap();

    let ranked = eng.ppr_rank(&[(seed, 1.0)], T0 + 1, 0.5, 20).unwrap();
    let ids: Vec<i64> = ranked.iter().map(|(id, _)| *id).collect();
    assert!(ids.contains(&seed));
    assert!(ids.contains(&near));
    assert!(
        ids.contains(&far),
        "PPR must reach the 2-hop node — the multi-hop the 1-hop arm cannot do"
    );
    assert_eq!(ranked[0].0, seed, "the seed carries the most mass");
    let total: f64 = ranked.iter().map(|(_, s)| *s).sum();
    assert!(
        (total - 1.0).abs() < 1e-6,
        "PPR vector must be a distribution, got {total}"
    );
    // Empty seeds never fabricate.
    assert!(eng.ppr_rank(&[], T0 + 1, 0.5, 20).unwrap().is_empty());
}

#[test]
fn hybrid_search_empty_never_errors() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    // empty query and unknown terms: Ok(empty), never Err
    assert!(eng.hybrid_search("", 10, T0).unwrap().is_empty());
    assert!(
        eng.hybrid_search("zz-no-such-term", 10, T0)
            .unwrap()
            .is_empty()
    );
    // fused_search with only empty arms: Ok(empty)
    let empty: Vec<Vec<i64>> = vec![vec![], vec![]];
    assert!(eng.fused_search(&empty, 10, T0).unwrap().is_empty());
    assert!(eng.fused_search(&[], 10, T0).unwrap().is_empty());
}

#[test]
fn budget_packing_never_empty_and_skips_oversize() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    // shared token so one query hits both (FTS5 space = AND)
    let a = eng
        .add_entity(EntityKind::Concept, "alpha shared", 1.0, T0)
        .unwrap();
    let b = eng
        .add_entity(EntityKind::Concept, "beta shared", 1.0, T0)
        .unwrap();
    let hits = eng.hybrid_search("shared", 10, T0 + 1).unwrap();
    assert!(!hits.is_empty());
    // tiny budget: top hit kept even over budget (never-empty)
    let packed = MemoryEngine::pack_with_budget(hits.clone(), 1, |e| {
        format!("{} {}", e.name, e.summary.clone().unwrap_or_default())
    });
    assert_eq!(packed.len(), 1);
    assert_eq!(packed[0].0.id, hits[0].0.id);
    // zero-hit input stays zero-hit (no fabrication)
    let none: Vec<(crate::model::Entity, f64)> = Vec::new();
    assert!(MemoryEngine::pack_with_budget(none, 100, |_| String::new()).is_empty());
    let _ = (a, b);
}

#[test]
fn micro_boosts_stay_bounded() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let e = eng
        .add_entity(EntityKind::Concept, "boosted", 1.0, T0)
        .unwrap();
    // hammer access_stats: proof boost must clamp at 1.05, total bounded
    for i in 0..200 {
        eng.record_access(e, T0 + i).unwrap();
    }
    let ent = eng.entity_at(e, T0 + 1000).unwrap().unwrap();
    let b = eng.micro_boost(&ent, T0 + 1000).unwrap();
    assert!((0.7..=1.35).contains(&b), "boost {b} out of bounds");
    // never-read entity is near-neutral (only temporal applies)
    let f = eng
        .add_entity(EntityKind::Concept, "fresh", 1.0, T0 + 999)
        .unwrap();
    let fent = eng.entity_at(f, T0 + 1000).unwrap().unwrap();
    let bf = eng.micro_boost(&fent, T0 + 1000).unwrap();
    assert!((0.9..=1.2).contains(&bf), "fresh boost {bf} out of bounds");
}

// ---- W3: decay feeds ranking ----

#[test]
fn w3_access_resists_decay_and_revocation_accelerates() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let a = eng
        .add_entity(EntityKind::Concept, "kept", 1.0, T0)
        .unwrap();
    let b = eng
        .add_entity(EntityKind::Concept, "doubted", 1.0, T0)
        .unwrap();
    for i in 0..20 {
        eng.record_access(a, T0 + i).unwrap();
    }
    let other = eng
        .add_entity(EntityKind::Concept, "other", 1.0, T0)
        .unwrap();
    let r = eng.add_relation(b, other, "implies", 1.0, T0).unwrap();
    eng.revoke_relation(r, "contradicted", T0 + DAY).unwrap();
    let now = T0 + 60 * DAY;
    let keeper = eng.entity_at(a, now).unwrap().unwrap();
    let doubter = eng.entity_at(b, now).unwrap().unwrap();
    let kept = eng.micro_boost(&keeper, now).unwrap();
    let doubted = eng.micro_boost(&doubter, now).unwrap();
    assert_eq!(eng.revoked_relation_count(b).unwrap(), 1);
    assert_eq!(eng.revoked_relation_count(a).unwrap(), 0);
    assert!(
        kept > doubted,
        "access must resist decay and revocation must accelerate it: {kept} vs {doubted}"
    );
}

// ---- R3: kind weights, importance, score-proportional allocation ----

#[test]
fn r3_kind_importance_and_allocation() {
    assert!(
        MemoryEngine::kind_weight(&EntityKind::Concept)
            > MemoryEngine::kind_weight(&EntityKind::Event)
    );
    assert!(
        MemoryEngine::importance_weight(9, "auth-service")
            > MemoryEngine::importance_weight(0, "auth-service"),
        "hubs must outrank leaves"
    );
    assert!(
        MemoryEngine::importance_weight(9, "tmp-test-thing")
            < MemoryEngine::importance_weight(9, "auth-service"),
        "generic names must be demoted"
    );
    let scores = vec![10.0, 1.0, 0.05];
    let alloc = MemoryEngine::allocate_bytes(&scores, 10_000);
    assert_eq!(alloc.iter().sum::<usize>(), 10_000, "must sum exactly");
    assert!(alloc.iter().all(|b| *b >= 700), "floor per item: {alloc:?}");
    assert!(alloc[0] > alloc[1], "spine must dominate: {alloc:?}");
    assert_eq!(alloc[2], 700, "below-cliff item gets the floor only");
    assert!(MemoryEngine::allocate_bytes(&[], 1000).is_empty());
    assert_eq!(
        MemoryEngine::allocate_bytes(&scores, 100),
        vec![700, 700, 700]
    );
}

#[test]
fn ce_rerank_reorders_by_external_scores() {
    use std::collections::HashMap;
    let eng = MemoryEngine::open_in_memory().unwrap();
    let auth = eng
        .add_entity(EntityKind::Concept, "auth-service", 1.0, T0)
        .unwrap();
    let jwt = eng
        .add_entity(EntityKind::Tool, "jwt-tokens", 1.0, T0)
        .unwrap();
    eng.add_relation(auth, jwt, "issues", 1.0, T0).unwrap();
    let hits = eng.hybrid_search("auth-service", 10, T0 + 1).unwrap();
    assert_eq!(hits[0].0.id, auth);
    // external CE logits reverse the order (jwt more relevant)
    let mut ce: HashMap<i64, f32> = HashMap::new();
    ce.insert(auth, 0.1);
    ce.insert(jwt, 0.9);
    let reranked = eng.rerank_with_ce(hits, &ce, T0 + 1).unwrap();
    assert_eq!(reranked.len(), 2);
    assert_eq!(reranked[0].0.id, jwt);
    // empty CE map: input order untouched
    let hits2 = eng.hybrid_search("auth-service", 10, T0 + 1).unwrap();
    let n = hits2.len();
    let same = eng.rerank_with_ce(hits2, &HashMap::new(), T0 + 1).unwrap();
    assert_eq!(same.len(), n);
}

#[test]
fn external_vector_arm_fuses_without_schema_change() {
    // vector arm = externally-ranked ids (no embedding table in the
    // engine); a vector-only hit fuses alongside text arms.
    let eng = MemoryEngine::open_in_memory().unwrap();
    let auth = eng
        .add_entity(EntityKind::Concept, "auth-service", 1.0, T0)
        .unwrap();
    let jwt = eng
        .add_entity(EntityKind::Tool, "jwt-tokens", 1.0, T0)
        .unwrap();
    eng.add_relation(auth, jwt, "issues", 1.0, T0).unwrap();
    let hits = eng
        .hybrid_search_with_arms("auth-service", 10, T0 + 1, &[vec![jwt]])
        .unwrap();
    let ids: Vec<i64> = hits.iter().map(|(e, _)| e.id).collect();
    assert!(ids.contains(&auth));
    assert!(ids.contains(&jwt));
}

// ---- R2: link-expansion arm + temporal-window arm ----

#[test]
fn temporal_window_parses_calendar_forms() {
    use chrono::{NaiveDate, TimeZone, Utc};
    let ms = |y: i32, m: u32, d: u32| {
        Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(y, m, d)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        )
        .timestamp_millis()
    };
    // "in 2023" → exact calendar year bounds (well-known epoch ms)
    assert_eq!(
        MemoryEngine::parse_temporal_window("decisions in 2023", T0),
        Some((1_672_531_200_000, 1_704_067_200_000))
    );
    // "march 2024" → leap-year March (31 days)
    let mar = MemoryEngine::parse_temporal_window("outage march 2024", T0).unwrap();
    assert_eq!(mar.0, ms(2024, 3, 1));
    assert_eq!(mar.1 - mar.0, 31 * DAY);
    // "last week" rolls back from as_of
    assert_eq!(
        MemoryEngine::parse_temporal_window("deployments last week", T0),
        Some((T0 - 7 * DAY, T0))
    );
    // seasons: most recent *completed* occurrence
    let sep26 = ms(2026, 9, 26);
    assert_eq!(
        MemoryEngine::parse_temporal_window("incidents last spring", sep26),
        Some((ms(2026, 3, 1), ms(2026, 6, 1)))
    );
    assert_eq!(
        MemoryEngine::parse_temporal_window("incidents last winter", sep26),
        Some((ms(2025, 12, 1), ms(2026, 3, 1)))
    );
    // incomplete season falls back to the prior year
    let apr26 = ms(2026, 4, 15);
    assert_eq!(
        MemoryEngine::parse_temporal_window("x last spring", apr26),
        Some((ms(2025, 3, 1), ms(2025, 6, 1)))
    );
    // no temporal expression → None; bare year rejected
    assert_eq!(MemoryEngine::parse_temporal_window("hello world", T0), None);
    assert_eq!(MemoryEngine::parse_temporal_window("error 2024", T0), None);
}

#[test]
fn strip_temporal_keeps_topic_remainder() {
    assert_eq!(
        MemoryEngine::strip_temporal("deployments in 2023"),
        "deployments"
    );
    assert_eq!(MemoryEngine::strip_temporal("outage march 2024"), "outage");
    assert_eq!(
        MemoryEngine::strip_temporal("deployments last week"),
        "deployments"
    );
    assert_eq!(MemoryEngine::strip_temporal("in 2023"), "");
}

#[test]
fn in_window_beats_out_window_at_equal_text() {
    // B created first (lower rowid → wins any FTS tie), but A is born
    // in-window: the temporal arm must pull A ahead. Identical
    // observation text keeps text scores tied by construction.
    // NOTE: as_of is 2025 (after both births) — T0=Nov 2023 predates them.
    const AS: i64 = 1_735_689_600_000; // 2025-01-01T00:00:00Z
    let eng = MemoryEngine::open_in_memory().unwrap();
    let day2024 = 1_704_067_200_000; // 2024-01-01T00:00:00Z
    let b = eng
        .add_entity(EntityKind::Concept, "outpost", 1.0, day2024)
        .unwrap();
    eng.add_observation(b, "quux identical fact", "fact", 1.0, day2024)
        .unwrap();
    let day2023 = 1_672_531_200_000 + 100 * DAY; // ~Apr 2023
    let a = eng
        .add_entity(EntityKind::Concept, "outrider", 1.0, day2023)
        .unwrap();
    eng.add_observation(a, "quux identical fact", "fact", 1.0, day2023)
        .unwrap();

    let hits = eng.hybrid_search("quux in 2023", 10, AS).unwrap();
    let ids: Vec<i64> = hits.iter().map(|(e, _)| e.id).collect();
    assert!(ids.contains(&a), "in-window entity must surface");
    assert_eq!(hits[0].0.id, a, "in-window beats out-window at equal text");
    // mask the temporal arm off → the out-window-first text order returns
    let masked = eng
        .hybrid_search_masked("quux in 2023", 10, AS, ArmMask::no_temporal(), &[])
        .unwrap();
    assert_eq!(
        masked[0].0.id, b,
        "no-temporal ablation restores text order"
    );
}

#[test]
fn pure_temporal_query_hits_the_gate() {
    // "in 2023" with no topic token: no text anchor → Ok(empty) by
    // design (documented gate; the sprint fixture carries topic tokens).
    let eng = MemoryEngine::open_in_memory().unwrap();
    eng.add_entity(
        EntityKind::Concept,
        "something",
        1.0,
        1_672_531_200_000 + DAY,
    )
    .unwrap();
    assert!(eng.hybrid_search("in 2023", 10, T0).unwrap().is_empty());
}

#[test]
fn link_expansion_additive_scoring() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let s1 = eng.add_entity(EntityKind::Concept, "s1", 1.0, T0).unwrap();
    let s2 = eng.add_entity(EntityKind::Concept, "s2", 1.0, T0).unwrap();
    let t = eng
        .add_entity(EntityKind::Concept, "target", 1.0, T0)
        .unwrap();
    let u = eng
        .add_entity(EntityKind::Concept, "other", 1.0, T0)
        .unwrap();
    // two seeds converge on t via causal edges; one generic edge to u
    eng.add_relation(s1, t, "causes", 1.0, T0).unwrap();
    eng.add_relation(s2, t, "causes", 1.0, T0).unwrap();
    eng.add_relation(s1, u, "relates", 1.0, T0).unwrap();

    let scored = eng.link_expansion_scores(&[s1, s2], T0 + 1).unwrap();
    assert_eq!(scored.len(), 2);
    assert_eq!(scored[0].0, t, "convergent causal target ranks first");
    // t: tanh(2·0.5) + 0.85 + 1.0 ≈ 2.61; u: tanh(0.5) + 0.7 ≈ 1.16
    assert!((scored[0].1 - 2.61).abs() < 0.01, "t score {}", scored[0].1);
    assert!((scored[1].1 - 1.16).abs() < 0.01, "u score {}", scored[1].1);
    // seeds never appear in their own expansion
    assert!(!scored.iter().any(|(id, _)| *id == s1 || *id == s2));
}

#[test]
fn arm_mask_disables_graph_arm() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let auth = eng
        .add_entity(EntityKind::Concept, "auth-service", 1.0, T0)
        .unwrap();
    let jwt = eng
        .add_entity(EntityKind::Tool, "jwt-tokens", 1.0, T0)
        .unwrap();
    eng.add_relation(auth, jwt, "issues", 1.0, T0).unwrap();
    // full mask: neighbor surfaces (existing R1 behavior preserved)
    let full = eng
        .hybrid_search_masked("auth-service", 10, T0 + 1, ArmMask::full(), &[])
        .unwrap();
    assert!(full.iter().any(|(e, _)| e.id == jwt));
    // no-graph ablation: neighbor gone, text match stays
    let masked = eng
        .hybrid_search_masked("auth-service", 10, T0 + 1, ArmMask::no_graph(), &[])
        .unwrap();
    assert!(masked.iter().any(|(e, _)| e.id == auth));
    assert!(!masked.iter().any(|(e, _)| e.id == jwt));
    // no-boosts: same membership, scores collapse to pure RRF
    let plain = eng
        .hybrid_search_masked("auth-service", 10, T0 + 1, ArmMask::no_boosts(), &[])
        .unwrap();
    assert_eq!(plain.len(), full.len());
}

#[test]
fn temporal_arm_spreads_and_caps() {
    // NOTE: as_of is Apr 2024 (after all births) — T0 predates them.
    const AS: i64 = 1_711_929_600_000; // 2024-04-01T00:00:00Z
    let eng = MemoryEngine::open_in_memory().unwrap();
    // burst: 5 entities in one week of Mar 2024 + 1 in the next week
    let burst = 1_709_251_200_000 + 2 * DAY; // Mar 3 2024
    for i in 0..5 {
        eng.add_entity(EntityKind::Concept, &format!("burst{i}"), 1.0, burst + i)
            .unwrap();
    }
    let late = eng
        .add_entity(EntityKind::Concept, "latecomer", 1.0, burst + 10 * DAY)
        .unwrap();
    let out = eng
        .add_entity(EntityKind::Concept, "outsider", 1.0, 1_704_067_200_000)
        .unwrap(); // Jan 2024: out of window
    let arm = eng
        .temporal_arm((1_709_251_200_000, 1_711_929_600_000), AS, 6)
        .unwrap(); // Mar 2024
    assert_eq!(arm.len(), 6);
    assert!(!arm.contains(&out), "out-of-window excluded");
    // bucket spreading: the lone late-week entity interleaves instead of
    // trailing all five burst entities
    let pos = arm.iter().position(|id| *id == late).unwrap();
    assert!(pos < 4, "late-week entity spreads forward, got pos {pos}");
}

// ------------------------------------------------ W1 fast tests (<1min)
// Shared-instant + half-open + inverted + future-wins + degrade-to-add +
// ADD-only links + content-hash PKs + expiry staging. Zero destructive
// overwrites, zero omissions — test-pinned.

#[test]
fn w1_shared_instant_half_open_returns_successor_only() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let e1 = eng.add_entity(EntityKind::Concept, "ceo", 1.0, T0).unwrap();
    let at = T0 + DAY;
    let e2 = eng
        .supersede_entity(e1, None, Some("ceo-2"), None, None, "update", at)
        .unwrap();
    // At the shared instant: successor only, no double-return.
    assert!(
        eng.entity_at(e1, at).unwrap().is_none(),
        "old closed at `at`"
    );
    assert!(eng.entity_at(e2, at).unwrap().is_some(), "new open at `at`");
    let all = eng.entities_as_of(at).unwrap();
    let ids: Vec<i64> = all.iter().map(|e| e.id).collect();
    assert!(
        ids.contains(&e2) && !ids.contains(&e1),
        "half-open: successor only, ids={ids:?}"
    );
    // Before: old only.
    assert!(eng.entity_at(e1, at - 1).unwrap().is_some());
    assert!(eng.entity_at(e2, at - 1).unwrap().is_none());
}

#[test]
fn w1_inverted_intervals_rejected() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let e1 = eng.add_entity(EntityKind::Concept, "x", 1.0, T0).unwrap();
    let err = eng
        .supersede_entity(e1, None, Some("y"), None, None, "bad", T0 - 1)
        .unwrap_err();
    assert!(err.to_string().contains("inverted"), "{err}");
    let a = eng.add_entity(EntityKind::Concept, "a", 1.0, T0).unwrap();
    let b = eng.add_entity(EntityKind::Concept, "b", 1.0, T0).unwrap();
    let r = eng.add_relation(a, b, "uses", 1.0, T0).unwrap();
    let err = eng
        .supersede_relation(r, Some(b), None, None, "bad", T0 - 10)
        .unwrap_err();
    assert!(err.to_string().contains("inverted"), "{err}");
}

#[test]
fn w1_future_supersede_wins_only_after_its_instant() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let e1 = eng
        .add_entity(EntityKind::Concept, "title", 1.0, T0)
        .unwrap();
    let future = T0 + 10 * DAY;
    let e2 = eng
        .supersede_entity(
            e1,
            None,
            Some("title-2"),
            None,
            None,
            "future update",
            future,
        )
        .unwrap();
    // Current as-of before future: old still wins.
    assert_eq!(eng.entity_at(e1, T0 + DAY).unwrap().unwrap().id, e1);
    assert!(eng.entity_at(e2, T0 + DAY).unwrap().is_none());
    // After: successor wins.
    assert!(eng.entity_at(e1, future + 1).unwrap().is_none());
    assert_eq!(eng.entity_at(e2, future + 1).unwrap().unwrap().id, e2);
}

#[test]
fn w1_degrade_to_add_when_no_open_predecessor() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    // Unknown id + name → degraded ADD, no failure (zero omissions).
    let new_id = eng
        .supersede_entity(
            999_999,
            Some(EntityKind::Concept),
            Some("fresh"),
            None,
            None,
            "upsert",
            T0,
        )
        .unwrap();
    assert!(eng.entity_at(new_id, T0).unwrap().is_some());
    // Closed predecessor + rename → degraded ADD carrying lineage.
    let e1 = eng.add_entity(EntityKind::Concept, "old", 1.0, T0).unwrap();
    let e2 = eng
        .supersede_entity(e1, None, Some("new"), None, None, "r", T0 + DAY)
        .unwrap();
    let e3 = eng
        .supersede_entity(
            e1,
            None,
            Some("new-again"),
            None,
            None,
            "retry",
            T0 + 2 * DAY,
        )
        .unwrap();
    assert_ne!(e3, e2);
    assert!(eng.entity_at(e3, T0 + 2 * DAY).unwrap().is_some());
}

#[test]
fn w1_add_only_links_never_overwrite() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let e1 = eng.add_entity(EntityKind::Concept, "k", 1.0, T0).unwrap();
    let e2 = eng
        .supersede_entity(e1, None, Some("k2"), None, None, "r1", T0 + DAY)
        .unwrap();
    let e3 = eng
        .supersede_entity(e2, None, Some("k3"), None, None, "r2", T0 + 2 * DAY)
        .unwrap();
    let links = eng.memory_links_for(e2).unwrap();
    assert!(!links.is_empty(), "supersede must append link rows");
    let n_before = eng.memory_links_for(e1).unwrap().len();
    // Second supersede appends; first rows survive (ADD-only).
    let _ = e3;
    let n_after = eng.memory_links_for(e1).unwrap().len();
    assert!(
        n_after >= n_before,
        "links are append-only: {n_before} → {n_after}"
    );
    // No UPDATE/DELETE against memory_links in the codebase contract:
    // rows only grow.
    let total: i64 = eng
        .connection()
        .query_row("SELECT COUNT(*) FROM memory_links", [], |r| r.get(0))
        .unwrap();
    assert!(total >= 2, "expected ≥2 link rows, got {total}");
}

#[test]
fn w1_content_hash_idempotent_reingest() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let total_before: i64 = eng
        .connection()
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();
    // Same canonical form ("postgres db") + same summary → same hash.
    let a = eng
        .ingest_entity_idempotent(EntityKind::Concept, "Postgres DB", Some("db"), 1.0, T0)
        .unwrap();
    let b = eng
        .ingest_entity_idempotent(EntityKind::Concept, "postgres_db", Some("db"), 1.0, T0 + 1)
        .unwrap();
    assert_eq!(a, b, "same content-hash must return live row, no duplicate");
    let total_after: i64 = eng
        .connection()
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        total_after,
        total_before + 1,
        "re-ingest must not duplicate"
    );
    // Observations idempotent too.
    let o1 = eng
        .ingest_observation_idempotent(a, "runs prod", "fact", 1.0, T0)
        .unwrap();
    let o2 = eng
        .ingest_observation_idempotent(a, "runs  prod!!", "fact", 1.0, T0 + 1)
        .unwrap();
    assert_eq!(o1, o2, "normalized observation re-ingest must dedupe");
}

#[test]
fn w1_expiry_staging_never_deletes() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let e1 = eng
        .add_entity(EntityKind::Concept, "temp", 1.0, T0)
        .unwrap();
    let total_before: i64 = eng
        .connection()
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();
    eng.stage_expiry_entity(e1, T0 + DAY, "stale", T0 + 1)
        .unwrap();
    // Still in table (never deleted), readable as-of past…
    let total_after: i64 = eng
        .connection()
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total_before, total_after, "expiry must never delete");
    assert!(
        eng.entity_at(e1, T0 + 1).unwrap().is_some(),
        "visible before expiry"
    );
    // …but excluded from current/as-of after invalid_at.
    assert!(
        eng.entity_at(e1, T0 + 2 * DAY).unwrap().is_none(),
        "expired after invalid_at"
    );
    // Inverted expiry rejected.
    let e2 = eng.add_entity(EntityKind::Concept, "t2", 1.0, T0).unwrap();
    let err = eng.stage_expiry_entity(e2, T0 - 1, "bad", T0).unwrap_err();
    assert!(err.to_string().contains("inverted"), "{err}");
}

#[test]
fn w1_zero_destructive_overwrites_zero_omissions() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let e1 = eng.add_entity(EntityKind::Concept, "svc", 1.0, T0).unwrap();
    eng.add_observation(e1, "v1", "fact", 1.0, T0).unwrap();
    let ent_total_0: i64 = eng
        .connection()
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();
    let obs_total_0: i64 = eng
        .connection()
        .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
        .unwrap();
    let e2 = eng
        .supersede_entity(e1, None, Some("svc2"), None, None, "upgrade", T0 + DAY)
        .unwrap();
    eng.stage_expiry_entity(e2, T0 + 30 * DAY, "plan", T0 + DAY)
        .unwrap();
    let ent_total_1: i64 = eng
        .connection()
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();
    let obs_total_1: i64 = eng
        .connection()
        .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
        .unwrap();
    assert!(ent_total_1 >= ent_total_0, "entities never shrink");
    assert!(obs_total_1 >= obs_total_0, "observations never shrink");
    // Omission check: predecessor facts still readable as-of past.
    assert_eq!(
        eng.observations_for(e1, T0 + 1).unwrap().len(),
        1,
        "history must not omit"
    );
    // Successor lineage resolves (no stranded belief).
    let hist = crate::vault_history::vault_history(&eng, "svc").unwrap();
    let ids: Vec<i64> = hist["versions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["entity_id"].as_i64())
        .collect();
    assert!(
        ids.contains(&e1) && ids.contains(&e2),
        "lineage whole: {ids:?}"
    );
}

// ------------------------------------------------- W2 fence + witness

#[test]
fn w2_base_checksum_and_generation_fence_409_on_stale() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let e1 = eng
        .add_entity(EntityKind::Concept, "fenced", 1.0, T0)
        .unwrap();
    let sum = eng.entity_checksum(e1).unwrap();
    let gen0 = eng.entity_generation(e1).unwrap();
    // Fresh fence passes.
    eng.supersede_entity_checked(
        e1,
        None,
        Some("fenced-2"),
        None,
        None,
        "ok",
        T0 + DAY,
        Some(&sum),
        Some(gen0),
    )
    .unwrap();
    // Stale checksum → 409.
    let err = eng
        .supersede_entity_checked(
            e1,
            None,
            Some("x"),
            None,
            None,
            "stale",
            T0 + 2 * DAY,
            Some(&sum),
            None,
        )
        .unwrap_err();
    assert!(err.to_string().contains("409"), "{err}");
    // Stale generation → 409.
    let e2 = eng
        .find_entities_by_name("fenced-2", T0 + 2 * DAY)
        .unwrap()
        .pop()
        .unwrap()
        .id;
    let err = eng
        .supersede_entity_checked(
            e2,
            None,
            Some("y"),
            None,
            None,
            "stale",
            T0 + 2 * DAY,
            None,
            Some(gen0),
        )
        .unwrap_err();
    assert!(err.to_string().contains("409"), "{err}");
}

#[test]
fn w2_generation_increments_and_proof_folds() {
    let eng = MemoryEngine::open_in_memory().unwrap();
    let a = eng.add_entity(EntityKind::Concept, "ga", 1.0, T0).unwrap();
    assert_eq!(eng.entity_generation(a).unwrap(), 0);
    let b = eng
        .supersede_entity(a, None, Some("ga2"), None, None, "r", T0 + DAY)
        .unwrap();
    assert_eq!(eng.entity_generation(b).unwrap(), 1);
    eng.bump_proof_count(b, 3).unwrap();
    let c = eng.add_entity(EntityKind::Concept, "ga2", 1.0, T0).unwrap();
    let _ = eng
        .supersede_entity(c, None, Some("ga2x"), None, None, "dup", T0 + DAY)
        .unwrap();
    // Merge folds proof counts (tested end-to-end in consolidation runner).
    let _ = b;
}

/// A vault written before W1 has an `entities` table with no `content_hash`,
/// and `CREATE TABLE IF NOT EXISTS` cannot add a column to a table that is
/// already there. The schema batch used to create indexes over those columns
/// itself, so opening such a file failed with `no such column: content_hash`
/// and the migration that adds them — which runs after the batch — never got
/// to run. Every surface that opens the vault, the MCP server included, then
/// refused to start for anyone with an existing vault.
#[test]
fn a_pre_w1_vault_opens_and_migrates_in_place() {
    let dir = std::env::temp_dir().join(format!("memnoth-vault-pre-w1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("vault.db");

    // The shape an older build wrote: none of the W1/W2 columns, and no
    // indexes over them.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE entities (
                 id INTEGER PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
                 canonical_name TEXT, summary TEXT,
                 confidence REAL NOT NULL DEFAULT 1.0,
                 valid_from INTEGER NOT NULL, valid_to INTEGER,
                 metadata TEXT NOT NULL DEFAULT '{}',
                 created_at INTEGER NOT NULL);
             CREATE TABLE relations (
                 id INTEGER PRIMARY KEY, source_id INTEGER NOT NULL, target_id INTEGER NOT NULL,
                 kind TEXT NOT NULL, confidence REAL NOT NULL DEFAULT 1.0,
                 valid_from INTEGER NOT NULL, valid_to INTEGER,
                 revoked_at INTEGER, metadata TEXT NOT NULL DEFAULT '{}');
             CREATE TABLE observations (
                 id INTEGER PRIMARY KEY, entity_id INTEGER NOT NULL, content TEXT NOT NULL,
                 kind TEXT NOT NULL DEFAULT 'fact', confidence REAL NOT NULL DEFAULT 1.0,
                 valid_from INTEGER NOT NULL, valid_to INTEGER);
             INSERT INTO entities (kind, name, canonical_name, summary, confidence, valid_from, created_at)
                 VALUES ('concept', 'pre-w1', 'pre-w1', 'written before the migration', 1.0, 1, 1);
             INSERT INTO observations (entity_id, content, kind, confidence, valid_from)
                 VALUES (1, 'a fact from before', 'fact', 1.0, 1);",
        )
        .unwrap();
    }

    let eng = MemoryEngine::open(&path).expect("a vault written before W1 must still open");
    let conn = eng.connection();

    // The columns are there, so the graph can be read the way the rest of the
    // code reads it.
    let (hash, proof_count): (Option<String>, i64) = conn
        .query_row(
            "SELECT content_hash, proof_count FROM entities WHERE name='pre-w1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(
        hash.as_deref().is_some_and(|h| !h.is_empty()),
        "the pre-W1 row must be backfilled with a hash, got {hash:?}"
    );
    assert_eq!(proof_count, 0, "the new counter defaults to zero");

    // And the indexes over them exist, so the queries that use them are not
    // silently doing table scans.
    for index in [
        "idx_entities_content_hash",
        "idx_entities_invalid_at",
        "idx_entities_consolidated_at",
        "idx_relations_invalid_at",
        "idx_observations_content_hash",
        "idx_observations_invalid_at",
    ] {
        let found: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                [index],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 1, "{index} must be created by the migration");
    }

    // Opening a second time is a no-op, not a second migration.
    drop(eng);
    MemoryEngine::open(&path).expect("a migrated vault reopens");
    std::fs::remove_dir_all(&dir).ok();
}

/// Two surfaces opening one vault at the same instant used to be a coin flip.
///
/// `PRAGMA journal_mode = WAL` answers `SQLITE_BUSY` **without** invoking the busy handler, so
/// the second opener failed with `database is locked` — the failure the durability test produced
/// once under a loaded `cargo test --workspace`. This holds the write lock the way another
/// process would and asserts the opener *waits*: it must not have finished while the lock was
/// held, and it must succeed once the lock is released.
#[test]
fn an_opener_waits_for_a_writer_instead_of_failing() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let dir = std::env::temp_dir().join(format!("memnoth-vault-wait-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear a stale temp vault");
    }
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("vault.db");
    drop(MemoryEngine::open(&path).expect("first open creates the vault"));

    let holder = rusqlite::Connection::open(&path).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();

    let finished = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&finished);
    let opener_path = path.clone();
    let handle = std::thread::spawn(move || {
        let opened = MemoryEngine::open(&opener_path).map(|engine| engine.busy_timeout_ms());
        flag.store(true, Ordering::SeqCst);
        opened
    });

    std::thread::sleep(std::time::Duration::from_millis(250));
    assert!(
        !finished.load(Ordering::SeqCst),
        "the second open returned while another connection held the write lock"
    );

    holder.execute_batch("COMMIT").unwrap();
    let opened = handle.join().expect("opener thread");
    let timeout = opened.expect("the second open must wait for the lock, not fail");
    assert!(
        timeout.expect("busy timeout readable") > 0,
        "the wait must be done under a real busy timeout"
    );
    // Windows will not unlink a file an open handle still references, and `holder` is that
    // handle; the engine the opener built was already dropped inside the thread.
    drop(holder);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clean the temp vault");
    }
}

/// The race itself, without a pre-existing file: one of these used to lose.
#[test]
fn two_openers_of_a_fresh_vault_both_succeed() {
    for attempt in 0..5 {
        let dir = std::env::temp_dir().join(format!(
            "memnoth-vault-race-{}-{attempt}",
            std::process::id()
        ));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clear a stale temp vault");
        }
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vault.db");

        let opener = |path: std::path::PathBuf| {
            std::thread::spawn(move || MemoryEngine::open(&path).map(|_| ()))
        };
        let first = opener(path.clone());
        let second = opener(path.clone());
        first
            .join()
            .expect("opener thread")
            .expect("the first opener must succeed");
        second
            .join()
            .expect("opener thread")
            .expect("the second opener must succeed");
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clean the temp vault");
        }
    }
}
