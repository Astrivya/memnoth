use super::*;

#[test]
fn tools_list_order_is_deterministic() {
    let (e, c) = setup();
    let names = |resp: &Value| -> Vec<String> {
        resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    let first = names(&handle_message(&e, &c, rpc(1, "tools/list", json!({}))).unwrap());
    let second = names(&handle_message(&e, &c, rpc(2, "tools/list", json!({}))).unwrap());
    assert_eq!(
        first, second,
        "stable order lets hosts cache the list and the prompt prefix"
    );
    let mut sorted = first.clone();
    sorted.sort();
    assert_eq!(first, sorted, "sorted by name, not by table position");
}

#[test]
fn kg_index_returns_a_task_only_when_the_client_declares_the_extension() {
    // The task registry is process-global: without this, a sibling test's
    // `clear()` can delete this task while it is being polled.
    let _registry = crate::tasks::registry_test_lock();
    let (e, c) = setup();
    let workspace = task_workspace();
    let args = json!({"workspace": workspace.to_string_lossy()});

    // No declared capabilities: today's synchronous answer, unchanged.
    let sync = handle_message(
        &e,
        &c,
        rpc(
            1,
            "tools/call",
            json!({"name": "kg_index", "arguments": args.clone()}),
        ),
    )
    .unwrap();
    assert!(
        sync["result"].get("taskId").is_none(),
        "a client that did not opt in must never receive a task: {sync}"
    );
    assert!(
        sync["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains('{'),
        "the synchronous path still returns the index report"
    );

    // Declared: a flat `CreateTaskResult` in place of the tool result.
    let declared = handle_message(
        &e,
        &c,
        rpc(
            2,
            "tools/call",
            json!({
                "name": "kg_index",
                "arguments": args,
                "_meta": {
                    "io.modelcontextprotocol/clientCapabilities": {
                        "extensions": {"io.modelcontextprotocol/tasks": {}}
                    }
                }
            }),
        ),
    )
    .unwrap();
    let result = &declared["result"];
    assert_eq!(result["resultType"], "task");
    assert!(
        result.get("task").is_none(),
        "the schema is flat: no nested task object"
    );
    let task_id = result["taskId"].as_str().unwrap().to_string();
    assert!(result["createdAt"].as_str().unwrap().ends_with('Z'));
    assert_eq!(result["pollIntervalMs"], 1_000);

    // The id was registered before the handle went out, so an immediate
    // poll already resolves it.
    let immediate =
        handle_message(&e, &c, rpc(3, "tasks/get", json!({"taskId": task_id}))).unwrap();
    assert_eq!(immediate["result"]["resultType"], "complete");

    let done = wait_for_task(&e, &c, &task_id);
    assert_eq!(done["status"], "completed", "{done}");
    // The completed result is the CallToolResult the synchronous path would
    // have returned, which is what the extension promises.
    let text = done["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.is_object(),
        "task result must be the tool's own payload"
    );
}

/// The other half of `L-MCP-16`. Bulk ingest is the run that most needs a
/// handle, and it is the one tool whose worker cannot borrow the server's
/// engine — so this test exists to prove the workaround (a worker with its
/// own connection to the vault) actually completes and reports, rather than
/// handing a client a task id that never resolves.
#[test]
fn mem_ingest_returns_a_task_only_when_the_client_declares_the_extension() {
    let _registry = crate::tasks::registry_test_lock();
    // A *file* vault: the worker reopens exactly this path, so an in-memory
    // engine could not exercise the path at all.
    let vault = std::env::temp_dir().join(format!(
        "memnoth-mcp-ingest-task-{}-{}.db",
        std::process::id(),
        memnoth_engine::now_ms()
    ));
    let engine = memnoth_engine::open_vault(&vault).unwrap();
    let coord = Coord::open(&vault).unwrap();
    let workspace = task_workspace();
    std::fs::write(workspace.join("notes.md"), "# Task ingest\n\nhello\n").unwrap();
    let path = workspace.to_string_lossy().to_string();

    // No declared capabilities: the synchronous answer, unchanged.
    let sync = handle_message_for_profile(
        &engine,
        &coord,
        Profile::Full,
        rpc(
            1,
            "tools/call",
            json!({"name": "mem_ingest", "arguments": {"path": path}}),
        ),
        None,
        Some(&vault),
    )
    .unwrap();
    assert!(
        sync["result"].get("taskId").is_none(),
        "a client that did not opt in must never receive a task: {sync}"
    );
    assert!(
        sync["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("files_seen"),
        "the synchronous path still returns the ingest report: {sync}"
    );

    // Declared: a task handle instead of a blocking ingest.
    let declared = handle_message_for_profile(
        &engine,
        &coord,
        Profile::Full,
        rpc(
            2,
            "tools/call",
            json!({
                "name": "mem_ingest",
                "arguments": {"path": path},
                "_meta": {
                    "io.modelcontextprotocol/clientCapabilities": {
                        "extensions": {"io.modelcontextprotocol/tasks": {}}
                    }
                }
            }),
        ),
        None,
        Some(&vault),
    )
    .unwrap();
    let result = &declared["result"];
    assert_eq!(result["resultType"], "task", "{declared}");
    assert!(
        result.get("task").is_none(),
        "the schema is flat: no nested task object"
    );
    let task_id = result["taskId"].as_str().unwrap().to_string();

    // The worker's own connection must land the rows in *this* vault, and
    // the completed result is the same payload the synchronous path returns.
    let done = wait_for_task(&engine, &coord, &task_id);
    assert_eq!(done["status"], "completed", "{done}");
    let text = done["result"]["content"][0]["text"].as_str().unwrap();
    let report: Value = serde_json::from_str(text).unwrap();
    assert!(
        report["files_seen"].as_u64().unwrap() >= 1,
        "the task must report a real ingest, not an empty run: {done}"
    );
    let _ = std::fs::remove_file(&vault);
}

/// A deploy with no vault *file* — an in-memory engine, an embedder, CI —
/// must not lose `mem_ingest` to the task machinery. Declaring the extension
/// is not enough on its own: without a path, `spawn_task` reports the tool
/// as not async-eligible and the call answers synchronously, exactly as it
/// did before this existed.
#[test]
fn mem_ingest_stays_synchronous_without_a_vault_path() {
    let (e, c) = setup();
    let workspace = task_workspace();
    std::fs::write(workspace.join("notes.md"), "# fallback\n").unwrap();
    let resp = handle_message_for_profile(
        &e,
        &c,
        Profile::Full,
        rpc(
            1,
            "tools/call",
            json!({
                "name": "mem_ingest",
                "arguments": {"path": workspace.to_string_lossy()},
                "_meta": {
                    "io.modelcontextprotocol/clientCapabilities": {
                        "extensions": {"io.modelcontextprotocol/tasks": {}}
                    }
                }
            }),
        ),
        None,
        None,
    )
    .unwrap();
    assert!(
        resp["result"].get("taskId").is_none(),
        "no vault path means no worker, so the call must not pretend: {resp}"
    );
    assert!(
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("files_seen"),
        "and it still ingests: {resp}"
    );
}

/// The defect the first external-dataset run exposed: `memnoth-ingest` keeps
/// structure and drops prose, so ingesting a directory of documents used to
/// produce a list of filenames you could not search. The query word below
/// exists nowhere but the document body — not in a filename, heading or
/// symbol — so this fails the moment `mem_ingest` stops storing text.
#[test]
fn ingested_document_text_is_searchable() {
    let (e, c) = setup();
    let workspace = task_workspace();
    std::fs::write(
        workspace.join("ledger-prose.md"),
        "# Ledger\n\nThe quarterly reconciliation flagged a discrepancy in the \
             Frankfurt settlement account.\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("notes-prose.txt"),
        "The workshop preferred the cobalt anodised finish for the housing.\n",
    )
    .unwrap();

    let report = handle_message_for_profile(
        &e,
        &c,
        Profile::Full,
        rpc(
            1,
            "tools/call",
            json!({
                "name": "mem_ingest",
                "arguments": {"path": workspace.to_string_lossy()}
            }),
        ),
        None,
        None,
    )
    .unwrap();
    let payload: Value =
        serde_json::from_str(report["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(
        payload["text_observations"].as_u64().unwrap_or(0) >= 2,
        "both bodies must be stored, not merely their filenames: {payload}"
    );

    let found = handle_message_for_profile(
        &e,
        &c,
        Profile::Full,
        rpc(
            2,
            "tools/call",
            json!({
                "name": "mem_search",
                "arguments": {"query": "cobalt anodised housing finish"}
            }),
        ),
        None,
        None,
    )
    .unwrap();
    let hits: Value =
        serde_json::from_str(found["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let names: Vec<&str> = hits["results"]
        .as_array()
        .map(|rows| rows.iter().filter_map(|r| r["name"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        names.iter().any(|n| n.contains("notes-prose.txt")),
        "mem_search must reach text that mem_ingest stored, got {names:?}"
    );
}

/// `kg_index` is the one tool whose engine can report a *real* percentage,
/// because the indexer exposes its own progress. This checks the whole path
/// rather than the callback in isolation: a client that opted in receives
/// the engine's own snapshots bound to its token, and a client that did not
/// receives nothing it never asked for.
#[test]
fn kg_index_reports_real_progress_to_a_client_that_opted_in() {
    // Held because this is the only test that asserts on the global notification queue, and a
    // `serve` loop in a concurrent test drains it into another transport. Without this the test
    // passes alone and fails in the suite — the exact failure it was written to prevent.
    let _queue = hold_notification_queue();
    let root = task_workspace();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn alpha() -> u32 { 1 }\n").unwrap();
    let (e, c) = setup();

    let call = |token: Option<&str>| {
        let mut params = json!({
            "name": "kg_index",
            "arguments": {"workspace": root.to_string_lossy()},
        });
        if let Some(token) = token {
            params["_meta"] = json!({"progressToken": token});
        }
        // Drained before the call as well as after, so the assertion is
        // about this call's notifications and not a previous test's.
        drain_notifications();
        let _ = handle_message(&e, &c, rpc(1, "tools/call", params));
        drain_notifications()
    };

    let with_token = call(Some("tok-kg-index"));
    assert!(
        with_token.len() >= 3,
        "expected the dispatcher's start/finish pair plus the engine's own \
             snapshots, got {with_token:?}"
    );
    assert!(
        with_token
            .iter()
            .all(|n| n["method"] == "notifications/progress"),
        "{with_token:?}"
    );
    assert!(
        with_token
            .iter()
            .all(|n| n["params"]["progressToken"] == "tok-kg-index"),
        "a progress update bound to the wrong token is one a client cannot route"
    );
    let messages: Vec<&str> = with_token
        .iter()
        .filter_map(|n| n["params"]["message"].as_str())
        .collect();
    assert!(
        messages.iter().any(|m| m.contains("scanning")),
        "the engine's own phase never reached the client: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("files")),
        "a progress update that names no file count tells the user nothing: {messages:?}"
    );
    assert_eq!(
        with_token.last().unwrap()["params"]["progress"],
        json!(1.0),
        "a run that finished must not be left looking unfinished"
    );

    assert!(
        call(None).is_empty(),
        "no progressToken is the client's way of saying it renders no progress"
    );
}

/// Every tool advertised as task-backed must actually have an async body,
/// or the guarantee hosts size their timeouts on is a lie. This is the test
/// that fails if someone adds a name to `TASK_ELIGIBLE_TOOLS` and forgets
/// the arm in `spawn_task`.
#[test]
fn every_task_eligible_tool_can_actually_start_a_worker() {
    let _registry = crate::tasks::registry_test_lock();
    let vault = std::env::temp_dir().join(format!(
        "memnoth-mcp-eligible-{}-{}.db",
        std::process::id(),
        memnoth_engine::now_ms()
    ));
    for name in TASK_ELIGIBLE_TOOLS {
        let args = json!({"workspace": task_workspace().to_string_lossy()});
        let handle = spawn_task(name, &args, Some(&vault), None);
        let handle = handle.unwrap_or_else(|| {
            panic!("`{name}` is advertised as task-backed but has no async body")
        });
        assert_eq!(handle["resultType"], "task", "{handle}");
        assert!(handle["taskId"].as_str().is_some(), "{handle}");
    }
    let _ = std::fs::remove_file(&vault);
}

#[test]
fn tasks_get_and_cancel_reject_unknown_tasks() {
    let (e, c) = setup();
    let get = handle_message(&e, &c, rpc(1, "tasks/get", json!({"taskId": "nope"}))).unwrap();
    assert_eq!(get["error"]["code"], -32602);
    let cancel = handle_message(&e, &c, rpc(2, "tasks/cancel", json!({"taskId": "nope"}))).unwrap();
    assert_eq!(cancel["error"]["code"], -32602);
    let no_id = handle_message(&e, &c, rpc(3, "tasks/get", json!({}))).unwrap();
    assert_eq!(no_id["error"]["code"], -32602);
    assert_eq!(
        no_id["error"]["message"], "missing taskId",
        "the spec's param is camelCase: taskId"
    );
}

#[test]
fn tasks_cancel_acknowledges_with_an_empty_result() {
    let _registry = crate::tasks::registry_test_lock();
    let (e, c) = setup();
    let task = tasks::create("kg_index", Some(tasks::DEFAULT_TASK_TTL_MS));
    let cancel = handle_message(
        &e,
        &c,
        rpc(1, "tasks/cancel", json!({"taskId": task.task_id})),
    )
    .unwrap();
    assert_eq!(cancel["result"]["resultType"], "complete");
    assert!(cancel["result"].get("error").is_none());
    let state =
        handle_message(&e, &c, rpc(2, "tasks/get", json!({"taskId": task.task_id}))).unwrap();
    assert_eq!(state["result"]["status"], "cancelled");
}

#[test]
fn tasks_update_is_acknowledged_rather_than_unknown() {
    let _registry = crate::tasks::registry_test_lock();
    let (e, c) = setup();
    let task = tasks::create("kg_index", Some(tasks::DEFAULT_TASK_TTL_MS));
    let resp = handle_message(
        &e,
        &c,
        rpc(
            1,
            "tasks/update",
            json!({
                "taskId": task.task_id,
                "inputResponses": {"whatever": {"action": "accept"}}
            }),
        ),
    )
    .unwrap();
    assert_eq!(resp["result"]["resultType"], "complete");
}

// ------------------------------------------------------------------
// Published-manifest guards.
//
// The registry entry, its version and the profile costs in it are claims
// *made to strangers*. Every one of them is derivable from this crate, so
// every one of them gets asserted against it — a marketing table cannot be
// the only place a number lives.
// ------------------------------------------------------------------

#[test]
fn registry_manifest_profile_counts_match_the_table() {
    let published =
        manifest()["_meta"]["io.modelcontextprotocol.registry/publisher-provided"]["profiles"]
            .clone();
    for (name, profile) in [
        ("micro", Profile::Micro),
        ("memory", Profile::Memory),
        ("code", Profile::Code),
        ("coord", Profile::Coord),
        ("full", Profile::Full),
        ("agent", Profile::Agent),
    ] {
        let actual = profile.tool_names().len();
        assert_eq!(
            published[name]["toolCount"].as_u64().map(|n| n as usize),
            Some(actual),
            "server.json publishes the wrong size for '{name}': a registry entry that \
                 overstates the surface is how a host installs a tool list it then cannot afford"
        );
    }
}

/// The `//!` profile table at the top of this file is the first surface a
/// host author reads, and it already drifted once (Full 42 vs 44, Memory 19
/// vs 21) while every mechanical guard stayed green — because no guard read
/// the header. This one does: each documented count must match
/// `Profile::tool_names`, so the header, the manifest and the CLI docs move
/// together with the table or the build fails here instead of in a host
/// session. Fix the docs, not this test.
#[test]
fn doc_header_profile_counts_match_the_table() {
    let src = include_str!("../lib.rs");
    for (name, profile) in PROFILE_TABLE {
        let actual = profile.tool_names().len();
        let row = format!("//! | `{name}` | {actual} |");
        assert!(
            src.lines().any(|line| line.trim_end().starts_with(&row)),
            "the `//!` profile table at the top of lib.rs documents the wrong size for \
                 '{name}': expected a row starting `{row}` — fix the header, not this test"
        );
    }
    let full = Profile::Full.tool_names().len();
    let headline = format!("//! memnoth exposes {full} tools on the full profile");
    assert!(
        src.lines()
            .any(|line| line.trim_end().starts_with(&headline)),
        "the lib.rs headline must read `{headline}…` — fix the header, not this test"
    );
    // The tier-size prose on `TOOL_PROFILES` names the live Memory size for
    // the same reason: a bare number in a doc comment rots silently.
    let memory = Profile::Memory.tool_names().len();
    assert!(
        src.contains(&format!("held at {memory}")),
        "the `TOOL_PROFILES` tier-size prose must name the live Memory size ({memory}) — \
             fix the prose, not this test"
    );
    // The CLI's install path (`src/cli/mcp_add.rs`) and its clap help in
    // `src/cli/args.rs` used to be checked here for the same reason. Both were
    // removed in the slim cut: the binary no longer ships an MCP installer, so
    // there is no CLI prose left to drift. If `memnoth mcp add` comes back,
    // reinstate these two blocks rather than trusting the same drift to be
    // caught somewhere else.
}

#[test]
fn registry_manifest_version_matches_the_published_artifact() {
    let published = manifest()["version"].as_str().unwrap().to_string();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let raw =
        std::fs::read_to_string(&root).unwrap_or_else(|e| panic!("read {}: {e}", root.display()));
    let declared = package_version(&raw).expect("root Cargo.toml [package] version");
    assert_eq!(
        published, declared,
        "server.json must name the version that is actually published: a stale version is \
             rejected by the registry publisher, and a wrong one describes a surface that may not \
             exist"
    );
    // The npm launcher is published from this same tree, so it drifts the
    // same way and is checked here rather than in a third place.
    let npm = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("npm/package.json");
    let npm_raw =
        std::fs::read_to_string(&npm).unwrap_or_else(|e| panic!("read {}: {e}", npm.display()));
    let npm_json: Value =
        serde_json::from_str(&npm_raw).expect("npm/package.json must be valid JSON");
    assert_eq!(
        npm_json["version"].as_str().unwrap(),
        published,
        "the npm launcher must ship the same version as the manifest"
    );
}

/// Write the measured costs into the manifest — both halves of the context
/// tax: the `tools/list` bytes per profile, and the per-query retrieval
/// tiers (S3).
///
/// `#[ignore]`d because it *writes a file* and must never run as part of a
/// normal test sweep. Run it once after a build that changes tool
/// descriptions or the recall tiers, then commit the manifest:
///
/// `cargo test -p memnoth-mcp -- --ignored regenerate_manifest_schema_cost`
///
/// This is why the numbers are trustworthy: they are produced by the code
/// that ships, not typed by a human from memory, and the guards below keep
/// them from drifting afterwards.
#[test]
#[ignore = "rewrites crates/memnoth-mcp/server.json; run explicitly"]
fn regenerate_manifest_schema_cost() {
    let raw = std::fs::read_to_string(manifest_path()).unwrap();
    let mut manifest: Value = serde_json::from_str(&raw).unwrap();
    let mut measured = serde_json::Map::new();
    for (name, profile) in PROFILE_TABLE {
        measured.insert(name.to_string(), json!(schema_cost(*profile)));
    }
    manifest["_meta"]["io.modelcontextprotocol.registry/publisher-provided"]["toolSchemaCost"]["bytesPerProfile"] =
        Value::Object(measured.clone());
    manifest["_meta"]["io.modelcontextprotocol.registry/publisher-provided"]["retrievalTierCost"] = json!({
        "//": "The retrieval half of the context tax: what one query costs at each recall tier, beside the tools/list half above. Values come from crates/memnoth-mcp/src/recall_budget.rs::tier_cost_rows and cannot drift (registry_manifest_retrieval_cost_cannot_drift).",
        "rows": recall_budget::tier_cost_rows(),
    });
    std::fs::write(
        manifest_path(),
        format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
    )
    .unwrap();
    eprintln!("wrote measured schema cost: {:?}", measured);
    eprintln!(
        "wrote retrieval tier costs: {:?}",
        recall_budget::tier_cost_rows()
    );
}

/// S3: the published retrieval cost may be *absent* (a number nobody has
/// dumped yet is not a false claim) but it may never be **stale**. A host
/// comparing its own tokenizer against `packTokens` is reading these rows.
#[test]
fn registry_manifest_retrieval_cost_cannot_drift() {
    let published = manifest()["_meta"]["io.modelcontextprotocol.registry/publisher-provided"]
            ["retrievalTierCost"]["rows"]
            .clone();
    let Some(published) = published.as_array().filter(|rows| !rows.is_empty()) else {
        eprintln!(
            "server.json publishes no retrieval tier costs yet; run \
                 `cargo test -p memnoth-mcp -- --ignored regenerate_manifest_schema_cost`"
        );
        return;
    };
    assert_eq!(
        Value::Array(published.clone()),
        Value::Array(recall_budget::tier_cost_rows()),
        "the published retrieval tier costs no longer match the shipping table.\n\
             Regenerate with \
             `cargo test -p memnoth-mcp -- --ignored regenerate_manifest_schema_cost` and commit \
             server.json: a host sizing a query against this table would be reading a stale figure."
    );
}

/// The published context tax may be *absent* — a number nobody has measured
/// yet is not a false claim — but it may never be **stale**: once the
/// manifest carries a figure, it has to match what the shipping definitions
/// actually serialize to. Rivals publish this number; the difference here is
/// that it cannot silently become a lie.
#[test]
fn registry_manifest_schema_cost_cannot_drift() {
    let published = manifest()["_meta"]["io.modelcontextprotocol.registry/publisher-provided"]
            ["toolSchemaCost"]["bytesPerProfile"]
            .clone();
    let Some(published) = published.as_object().filter(|map| !map.is_empty()) else {
        eprintln!(
            "server.json publishes no measured schema cost yet; run \
                 `cargo test -p memnoth-mcp -- --ignored regenerate_manifest_schema_cost`"
        );
        return;
    };
    for (name, profile) in PROFILE_TABLE {
        let measured = schema_cost(*profile);
        assert_eq!(
            published
                .get(*name)
                .and_then(Value::as_u64)
                .map(|n| n as usize),
            Some(measured),
            "the published cost for '{name}' no longer matches the shipping tools/list payload.\n\
                 Re-measure with \
                 `cargo test -p memnoth-mcp -- --ignored regenerate_manifest_schema_cost` and commit \
                 server.json: a host sizing its context budget is reading this number."
        );
    }
}

#[test]
fn registry_manifest_matches_what_the_server_advertises() {
    let published =
        manifest()["_meta"]["io.modelcontextprotocol.registry/publisher-provided"].clone();
    let versions: Vec<&str> = published["protocolVersions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(versions, supported_protocol_versions());
    let extensions: Vec<&str> = published["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(extensions, vec![tasks::TASKS_EXTENSION]);
    // And the live server agrees, which is what a host actually sees.
    let (e, c) = setup();
    let discover = handle_message(&e, &c, rpc(1, "server/discover", json!({}))).unwrap();
    let advertised: Vec<&str> = discover["result"]["protocolVersions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        advertised, versions,
        "the registry entry and the running server must agree on the protocol versions"
    );
}

/// A tool result carries what it cost, in the one place a host is allowed to add fields.
///
/// The receipt goes into `_meta` so a host that knows it can render `6 ms · 1.8 KB` from the
/// response it already has, and one that does not ignores an unknown key — the protocol's own
/// extension rule. The envelope's server stamp must survive: `complete_result` runs after this.
#[test]
fn a_tool_result_carries_its_receipt_in_meta() {
    let started = std::time::Instant::now();
    let receipt = memnoth_capability::Receipt::measured(
        "kg_status",
        "full",
        memnoth_capability::CacheClass::NoStore,
        120,
        started,
    );
    let out = complete_result(with_receipt(
        json!({"content": [{"type": "text", "text": "ok"}]}),
        receipt,
    ));

    assert_eq!(out["_meta"]["receipt"]["tool"], "kg_status");
    assert_eq!(out["_meta"]["receipt"]["bytes_out"], 120);
    assert_eq!(out["_meta"]["receipt"]["tokens_out"], 30);
    assert_eq!(out["_meta"]["receipt"]["refused"], false);
    assert_eq!(out["resultType"], "complete");
    assert!(
        out["_meta"][META_SERVER_INFO].is_object(),
        "the envelope's server stamp must survive the receipt: {out}"
    );
}
