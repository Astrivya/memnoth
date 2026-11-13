use super::*;

#[test]
fn in_profile_call_still_works_under_restriction() {
    let (e, c) = setup();
    let resp = handle_message_for_profile(
        &e,
        &c,
        Profile::Micro,
        rpc(
            1,
            "tools/call",
            json!({"name": "mem_add_fact", "arguments": {"entity_name": "p", "content": "c"}}),
        ),
        None,
        None,
    )
    .unwrap();
    assert!(resp["result"]["content"][0]["text"].is_string());
}

#[test]
fn prompts_list_get_and_errors() {
    let (e, c) = setup();
    let resp = handle_message(&e, &c, rpc(1, "prompts/list", json!({}))).unwrap();
    let prompts = resp["result"]["prompts"].as_array().unwrap();
    assert_eq!(prompts.len(), 4);

    let resp = handle_message(
        &e,
        &c,
        rpc(
            2,
            "prompts/get",
            json!({"name": "recall", "arguments": {"topic": "auth design"}}),
        ),
    )
    .unwrap();
    let message = &resp["result"]["messages"][0];
    assert_eq!(message["role"], "user");
    assert_eq!(message["content"]["type"], "text");
    assert!(
        message["content"]["text"]
            .as_str()
            .unwrap()
            .contains("auth design"),
        "prompt must render its argument"
    );

    let resp = handle_message(&e, &c, rpc(3, "prompts/get", json!({"name": "handoff"}))).unwrap();
    assert!(
        resp["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("coord_status")
    );

    let resp = handle_message(&e, &c, rpc(4, "prompts/get", json!({"name": "bogus"}))).unwrap();
    assert_eq!(resp["error"]["code"], -32602);

    let resp = handle_message(&e, &c, rpc(5, "prompts/get", json!({"name": "ingest"}))).unwrap();
    assert_eq!(resp["error"]["code"], -32602);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("'path'")
    );

    // skills-as-commands over MCP: the /<skill> equivalent for hosts
    // without slash commands.
    let resp = handle_message(
        &e,
        &c,
        rpc(
            6,
            "prompts/get",
            json!({"name": "skill", "arguments": {"name": "review"}}),
        ),
    )
    .unwrap();
    assert!(
        resp["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("review")
    );
    let resp = handle_message(&e, &c, rpc(7, "prompts/get", json!({"name": "skill"}))).unwrap();
    assert_eq!(resp["error"]["code"], -32602);
}

/// Dedup is only useful if it is *deterministic and wrong in the safe
/// direction*: same name in different dresses is grouped, a genuine
/// near-miss is not, and the suggested keeper is chosen from evidence
/// (facts, then access) rather than from whichever row happened to come
/// first.
#[test]
fn mem_duplicates_groups_normalized_names_and_repeated_facts() {
    let (e, c) = setup();
    let now = memnoth_engine::now_ms();
    let entity = |name: &str| e.add_entity(EntityKind::Other, name, 0.9, now).unwrap();

    let keeper = entity("AuthService");
    let variant = entity("auth_service");
    let spaced = entity("Auth Service");
    let reordered_a = entity("session manager");
    let reordered_b = entity("manager session");
    let lonely = entity("BillingService");

    // The keeper is the one with the most facts — that is the evidence the
    // suggestion has to be based on, so the fixture has to contain it.
    let shared = "The auth token is cached for fifteen minutes";
    e.add_observation(keeper, shared, "fact", 0.9, now).unwrap();
    e.add_observation(keeper, "Refresh uses a rotating secret", "fact", 0.9, now)
        .unwrap();
    e.add_observation(variant, shared, "fact", 0.9, now)
        .unwrap();
    e.add_observation(
        spaced,
        "A claim that differs only in its spacing",
        "fact",
        0.9,
        now,
    )
    .unwrap();
    e.add_observation(lonely, "Billing rounds at the cent", "fact", 0.9, now)
        .unwrap();

    let resp = handle_message(
        &e,
        &c,
        rpc(
            1,
            "tools/call",
            json!({"name": "mem_duplicates", "arguments": {}}),
        ),
    )
    .unwrap();
    assert!(
        resp.get("result").is_some(),
        "mem_duplicates must be callable in the memory profile's vault: {resp}"
    );
    let out: Value =
        serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();

    let groups = out["entity_groups"].as_array().unwrap();
    // Case + separators, reported once even though all three collide.
    let auth = groups
        .iter()
        .find(|g| g["key"] == "authservice")
        .unwrap_or_else(|| panic!("case collision not grouped: {out}"));
    assert_eq!(auth["size"], json!(3), "{out}");
    assert_eq!(
        auth["suggested_keeper"]["entity_id"],
        json!(keeper),
        "the keeper must be the entity with the most facts, not the first row: {out}"
    );
    assert_eq!(auth["members"][0]["facts"], json!(2), "{out}");

    // Word order, via the second key.
    let reordered = groups
        .iter()
        .find(|g| g["key"] == "manager session")
        .unwrap_or_else(|| panic!("word-order collision not grouped: {out}"));
    let mut ids: Vec<i64> = reordered["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["entity_id"].as_i64().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![reordered_a, reordered_b], "{out}");

    // A unique name is not a duplicate — the tool must not manufacture groups.
    assert!(
        !groups.iter().any(|g| g["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "BillingService")),
        "a name with no twin must not appear in a group: {out}"
    );

    // The same fact on two entities is the cross-entity signal.
    let facts = out["fact_groups"].as_array().unwrap();
    let repeated = facts
        .iter()
        .find(|g| g["occurrences"] == json!(2) && g["entities"].as_array().unwrap().len() == 2)
        .unwrap_or_else(|| panic!("cross-entity repeated fact not reported: {out}"));
    assert_eq!(repeated["sample"], json!(shared), "{out}");

    assert_eq!(out["counts"]["entities_current"], json!(6), "{out}");
    assert_eq!(out["counts"]["entity_groups"], json!(2), "{out}");
    // The limits are stated, not implied.
    assert_eq!(out["truncated"]["entity_groups"], json!(false), "{out}");
}

#[test]
fn resources_list_static_and_entities() {
    let (e, c) = setup();
    add_fact(&e, &c, 1, "res-listed");
    let resp = handle_message(&e, &c, rpc(2, "resources/list", json!({}))).unwrap();
    let resources = resp["result"]["resources"].as_array().unwrap();
    for uri in ["mem://stats", "mem://tasks", "mem://room"] {
        assert!(
            resources.iter().any(|r| r["uri"] == uri),
            "{uri} missing from {resources:?}"
        );
    }
    assert!(
        resources.iter().any(|r| r["name"] == "res-listed"),
        "current entities must be enumerated: {resources:?}"
    );
    assert!(resources.len() <= 3 + ENTITY_LIST_CAP);
}

#[test]
fn resources_templates_declared() {
    let (e, c) = setup();
    let resp = handle_message(&e, &c, rpc(1, "resources/templates/list", json!({}))).unwrap();
    let templates = resp["result"]["resourceTemplates"].as_array().unwrap();
    assert_eq!(templates.len(), 3);
    assert_eq!(templates[0]["uriTemplate"], "mem://entity/{id}");
    assert_eq!(templates[1]["uriTemplate"], "mem://related/{id}?hops=n");
    // The code graph is addressable beside the vault: a host should be able to read one symbol
    // without spending a tool round trip deciding to.
    assert_eq!(templates[2]["uriTemplate"], "kg://symbol/{qname}");
}

#[test]
fn resources_read_stats_entity_related_and_errors() {
    let (e, c) = setup();
    let entity_id = add_fact(&e, &c, 1, "res-read");

    let read = |id: i64, uri: &str| {
        handle_message(&e, &c, rpc(id, "resources/read", json!({"uri": uri}))).unwrap()
    };

    let resp = read(2, "mem://stats");
    let text = resp["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(
        serde_json::from_str::<Value>(text).unwrap()["entities_current"]
            .as_i64()
            .unwrap()
            >= 1
    );

    let resp = read(3, &format!("mem://entity/{entity_id}"));
    let contents = &resp["result"]["contents"][0];
    assert_eq!(contents["mimeType"], "application/json");
    let page: Value = serde_json::from_str(contents["text"].as_str().unwrap()).unwrap();
    assert_eq!(page["entity"]["id"], entity_id);
    assert_eq!(page["entity"]["name"], "res-read");
    assert_eq!(page["observations"][0]["content"], "resource test fact");
    assert!(page["outgoing_relations"].is_array());

    let missing = read(4, "mem://entity/999999");
    // -32602 (Invalid Params), not the -32002 the 2026-07-28 revision
    // removed. Asserted here so a regression is a failing test rather than
    // a spec-conformance bug found by a host.
    assert_eq!(missing["error"]["code"], -32602);

    let malformed = read(5, "mem://entity/not-a-number");
    assert_eq!(malformed["error"]["code"], -32602);

    let unknown = read(6, "mem://bogus/thing");
    assert_eq!(unknown["error"]["code"], -32602);

    let no_uri = handle_message(&e, &c, rpc(7, "resources/read", json!({}))).unwrap();
    assert_eq!(no_uri["error"]["code"], -32602);
}

#[test]
fn resources_read_related_with_hops_clamped() {
    let (e, c) = setup();
    let a = add_fact(&e, &c, 1, "hop-a");
    let _b = add_fact(&e, &c, 2, "hop-b");

    let ok = handle_message(
        &e,
        &c,
        rpc(
            3,
            "resources/read",
            json!({"uri": format!("mem://related/{a}?hops=2")}),
        ),
    )
    .unwrap();
    assert!(ok["result"]["contents"][0]["text"].is_string());

    let clamped = handle_message(
        &e,
        &c,
        rpc(
            4,
            "resources/read",
            json!({"uri": format!("mem://related/{a}?hops=99")}),
        ),
    )
    .unwrap();
    assert!(clamped["result"]["contents"][0]["text"].is_string());

    let bad_query = handle_message(
        &e,
        &c,
        rpc(
            5,
            "resources/read",
            json!({"uri": format!("mem://related/{a}?wat=1")}),
        ),
    )
    .unwrap();
    assert_eq!(bad_query["error"]["code"], -32602);

    let bad_hops = handle_message(
        &e,
        &c,
        rpc(
            6,
            "resources/read",
            json!({"uri": format!("mem://related/{a}?hops=x")}),
        ),
    )
    .unwrap();
    assert_eq!(bad_hops["error"]["code"], -32602);
}

#[test]
fn notification_gets_no_response() {
    let (e, c) = setup();
    let msg = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert!(handle_message(&e, &c, msg).is_none());
}

#[test]
fn mem_stats_and_add_fact_flow() {
    let (e, c) = setup();
    let resp = handle_message(
            &e,
            &c,
            rpc(3, "tools/call", json!({"name": "mem_add_fact", "arguments": {"entity_name": "memnoth", "content": "is an agent-memory OS"}})),
        )
        .unwrap();
    assert!(resp["result"]["content"][0]["text"].is_string());

    let resp = handle_message(
        &e,
        &c,
        rpc(
            4,
            "tools/call",
            json!({"name": "mem_stats", "arguments": {}}),
        ),
    )
    .unwrap();
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let stats: Value = serde_json::from_str(text).unwrap();
    assert_eq!(stats["entities_current"], 1);

    let resp = handle_message(
        &e,
        &c,
        rpc(
            5,
            "tools/call",
            json!({"name": "mem_search", "arguments": {"query": "memnoth"}}),
        ),
    )
    .unwrap();
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("memnoth"));
}

#[test]
fn coord_claim_conflict_over_mcp() {
    let (e, c) = setup();
    let sid_a = join_session(&e, &c, 10, "alpha");
    let sid_b = join_session(&e, &c, 11, "beta");
    call_tool(
        &e,
        &c,
        12,
        "coord_claim",
        json!({"session_id": sid_a, "path_glob": "src/a/*"}),
    );
    let refused = call_tool(
        &e,
        &c,
        13,
        "coord_claim",
        json!({"session_id": sid_b, "path_glob": "src/a/x.rs"}),
    );
    assert_eq!(refused["granted"], false);
    assert_eq!(refused["holder_session"], sid_a);
}

#[test]
fn unknown_method_is_error() {
    let (e, c) = setup();
    let resp = handle_message(&e, &c, rpc(99, "bogus/method", json!({}))).unwrap();
    assert_eq!(resp["error"]["code"], -32601);
}

#[test]
fn task_status_conversion() {
    assert_eq!(
        "in_progress".parse::<TaskStatus>().unwrap(),
        TaskStatus::InProgress
    );
    // Unrecognised labels are rejected, not silently coerced to Pending.
    assert!("nonsense".parse::<TaskStatus>().is_err());
}

// ------------------------------------------------------------------
// 2026-07-28 spec currency: discovery, dual-era versioning, result
// envelope, SEP-2549 cache hints, and the Tasks extension.
//
// Wire keys are written as literals on purpose: these tests are checking
// the protocol, so a typo in a constant must fail here.
// ------------------------------------------------------------------

#[test]
fn server_discover_advertises_versions_capabilities_and_identity() {
    let (e, c) = setup();
    // Callable before any version declaration, which is the point: it is
    // the probe a dual-era client uses to learn which era we speak.
    let resp = handle_message(&e, &c, rpc(1, "server/discover", json!({}))).unwrap();
    let result = &resp["result"];
    let versions: Vec<&str> = result["protocolVersions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        versions,
        vec![LATEST_PROTOCOL_VERSION],
        "exactly the one named stateless revision"
    );
    assert_eq!(result["serverInfo"]["name"], "memnoth");
    assert!(result["capabilities"]["tools"].is_object());
    assert!(
        result["capabilities"]["extensions"]["io.modelcontextprotocol/tasks"].is_object(),
        "the Tasks extension is advertised in server/discover, per the extension spec"
    );
    assert!(
        result["capabilities"].get("roots").is_none(),
        "roots is deprecated (SEP-2577); do not re-advertise it"
    );
}

#[test]
fn modern_request_with_an_unsupported_version_is_refused_with_the_supported_list() {
    let (e, c) = setup();
    let resp = handle_message(
        &e,
        &c,
        rpc(
            1,
            "tools/list",
            json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "1900-01-01"}}),
        ),
    )
    .unwrap();
    assert_eq!(resp["error"]["code"], -32022);
    assert_eq!(resp["error"]["message"], "Unsupported protocol version");
    assert_eq!(resp["error"]["data"]["requested"], "1900-01-01");
    assert_eq!(
        resp["error"]["data"]["supported"][0],
        LATEST_PROTOCOL_VERSION
    );
    assert!(
        resp.get("result").is_none(),
        "a refused request must not also carry a result"
    );
}

#[test]
fn modern_request_with_the_current_version_needs_no_handshake() {
    let (e, c) = setup();
    let resp = handle_message(
        &e,
        &c,
        rpc(
            1,
            "tools/list",
            json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}),
        ),
    )
    .unwrap();
    assert_eq!(resp["result"]["resultType"], "complete");
    assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 47);
}

#[test]
fn the_legacy_handshake_is_not_served() {
    let (e, c) = setup();
    // The legacy `initialize` era is removed: it answers method-not-found
    // whatever version the client asks for, and never selects a revision.
    for requested in [
        LATEST_PROTOCOL_VERSION,
        "2025-06-18",
        "2099-01-01",
        "2024-11-05",
    ] {
        let resp = handle_message(
            &e,
            &c,
            rpc(1, "initialize", json!({"protocolVersion": requested})),
        )
        .unwrap();
        assert_eq!(
            resp["error"]["code"], -32601,
            "initialize({requested}) must not be served"
        );
        assert!(resp.get("result").is_none());
    }
}

#[test]
fn every_result_carries_result_type_and_server_identity() {
    let (e, c) = setup();
    for (id, method) in [
        (1, "tools/list"),
        (2, "server/discover"),
        (3, "prompts/list"),
        (4, "resources/list"),
        (5, "resources/templates/list"),
    ] {
        let resp = handle_message(&e, &c, rpc(id, method, json!({}))).unwrap();
        assert_eq!(resp["result"]["resultType"], "complete", "{method}");
        assert_eq!(
            resp["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "memnoth",
            "{method} must identify the server that answered"
        );
    }
}

#[test]
fn list_and_read_results_carry_sep2549_cache_hints() {
    let (e, c) = setup();
    for (id, method) in [
        (1, "tools/list"),
        (2, "prompts/list"),
        (3, "resources/list"),
        (4, "resources/templates/list"),
    ] {
        let resp = handle_message(&e, &c, rpc(id, method, json!({}))).unwrap();
        assert!(
            resp["result"]["ttlMs"].is_i64(),
            "{method} must carry ttlMs (SEP-2549): {resp}"
        );
        assert_eq!(resp["result"]["cacheScope"], "private", "{method}");
    }
    // The values are the *kinds*' values, not arbitrary numbers: definitions
    // outlive the process's data, and this server has no workspace index
    // here (no vault path), so `resources/list` answers as vault content.
    let defs = handle_message(&e, &c, rpc(6, "tools/list", json!({}))).unwrap();
    assert_eq!(defs["result"]["ttlMs"], DEFINITION_CACHE_TTL_MS);
    let list = handle_message(&e, &c, rpc(7, "resources/list", json!({}))).unwrap();
    assert_eq!(list["result"]["ttlMs"], WORKSPACE_DERIVED_TTL_MS);
    // Zero on reads, deliberately: a client-side cached belief is a wrong
    // belief in a bi-temporal vault.
    let read = handle_message(
        &e,
        &c,
        rpc(5, "resources/read", json!({"uri": "mem://stats"})),
    )
    .unwrap();
    assert_eq!(read["result"]["ttlMs"], 0);
    assert_eq!(read["result"]["cacheScope"], "private");
}

// ------------------------------------------------------------------
// Ledger-derived cache hints (the parse ledger, not a constant).

#[test]
fn cache_ttl_is_the_ledgers_opinion_about_index_content() {
    assert_eq!(cache_ttl(CacheKind::Definitions), DEFINITION_CACHE_TTL_MS);
    assert_eq!(cache_ttl(CacheKind::Vault), WORKSPACE_DERIVED_TTL_MS);
    assert_eq!(
        cache_ttl(CacheKind::WorkspaceIndex {
            stale_files: 0,
            watcher_pending: 0
        }),
        WORKSPACE_DERIVED_TTL_MS,
        "a ledger with nothing outstanding is what licenses caching at all"
    );
    assert_eq!(
        cache_ttl(CacheKind::WorkspaceIndex {
            stale_files: 1,
            watcher_pending: 0
        }),
        0,
        "one known-stale file means the answer is already out of date"
    );
    assert_eq!(
        cache_ttl(CacheKind::WorkspaceIndex {
            stale_files: 0,
            watcher_pending: 3
        }),
        0,
        "unprocessed watcher events are staleness we have not counted yet"
    );
}

#[test]
fn a_vault_is_the_only_thing_that_names_a_workspace_root() {
    assert_eq!(
        workspace_root_for_vault(Some(Path::new("/repo/.memnoth/vault.db"))),
        Some(PathBuf::from("/repo"))
    );
    assert_eq!(
        workspace_root_for_vault(Some(Path::new("/tmp/scratch.db"))),
        None,
        "a vault outside `.memnoth` names no workspace, so no index is claimed"
    );
    assert_eq!(workspace_root_for_vault(None), None);
}

/// The end-to-end claim: change the workspace, watch the *cache hint* change
/// with no code path in between. This is the difference between emitting
/// `ttlMs` and deriving it — a static 5 000 would pass a shape test forever.
#[test]
fn index_cache_hints_track_the_parse_ledger() {
    let root = task_workspace();
    let memnoth_dir = root.join(".memnoth");
    std::fs::create_dir_all(&memnoth_dir).unwrap();
    // Outside `.memnoth`: that directory is in the indexer's `SKIP_DIRS`
    // (deliberately — it holds the graph itself), so a source file placed
    // there would never be indexed and would prove nothing.
    std::fs::create_dir_all(root.join("src")).unwrap();
    let source = root.join("src/lib.rs");
    std::fs::write(&source, "pub fn alpha() -> u32 { 1 }\n").unwrap();
    // The vault path is what names the workspace root, so it has to live in
    // the layout the server derives from: `<root>/.memnoth/vault.db`.
    let vault = memnoth_dir.join("vault.db");

    // Build a real code graph, then a real client connection onto it.
    let mut store = open_store(&memnoth_dir.join("kg.db")).unwrap();
    index_full(&mut store, &root).unwrap();
    drop(store);

    let engine = memnoth_engine::open_vault(&vault).unwrap();
    let coord = Coord::open(&vault).unwrap();
    let call = |id: i64, method: &str, params: Value| {
        handle_message_for_profile(
            &engine,
            &coord,
            Profile::Full,
            rpc(id, method, params),
            None,
            Some(&vault),
        )
        .unwrap()
    };

    // Fresh: the resource is listed, and a host may hold the answer briefly.
    let fresh_list = call(1, "resources/list", json!({}));
    assert!(
        fresh_list["result"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["uri"] == "mem://index/status"),
        "an indexed workspace must advertise its ledger: {fresh_list}"
    );
    assert_eq!(fresh_list["result"]["ttlMs"], WORKSPACE_DERIVED_TTL_MS);

    let fresh_read = call(2, "resources/read", json!({"uri": "mem://index/status"}));
    assert_eq!(fresh_read["result"]["ttlMs"], WORKSPACE_DERIVED_TTL_MS);
    let text = fresh_read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap();
    let report: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        report["stale_files"], 0,
        "a just-indexed workspace has nothing outstanding: {fresh_read}"
    );
    assert!(report["generation"].as_u64().unwrap() >= 1);

    // Edit a file: nothing in the server is told, and the hint must fall to
    // zero anyway — that is the ledger doing the work.
    std::fs::write(
        &source,
        "pub fn alpha() -> u32 { 2 }\npub fn beta() -> u32 { 3 }\n",
    )
    .unwrap();

    let stale_read = call(3, "resources/read", json!({"uri": "mem://index/status"}));
    assert_eq!(
        stale_read["result"]["ttlMs"], 0,
        "a stale ledger must forbid reuse outright: {stale_read}"
    );
    let text = stale_read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap();
    let report: Value = serde_json::from_str(text).unwrap();
    assert_eq!(report["stale_files"], 1, "{stale_read}");

    let stale_list = call(4, "resources/list", json!({}));
    assert_eq!(
        stale_list["result"]["ttlMs"], 0,
        "and the list that advertises it inherits that: {stale_list}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `kg_communities` exposes the graph's clusters. Two things have to hold:
/// the report is the graph's own (deterministic, with cohesion and named
/// representatives), and a **narrow first request must not poison the
/// generation-stamped cache for a wider one** — the failure
/// `load_or_compute` cannot see, because its only other caller always asks
/// for the default width.
#[test]
fn kg_communities_exposes_clusters_and_survives_a_narrow_first_request() {
    let root = task_workspace();
    let memnoth_dir = root.join(".memnoth");
    std::fs::create_dir_all(&memnoth_dir).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // Two disjoint call chains: label propagation can produce two
    // communities only if the components really are disconnected, so no
    // symbol in `a.rs` may call one in `b.rs`.
    std::fs::write(
        root.join("src/a.rs"),
        "pub fn a1() -> u32 { a2() }\npub fn a2() -> u32 { 1 }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/b.rs"),
        "pub fn b1() -> u32 { b2() }\npub fn b2() -> u32 { 2 }\n",
    )
    .unwrap();

    let mut store = open_store(&memnoth_dir.join("kg.db")).unwrap();
    index_full(&mut store, &root).unwrap();
    drop(store);

    let (e, c) = setup();
    let ws = root.to_string_lossy().to_string();

    // Narrow first: exactly one community, and the answer must declare
    // that it withheld more instead of presenting a clip as the whole map.
    let narrow = call_tool(
        &e,
        &c,
        1,
        "kg_communities",
        json!({"workspace": ws, "limit": 1}),
    );
    assert_eq!(
        narrow["communities"].as_array().unwrap().len(),
        1,
        "limit=1 must return exactly one community: {narrow}"
    );
    assert_eq!(narrow["truncated"], json!(true), "{narrow}");

    // Wider second, against the cache the narrow call just persisted.
    let wide = call_tool(
        &e,
        &c,
        2,
        "kg_communities",
        json!({"workspace": ws, "limit": 8}),
    );
    let wide_communities = wide["communities"].as_array().unwrap();
    assert!(
        wide_communities.len() > 1,
        "two disconnected call chains must cluster separately, and a wider request must not \
             be answered from the one-community cache: {wide}"
    );
    assert!(wide["generation"].as_u64().unwrap() >= 1, "{wide}");
    for community in wide_communities {
        let cohesion = community["cohesion"].as_f64().unwrap();
        assert!(
            (0.0..=1.0).contains(&cohesion),
            "cohesion is a real fraction, not a placeholder: {community}"
        );
        assert!(
            !community["top_symbols"].as_array().unwrap().is_empty(),
            "each community names its representatives: {community}"
        );
    }

    // Back to the narrow width: the wider cache is clipped, not returned
    // whole — a `limit` a host cannot see past is the point of the cap.
    let narrow_again = call_tool(
        &e,
        &c,
        3,
        "kg_communities",
        json!({"workspace": ws, "limit": 1}),
    );
    assert_eq!(
        narrow_again["communities"].as_array().unwrap().len(),
        1,
        "a wider cached map must still be clipped to the requested width: {narrow_again}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Absence is not freshness. A host that cannot tell "no index" from "index
/// is current" would cache a workspace it has not indexed.
#[test]
fn index_status_without_an_index_is_an_error_not_a_fresh_verdict() {
    let (e, c) = setup();
    let no_vault = handle_message(
        &e,
        &c,
        rpc(1, "resources/read", json!({"uri": "mem://index/status"})),
    )
    .unwrap();
    assert_eq!(no_vault["error"]["code"], -32602, "{no_vault}");

    // A vault in the right layout but with no code graph next to it.
    let root = task_workspace();
    let memnoth_dir = root.join(".memnoth");
    std::fs::create_dir_all(&memnoth_dir).unwrap();
    let vault = memnoth_dir.join("vault.db");
    memnoth_engine::open_vault(&vault).unwrap();
    let unindexed = handle_message_for_profile(
        &e,
        &c,
        Profile::Full,
        rpc(2, "resources/read", json!({"uri": "mem://index/status"})),
        None,
        Some(&vault),
    )
    .unwrap();
    assert_eq!(unindexed["error"]["code"], -32602, "{unindexed}");
    let list = handle_message_for_profile(
        &e,
        &c,
        Profile::Full,
        rpc(3, "resources/list", json!({})),
        None,
        Some(&vault),
    )
    .unwrap();
    assert!(
        !list["result"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["uri"] == "mem://index/status"),
        "no index, no freshness resource: {list}"
    );
    assert_eq!(list["result"]["ttlMs"], WORKSPACE_DERIVED_TTL_MS);
    let _ = std::fs::remove_dir_all(&root);
}

/// A real indexed workspace for the typed walk.
///
/// The graph-backed primaries cannot answer structurally without one, and a gate
/// that skips them is a gate that checks the easy half. This builds the legacy
/// store (`kg.db`) the server reads when the CLI's `graph.db` is absent, plus a
/// one-commit git repo so `kg_changes` has a diff to map.
fn typed_workspace() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("memnoth-typed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".memnoth")).expect("temp workspace");
    std::fs::write(
        dir.join("lib.rs"),
        "pub fn typed_helper() -> u32 { 1 }\npub fn typed_caller() -> u32 { typed_helper() }\n",
    )
    .expect("source file");
    let mut store =
        memnoth_codegraph::Store::open(&dir.join(".memnoth").join("kg.db")).expect("a store opens");
    memnoth_codegraph::index_full(&mut store, &dir).expect("the workspace indexes");
    for args in [
        vec!["init", "-q"],
        vec!["add", "lib.rs"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "seed",
        ],
    ] {
        let _ = std::process::Command::new("git")
            .args(&args)
            .current_dir(&dir)
            .output();
    }
    dir
}

/// A declared `outputSchema` is a promise a host may **validate against**, so
/// it has to describe the payload exactly. This walks every typed tool: the
/// definition must carry a schema, the call must return `structuredContent`,
/// the object must satisfy the schema, and the text half must still be the
/// same JSON - because a client that predates the field reads only that.
#[test]
fn typed_outputs_match_their_declared_schemas() {
    let (e, c) = setup();
    add_fact(&e, &c, 1, "typed-entity");
    let code = typed_workspace();
    let code_arg = code.to_string_lossy().to_string();
    let definitions = visible_tools(Profile::Full);
    let schema_of = |name: &str| -> Value {
        definitions
            .iter()
            .find(|d| d["name"] == name)
            .unwrap_or_else(|| panic!("`{name}` must be advertised in Full"))["outputSchema"]
            .clone()
    };
    // Exactly the named set declares a schema. A tool with a schema that
    // nothing populates, or one that returns structured content nothing
    // declared, is the drift this catches.
    for definition in &definitions {
        let name = definition["name"].as_str().unwrap();
        assert_eq!(
            definition.get("outputSchema").is_some(),
            declares_output_schema(name),
            "`{name}` declares an outputSchema but is not in TYPED_OUTPUT_TOOLS, or the reverse"
        );
    }
    // The coordination arms need a live session, so one is opened here rather
    // than faked: a typed schema checked only against error envelopes is not
    // checked at all, and `coord_join` is itself one of the tools under test.
    let joined = handle_message(
        &e,
        &c,
        rpc(
            0,
            "tools/call",
            json!({"name": "coord_join", "arguments": {"name": "typed-session"}}),
        ),
    )
    .unwrap();
    let session_id = joined["result"]["structuredContent"]["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("coord_join must answer with a session id: {joined}"))
        .to_string();
    let mut problems: Vec<String> = Vec::new();
    for (index, name) in TYPED_OUTPUT_TOOLS.iter().enumerate() {
        let args = match *name {
            "vault_asof" => json!({"name": "typed-entity", "as_of_ms": 1}),
            "vault_top" => json!({"limit": 5}),
            "vault_proposals" => json!({}),
            "coord_heartbeat" | "coord_leave" => json!({"session_id": session_id.clone()}),
            "coord_task_create" => json!({"title": "typed-task"}),
            "kg_impact" | "kg_def" | "kg_refs" => json!({"symbol": "typed-entity"}),
            "kg_search" => json!({"query": "typed"}),
            "kg_status" => json!({"workspace": code_arg.clone()}),
            "kg_index" => json!({"workspace": code_arg.clone(), "embed": false}),
            "kg_trace" | "kg_callees" => {
                json!({"workspace": code_arg.clone(), "symbol": "typed_helper"})
            }
            "kg_explore" => json!({"workspace": code_arg.clone(), "query": "typed_helper"}),
            "kg_dead" | "kg_changes" => json!({"workspace": code_arg.clone()}),
            _ => json!({"name": "typed-entity"}),
        };
        let resp = handle_message(
            &e,
            &c,
            rpc(
                index as i64 + 1,
                "tools/call",
                json!({"name": name, "arguments": args}),
            ),
        )
        .unwrap();
        let result = &resp["result"];
        let structured = result
            .get("structuredContent")
            .unwrap_or_else(|| panic!("`{name}` must answer with structuredContent: {resp}"));
        problems.extend(schema_problems(structured, &schema_of(name), name));
        let text = result["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            &parsed, structured,
            "`{name}`: the text and typed halves must agree"
        );
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// `resources/list` used to truncate entities at `ENTITY_LIST_CAP` with no
/// signal at all — a host enumerating the vault simply never saw the rest.
/// The cursor makes the truncation honest: more results are declared, and a
/// host can ask for them without risking an overlap or a missing row.

#[test]
fn resource_entities_page_with_a_cursor_instead_of_truncating() {
    let (e, c) = setup();
    let total = ENTITY_LIST_CAP + 5;
    for index in 0..total {
        add_fact(&e, &c, index as i64 + 1, &format!("page-entity-{index:03}"));
    }
    let first = handle_message(&e, &c, rpc(1, "resources/list", json!({}))).unwrap();
    let items = first["result"]["resources"].as_array().unwrap();
    assert_eq!(
        items.len(),
        3 + ENTITY_LIST_CAP,
        "the first page is the static three plus one full page of entities"
    );
    let cursor = first["result"]["nextCursor"]
        .as_str()
        .unwrap_or_else(|| panic!("a truncated list must say there is more: {first}"))
        .to_string();

    let second =
        handle_message(&e, &c, rpc(2, "resources/list", json!({"cursor": cursor}))).unwrap();
    let items2 = second["result"]["resources"].as_array().unwrap();
    assert_eq!(
        items2.len(),
        5,
        "the second page is exactly the remainder: {second}"
    );
    assert!(
        second["result"].get("nextCursor").is_none(),
        "the last page says so by omission, not with a null cursor: {second}"
    );
    for item in items2 {
        assert!(
            item["uri"].as_str().unwrap().starts_with("mem://entity/"),
            "the header entries belong to the first page only: {item}"
        );
    }

    // The pages must tile the entity set exactly: no overlap, nothing missed.
    let mut uris: Vec<&str> = items
        .iter()
        .chain(items2.iter())
        .filter_map(|item| item["uri"].as_str())
        .collect();
    let seen = uris.len();
    uris.sort();
    uris.dedup();
    assert_eq!(uris.len(), seen, "pages must not overlap");
    assert_eq!(uris.len(), 3 + total, "and must cover every resource");

    let bad = handle_message(&e, &c, rpc(3, "resources/list", json!({"cursor": "nope"})));
    assert_eq!(
        bad.unwrap()["error"]["code"],
        -32602,
        "a bad cursor is invalid params, not a silent restart"
    );
}

/// The autocompletion surface, answered from real state.
#[test]
fn completions_answer_for_our_own_templates_and_prompts() {
    let (e, c) = setup();
    add_fact(&e, &c, 1, "completion-target");
    // The capability must be advertised, or no host ever asks.
    let discover = handle_message(&e, &c, rpc(2, "server/discover", json!({}))).unwrap();
    assert!(
        discover["result"]["capabilities"]
            .get("completions")
            .is_some(),
        "and a stateless host reads it from server/discover: {discover}"
    );

    let complete = |id: i64, params: Value| {
        handle_message(&e, &c, rpc(id, "completion/complete", params)).unwrap()
    };
    // A resource template variable is an entity, so the vault is the menu.
    let resource = complete(
        3,
        json!({
            "ref": {"type": "ref/resource", "uri": "mem://entity/{id}"},
            "argument": {"name": "id", "value": "completion"},
        }),
    );
    assert_eq!(resource["result"]["resultType"], "complete", "{resource}");
    assert!(
        resource["result"]["completion"]["values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "completion-target"),
        "{resource}"
    );
    assert_eq!(
        resource["result"]["completion"]["hasMore"], false,
        "{resource}"
    );

    // Ranking: a prefix match beats a substring match, and a non-match is
    // not returned at all.
    add_fact(&e, &c, 4, "other-target");
    let ranked = complete(
        4,
        json!({
            "ref": {"type": "ref/resource", "uri": "mem://related/{id}?hops=n"},
            "argument": {"name": "id", "value": "completion"},
        }),
    );
    assert_eq!(
        ranked["result"]["completion"]["values"][0], "completion-target",
        "a prefix match must rank first: {ranked}"
    );
    assert_eq!(
        ranked["result"]["completion"]["total"], 1,
        "and a non-match must not be suggested: {ranked}"
    );

    // A prompt argument: `recall.topic` is also a thing in the vault.
    let topic = complete(
        5,
        json!({
            "ref": {"type": "ref/prompt", "name": "recall"},
            "argument": {"name": "topic", "value": "other"},
        }),
    );
    assert!(
        topic["result"]["completion"]["values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "other-target"),
        "{topic}"
    );

    // Unknown prompt, unknown argument, unknown template: invalid params, as
    // the spec says, rather than a silently empty menu.
    for (id, params) in [
        (
            6,
            json!({"ref": {"type": "ref/prompt", "name": "nope"}, "argument": {"name": "x", "value": ""}}),
        ),
        (
            7,
            json!({"ref": {"type": "ref/prompt", "name": "handoff"}, "argument": {"name": "any", "value": ""}}),
        ),
        (
            8,
            json!({"ref": {"type": "ref/resource", "uri": "mem://nope/{id}"}, "argument": {"name": "id", "value": ""}}),
        ),
        (
            9,
            json!({"ref": {"type": "ref/resource", "uri": "mem://entity/{id}"}, "argument": {"name": "wrong", "value": ""}}),
        ),
        (
            10,
            json!({"ref": {"type": "ref/other"}, "argument": {"name": "x", "value": ""}}),
        ),
    ] {
        let resp = complete(id, params);
        assert_eq!(resp["error"]["code"], -32602, "{resp}");
    }
}

/// The spec's security rule, taken seriously: a completion is a read, so it
/// must not spell out data the caller cannot otherwise read. Under a profile
/// with no vault access the menu is empty — an empty list, not an error,
/// because the reference itself is valid.
#[test]
fn completions_do_not_leak_around_a_profile() {
    let (e, c) = setup();
    add_fact(&e, &c, 1, "secret-entity");
    let denied = handle_message_for_profile(
        &e,
        &c,
        Profile::Coord,
        rpc(
            1,
            "completion/complete",
            json!({
                "ref": {"type": "ref/resource", "uri": "mem://entity/{id}"},
                "argument": {"name": "id", "value": "secret"},
            }),
        ),
        None,
        None,
    )
    .unwrap();
    assert!(denied.get("error").is_none(), "{denied}");
    assert_eq!(
        denied["result"]["completion"]["values"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "a profile without vault reads must not be handed the vault's vocabulary: {denied}"
    );
    assert_eq!(denied["result"]["completion"]["total"], 0);
    assert_eq!(denied["result"]["completion"]["hasMore"], false, "{denied}");
}

/// The 100-value cap is the spec's, and `total`/`hasMore` are how a host is
/// told it is a cap rather than the whole menu.
#[test]
fn completion_cap_is_declared_not_silent() {
    let many: Vec<String> = (0..250).map(|i| format!("entity-{i:03}")).collect();
    let result = rank_completions(many, "entity");
    assert_eq!(
        result["completion"]["values"].as_array().unwrap().len(),
        COMPLETION_MAX_VALUES
    );
    assert_eq!(
        result["completion"]["total"], 250,
        "a host must learn the real size, not the page size"
    );
    assert_eq!(result["completion"]["hasMore"], true);
}

/// The code graph is addressable as a resource, and a read that cannot be answered says why.
///
/// The template is listed even without a workspace: it is a *definition*, and hiding it would
/// make the resource undiscoverable in exactly the session that has not built an index yet.
#[test]
fn the_symbol_resource_is_advertised_and_explains_what_is_missing() {
    let (e, c) = setup();
    let templates = handle_message(&e, &c, rpc(1, "resources/templates/list", json!({}))).unwrap();
    let listed: Vec<&str> = templates["result"]["resourceTemplates"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|template| template["uriTemplate"].as_str())
        .collect();
    assert!(
        listed.contains(&"kg://symbol/{qname}"),
        "the code graph must be addressable: {listed:?}"
    );

    // No vault means no workspace to find a graph in — an error that names the reason, never an
    // empty page that reads as "this symbol has no callers".
    let read = handle_message(
        &e,
        &c,
        rpc(
            2,
            "resources/read",
            json!({ "uri": "kg://symbol/demo%3A%3Alib%3A%3Arun" }),
        ),
    )
    .unwrap();
    let message = read["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("not serving a workspace vault"),
        "the read must say why it cannot answer: {read}"
    );
}

/// W11-02: the team vault is on the same MCP surface as the personal one, so
/// Claude Code, Cursor and Codex read one vocabulary. The tools are listed,
/// dispatch, and stamp `scope: "team"` on their answers.
#[test]
fn the_team_vault_tools_are_exposed_and_stamp_the_team_scope() {
    let _lock = hold_notification_queue();
    let (e, c) = setup();
    let list = handle_message(&e, &c, rpc(1, "tools/list", json!({}))).unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(
        names.contains(&"mem_search_team") && names.contains(&"vault_asof_team"),
        "both team tools must be listed: {names:?}"
    );

    let entity_id = add_fact(&e, &c, 2, "team-wedge");
    // A version row the as-of read can answer from (consolidation writes these
    // in production; the test states the row it needs).
    e.connection()
        .execute(
            "INSERT INTO entity_versions (entity_id, snapshot, valid_from, valid_to, change_reason) \
             VALUES (?1, ?2, ?3, NULL, NULL)",
            rusqlite::params![entity_id, json!({"confidence": 0.9}).to_string(), 1],
        )
        .unwrap();

    let raw = handle_message(
        &e,
        &c,
        rpc(
            3,
            "tools/call",
            json!({"name": "mem_search_team", "arguments": {"query": "team-wedge"}}),
        ),
    )
    .unwrap();
    let raw_text = raw["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    let search: Value =
        serde_json::from_str(&raw_text).unwrap_or_else(|_| panic!("search payload: {raw_text}"));
    assert_eq!(search["scope"], "team");
    let hits = search["hits"].as_array().unwrap();
    assert!(
        hits.iter().any(|hit| hit["name"] == "team-wedge"),
        "the seeded fact must be found: {search}"
    );

    let raw = handle_message(
        &e,
        &c,
        rpc(
            4,
            "tools/call",
            json!({"name": "vault_asof_team", "arguments": {"name": "team-wedge", "at": 2}}),
        ),
    )
    .unwrap();
    let raw_text = raw["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    let asof: Value = serde_json::from_str(&raw_text)
        .unwrap_or_else(|_| panic!("asof payload: {raw_text} (full: {raw})"));
    assert_eq!(asof["scope"], "team");
    assert_eq!(asof["entity"], "team-wedge");
}
