//! What `tools/list` advertises, for one profile.
//!
//! Two rules live here and nowhere else:
//!
//! * **Membership is the shared table's decision.** `Profile::allows` reads
//!   `memnoth_capability::TOOL_PROFILES`, so the server and the runtime cannot
//!   disagree about which names a profile carries.
//! * **The list is stable.** Sorted by name, not by definition order, because a
//!   list that reorders when a row is inserted is a list a host cannot cache.
//!
//! Definitions still come from this crate for the tools the capability registry
//! has not ported yet — the registry answers *who may see this name*, the table
//! below answers *what the schema is*. When a tool is ported, its body moves and
//! the name stops appearing here; the parity test fails if the two lists overlap
//! or if a table row has no definition anywhere.

use serde_json::Value;

use crate::Profile;

/// The tools a profile advertises: the base definitions **and** the
/// agent-utility family, filtered by [`Profile::allows`] (which reads
/// `TOOL_PROFILES`), plus the agent-session family when the profile is
/// `Agent`.
///
/// Note what is gone: the agent-utility family used to be added under
/// `if matches!(profile, Profile::Full)`, so `now` / `think` / `fs_*` /
/// `git_*` / `vault_*` were unreachable from every narrow profile — the reason
/// `Profile::Memory` could not reach the temporal quartet. It is now filtered
/// like every other tool, and Full's surface is derived rather than
/// special-cased.
pub fn visible_tools(profile: Profile) -> Vec<Value> {
    let mut tools: Vec<Value> = crate::tool_definitions()
        .into_iter()
        .chain(crate::tools_agent::tool_definitions())
        .filter(|t| {
            t["name"]
                .as_str()
                .map(|n| profile.allows(n))
                .unwrap_or(false)
        })
        .collect();
    // Deterministic order (spec: servers SHOULD, so client caches and the
    // host's own prompt cache see a stable prefix). Sorted by name rather than
    // left in definition order: the table is edited often, and a stable prefix
    // must not depend on where a new row was inserted.
    tools.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["name"].as_str().unwrap_or_default())
    });
    // The agent-session family is transport-gated (HTTP + `AgentBridge`), so it
    // is added for the Agent profile only — never for Full, which also serves
    // stdio. Its membership list lives in `agent::AGENT_TOOLS`, and
    // `tests::profile_membership_is_a_pure_function_of_metadata` keeps that
    // list and the table from drifting apart.
    if profile == Profile::Agent {
        tools.extend(crate::agent_tool_definitions());
    }
    tools
}

/// Public composition seam (strategy §4 lane split): build a filtered
/// toolset for an embedding host — e.g. the workspace registry — WITHOUT
/// reaching into server internals.
///
/// The result is the intersection of the profile's visible tools and
/// `allowed_tool_ids`. Ids that the profile does not expose (or that do not
/// exist) are ignored, and an empty allow list yields an empty toolset, so a
/// host can only ever narrow what the profile already permits. Tool
/// definitions carry full input schemas and annotations, ready to be served
/// as a `tools/list` payload.
pub fn compose_toolset(profile: Profile, allowed_tool_ids: &[&str]) -> Vec<Value> {
    visible_tools(profile)
        .into_iter()
        .filter(|t| {
            t["name"]
                .as_str()
                .map(|n| allowed_tool_ids.contains(&n))
                .unwrap_or(false)
        })
        .collect()
}

/// Every tool name this crate can run, sorted and de-duplicated.
///
/// This is the definition half of the parity rule: a name in
/// `TOOL_PROFILES` with no definition here and no body in the capability
/// registry is a tool the server advertises and cannot answer, which is the
/// exact drift the shared table exists to prevent. Names are owned strings
/// because a definition is built at runtime from its schema.
pub fn declared_names() -> Vec<String> {
    let mut names: Vec<String> = crate::tool_definitions()
        .iter()
        .chain(crate::tools_agent::tool_definitions().iter())
        .filter_map(|definition| definition["name"].as_str().map(str::to_string))
        .collect();
    names.extend(crate::agent::AGENT_TOOLS.iter().map(|n| (*n).to_string()));
    names.sort();
    names.dedup();
    names
}
