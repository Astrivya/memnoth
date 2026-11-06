use super::*;

#[test]
fn initialize_roundtrip() {
    let (e, c) = setup();
    let resp = handle_message(&e, &c, rpc(1, "initialize", json!({}))).unwrap();
    assert_eq!(resp["result"]["serverInfo"]["name"], "memnoth");
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
}

#[test]
fn initialize_negotiates_unsupported_versions_down() {
    let (e, c) = setup();
    for requested in [json!("2099-01-01"), json!("2025-06-18"), json!(7)] {
        let resp = handle_message(
            &e,
            &c,
            rpc(1, "initialize", json!({"protocolVersion": requested})),
        )
        .unwrap();
        assert_eq!(
            resp["result"]["protocolVersion"], "2024-11-05",
            "unsupported version must not be echoed"
        );
    }
    let resp = handle_message(
        &e,
        &c,
        rpc(1, "initialize", json!({"protocolVersion": "2024-11-05"})),
    )
    .unwrap();
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
}

#[test]
fn tools_list_full_surface_has_51() {
    let (e, c) = setup();
    let resp = handle_message(&e, &c, rpc(2, "tools/list", json!({}))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 51);
}

#[test]
fn schemas_declare_required_and_descriptions() {
    let defs = tool_definitions();
    assert_eq!(defs.len(), 40);
    for def in &defs {
        let name = def["name"].as_str().unwrap();
        let s = &def["inputSchema"];
        assert_eq!(s["type"], "object", "{name}");
        let props = s["properties"].as_object().unwrap();
        for (key, val) in props {
            assert!(
                !val["description"].as_str().unwrap_or_default().is_empty(),
                "{name}.{key} missing description"
            );
        }
        for r in s["required"].as_array().unwrap() {
            assert!(
                props.contains_key(r.as_str().unwrap()),
                "{name} requires unknown arg {r}"
            );
        }
    }
    let find =
        |n: &str| defs.iter().find(|d| d["name"] == n).unwrap()["inputSchema"]["required"].clone();
    assert_eq!(find("mem_search"), json!(["query"]));
    assert_eq!(find("mem_add_fact"), json!(["entity_name", "content"]));
    assert_eq!(find("coord_claim"), json!(["session_id", "path_glob"]));
    assert_eq!(find("coord_task_create"), json!(["title"]));
    assert_eq!(find("coord_send"), json!(["from_session", "body"]));
    assert_eq!(find("coord_inbox"), json!(["session_id"]));
    assert_eq!(find("mem_stats"), json!([]));
    assert_eq!(find("coord_task_list"), json!([]));
}

/// `kg_index` advertises the embed opt-out, so a host can buy speed on a
/// structural index without a server-side env change it cannot see.
///
/// The engine hook (`with_embedding`) is inert unless a surface calls it; this
/// pins the surface half, since a tool that silently ignores a documented
/// argument is worse than one that never had it.
#[test]
fn kg_index_advertises_the_embed_opt_out() {
    let defs = tool_definitions();
    let schema = &defs.iter().find(|d| d["name"] == "kg_index").unwrap()["inputSchema"];
    let embed = &schema["properties"]["embed"];
    assert_eq!(embed["type"], "boolean", "embed must be a boolean flag");
    assert!(
        !embed["description"].as_str().unwrap_or_default().is_empty(),
        "the flag is a trade-off; it needs a description to be usable"
    );
    assert!(
        !schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "embed"),
        "embed is optional — defaulting to on is the documented behaviour"
    );
}

#[test]
fn annotations_classify_all_tools() {
    let defs = tool_definitions();
    assert_eq!(defs.len(), 40);
    let mut read_only = Vec::new();
    for def in &defs {
        let name = def["name"].as_str().unwrap().to_string();
        let a = &def["annotations"];
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(a[hint].is_boolean(), "{name} missing {hint}");
        }
        if a["readOnlyHint"] == json!(true) {
            assert_eq!(
                a["destructiveHint"],
                json!(false),
                "{name} read-only but destructive"
            );
            read_only.push(name);
        }
    }
    read_only.sort();
    assert_eq!(
        read_only,
        vec![
            "coord_inbox",
            "coord_status",
            "coord_task_list",
            "kg_callees",
            "kg_changes",
            "kg_communities",
            "kg_complexity",
            "kg_coverage",
            "kg_dead",
            "kg_def",
            "kg_doctor",
            "kg_explore",
            "kg_impact",
            "kg_refs",
            "kg_risk",
            "kg_search",
            "kg_services",
            "kg_status",
            "kg_trace",
            "kg_unresolved",
            "kg_why",
            "mem_duplicates",
            "mem_find_facts",
            "mem_related",
            "mem_search",
            "mem_stats",
        ]
    );
    assert_eq!(
        defs.iter().find(|d| d["name"] == "mem_ingest").unwrap()["annotations"]["openWorldHint"],
        json!(true)
    );
    assert_eq!(
        defs.iter()
            .find(|d| d["name"] == "coord_heartbeat")
            .unwrap()["annotations"]["idempotentHint"],
        json!(true)
    );
}

#[test]
fn every_tool_in_every_profile_carries_annotations() {
    for profile in [
        Profile::Micro,
        Profile::Memory,
        Profile::Coord,
        Profile::Code,
        Profile::Full,
        Profile::Agent,
    ] {
        let defs = visible_tools(profile);
        assert!(!defs.is_empty(), "{profile:?} exposes no tools");
        for def in &defs {
            let name = def["name"].as_str().unwrap().to_string();
            let a = &def["annotations"];
            for hint in [
                "readOnlyHint",
                "destructiveHint",
                "idempotentHint",
                "openWorldHint",
            ] {
                assert!(
                    a[hint].is_boolean(),
                    "{profile:?} tool {name} missing {hint}: no untagged tools survive"
                );
            }
            // read-only tools must never claim destructiveness: hosts
            // auto-approve on this pair.
            if a["readOnlyHint"] == json!(true) {
                assert_eq!(
                    a["destructiveHint"],
                    json!(false),
                    "{profile:?} tool {name} is read-only but destructive"
                );
            }
        }
    }
}

#[test]
fn compose_toolset_intersects_profile_and_allowlist() {
    // Empty allow list = empty toolset, even at full profile.
    assert!(compose_toolset(Profile::Full, &[]).is_empty());

    // Unknown ids and out-of-profile ids are dropped, known ids kept.
    let mixed = compose_toolset(Profile::Full, &["mem_search", "not_a_tool", "coord_status"]);
    let mut names: Vec<&str> = mixed.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["coord_status", "mem_search"]);

    // The profile is the ceiling: an allow list can only narrow it.
    assert!(compose_toolset(Profile::Micro, &["agent_prompt"]).is_empty());
    let agent_pair = compose_toolset(Profile::Agent, &["agent_prompt", "agent_wait", "mem_stats"]);
    let mut agent_names: Vec<&str> = agent_pair
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    agent_names.sort_unstable();
    assert_eq!(agent_names, vec!["agent_prompt", "agent_wait"]);

    // Full allow list passes the whole profile through unchanged.
    let all_full: Vec<String> = visible_tools(Profile::Full)
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let all_full_refs: Vec<&str> = all_full.iter().map(String::as_str).collect();
    assert_eq!(
        compose_toolset(Profile::Full, &all_full_refs).len(),
        all_full.len()
    );

    // Composed definitions stay complete: schema + annotations intact.
    let one = &compose_toolset(Profile::Full, &["mem_search"])[0];
    assert_eq!(one["inputSchema"]["required"], json!(["query"]));
    assert!(one["annotations"]["readOnlyHint"].is_boolean());
}

#[test]
fn mem_search_packing_caps_tokens_preserves_order_and_never_empties() {
    // A small answer: kept whole, no breach, tokens accounted.
    let (kept, rep) =
        pack_search_results(vec![json!({"id": 1, "name": "a", "text": "x".repeat(100)})]);
    assert_eq!(kept.len(), 1);
    assert_eq!(rep["over_budget"], false);
    assert_eq!(rep["skipped_oversize"], 0);
    assert!(rep["tokens"].as_u64().unwrap() > 0);

    // A row larger than the whole budget is kept alone and the breach is
    // reported — an empty answer to a recall query is a failure the budget
    // caused, and failures must be visible.
    let (kept, rep) = pack_search_results(vec![json!({"id": 1, "text": "x".repeat(60_000)})]);
    assert_eq!(kept.len(), 1, "never empty");
    assert_eq!(rep["over_budget"], true);
    assert!(rep["tokens"].as_u64().unwrap() > rep["budget"].as_u64().unwrap());

    // A long answer is truncated by the budget in rank order, never reordered.
    let many: Vec<Value> = (0..300)
        .map(|i| json!({"id": i, "text": "x".repeat(200)}))
        .collect();
    let (kept, rep) = pack_search_results(many);
    assert!(
        rep["skipped_oversize"].as_u64().unwrap() > 0,
        "budget must bind"
    );
    assert!(rep["kept"].as_u64().unwrap() < 300);
    assert!(rep["tokens"].as_u64().unwrap() <= rep["budget"].as_u64().unwrap());
    let ids: Vec<i64> = kept.iter().map(|r| r["id"].as_i64().unwrap()).collect();
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "packing preserves rank order"
    );
}

#[test]
fn profile_parse_maps_env_values() {
    assert_eq!(Profile::parse(Some("micro")), Profile::Micro);
    assert_eq!(Profile::parse(Some("memory")), Profile::Memory);
    assert_eq!(Profile::parse(Some("coord")), Profile::Coord);
    assert_eq!(Profile::parse(Some("code")), Profile::Code);
    assert_eq!(Profile::parse(Some("full")), Profile::Full);
    assert_eq!(Profile::parse(None), Profile::Full);
    assert_eq!(Profile::parse(Some("bogus")), Profile::Full);
}

#[test]
fn known_names_are_exactly_what_parse_accepts() {
    // `mcp doctor` asks this instead of re-listing the names: a name the
    // parser accepts but the doctor calls unknown (or the reverse) would
    // report a healthy install as broken, or bless a silent fallback.
    for name in ["micro", "memory", "coord", "code", "agent"] {
        assert!(
            Profile::is_known_name(name),
            "'{name}' must be a known profile name"
        );
        assert_ne!(
            Profile::parse(Some(name)),
            Profile::Full,
            "'{name}' must not fall back to Full"
        );
    }
    // `full` parses to `Full` without the unknown-value warning path: it is
    // the default the installer writes, so warning on it would cry wolf on
    // every default install.
    assert!(Profile::is_known_name("full"));
    assert!(!Profile::is_known_name("bogus"));
    assert!(!Profile::is_known_name(""));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn serve_loop_end_to_end_over_in_memory_pipes() {
    // Held for the whole test: this loop drains the process-global queue, so running it beside the
    // progress test would take that test's notifications and deliver them here.
    let _queue = hold_notification_queue();
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// One response, plus whatever the server interleaved before it.
    ///
    /// **Responses are matched by `id`, never by arrival order.** The protocol lets a server emit
    /// `notifications/*` at any point — including between a request and its response — and that is
    /// exactly why every real client routes by id. Reading the next line and calling it "the
    /// response" is not a shortcut, it is a different protocol: the first notification the server
    /// sends is parsed as the answer to whichever request happened to be in flight, which is how a
    /// working dispatcher came to look like a broken one here. The failure was doubly misleading —
    /// `tools["result"]["tools"]` on a *notification* is `Null`, so the panic surfaced as
    /// `Option::unwrap()` on `None` with no mention of a notification anywhere.
    ///
    /// Interleaved notifications are returned rather than discarded: they are legitimate, but they
    /// are also a behaviour of the server worth recording, and a test that silently swallows them
    /// cannot tell "the server said nothing" from "the server said something I ignored".
    async fn rpc(
        req_tx: &mut tokio::io::DuplexStream,
        responses: &mut BufReader<tokio::io::DuplexStream>,
        id: u64,
        method: &str,
        params: Value,
    ) -> (Value, Vec<Value>) {
        let mut body = serde_json::to_string(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .unwrap();
        body.push('\n');
        req_tx.write_all(body.as_bytes()).await.unwrap();
        req_tx.flush().await.unwrap();
        let mut interleaved: Vec<Value> = Vec::new();
        loop {
            let mut line = String::new();
            let read = responses.read_line(&mut line).await.unwrap();
            assert!(
                read > 0,
                "the server closed the pipe while `{method}` (id {id}) was in flight; \
                     {interleaved:?} arrived first"
            );
            let value: Value = serde_json::from_str(&line).unwrap();
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return (value, interleaved);
            }
            // Anything else must be a *notification*: a message with a method and no id. A stray
            // response, or an id-less response, would mean the server mis-addressed a reply, and
            // skipping it here would turn that into a passing test.
            assert!(
                value.get("method").is_some() && value.get("id").is_none(),
                "a message that is neither the response to `{method}` (id {id}) nor a \
                     notification: {value}"
            );
            interleaved.push(value);
        }
    }

    let (engine, coord) = setup();
    let (mut req_tx, req_rx) = tokio::io::duplex(8192);
    let (resp_tx, resp_rx) = tokio::io::duplex(65536);

    let server =
            // No vault path: this test drives an in-memory engine through the
            // real loop, and the `None` is exactly what an embedder passes.
            tokio::spawn(serve(
                engine,
                coord,
                Profile::Micro,
                None,
                req_rx,
                resp_tx,
                None,
            ));
    let mut responses = BufReader::new(resp_rx);

    let (init, mut interleaved) =
        rpc(&mut req_tx, &mut responses, 1, "initialize", json!({})).await;
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    // Explicit `false`, not an omitted flag — see the http transport's identical assertion for
    // why an absent capability and a declined one must not look the same to a host.
    assert_eq!(
        init["result"]["capabilities"]["prompts"],
        json!({"listChanged": false})
    );

    let (tools, extra) = rpc(&mut req_tx, &mut responses, 2, "tools/list", json!({})).await;
    interleaved.extend(extra);
    let listed: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(listed.len(), 4, "{listed:?}");

    let (rejected, extra) = rpc(
        &mut req_tx,
        &mut responses,
        3,
        "tools/call",
        json!({"name": "coord_join", "arguments": {"name": "x"}}),
    )
    .await;
    interleaved.extend(extra);
    assert_eq!(rejected["result"]["isError"], true);
    assert!(
        rejected["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("MEMNOTH_MCP_PROFILE")
    );

    let (added, extra) = rpc(
        &mut req_tx,
        &mut responses,
        4,
        "tools/call",
        json!({"name": "mem_add_fact", "arguments": {"entity_name": "e2e", "content": "works"}}),
    )
    .await;
    interleaved.extend(extra);
    assert!(added["result"]["content"][0]["text"].is_string());

    let (prompt, extra) = rpc(
        &mut req_tx,
        &mut responses,
        5,
        "prompts/get",
        json!({"name": "handoff"}),
    )
    .await;
    interleaved.extend(extra);
    assert!(
        prompt["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("coord_status")
    );

    let (stats, extra) = rpc(
        &mut req_tx,
        &mut responses,
        6,
        "resources/read",
        json!({"uri": "mem://stats"}),
    )
    .await;
    interleaved.extend(extra);
    assert_eq!(stats["result"]["contents"][0]["uri"], "mem://stats");
    assert!(
        stats["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("entities_current")
    );

    // Recorded, not swallowed. Interleaving is legal, so tolerating it is correct — but a test
    // that discards it cannot tell "the server said nothing" from "the server said something this
    // reader ignored", and only the former is what the six assertions above are about.
    assert!(
        interleaved.iter().all(|n| n["method"]
            .as_str()
            .is_some_and(|m| m.starts_with("notifications/"))),
        "{} notification(s) interleaved; only `notifications/*` may: {interleaved:?}",
        interleaved.len()
    );

    drop(req_tx);
    server.await.unwrap().unwrap();
}

#[test]
fn full_profile_is_allow_all_with_no_second_list() {
    // Full must stay allow-all: any hardcoded list here is a second list
    // that will rot, and Full has to pick a new tool up by merely defining
    // it. Narrow profiles narrow through `TOOL_PROFILES` instead.
    assert!(Profile::Full.allows("no_such_tool_yet"));
    for profile in [
        Profile::Micro,
        Profile::Memory,
        Profile::Coord,
        Profile::Code,
        Profile::Agent,
    ] {
        assert!(
            !profile.allows("no_such_tool_yet"),
            "{profile:?} must not expose a tool that does not exist — narrow profiles are \
                 closed sets, so a typo hides a tool instead of inventing exposure for it"
        );
    }
    // Full's visible surface must equal exactly the union of the two
    // definition sources — derived, never a hardcoded count.
    let mut expected: Vec<String> = tool_definitions()
        .iter()
        .filter_map(|d| d["name"].as_str().map(str::to_owned))
        .chain(
            tools_agent::tool_definitions()
                .iter()
                .filter_map(|d| d["name"].as_str().map(str::to_owned)),
        )
        .collect();
    expected.sort();
    let visible = visible_tools(Profile::Full);
    let mut actual: Vec<String> = visible
        .iter()
        .filter_map(|d| d["name"].as_str().map(str::to_owned))
        .collect();
    actual.sort();
    assert_eq!(actual, expected);
}

#[test]
fn profile_filters_tool_list() {
    let (e, c) = setup();
    let count = |profile: Profile| {
        handle_message_for_profile(&e, &c, profile, rpc(1, "tools/list", json!({})), None, None)
            .unwrap()["result"]["tools"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(count(Profile::Micro), 4);
    assert_eq!(count(Profile::Memory), 21);
    assert_eq!(count(Profile::Code), 26);
    assert_eq!(count(Profile::Coord), 11);
    assert_eq!(count(Profile::Full), 51);
}

/// The moat, asserted by name — with the docstring's own reason in the
/// message, so "tidying" the table means arguing with the moat's words.
///
/// `tools_agent.rs` documents the quartet as *"Temporal reasoning no
/// competitor MCP offers"* and *"That is the moat; these tools make it
/// callable."* Before `TOOL_PROFILES` existed, all four were reachable from
/// `Profile::Full` only — i.e. from no profile a memory-focused host loads.
#[test]
fn memory_profile_exposes_the_temporal_quartet() {
    const QUARTET: [&str; 4] = [
        "vault_history",
        "vault_asof",
        "vault_contradictions",
        "vault_top",
    ];
    let names = Profile::Memory.tool_names();
    for tool in QUARTET {
        assert!(
            names.iter().any(|name| name.as_str() == tool),
            "Profile::Memory must expose `{tool}`.\n\
                 tools_agent.rs says why: `vault_asof` is \"Temporal reasoning no competitor MCP \
                 offers\", and the temporal quartet is \"the moat; these tools make it \
                 callable\".\n\
                 These four were reachable only from Profile::Full once, so the profile named for \
                 memory could not reach the category it names. If you are removing `{tool}`: you \
                 are removing the one category no competitor MCP exposes, and you have to argue \
                 with those words — not with this assertion.\n\
                 Memory profile currently exposes: {names:?}"
        );
    }
}

/// A memory host must be able to check whether its **own** index is stale:
/// the vault answers questions, the code graph says whether the answers are
/// current. `Profile::Memory` omitted these verbs, so staleness was
/// invisible to the host most likely to care.
#[test]
fn memory_profile_can_check_its_own_index() {
    let names = Profile::Memory.tool_names();
    for tool in [
        "kg_index",
        "kg_status",
        "kg_coverage",
        "kg_unresolved",
        "kg_search",
        "kg_changes",
    ] {
        assert!(
            names.iter().any(|name| name.as_str() == tool),
            "Profile::Memory must expose `{tool}` so a memory-focused host can tell whether \
                 its own index is stale — kg_status (staleness), kg_coverage (the parse ledger), \
                 kg_unresolved (refs the graph could not bind), kg_search (look a symbol up), \
                 kg_changes (what the working tree would break), kg_index (rebuild it).\n\
                 Memory profile currently exposes: {names:?}"
        );
    }
}

/// Every profile's surface is a subset of `Full`'s — the invariant that
/// makes a future split (moving verbs into `code`) safe — with the one
/// deliberate exception documented below.
#[test]
fn every_profile_tools_are_a_subset_of_full() {
    let full = Profile::Full.tool_names();
    for profile in [
        Profile::Micro,
        Profile::Memory,
        Profile::Coord,
        Profile::Code,
    ] {
        for tool in profile.tool_names() {
            assert!(
                full.contains(&tool),
                "{profile:?} exposes `{tool}` but Full does not: Full is allow-all over the \
                     base + agent-utility families, so a tool outside it is unreachable from the \
                     profile that can see everything"
            );
        }
    }
    // The agent-session family is the exception, and it is deliberate: those
    // tools need the HTTP transport and an `AgentBridge`, so Full (which
    // also serves stdio) must not advertise them.
    let agent_tools = Profile::Agent.tool_names();
    assert_eq!(
        agent_tools.len(),
        agent::AGENT_TOOLS.len(),
        "the Agent profile is exactly the agent-session family"
    );
    for tool in &agent_tools {
        assert!(
            !full.contains(tool),
            "`{tool}` is agent-session-only: it must not appear in Full, which serves stdio \
                 where there is no `AgentBridge` to call it"
        );
    }
}

/// Membership is a pure function of the metadata table, in both directions:
///
/// * **complete** — every defined tool has a row (`TOOL_PROFILES` ∪
///   `agent::AGENT_TOOLS`), so a new tool cannot silently become Full-only.
///   This is the assertion that would have caught the temporal quartet, and
///   it is the one that must fail first when someone adds tool 46.
/// * **exact** — a profile's resolved surface equals the rows naming it, so
///   no control-flow branch can add a tool behind the table's back (the old
///   `if matches!(profile, Profile::Full)` shape).
#[test]
fn profile_membership_is_a_pure_function_of_metadata() {
    // All three definition sources, so this covers the agent-session
    // family too — otherwise the comparison would be 39 declared rows
    // against 45 defined tools and would fail for the wrong reason.
    let mut defined: Vec<String> = tool_definitions()
        .iter()
        .chain(tools_agent::tool_definitions().iter())
        .chain(agent_tool_definitions().iter())
        .filter_map(|d| d["name"].as_str().map(str::to_owned))
        .collect();
    defined.sort();
    let mut declared: Vec<String> = memnoth_capability::TOOL_PROFILES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .chain(agent::AGENT_TOOLS.iter().map(|name| (*name).to_owned()))
        .collect();
    declared.sort();
    assert_eq!(
        declared, defined,
        "every defined tool needs a membership row: a tool that is in neither TOOL_PROFILES \
             nor agent::AGENT_TOOLS is reachable from Profile::Full only — the exact shape that \
             cost Profile::Memory the temporal quartet"
    );

    for profile in [
        Profile::Micro,
        Profile::Memory,
        Profile::Coord,
        Profile::Code,
    ] {
        let mut expected: Vec<String> = memnoth_capability::TOOL_PROFILES
            .iter()
            .filter(|(_, profiles)| {
                profiles.contains(
                    &memnoth_capability::Profile::from_name(
                        &format!("{profile:?}").to_ascii_lowercase(),
                    )
                    .expect("shared profile"),
                )
            })
            .map(|(name, _)| (*name).to_owned())
            .collect();
        expected.sort_unstable();
        let mut actual = profile.tool_names();
        actual.sort_unstable();
        assert_eq!(
            actual, expected,
            "{profile:?}'s tools must come from TOOL_PROFILES alone: a tool added by control \
                 flow instead of a table row is the bug this table exists to prevent"
        );
    }

    assert!(
        memnoth_capability::TOOL_PROFILES
            .iter()
            .all(|(name, _)| !agent::AGENT_TOOLS.contains(name)),
        "agent-session tools must not also appear in TOOL_PROFILES: two lists for one family \
             is how membership drifts"
    );
}

/// `Memory` is the profile a memory-focused host loads *instead of* a
/// bigger one, so its size is a contract, not an accident.
///
/// The ceiling is the rival's own solo-memory tier (memorix ships micro 9 /
/// lite 20 / team 28 / full 40+), and codegraph's answer to context tax is
/// *one* advertised tool — so "put everything in Memory and move on" is not
/// available.
///
/// Raised 20 → 21 once, deliberately, for `mem_supersede` + `mem_forget`: the
/// write half of the temporal vocabulary. Every read of that journal already
/// existed while nothing could write to one, which made the moat unexercisable
/// — a host could see a superseded belief and not supersede anything. Two tools
/// buying the only capability in this table no rival can answer is cheap at
/// one slot; the next raise is not, and the message below says where to look
/// instead.
#[test]
fn memory_profile_stays_within_a_defensible_size() {
    let names = Profile::Memory.tool_names();
    assert!(
        names.len() <= 21,
        "Profile::Memory exposes {} tools, over the 21-tool ceiling.\n\
             Profiles exist to cut context tax, which memorix markets explicitly: it ships micro \
             9 / lite 20, codegraph advertises one tool, codebase-memory-mcp ships scout 7 / \
             analysis 11. A host that wanted everything would load `full`.\n\
             If the quartet or the index verbs are what pushed it over, move the *navigation* \
             verbs to the `code` profile — never drop the quartet, and never grow `memory` into a \
             second `full`.\n\
             Currently: {names:?}",
        names.len()
    );
    // The split is where the design says it should be: `code` owns the
    // navigation verbs, and the quartet did not move with them.
    let code = Profile::Code.tool_names();
    for tool in ["kg_explore", "kg_impact", "kg_dead", "fs_read", "git_diff"] {
        assert!(
            code.iter().any(|name| name.as_str() == tool),
            "the `code` profile owns the navigation + toolbox verbs, so it must expose \
                 `{tool}`; currently: {code:?}"
        );
    }
    for tool in [
        "vault_history",
        "vault_asof",
        "vault_contradictions",
        "vault_top",
    ] {
        assert!(
            names.iter().any(|name| name.as_str() == tool),
            "after any split the temporal quartet stays in `memory` — `{tool}` must not move \
                 to `code`: the profile that names a category must reach that category's moat"
        );
    }
}

/// The default fusion is a **measured** decision, not a preference: the
/// `bm25_graph` arm beat every rank-fusion arm on full LoCoMo and on a
/// LongMemEval-S stride sample (`eval::evidence_fusion_arena`). This test exists
/// so reverting it is a deliberate act with a failing test attached, instead of
/// an edit that looks harmless in review.
#[test]
fn the_default_fusion_is_the_arm_the_arena_measured_as_best() {
    // `bm25_graph`, not `causal_graph`, and the doc comment above `DEFAULT_FUSION`
    // carries both datasets that decided it: causal won LoCoMo (+2.9 pp any@1)
    // and lost LongMemEval-S (−4.0 pp any@1, worse in every shard). A default that
    // trades datasets is a regression with a nicer table.
    assert_eq!(DEFAULT_FUSION, Fusion::Bm25Graph);
    // Caller argument wins over the environment; a typo falls through to the
    // default rather than failing the search.
    assert_eq!(active_fusion(Some("rrf")), Fusion::Rrf);
    assert_eq!(active_fusion(Some("BM25_GRAPH")), Fusion::Bm25Graph);
    assert_eq!(active_fusion(Some("not_a_fusion")), DEFAULT_FUSION);
    // Only asserted when nothing in the environment is overriding it — the env
    // var is the documented escape hatch, so a developer who set it should see
    // the override honoured, not a failing test.
    if std::env::var("MEMNOTH_MCP_FUSION").is_err() {
        assert_eq!(active_fusion(None), DEFAULT_FUSION);
    }
}

/// Every arm has a distinct name, and the name a caller can pass round-trips.
/// Without this, the arena could report a table of arms whose names no longer
/// select distinct retrievers — which is precisely how two arms silently
/// changed definition mid-session once already.
#[test]
fn every_fusion_arm_has_a_round_tripping_name() {
    let arms = Fusion::ALL;
    let mut names: Vec<&str> = arms.iter().map(|f| f.label()).collect();
    let count = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(count, names.len(), "two arms share a name: {names:?}");
    for arm in arms {
        assert_eq!(Fusion::parse(arm.label()), Some(*arm), "{arm:?}");
    }
}

/// `fusion=dense_hybrid` must be *plumbed* even where the embedder is absent:
/// with a model it fuses a dense ranking in, without one it keeps the lexical
/// order exactly as `bm25_graph` would. Neither outcome may error, empty the
/// result set, or lose the graph tail. Asserting the union (rather than the dense
/// branch) keeps this deterministic on a machine with no ONNX model while still
/// proving the arm is reachable — the same discipline as the cross-encoder test.
#[test]
fn dense_hybrid_fuses_when_available_and_degrades_to_lexical_order_when_not() {
    let (e, c) = setup();
    let now = memnoth_engine::now_ms();
    let a = e
        .add_entity(EntityKind::Other, "deploy pipeline", 0.9, now)
        .unwrap();
    let b = e
        .add_entity(EntityKind::Other, "rollback runbook", 0.9, now)
        .unwrap();
    e.add_observation(
        a,
        "the deploy pipeline runs migrations before rollout",
        "fact",
        0.9,
        now,
    )
    .unwrap();
    // Deliberately shares no token with the query: this is the document the
    // lexical half cannot reach and the dense half is supposed to.
    e.add_observation(
        b,
        "if the release goes wrong, revert the previous build and page the on-call engineer",
        "fact",
        0.9,
        now,
    )
    .unwrap();

    let lexical = call_tool(
        &e,
        &c,
        1,
        "mem_search",
        json!({"query": "pipeline", "limit": 5, "fusion": "bm25_graph"}),
    );
    let dense = call_tool(
        &e,
        &c,
        2,
        "mem_search",
        json!({"query": "pipeline", "limit": 5, "fusion": "dense_hybrid"}),
    );
    assert!(
        dense["ranking"]
            .as_str()
            .unwrap_or_default()
            .contains("dense_hybrid"),
        "the response must name the dense arm: {dense}"
    );
    assert!(
        !dense["results"].as_array().unwrap().is_empty(),
        "a missing embedder must not empty the result set: {dense}"
    );
    // Degradation is the lexical arm, so it can never return *less* than it did.
    assert!(
        dense["results"].as_array().unwrap().len() >= lexical["results"].as_array().unwrap().len(),
        "dense_hybrid dropped results the lexical arm returned:\nlexical={lexical}\ndense={dense}"
    );
}

/// The dense arm must be *selectable by name*, because an arm that cannot be
/// named cannot be measured, and the arena is the only reason to add one.
#[test]
fn the_dense_arm_is_addressable_and_distinct_from_the_lexical_arms() {
    assert_eq!(Fusion::parse("dense_hybrid"), Some(Fusion::DenseHybrid));
    assert!(Fusion::ALL.contains(&Fusion::DenseHybrid));
    // It reads through the score-ranked lexical path (Bm25Graph's half), which is
    // the measured default — so it must not silently differ from that half.
    assert!(Fusion::DenseHybrid.uses_scores());
    assert!(Fusion::DenseHybrid.uses_idf());
    assert!(!Fusion::DenseHybrid.fuses_graph_inline());
}

/// The causal arm must be addressable, and its difference from its **control** must be
/// expressible as a property. `bm25_graph` is that control because it reads every edge while
/// this arm reads only causal ones — and `bm25_graph` is also the reason this test exists: it
/// scored identical to its own control to the digit on all 500 arena groups, which is what
/// happens when an arm's distinguishing behaviour lives only in placement code.
#[test]
fn the_causal_arm_is_addressable_and_its_control_is_bm25_graph() {
    assert_eq!(Fusion::parse("causal_graph"), Some(Fusion::CausalGraph));
    // `causal` alone too, because the arena's arm selector matches by **substring**: a
    // `--arms causal` sweep has to select one arm, not a family of them.
    assert_eq!(Fusion::parse("causal"), Some(Fusion::CausalGraph));
    assert!(Fusion::ALL.contains(&Fusion::CausalGraph));
    assert!(
        Fusion::ALL.contains(&Fusion::CausalSoft),
        "both causal arms are measured"
    );
    assert_eq!(
        Fusion::CausalGraph.label(),
        "causal_graph",
        "label round-trip"
    );
    assert!(Fusion::CausalGraph.follows_causal_edges());
    // The control reads edges too, and must not claim the causal property.
    assert!(!Fusion::Bm25Graph.follows_causal_edges());
    assert!(!Fusion::GraphReserve.follows_causal_edges());
    // And this arm must not claim `graph_reserve`'s property, or the two arms would differ in
    // two places and a comparison between them would attribute both to causality.
    assert!(!Fusion::CausalGraph.reserves_graph_slots());
    // Weights are ordered and none is zero: a zero weight would make a kind unfollowable while
    // its edge was still written and counted, which is an invisible edge by another route.
    let w = [
        causal_kind_weight(crate::gauntlet::CausalKind::Causes),
        causal_kind_weight(crate::gauntlet::CausalKind::Enables),
        causal_kind_weight(crate::gauntlet::CausalKind::Prevents),
    ];
    assert!(
        w[0] > w[1] && w[1] > w[2],
        "kind weights must be ordered: {w:?}"
    );
    assert!(
        w[2] > 0.0,
        "every kind must be followable, or it is a dead edge"
    );

    // `causal_soft` differs from `causal_graph` in exactly one property, and that
    // property is what keeps a neighbour from tying its own seed. Both halves are
    // asserted: the cap must be *below* 1.0 for every kind, and the two arms must
    // share every other flag or the before/after comparison would measure two
    // changes and attribute both to the cap.
    assert!(Fusion::CausalSoft.caps_causal_inheritance());
    assert!(!Fusion::CausalGraph.caps_causal_inheritance());
    assert_eq!(
        Fusion::CausalSoft.uses_scores(),
        Fusion::CausalGraph.uses_scores()
    );
    assert_eq!(
        Fusion::CausalSoft.uses_idf(),
        Fusion::CausalGraph.uses_idf()
    );
    let s = [
        causal_kind_weight_soft(crate::gauntlet::CausalKind::Causes),
        causal_kind_weight_soft(crate::gauntlet::CausalKind::Enables),
        causal_kind_weight_soft(crate::gauntlet::CausalKind::Prevents),
    ];
    assert!(
        s.iter().all(|v| *v > 0.0 && *v < 1.0),
        "no soft weight may tie or exceed its seed: {s:?}"
    );
    assert!(
        w.iter().zip(s.iter()).all(|(hard, soft)| soft < hard),
        "soft weights must be strictly below the hard ones"
    );
}

/// The preference arm is `bm25_graph` with **one** property added, so every other
/// flag must match its control — otherwise the pair measures two changes and the
/// arena would attribute both to the preference channel.
#[test]
fn the_preference_arm_differs_from_its_control_in_exactly_one_property() {
    assert_eq!(Fusion::parse("preference"), Some(Fusion::PreferenceBoost));
    assert!(Fusion::PreferenceBoost.boosts_preference_docs());
    assert!(
        !Fusion::Bm25Graph.boosts_preference_docs(),
        "the control must not boost"
    );
    assert_eq!(
        Fusion::PreferenceBoost.uses_scores(),
        Fusion::Bm25Graph.uses_scores()
    );
    assert_eq!(
        Fusion::PreferenceBoost.uses_idf(),
        Fusion::Bm25Graph.uses_idf()
    );
    assert_eq!(
        Fusion::PreferenceBoost.fuses_graph_inline(),
        Fusion::Bm25Graph.fuses_graph_inline()
    );
    assert!(!Fusion::PreferenceBoost.has_dense_arm());
}

/// The intent gate is what keeps the channel from trading one class's ranking for
/// another's: a question that is not about preferences must not pay for the marker
/// passes, and `like` inside a simile is the lookalike that motivates phrasing the
/// rule instead of matching the word.
#[test]
fn preference_intent_matches_preference_questions_and_rejects_lookalikes() {
    for yes in [
        "What do I prefer for long flights?",
        "Which is my favourite city to visit?",
        "Would I rather take the train or drive?",
        "Any advice you would recommend for my desk setup?",
        "What is your opinion on my current routine?",
        "What should I change about my morning schedule?",
        "Are there topics I am interested in lately?",
    ] {
        assert!(
            preference_intent(yes),
            "must read as a preference question: {yes}"
        );
    }
    for no in [
        "When did I move to Berlin?",
        "What was the trip like?",
        "How many sessions did I book?",
        "Where is the travel handbook documented?",
        "Who reviewed the pull request?",
    ] {
        assert!(
            !preference_intent(no),
            "must not read as a preference question: {no}"
        );
    }
}

/// Markers are searched as **quoted FTS terms**, so a marker containing a space or a
/// quote would be escaped into a phrase the marker pass can never match — a silent
/// no-op arm. Uppercase would be a second silent no-op for the same reason.
#[test]
fn preference_markers_are_single_lowercase_searchable_tokens() {
    assert!(PREFERENCE_MARKERS.contains(&"prefer"));
    for marker in PREFERENCE_MARKERS {
        assert!(
            !marker.is_empty()
                && !marker.contains(char::is_whitespace)
                && !marker.contains('"')
                && marker.chars().all(|c| c.is_ascii_lowercase()),
            "marker {marker:?} is not a single lowercase searchable token"
        );
    }
    // The bound is what keeps a ranking experiment a ranking experiment.
    assert!((1.0..=4.0).contains(&preference_boost()));
}

/// End-to-end through the shipping dispatch path: the arm names itself on a
/// preference question, stays silent on a factual one, and never *lowers* the
/// marker-bearing document's position — the recall-safe half of the contract.
#[test]
fn the_preference_arm_reorders_only_on_intent_and_only_marker_bearing_documents() {
    let (e, c) = setup();
    let now = memnoth_engine::now_ms();
    let topic = e
        .add_entity(EntityKind::Other, "flight booking policy", 0.9, now)
        .unwrap();
    e.add_observation(
        topic,
        "the flight booking policy documents the window seat allocation in the travel handbook",
        "fact",
        0.9,
        now,
    )
    .unwrap();
    let pref = e
        .add_entity(EntityKind::Other, "travel habits", 0.9, now)
        .unwrap();
    e.add_observation(
        pref,
        "I would rather sit by the window than in the aisle when I fly",
        "fact",
        0.9,
        now,
    )
    .unwrap();

    let question = "What do I prefer on flights, window or aisle?";
    let control = call_tool(
        &e,
        &c,
        1,
        "mem_search",
        json!({"query": question, "limit": 5, "fusion": "bm25_graph"}),
    );
    let boosted = call_tool(
        &e,
        &c,
        2,
        "mem_search",
        json!({"query": question, "limit": 5, "fusion": "preference_boost"}),
    );
    assert!(
        !control["ranking"]
            .as_str()
            .unwrap_or_default()
            .contains("preference boost"),
        "the control arm must not claim the boost: {control}"
    );
    assert!(
        boosted["ranking"]
            .as_str()
            .unwrap_or_default()
            .contains("preference boost"),
        "a preference question with a marker-bearing document must name the boost: {boosted}"
    );
    let position = |v: &Value, name: &str| -> Option<usize> {
        v["results"]
            .as_array()?
            .iter()
            .position(|r| r["name"].as_str() == Some(name))
    };
    let before = position(&control, "travel habits");
    let after = position(&boosted, "travel habits");
    assert!(
        after.is_some(),
        "the marker-bearing document must be returned: {boosted}"
    );
    if let Some(before) = before {
        assert!(
            after.unwrap() <= before,
            "the boost must never lower a marker-bearing document: {before} -> {:?}",
            after
        );
    }
    // A factual question must be byte-identical to the control: the intent gate is
    // the whole reason this arm is not a global re-scorer.
    let neutral = "Where is the travel handbook documented?";
    let neutral_control = call_tool(
        &e,
        &c,
        3,
        "mem_search",
        json!({"query": neutral, "limit": 5, "fusion": "bm25_graph"}),
    );
    let neutral_boost = call_tool(
        &e,
        &c,
        4,
        "mem_search",
        json!({"query": neutral, "limit": 5, "fusion": "preference_boost"}),
    );
    assert!(
        !neutral_boost["ranking"]
            .as_str()
            .unwrap_or_default()
            .contains("preference boost")
    );
    assert_eq!(
        serde_json::to_string(&neutral_control["results"]).unwrap(),
        serde_json::to_string(&neutral_boost["results"]).unwrap(),
        "a factual question must be untouched by the preference arm"
    );
}
