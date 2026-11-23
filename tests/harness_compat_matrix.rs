//! P22-016/025 — focused checks for the harness compatibility manifest.
//!
//! The module is included by path so this test needs no `lib.rs` wiring; the
//! exact one-line wiring diff is reported separately for the parent session.

#[path = "../src/harness_compat_matrix.rs"]
mod harness_compat_matrix;

use harness_compat_matrix as matrix;
use serde_json::Value;

fn manifest() -> Value {
    matrix::load().expect("the embedded harness-compat manifest must parse")
}

fn clients(manifest: &Value) -> &serde_json::Map<String, Value> {
    manifest
        .get("clients")
        .and_then(Value::as_object)
        .expect("clients must be an object")
}

#[test]
fn manifest_is_complete_and_source_derived() {
    let manifest = manifest();
    matrix::validate(&manifest).expect("manifest must pass full validation");

    assert_eq!(manifest["schema"], matrix::SCHEMA);
    assert_eq!(manifest["generated"], false);

    let all = clients(&manifest);
    for name in matrix::REQUIRED_CLIENTS {
        let client = &all[*name];
        for field in [
            "display_name",
            "status",
            "config_path",
            "config_format",
            "cli_managed_command",
            "profile",
            "handshake_protocol_version",
            "handshake_source",
            "discovered_tools",
            "safe_retrieval",
            "expected_denial",
            "verification",
            "last_seen",
        ] {
            assert!(
                client.get(field).is_some(),
                "{name} is missing required field {field}"
            );
        }
    }

    // Codex and OpenCode are the P22-016 first wave; Claude/Cursor stay preview.
    for name in ["codex", "opencode"] {
        let command = all[name]["cli_managed_command"]
            .as_str()
            .expect("cli-managed registration command");
        assert!(
            command.contains("--local"),
            "{name}: expected a local registration"
        );
        assert!(
            command.contains(name),
            "{name}: command names a different client"
        );
    }
}

#[test]
fn unknown_fields_are_null_with_a_reason() {
    let manifest = manifest();
    matrix::validate_unknowns(&manifest).expect("every null must be explained");

    let unknowns = manifest["unknowns"].as_object().expect("unknowns object");
    assert!(
        unknowns["health_semantics.live_health"]
            .as_str()
            .is_some_and(|reason| reason.contains("health")),
        "live_health null needs an explicit reason"
    );
    assert!(
        unknowns["clients.codex.handshake_protocol_version"]
            .as_str()
            .is_some_and(|reason| reason.contains("P22-025")),
        "Codex handshake null should point at the open runtime transcript"
    );
    assert!(
        unknowns["clients.cursor.registration_command"]
            .as_str()
            .is_some_and(|reason| reason.contains("no `mcp add`")),
        "Cursor's null command needs the no-host-CLI reason"
    );

    // Nothing under `observed` or `last_seen` is invented from documentation.
    for (name, client) in clients(&manifest) {
        assert!(
            client["last_seen"].is_null(),
            "{name}: last_seen must be null"
        );
        assert!(
            client["discovered_tools"]["observed"].is_null(),
            "{name}: discovered tools were not observed"
        );
        assert!(
            client["safe_retrieval"]["observed"].is_null(),
            "{name}: safe retrieval was not executed"
        );
        assert!(
            client["expected_denial"]["observed"].is_null(),
            "{name}: expected denial was not executed"
        );
        // A client-level reason covers these grouped observations.
        assert!(
            unknowns.contains_key(&format!("clients.{name}")),
            "{name}: grouped observation nulls need a reason"
        );
    }
}

#[test]
fn per_client_verification_is_recorded_separately() {
    let manifest = manifest();
    let all = clients(&manifest);

    // No client may borrow another client's verification claim.
    let mut seen = std::collections::BTreeSet::new();
    for name in matrix::REQUIRED_CLIENTS {
        let client = &all[*name];
        let verification = client["verification"]
            .as_object()
            .unwrap_or_else(|| panic!("{name} must carry its own verification object"));
        assert_eq!(verification["status"], "not-live-verified", "{name}");
        assert!(verification["client_version"].is_null(), "{name}");
        assert!(verification["server_version"].is_null(), "{name}");
        assert!(verification["transcript"].is_null(), "{name}");
        assert!(
            seen.insert((*name).to_owned()),
            "duplicate client entry {name}"
        );

        let profile = client["profile"].as_str().expect("profile string");
        let known = manifest["server"]["profiles"]["tool_counts"]
            .get(profile)
            .is_some();
        assert!(known, "{name}: unknown profile {profile}");
    }
}

#[test]
fn expected_denial_is_present_for_every_client() {
    let manifest = manifest();
    let all = clients(&manifest);
    let names = manifest["server"]["profiles"]["memory_tools"]
        .as_array()
        .expect("memory tool list");

    for name in matrix::REQUIRED_CLIENTS {
        let client = &all[*name];
        let denial = &client["expected_denial"];
        assert_eq!(
            denial["expect_is_error"], true,
            "{name}: denial must be an error"
        );
        let tool = denial["tool"].as_str().expect("denial tool name");
        assert_eq!(tool, "mem_add_fact", "{name}: canonical denial probe");
        assert!(names.iter().any(|entry| entry.as_str() == Some(tool)));
        assert!(
            denial["precondition"]
                .as_str()
                .is_some_and(|text| text.contains("READ_ONLY")),
            "{name}: denial needs its read-only precondition"
        );
        let contains = denial["expect_text_contains"]
            .as_array()
            .expect("denial text assertions");
        assert!(
            contains
                .iter()
                .any(|entry| entry.as_str() == Some("read-only"))
        );
        assert!(
            contains
                .iter()
                .any(|entry| entry.as_str() == Some("--allow-writes"))
        );

        let safe = &client["safe_retrieval"];
        let safe_tool = safe["tool"].as_str().expect("safe tool name");
        assert_eq!(safe_tool, "mem_search", "{name}: canonical safe retrieval");
        assert_ne!(
            safe_tool, tool,
            "{name}: safe retrieval and denial must differ"
        );
        assert!(names.iter().any(|entry| entry.as_str() == Some(safe_tool)));
        assert!(
            safe["arguments"].is_object(),
            "{name}: safe probe arguments"
        );
    }
}

#[test]
fn last_seen_is_explicitly_not_live_health() {
    let manifest = manifest();
    assert_eq!(manifest["last_seen != live_health"], true);
    assert!(
        manifest["health_semantics"]["assertion"]
            .as_str()
            .is_some_and(|text| text.contains("not a live health probe"))
    );
    assert!(manifest["health_semantics"]["live_health"].is_null());
    assert!(
        clients(&manifest)
            .values()
            .all(|client| client["last_seen"].is_null())
    );
}
