//! The parity gates for `memnoth mcp serve`.
//!
//! The server used to advertise its own list and the runtime its own eleven
//! tools, and nothing compared them. These tests are that comparison: the shared
//! table decides membership, a ported body replaces its definition instead of
//! shadowing it, and `memnoth mcp call` reaches the same dispatcher a host does.

use super::*;

/// A vault path in its own directory. `memnoth-mcp` has no `tempfile` dev-dependency, and
/// adding one for four tests is a dependency the product would carry for nothing, so the
/// directory is unique-by-counter and simply left behind, like the rest of this suite.
fn temp_vault() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "memnoth-mcp-call-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("vault.db")
}

/// Names the server advertises but no profile table owns.
///
/// The agent family is transport-gated by `agent::AGENT_TOOLS` rather than by
/// `TOOL_PROFILES`, because those tools only exist on the HTTP transport.
fn agent_family() -> Vec<String> {
    crate::agent::AGENT_TOOLS
        .iter()
        .map(|n| (*n).to_string())
        .collect()
}

#[test]
fn every_advertised_tool_is_in_the_shared_table() {
    let agent = agent_family();
    let declared = crate::declared_names();
    assert!(!declared.is_empty(), "the definition tables are empty");

    for name in &declared {
        if agent.contains(name) {
            continue;
        }
        assert!(
            memnoth_capability::TOOL_PROFILES
                .iter()
                .any(|(row, _)| row == name)
                || memnoth_capability::RUNTIME_TOOL_PROFILES
                    .iter()
                    .any(|(row, _)| row == name),
            "`{name}` is advertised by the server but has no row in the shared \
             exposure table — it would be reachable from `full` only, silently"
        );
    }
}

#[test]
fn every_table_row_has_a_body_on_some_surface() {
    let registry = memnoth_capability::Registry::with_builtins();
    let ported: Vec<&str> = registry.names();
    let declared = crate::declared_names();

    for (name, profiles) in memnoth_capability::TOOL_PROFILES {
        let served = declared.iter().any(|d| d == name);
        let ran = ported.contains(name);
        assert!(
            served || ran,
            "`{name}` ({profiles:?}) is exposed by the table but neither the server \
             nor the capability registry can run it"
        );
    }
}

#[test]
fn a_ported_tool_is_callable_with_the_arguments_either_side_declares() {
    // The registry and the server both define the three ported tools today, and the
    // registry will replace the server's definition when the body moves. What must not
    // wait for that move is the *contract*: a host that reads one side's `tools/list`
    // and calls the other must not get a rejected call.
    let registry = memnoth_capability::Registry::with_builtins();
    let declared: Vec<Value> = crate::tool_definitions()
        .into_iter()
        .chain(crate::tools_agent::tool_definitions())
        .collect();

    for name in registry.names() {
        let Some(server) = declared
            .iter()
            .find(|definition| definition["name"] == json!(name))
        else {
            continue;
        };
        let ported = memnoth_capability::Registry::with_builtins()
            .definitions(memnoth_capability::Profile::Full)
            .into_iter()
            .find(|definition| definition["name"] == json!(name))
            .expect("the registry just named it");
        assert_eq!(
            server["inputSchema"]["type"], ported["inputSchema"]["type"],
            "`{name}` disagrees about being an object"
        );
        let required = |definition: &Value| -> Vec<String> {
            definition["inputSchema"]["required"]
                .as_array()
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(
            required(server),
            required(&ported),
            "`{name}` requires different arguments on the two surfaces"
        );
    }
}

#[test]
fn a_narrow_profile_is_at_least_sixty_percent_smaller_than_full() {
    let bytes = |profile: Profile| -> usize {
        crate::visible_tools(profile)
            .iter()
            .map(|tool| tool.to_string().len())
            .sum()
    };
    let full = bytes(Profile::Full);
    let micro = bytes(Profile::Micro);
    assert!(full > 0 && micro > 0);
    // The gate in `docs/IMPLEMENTATION-PHASES-2026-10-24.md` §B is a 60% drop.
    assert!(
        micro * 100 <= full * 40,
        "micro is {micro} bytes against full's {full}: under a 60% saving, so the \
         profile is not buying the context it claims"
    );
}

#[test]
fn call_reaches_the_servers_own_dispatcher() {
    let db = temp_vault();
    // `now` is a server-family tool: the runtime router does not own it, so this
    // only passes if `mcp call` goes through the dispatcher rather than the
    // eleven-tool router.
    let value = crate::call(&db, memnoth_capability::Profile::Code, "now", &json!({}))
        .expect("the server's own dispatcher answers `now`");
    assert!(
        value.get("epoch_ms").is_some() || value.get("now_ms").is_some(),
        "unexpected `now` payload: {value}"
    );
}

#[test]
fn call_applies_the_profile_gate_and_names_it() {
    let db = temp_vault();
    let error = crate::call(
        &db,
        memnoth_capability::Profile::Memory,
        "git_status",
        &json!({}),
    )
    .expect_err("git_status is not in the memory profile");
    let message = error.to_string();
    assert!(
        message.contains("git_status") && message.contains("memory"),
        "the refusal must name the tool and the profile: {message}"
    );
}

#[test]
fn call_refuses_an_unknown_tool_without_panicking() {
    let db = temp_vault();
    let error = crate::call(
        &db,
        memnoth_capability::Profile::Full,
        "fs_purge_everything",
        &json!({}),
    )
    .expect_err("there is no such tool");
    assert!(!error.to_string().is_empty());
}

#[test]
fn call_rejects_the_agent_profile_with_a_remedy() {
    let db = temp_vault();
    let error = crate::call(
        &db,
        memnoth_capability::Profile::Agent,
        "agent_session_new",
        &json!({ "cwd": "." }),
    )
    .expect_err("agent tools need the HTTP bridge");
    let message = error.to_string();
    assert!(message.contains("HTTP"), "{message}");
}
