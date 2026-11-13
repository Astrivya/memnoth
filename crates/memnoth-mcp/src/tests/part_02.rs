use super::*;

#[test]
fn read_only_refuses_writes_but_not_reads() {
    assert!(read_only_refusal("mem_search", true).is_none());
    assert!(read_only_refusal("vault_asof", true).is_none());
    let refusal = read_only_refusal("mem_add_fact", true).expect("a write must be refused");
    assert!(refusal.contains("read-only"), "{refusal}");
    assert!(refusal.contains("--allow-writes"), "{refusal}");
    assert!(read_only_refusal("mem_add_fact", false).is_none());
}

/// Every arm added for the optimisation waves must be **nameable and
/// parseable**, because an arm the arena cannot address is an arm that cannot
/// be measured — which is the failure the `Fusion::ALL` list exists to prevent.
/// The flag assertions are the ablation's honesty check: each of these differs
/// from the arm it is measured against in exactly one property.
#[test]
fn the_optimisation_arms_are_addressable_and_differ_from_their_controls() {
    for (name, arm) in [
        ("best_chunk", Fusion::BestChunk),
        ("facet_coverage", Fusion::FacetCoverage),
        ("temporal_intent", Fusion::TemporalIntent),
        ("rrf_idf_norm_bounded", Fusion::RrfIdfNormBounded),
        ("dense_tail", Fusion::DenseTail),
        ("dense_weighted", Fusion::DenseWeighted),
        ("prf", Fusion::Prf),
        ("union", Fusion::Union),
        ("graph_reserve", Fusion::GraphReserve),
        ("causal_graph", Fusion::CausalGraph),
        ("causal_soft", Fusion::CausalSoft),
        ("preference_boost", Fusion::PreferenceBoost),
    ] {
        assert_eq!(Fusion::parse(name), Some(arm), "`{name}` must parse");
        assert!(Fusion::ALL.contains(&arm), "`{name}` must be in ALL");
        assert_eq!(arm.label(), name, "label round-trip");
    }
    // `graph_reserve` is `bm25` with **one** property changed — the traversal gets slots
    // inside the cut. Everything else must be identical, or the pair measures two changes
    // and attributes both to the graph.
    assert!(Fusion::GraphReserve.reserves_graph_slots());
    assert!(!Fusion::Bm25.reserves_graph_slots());
    assert_eq!(
        Fusion::GraphReserve.uses_scores(),
        Fusion::Bm25.uses_scores()
    );
    assert_eq!(
        Fusion::GraphReserve.fuses_graph_inline(),
        Fusion::Bm25.fuses_graph_inline()
    );
    assert_eq!(Fusion::GraphReserve.uses_idf(), Fusion::Bm25.uses_idf());
    // J is B's combiner, so it must share B's config exactly.
    assert_eq!(Fusion::BestChunk.uses_scores(), Fusion::Bm25.uses_scores());
    assert_eq!(
        Fusion::BestChunk.fuses_graph_inline(),
        Fusion::Bm25.fuses_graph_inline()
    );
    assert_eq!(
        Fusion::FacetCoverage.uses_scores(),
        Fusion::Bm25.uses_scores()
    );
    // M is D with one clamp, so it must share every flag with D.
    assert_eq!(
        Fusion::RrfIdfNormBounded.uses_idf(),
        Fusion::RrfIdfNorm.uses_idf()
    );
    assert_eq!(
        Fusion::RrfIdfNormBounded.fuses_graph_inline(),
        Fusion::RrfIdfNorm.fuses_graph_inline()
    );
    assert_eq!(
        Fusion::RrfIdfNormBounded.uses_scores(),
        Fusion::RrfIdfNorm.uses_scores()
    );
    // The three dense arms are one retriever and three *placement rules*, so they
    // must share every flag the placement does not name — otherwise the arm
    // comparison would be measuring a different retriever, which is exactly the
    // failure the flag assertions exist to prevent.
    for arm in [Fusion::DenseTail, Fusion::DenseWeighted] {
        assert!(arm.has_dense_arm(), "{arm:?} must carry the dense arm");
        assert_eq!(
            arm.uses_scores(),
            Fusion::Bm25Graph.uses_scores(),
            "{arm:?}"
        );
        assert!(!arm.fuses_graph_inline(), "{arm:?}");
    }
    // PRF is a second pass with the shipping arm, not a fourth retriever.
    assert!(!Fusion::Prf.has_dense_arm());
    assert_eq!(Fusion::Prf.uses_scores(), Fusion::Bm25Graph.uses_scores());
    // The union wraps the shipping arm, so its primary half is the score-ranked
    // arm's own list. It must not carry the dense arm as a *placement* flag:
    // `Union` builds its pool by calling the other arms, and the dense generator
    // is one of its sources, so routing it through the dense block as well would
    // run the embedder twice over the same corpus for one search.
    assert!(!Fusion::Union.has_dense_arm());
    assert_eq!(Fusion::Union.uses_scores(), Fusion::Bm25Graph.uses_scores());
}

/// The union's contract, and the one thing it must never do.
///
/// Three claims, all load-bearing: **the pool's opening slots are the shipping
/// arm's own hits in the shipping arm's own order** (so a union cannot be worse
/// than the arm it wraps at the depths a reranker trusts), **it is bounded and
/// de-duplicated** (a repeated entity spends a rerank slot on nothing, and an
/// unbounded pool makes the pass unpriceable), and **its membership can include a
/// document the primary arm never returned** — without which it is an expensive
/// alias and the ceiling argument is dead. Nothing here claims the union *ranks*
/// better: a cross-source score column is not comparable, which is why the arena
/// only reads it through a reranker.
#[test]
fn union_pool_keeps_the_primary_order_and_only_adds_members() {
    let (e, _c) = setup();
    let now = memnoth_engine::now_ms();
    for i in 0..6 {
        let ent = e
            .add_entity(EntityKind::Other, &format!("alpha beta note {i}"), 0.9, now)
            .unwrap();
        e.add_observation(
            ent,
            &format!("alpha beta decision number {i}"),
            "fact",
            0.9,
            now,
        )
        .unwrap();
    }
    let limit = 5;
    let primary = relaxed_search(&e, "alpha beta", limit, now, Fusion::Bm25Graph).unwrap();
    let union = union_search(&e, "alpha beta", limit, now).unwrap();
    assert!(!primary.is_empty(), "the fixture must retrieve something");
    assert!(
        union.len() <= limit,
        "the pool is bounded by the caller's limit"
    );
    let ids: Vec<i64> = union.iter().map(|(u, _)| u.id).collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(ids.len(), unique.len(), "union must not repeat an entity");
    assert_eq!(
        union[0].0.id, primary[0].0.id,
        "the primary's best hit leads the pool"
    );
    let quota = (((limit as f64) * union_primary_share()).round() as usize).clamp(1, limit);
    for (ent, _) in primary.iter().take(quota) {
        assert!(
            union.iter().any(|(u, _)| u.id == ent.id),
            "primary hit {} is missing from the pool",
            ent.id
        );
    }
}

/// PRF has to actually raise the ceiling, and this is the cheapest place to prove
/// it: one document matches the question, a second document answers it in words
/// the question never uses. Only feedback can reach the second one.
#[test]
fn prf_reaches_a_document_the_query_vocabulary_cannot() {
    let (e, c) = setup();
    let now = memnoth_engine::now_ms();
    // (1) matches the query. Its facts carry the vocabulary of the answer.
    let seed = e
        .add_entity(EntityKind::Other, "release gate review notes", 0.9, now)
        .unwrap();
    e.add_observation(
        seed,
        "the release gate review flagged the backoff as too aggressive and we halted the deploy",
        "fact",
        0.9,
        now,
    )
    .unwrap();
    // (2) the actual answer, in words the query does not contain.
    let answer = e
        .add_entity(
            EntityKind::Other,
            "incident chronology for the rollout",
            0.9,
            now,
        )
        .unwrap();
    e.add_observation(
        answer,
        "backoff was aggressive so the deploy was halted mid rollout",
        "fact",
        0.9,
        now,
    )
    .unwrap();
    for i in 0..15 {
        e.add_entity(EntityKind::Other, &format!("unrelated {i}"), 0.4, now)
            .unwrap();
    }
    let query = "release gate review";
    let plain = call_tool(
        &e,
        &c,
        1,
        "mem_search",
        json!({"query": query, "limit": 10, "fusion": "bm25_graph", "text_chars": 0}),
    );
    let feedback = call_tool(
        &e,
        &c,
        2,
        "mem_search",
        json!({"query": query, "limit": 10, "fusion": "prf", "text_chars": 0}),
    );
    let names = |payload: &Value| -> Vec<String> {
        payload["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap_or_default().to_string())
            .collect()
    };
    assert!(
        !names(&plain)
            .iter()
            .any(|n| n == "incident chronology for the rollout"),
        "the control must NOT already find it, or this test proves nothing: {plain}"
    );
    assert!(
        names(&feedback)
            .iter()
            .any(|n| n == "incident chronology for the rollout"),
        "PRF must reach the answer document through the seed's vocabulary: {feedback}"
    );
    assert!(
        feedback["ranking"]
            .as_str()
            .unwrap_or_default()
            .contains("prf")
    );
}

/// W4.2's contract, forced rather than hoped for: a query made entirely of
/// ubiquitous tokens must still be searched. This is the case that would block
/// recall if the skip rule were ever written to skip unconditionally.
#[test]
fn token_selection_can_never_block_recall() {
    let ubiquitous = |name: &str| (name.to_string(), 9_000.0_f64, true);
    let rare = ("instance-042".to_string(), 1.0_f64, false);
    // Every token common, and every probe capped: one is still searched.
    let all_common = vec![ubiquitous("the"), ubiquitous("record"), ubiquitous("run")];
    assert_eq!(select_searchable_tokens(&all_common, 10_000.0).len(), 1);
    // A rare token is never the one skipped in favour of a common one.
    let mixed = vec![
        ("common".to_string(), 5_000.0_f64, false),
        rare,
        ubiquitous("record"),
    ];
    let chosen = select_searchable_tokens(&mixed, 10_000.0);
    assert_eq!(chosen.first().copied(), Some(0));
    assert!(
        chosen.contains(&1),
        "the rare token must survive: {chosen:?}"
    );
    // An empty token list is empty, not a panic.
    assert!(select_searchable_tokens(&[], 10_000.0).is_empty());
}

/// W4.1 must read dates without reading ordinary words as weekdays or months —
/// the false-positive mode that would silently delete the answer.
#[test]
fn query_dates_are_real_dates_only() {
    // Order is the question's order, not a sort: the token list is a filter,
    // and its order is only ever read by `all(...)`, so re-sorting it would be
    // churn that implies a meaning the caller does not have.
    assert_eq!(
        query_date_tokens("what did we do in march 2024"),
        vec!["march", "2024"]
    );
    assert_eq!(
        query_date_tokens("when was the review"),
        Vec::<String>::new()
    );
    // "may" as a modal verb and "mar" as a verb fragment are not dates. This is
    // the assertion that caught the bug: lowercase `may` passed the month filter
    // and then gated a filter requiring the word in every candidate's facts.
    assert!(query_date_tokens("you may mar the file").is_empty());
    assert!(query_date_tokens("the 1840 record").is_empty());
    // Capitalised, it is the month — and an all-lowercase question keeps its
    // year either way, so the filter never loses its anchor entirely.
    assert_eq!(
        query_date_tokens("what happened in May 2024"),
        vec!["may", "2024"]
    );
    assert_eq!(query_date_tokens("what happened in may 2024"), vec!["2024"]);
}

/// W4.4 — a term the index cannot lexeme never reaches the per-token pass.
#[test]
fn unlexable_tokens_are_not_searched() {
    let tokens = content_tokens("the pipeline ... --- 12345 cache");
    assert!(tokens.iter().all(|t| !t.is_empty()), "{tokens:?}");
    assert!(
        tokens.iter().any(|t| t == "pipeline"),
        "a real token must survive the lexeme filter: {tokens:?}"
    );
}

/// The relaxed path must run through the configured fusion and **say which
/// one** produced the order — the label is what keeps a stored result set
/// attributable after the default has moved on.
#[test]
fn the_relaxed_path_names_the_fusion_that_produced_the_order() {
    let (e, c) = setup();
    let now = memnoth_engine::now_ms();
    let target = e
        .add_entity(
            EntityKind::Other,
            "billing retry backoff instance-042",
            0.9,
            now,
        )
        .unwrap();
    e.add_observation(
        target,
        "billing retry backoff instance-042 was widened after the incident review",
        "fact",
        0.9,
        now,
    )
    .unwrap();
    for i in 0..20 {
        e.add_entity(
            EntityKind::Other,
            &format!("billing retry backoff instance-{i:03}"),
            0.9,
            now,
        )
        .unwrap();
    }
    let query = "what did we decide about billing retry backoff instance-042";
    let resp = call_tool(&e, &c, 1, "mem_search", json!({"query": query, "limit": 5}));
    assert!(
        resp["ranking"]
            .as_str()
            .unwrap_or_default()
            .contains("bm25_graph"),
        "the response must name the fusion that ran: {resp}"
    );
    assert_eq!(
        resp["results"][0]["id"].as_i64(),
        Some(target),
        "the discriminating memory must still rank first: {resp}"
    );

    // The off-schema escape hatch selects a different arm, and the label
    // follows it — so an operator can pin the pre-arena ordering.
    let older = call_tool(
        &e,
        &c,
        2,
        "mem_search",
        json!({"query": query, "limit": 5, "fusion": "rrf"}),
    );
    assert!(
        older["ranking"]
            .as_str()
            .unwrap_or_default()
            .contains("rrf ("),
        "a pinned fusion must be named in the response: {older}"
    );
}

/// A revision must be reachable from **either** name, and must not read as a
/// duplicate. All three assertions here are bugs the temporal benchmark found:
/// facts were moved (emptying the past), the chain was not linked (so
/// `vault_history` on the new name listed one version), and the chain edge was
/// rewired into a self-loop by the very operation that wrote it.
#[test]
fn a_revision_keeps_its_history_and_is_reachable_by_either_name() {
    let (e, c) = setup();
    let old = "auth-service";
    let new = "identity-service";
    let fact = "auth-service owns the legacy token store";
    call_tool(
        &e,
        &c,
        1,
        "mem_add_fact",
        json!({"entity_name": old, "content": fact}),
    );
    // A distinct millisecond on each side, so "believed before the revision"
    // is unambiguous rather than a boundary case.
    std::thread::sleep(std::time::Duration::from_millis(3));
    let before = memnoth_engine::now_ms();
    std::thread::sleep(std::time::Duration::from_millis(3));
    let revised = call_tool(
        &e,
        &c,
        2,
        "mem_supersede",
        json!({"name": old, "new_name": new, "reason": "renamed in consolidation"}),
    );
    assert_eq!(revised["facts_copied"], json!(1), "{revised}");

    // The current belief answers, with the predecessor's facts on it.
    let hits = call_tool(&e, &c, 3, "mem_search", json!({"query": new, "limit": 5}));
    assert_eq!(hits["results"][0]["name"], json!(new), "{hits}");

    // History is reachable by the name the host actually has now, and a revision
    // is one version per belief: the live row and the creation snapshot are the
    // same revision, so a one-step revision reads as two versions, not four.
    let history = call_tool(&e, &c, 4, "vault_history", json!({"name": new}));
    assert_eq!(
        history["count"],
        json!(2),
        "a one-step revision is two versions: {history}"
    );
    let versions = history["versions"].as_array().cloned().unwrap_or_default();
    let mut ids: Vec<i64> = versions
        .iter()
        .filter_map(|v| v["entity_id"].as_i64())
        .collect();
    ids.sort_unstable();
    let distinct = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), distinct, "each revision listed once: {history}");
    assert_eq!(ids.len(), 2, "predecessor and successor: {history}");
    // The kept version still carries the journal's reason and snapshot: the fold
    // merges the two copies of a revision, it does not drop one of them.
    assert_eq!(
        versions[0]["change_reason"],
        json!("renamed in consolidation"),
        "the closed revision keeps the reason it closed: {history}"
    );
    assert!(
        !versions[0]["snapshot"].is_null(),
        "the closed revision keeps its snapshot: {history}"
    );
    let contra = call_tool(&e, &c, 5, "vault_contradictions", json!({"name": new}));
    assert!(
        contra["count"].as_u64().unwrap_or(0) >= 1,
        "the revision reason must be explainable: {contra}"
    );

    // The earlier belief still answers, with the fact it held then.
    let asof = call_tool(
        &e,
        &c,
        6,
        "vault_asof",
        json!({"name": old, "as_of_ms": before}),
    );
    assert!(
        asof.to_string().contains(fact),
        "the pre-revision belief must still carry its facts: {asof}"
    );

    // And a revision is history, not duplication: a host acting on a duplicate
    // report would merge a successor into its own predecessor.
    let dups = call_tool(&e, &c, 7, "mem_duplicates", json!({"limit": 10}));
    assert!(
        !dups.to_string().contains(fact),
        "a revision must never be reported as a duplicate fact: {dups}"
    );
}

/// A retraction must remove a belief from search *without* removing it from
/// history — the distinction between "wrong" and "outdated" that
/// `mem_forget` and `mem_supersede` exist to keep.
#[test]
fn a_retraction_is_explainable_and_recoverable() {
    let (e, c) = setup();
    let fact = "staging cluster runs on the old scheduler";
    call_tool(
        &e,
        &c,
        1,
        "mem_add_fact",
        json!({"entity_name": "staging-cluster", "content": fact}),
    );
    std::thread::sleep(std::time::Duration::from_millis(3));
    let before = memnoth_engine::now_ms();
    std::thread::sleep(std::time::Duration::from_millis(3));
    let revoked = call_tool(
        &e,
        &c,
        2,
        "mem_forget",
        json!({"name": "staging-cluster", "reason": "wrong: it is the prod cluster"}),
    );
    assert_eq!(revoked["journaled"], json!(true), "{revoked}");

    let hits = call_tool(
        &e,
        &c,
        3,
        "mem_search",
        json!({"query": "staging cluster scheduler", "limit": 5}),
    );
    let names: Vec<&str> = hits["results"]
        .as_array()
        .map(|rows| rows.iter().filter_map(|r| r["name"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        !names.contains(&"staging-cluster"),
        "a retracted belief must leave search: {hits}"
    );

    let recovered = call_tool(
        &e,
        &c,
        4,
        "vault_asof",
        json!({"name": "staging-cluster", "as_of_ms": before}),
    );
    assert!(
        recovered.to_string().contains(fact),
        "it must still be recoverable as of before the retraction: {recovered}"
    );
    let contra = call_tool(
        &e,
        &c,
        5,
        "vault_contradictions",
        json!({"name": "staging-cluster"}),
    );
    assert!(
        contra.to_string().contains("wrong: it is the prod cluster"),
        "the retraction reason must be explainable: {contra}"
    );
}

/// A question asked in prose must reach the vault. The FTS layer AND-s all
/// query words against name/summary, so before relaxation this returned
/// nothing at all — the defect the evidence harness measured as 0% recall.
#[test]
fn mem_search_relaxes_a_prose_question_instead_of_returning_nothing() {
    let (e, c) = setup();
    let now = memnoth_engine::now_ms();
    let target = e
        .add_entity(
            EntityKind::Other,
            "deploy pipeline cache instance-007",
            0.9,
            now,
        )
        .unwrap();
    e.add_observation(
        target,
        "deploy pipeline cache instance-007 retries twice before surfacing the error",
        "fact",
        0.9,
        now,
    )
    .unwrap();
    // `instance-007` is the target above; the competing fixture must not try to
    // insert the same name twice, or the second insert hits the vault's unique
    // constraint before the search under test ever runs.
    for i in (0..30).filter(|i| *i != 7) {
        let other = e
            .add_entity(
                EntityKind::Other,
                &format!("deploy pipeline cache instance-{i:03}"),
                0.9,
                now,
            )
            .unwrap();
        e.add_observation(
            other,
            "a competing memory on the same topic",
            "fact",
            0.9,
            now,
        )
        .unwrap();
    }

    // The exact AND query cannot match: `what`, `happens`, `with`, `the`,
    // `discussed` are not in any entity name.
    let exact = engine_none(
        &e,
        "what happens with the deploy pipeline cache instance-007 that we discussed",
    );
    assert!(exact.is_empty(), "the AND query is what fails: {exact:?}");

    let resp = call_tool(
        &e,
        &c,
        1,
        "mem_search",
        json!({"query": "what happens with the deploy pipeline cache instance-007 that we discussed", "limit": 5}),
    );
    let results = resp["results"].as_array().unwrap();
    assert!(
        !results.is_empty(),
        "a prose question must not return an empty vault: {resp}"
    );
    assert!(
        resp["ranking"]
            .as_str()
            .unwrap_or_default()
            .contains("relaxed"),
        "the response must say the search was relaxed: {resp}"
    );
    assert_eq!(
        results[0]["id"].as_i64(),
        Some(target),
        "the entity carrying the discriminating id must rank first: {resp}"
    );
}

/// The rerank pass must actually fire on a multi-candidate result, keep the
/// lexically-loaded entity on top, and say which ranker produced the
/// answer. Default stays off and stays `rrf`.
#[test]
fn mem_search_rerank_demotes_redundancy_and_names_the_method() {
    let (e, c) = setup();
    let now = memnoth_engine::now_ms();
    // Both names share the query vocabulary, so both survive FTS; they
    // differ in how much of their fact text echoes the query.
    let rich = e
        .add_entity(EntityKind::Other, "deploy pipeline", 0.9, now)
        .unwrap();
    let thin = e
        .add_entity(EntityKind::Other, "pipeline stage", 0.9, now)
        .unwrap();
    e.add_observation(
        rich,
        "the deploy pipeline runs migrations before rollout",
        "fact",
        0.9,
        now,
    )
    .unwrap();
    e.add_observation(
        rich,
        "deploy pipeline stages build then test then deploy",
        "fact",
        0.9,
        now,
    )
    .unwrap();
    e.add_observation(
        rich,
        "deploy pipeline retries failed stages twice",
        "fact",
        0.9,
        now,
    )
    .unwrap();
    e.add_observation(
        thin,
        "unrelated content about gardening tools",
        "fact",
        0.9,
        now,
    )
    .unwrap();

    let plain = call_tool(
        &e,
        &c,
        1,
        "mem_search",
        json!({"query": "pipeline", "limit": 5}),
    );
    // The default arm, named: `mem_search` used to report `"rrf"` for every
    // search that was not relaxed, which named an arm it had not run.
    assert_eq!(plain["ranking"], json!("bm25_graph"), "{plain}");
    let plain_len = plain["results"].as_array().unwrap().len();
    assert!(
        plain_len >= 2,
        "fixture must produce multiple candidates: {plain}"
    );

    let reranked = call_tool(
        &e,
        &c,
        2,
        "mem_search",
        json!({"query": "pipeline", "limit": 5, "rerank": true}),
    );
    assert!(
        reranked["ranking"]
            .as_str()
            .unwrap_or_default()
            .starts_with("mmr"),
        "the response must name the rerank method: {reranked}"
    );
    let rows = reranked["results"].as_array().unwrap();
    assert_eq!(
        rows.first().and_then(|r| r["id"].as_i64()),
        Some(rich),
        "the entity whose name AND facts carry the query terms must rank first: {reranked}"
    );
    assert!(
        rows.iter().any(|r| r["id"] == json!(thin)),
        "a candidate that fused in must not be silently dropped by the rerank: {reranked}"
    );
    // Scores are normalized (0..1) in the reranked response.
    for row in rows {
        let s = row["score"].as_f64().unwrap();
        assert!(
            (0.0..=1.0).contains(&s),
            "mmr scores are normalized: {reranked}"
        );
    }
}

/// A mode only mentioned in a description is not reachable by a host that reads
/// the schema, so `rerank=cross_encoder` was unreachable in production while it
/// was fully implemented and measured. This pins the declaration, not just the
/// handler: if the enum disappears again the handler becomes dead code that no
/// compliant client can ask for.
#[test]
fn mem_search_declares_the_rerank_modes_it_implements() {
    let defs = tool_definitions();
    let search = defs.iter().find(|d| d["name"] == "mem_search").unwrap();
    let rerank = &search["inputSchema"]["properties"]["rerank"];
    let modes: Vec<&str> = rerank["enum"]
        .as_array()
        .expect("rerank must declare an enum so a host can select a mode")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(modes.contains(&"off"), "{modes:?}");
    assert!(modes.contains(&"mmr"), "{modes:?}");
    assert!(modes.contains(&"cross_encoder"), "{modes:?}");
    // The assisted mode must be *declared* too. An assisted capability that a
    // host cannot select does not exist, and one that exists but is undeclared
    // is worse: it can only be reached by a caller copying our benchmarks.
    assert!(modes.contains(&"llm"), "{modes:?}");
    assert!(
        rerank["description"]
            .as_str()
            .unwrap_or("")
            .contains("assisted"),
        "the assisted mode must say so in the schema a host reads: {rerank}"
    );
    assert!(
        !search["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("rerank")),
        "rerank is opt-in and must stay optional"
    );
}

/// The assisted mode must be plumbed **and fail-open**, for the same reason the
/// cross-encoder is: with a key it reorders and labels itself `assisted`; without
/// one it returns the fused order and says why. Neither outcome may error or empty
/// the result set — an optional external service is not allowed to break a search.
/// Asserting the union keeps this deterministic with or without a key set.
#[test]
fn mem_search_assisted_mode_is_reachable_and_never_fails_the_search() {
    let (e, c) = setup();
    let now = memnoth_engine::now_ms();
    let target = e
        .add_entity(
            EntityKind::Other,
            "release gate review for the billing retry",
            0.9,
            now,
        )
        .unwrap();
    e.add_observation(
        target,
        "the release gate review found the billing retry backoff was too aggressive",
        "fact",
        0.9,
        now,
    )
    .unwrap();
    // Several entities must match the query, or the rerank branch is never
    // reached and this test would pass while asserting nothing. The first
    // version added 25 *unrelated* notes and got a single hit, so the response
    // legitimately said `rrf` and the test failed for the right reason.
    for i in 0..6 {
        let peer = e
            .add_entity(
                EntityKind::Other,
                &format!("release gate review note {i} on billing retry"),
                0.6,
                now,
            )
            .unwrap();
        e.add_observation(
            peer,
            &format!("the release gate review of billing retry round {i} was recorded"),
            "fact",
            0.6,
            now,
        )
        .unwrap();
    }
    let response = call_tool(
        &e,
        &c,
        1,
        "mem_search",
        json!({"query": "release gate review billing retry", "limit": 5, "rerank": "llm"}),
    );
    let ranking = response["ranking"].as_str().unwrap_or_default();
    assert!(
        ranking.contains("assisted") || ranking.contains("llm rerank"),
        "the response must name what actually ran: {ranking}"
    );
    assert!(
        !response["results"].as_array().unwrap().is_empty(),
        "an unavailable provider must not empty the result set: {response}"
    );
    // Scores stay on one scale in every mode: the top hit is 1.0 or 0.0-normalized,
    // never a raw provider position or a logit.
    for row in response["results"].as_array().unwrap() {
        let score = row["score"].as_f64().unwrap_or(f64::NAN);
        assert!(
            (0.0..=1.0).contains(&score),
            "`score` must stay in 0..1 across rerank modes: {row}"
        );
    }
}

/// The cross-encoder arm must be *plumbed* whether or not the model is installed:
/// with a model it reorders, without one it falls back to fusion order. Both
/// outcomes name the cross-encoder, and neither may turn a search into an error
/// or silently drop the result set. Asserting the union (rather than one branch)
/// keeps this deterministic on a machine without the ONNX model while still
/// proving the knob reaches the reranker.
#[test]
fn mem_search_cross_encoder_mode_is_reachable_and_never_fails_the_search() {
    let (e, c) = setup();
    let now = memnoth_engine::now_ms();
    let a = e
        .add_entity(EntityKind::Other, "deploy pipeline", 0.9, now)
        .unwrap();
    let b = e
        .add_entity(EntityKind::Other, "pipeline stage", 0.9, now)
        .unwrap();
    e.add_observation(
        a,
        "the deploy pipeline runs migrations before rollout",
        "fact",
        0.9,
        now,
    )
    .unwrap();
    e.add_observation(b, "a pipeline stage is a unit of work", "fact", 0.9, now)
        .unwrap();

    let out = call_tool(
        &e,
        &c,
        1,
        "mem_search",
        json!({"query": "pipeline", "limit": 5, "rerank": "cross_encoder"}),
    );
    let ranking = out["ranking"].as_str().unwrap_or_default();
    assert!(
        ranking.contains("cross-encoder"),
        "the response must name the cross-encoder arm (scored or unavailable): {out}"
    );
    assert!(
        !out["results"].as_array().unwrap().is_empty(),
        "a missing optional model must not empty the result set: {out}"
    );
    // Whatever arm ran, the pool was not silently truncated below the request.
    assert!(
        out["results"].as_array().unwrap().len() <= 5,
        "limit must be respected: {out}"
    );
}

/// The merge is the write half of dedup, so the test has to prove the
/// properties that make it safe on a bi-temporal vault: facts move (not
/// copy), true duplicates are skipped, relations rewire without creating
/// self-loops or parallel edges, the closed entity is journaled with the
/// reason — and an MRTR-declaring host is asked before any of it happens.
#[test]
fn mem_merge_moves_facts_rewires_edges_journals_and_asks_first() {
    let (e, c) = setup();
    let now = memnoth_engine::now_ms();
    let keeper = e
        .add_entity(EntityKind::Other, "AuthService", 0.9, now)
        .unwrap();
    let dup = e
        .add_entity(EntityKind::Other, "auth_service", 0.9, now)
        .unwrap();
    let third = e
        .add_entity(EntityKind::Other, "SessionStore", 0.9, now)
        .unwrap();

    let shared = "the auth token is cached for fifteen minutes";
    e.add_observation(keeper, shared, "fact", 0.9, now).unwrap();
    e.add_observation(keeper, "refresh uses a rotating secret", "fact", 0.9, now)
        .unwrap();
    e.add_observation(dup, shared, "fact", 0.9, now).unwrap(); // true duplicate → skip
    e.add_observation(
        dup,
        "supports SAML through the enterprise IdP",
        "fact",
        0.9,
        now,
    )
    .unwrap(); // moves
    e.add_relation(dup, third, "calls", 0.9, now).unwrap(); // rewires to keeper→third
    e.add_relation(third, dup, "calls", 0.9, now).unwrap(); // rewires to third→keeper
    e.add_relation(dup, keeper, "calls", 0.9, now).unwrap(); // would self-loop → dropped

    // An elicitation-declaring host must be asked BEFORE the write. Client
    // capabilities travel in `_meta` per request (the stateless era's
    // mechanism — there is no initialize handshake carrying them anymore).
    let asked = handle_message_for_profile(
        &e,
        &c,
        Profile::Full,
        rpc(
            1,
            "tools/call",
            json!({
                "name": "mem_merge",
                "arguments": {
                    "keeper_id": keeper, "merge_id": dup, "reason": "same service, two spellings"},
                "_meta": {crate::META_CLIENT_CAPABILITIES: {"elicitation": {}}}
            }),
        ),
        None,
        None,
    )
    .unwrap();
    assert_eq!(asked["result"]["resultType"], "input_required", "{asked}");
    // And nothing may have moved yet — the ask precedes the write.
    assert_eq!(
        e.observations_for(dup, now).unwrap().len(),
        2,
        "an unapproved merge must not have touched the vault"
    );

    // Without an eliciting client (most hosts today), the write answers
    // synchronously — the declared-caps path is covered by the MRTR suite.
    let resp = call_tool(
        &e,
        &c,
        2,
        "mem_merge",
        json!({
            "keeper_id": keeper, "merge_id": dup, "reason": "same service, two spellings"}),
    );
    assert_eq!(resp["facts_moved"], json!(1), "{resp}");
    assert_eq!(resp["facts_skipped_duplicate"], json!(1), "{resp}");
    assert_eq!(resp["relations_rewired"], json!(3), "{resp}");
    assert_eq!(resp["journaled"], json!(true), "{resp}");

    // Facts moved, not copied.
    let keeper_facts: Vec<String> = e
        .observations_for(keeper, now)
        .unwrap()
        .iter()
        .map(|o| o.content.clone())
        .collect();
    assert!(
        keeper_facts.iter().any(|f| f.contains("SAML")),
        "the moving fact must now live on the keeper: {keeper_facts:?}"
    );
    // The skipped duplicate stays on the closed entity — it was not moved
    // precisely because the keeper already holds that text, so nothing is
    // lost, and the closed entity is history, not a live duplicate.
    let dup_left: Vec<String> = e
        .observations_for(dup, now)
        .unwrap()
        .iter()
        .map(|o| o.content.clone())
        .collect();
    assert_eq!(dup_left, vec![shared.to_string()], "{dup_left:?}");
    // The duplicate is closed — and the journal says why. `entities_current`
    // is the real "still believed" set; `entity_at(.., now)` is not usable
    // here because the dispatcher stamps its own (later) `now` on the close.
    let current_ids: Vec<i64> = e.entities_current().unwrap().iter().map(|x| x.id).collect();
    assert!(
        !current_ids.contains(&dup),
        "the merged entity must no longer be current: {current_ids:?}"
    );
    assert!(current_ids.contains(&keeper));
    let conn = e.connection();
    let (reason, closed): (Option<String>, Option<i64>) = conn
        .query_row(
            "SELECT change_reason, valid_to FROM entity_versions WHERE entity_id = ?1 \
                 ORDER BY id DESC LIMIT 1",
            [dup],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(closed.is_some(), "the close must be journaled: {reason:?}");
    assert!(
        reason.unwrap_or_default().contains("merged into"),
        "the journal carries the merge reason"
    );
    // Relations rewired, self-loop gone.
    let edges = e.read_graph().unwrap().1;
    assert_eq!(
        edges.len(),
        2,
        "self-loop dropped, two real edges remain: {edges:?}"
    );
    for edge in &edges {
        assert_ne!(edge.source_id, edge.target_id, "no self-loops: {edges:?}");
        assert!(
            (edge.source_id == keeper && edge.target_id == third)
                || (edge.source_id == third && edge.target_id == keeper),
            "edges rewire to the keeper: {edges:?}"
        );
    }
}

/// The fuzzy pass is opt-in, reported separately from the deterministic
/// groups, never auto-merged — and `AuthService`/`AuthSvc` (the exact pair
/// the old note said was NOT grouped) becomes a *candidate* only here.
#[test]
fn mem_duplicates_fuzzy_pass_reports_stem_candidates_separately() {
    let (e, c) = setup();
    let now = memnoth_engine::now_ms();
    e.add_entity(EntityKind::Other, "AuthService", 0.9, now)
        .unwrap();
    e.add_entity(EntityKind::Other, "AuthSvc", 0.9, now)
        .unwrap();
    e.add_entity(EntityKind::Other, "BillingService", 0.9, now)
        .unwrap();

    // Off by default: the deterministic passes must NOT group these.
    let plain = call_tool(&e, &c, 1, "mem_duplicates", json!({}));
    assert!(
        plain["entity_groups"].as_array().unwrap().is_empty(),
        "stem-similar names are not a normalization collision: {plain}"
    );
    assert!(
        plain["fuzzy_groups"].as_array().unwrap().is_empty(),
        "{plain}"
    );

    // Opt in: they become candidates, in their own section.
    let fuzzy = call_tool(&e, &c, 2, "mem_duplicates", json!({"fuzzy": true}));
    let groups = fuzzy["fuzzy_groups"].as_array().unwrap();
    assert!(
        !groups.is_empty(),
        "AuthService/AuthSvc share the `authserv` stem: {fuzzy}"
    );
    assert!(
        !groups.iter().any(|g| g["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "BillingService")),
        "a name with no shared stem stays out: {fuzzy}"
    );
}

/// Half-life decay must do the one thing its docstring promises: a
/// high-count entity whose accesses are all old loses to a low-count one
/// that was hit now. And the default path must stay untouched — a host that
/// never set the env var gets the same order it always did.
#[test]
fn vault_top_decay_ages_out_stale_hits_but_the_default_is_unchanged() {
    let (e, c) = setup();
    let now = memnoth_engine::now_ms();
    let day: i64 = 24 * 60 * 60 * 1000;
    let legacy = e
        .add_entity(EntityKind::Other, "legacy-hot", 0.9, now - 40 * day)
        .unwrap();
    let fresh = e
        .add_entity(EntityKind::Other, "fresh-hot", 0.9, now - day)
        .unwrap();
    // 10 hits, all ~30 half-lives back (half-life = 1 day).
    for _ in 0..10 {
        e.record_access(legacy, now - 30 * day)
            .expect("record legacy access");
    }
    // 2 hits, now.
    e.record_access(fresh, now).expect("record fresh access 1");
    e.record_access(fresh, now).expect("record fresh access 2");

    // Default: raw count wins, exactly as before the decay pass existed.
    let plain = call_tool(&e, &c, 1, "vault_top", json!({"limit": 10}));
    let plain_ids: Vec<i64> = plain["top"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["entity_id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        plain_ids.first(),
        Some(&legacy),
        "raw count must win by default: {plain}"
    );
    assert!(
        plain["top"].as_array().unwrap()[0]
            .get("decayed_score")
            .is_none(),
        "no decay configured means no decay fields: {plain}"
    );

    // Decay on (explicit parameter, no process-global env mutation —
    // parallel tests would race on it): fresh wins, the stale pile ages
    // out.
    let decayed = call_tool(
        &e,
        &c,
        2,
        "vault_top",
        json!({"limit": 10, "decay_ms": day}),
    );
    let rows = decayed["top"].as_array().unwrap();
    assert_eq!(
        rows.first().and_then(|r| r["entity_id"].as_i64()),
        Some(fresh),
        "under decay the currently-hot entity must outrank the stale pile: {decayed}"
    );
    assert!(
        rows[0].get("decayed_score").is_some(),
        "the decayed row carries its score: {decayed}"
    );
    // The raw counter is still visible for comparison.
    let legacy_row = rows.iter().find(|r| r["entity_id"] == json!(legacy));
    if let Some(row) = legacy_row {
        assert_eq!(row["access_count"], json!(10), "{decayed}");
    }
}

/// The per-profile schema cost, measured instead of claimed.
///
/// Rivals publish this number (codegraph ~1.9K tokens for 1 advertised
/// tool, codebase-memory-mcp ~7K for 15, trace-mcp ~11.6K for 28) and
/// memnoth published none, which is why its tool count read as maximalism
/// in a comparison table. Run
/// `cargo test -p memnoth-mcp profile_schema_cost -- --nocapture` to print
/// the table: bytes of the `tools/list` payload per profile, a
/// protocol-independent lower bound on the token cost.
#[test]
fn profile_schema_cost_is_published_and_monotone() {
    let cost = |profile: Profile| {
        serde_json::to_string(&visible_tools(profile))
            .expect("tool definitions serialize")
            .len()
    };
    for profile in [
        Profile::Micro,
        Profile::Memory,
        Profile::Coord,
        Profile::Code,
        Profile::Agent,
        Profile::Full,
    ] {
        let bytes = cost(profile);
        assert!(bytes > 0, "{profile:?} exposes no schema at all");
        // A narrow profile is a subset of Full (test above), so it can
        // never cost more.
        if profile != Profile::Full {
            assert!(
                bytes <= cost(Profile::Full),
                "{profile:?} costs {bytes} bytes against Full's {}: a profile narrows Full, \
                     so it can never be larger",
                cost(Profile::Full)
            );
        }
        eprintln!(
            "mcp profile {:<6} {:>2} tools {:>6} bytes of tools/list schema",
            profile_name(profile),
            profile.tool_names().len(),
            bytes
        );
    }
}

#[test]
fn out_of_profile_call_is_rejected_with_profile_pointer() {
    let (e, c) = setup();
    let resp = handle_message_for_profile(
        &e,
        &c,
        Profile::Memory,
        rpc(
            1,
            "tools/call",
            json!({"name": "coord_join", "arguments": {"name": "x"}}),
        ),
        None,
        None,
    )
    .unwrap();
    assert_eq!(resp["result"]["isError"], true);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("MEMNOTH_MCP_PROFILE"), "{text}");
    assert!(text.contains("'memory'"), "{text}");
}
