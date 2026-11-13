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

pub const KG_PRIMARY: [&str; 11] = [
    "kg_index",
    "kg_search",
    "kg_def",
    "kg_refs",
    "kg_impact",
    "kg_trace",
    "kg_explore",
    "kg_status",
    "kg_changes",
    "kg_callees",
    "kg_dead",
];

/// The `kg_*` tools this profile can reach only through `kg_more`.
///
/// Read from the live catalogue rather than a second list: a tool added to
/// `catalog.rs` appears here the moment it exists, which is what keeps the cap
/// from silently becoming a deletion.
pub fn kg_more_names(profile: Profile) -> Vec<String> {
    let mut names: Vec<String> = crate::tool_definitions()
        .iter()
        .filter_map(|definition| definition["name"].as_str().map(str::to_owned))
        .filter(|name| name.starts_with("kg_") && !KG_PRIMARY.contains(&name.as_str()))
        .filter(|name| profile.allows(name))
        .collect();
    names.sort();
    names
}

/// The tools this profile reaches only through `kg_more` — empty when the
/// profile's KG set already fits.
///
/// One function answers both "what is hidden" and "is the hatch advertised",
/// because two answers computed separately is how a host ends up told there is
/// a hatch with nothing behind it.
pub fn hidden_kg_tools(profile: Profile) -> Vec<String> {
    let permitted = crate::tool_definitions()
        .iter()
        .filter_map(|definition| definition["name"].as_str())
        .filter(|name| name.starts_with("kg_") && profile.allows(name))
        .count();
    if permitted <= 12 {
        return Vec::new();
    }
    kg_more_names(profile)
}

/// The `kg_more` definition, carrying the names it can reach.
///
/// The list is in the description rather than hidden behind a first call: a
/// model that has to probe to learn what exists pays a round trip for something
/// this server already knows.
pub fn kg_more_definition(hidden: &[String]) -> Value {
    let listed = hidden.join(", ");
    crate::tool(
        "kg_more",
        &format!(
            "Reach the code-graph tools that are not advertised directly: {listed}.              Call with no `tool` to list them again. `args` are that tool's own arguments,              e.g. {{\"tool\":\"kg_coverage\",\"args\":{{\"workspace\":\"/repo\"}}}}."
        ),
        &[
            crate::opt(
                "tool",
                "string",
                "The kg_* tool to run; omit to list the reachable names",
            ),
            crate::opt(
                "args",
                "object",
                "That tool's arguments, exactly as you would pass them directly",
            ),
        ],
        crate::READ_ONLY,
    )
}

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
/// The KG tools a profile advertises directly.
///
/// The family is nineteen verbs; a host pays for every schema in its prompt on
/// every turn, and eight of them are questions an agent asks once a session at
/// most. These eleven stay first-class — the ones an agent reaches for without
/// knowing the repository — and `kg_more` reaches the rest by name. The cap is
/// the plan's W7-13 limit and `every_profile_advertises_at_most_twelve_kg_tools`
/// holds it; adding a twelfth primary means moving one out, not growing the list.
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
    // The KG cap: everything outside the primary eleven is reached by name
    // through `kg_more`, and the escape hatch is advertised only when it has
    // something to reach.
    // A narrow profile's kg set is curated to fit already, so the cap only
    // applies where the family actually overflows: trimming `memory` would take
    // away the tools that profile exists to check its own index with.
    let hidden = hidden_kg_tools(profile);
    if !hidden.is_empty() {
        tools.retain(|tool| {
            tool["name"]
                .as_str()
                .map(|name| !name.starts_with("kg_") || KG_PRIMARY.contains(&name))
                .unwrap_or(true)
        });
        if !hidden.is_empty() {
            tools.push(kg_more_definition(&hidden));
        }
        // Re-sorted after the push: `tools/list` must stay alphabetical or a
        // host's prompt cache misses on every insertion.
        tools.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["name"].as_str().unwrap_or_default())
        });
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
