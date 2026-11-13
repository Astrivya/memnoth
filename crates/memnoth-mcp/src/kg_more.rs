//! `kg_more`: the door to the code-graph tools that are not advertised.
//!
//! The KG family is nineteen verbs and only eleven are first-class in
//! `tools/list` (see [`crate::visible::KG_PRIMARY`]). The rest are still
//! permitted — the capability table never stopped carrying them — so this
//! module is a *projection*, not a second gate: it names the reachable tools and
//! dispatches a call straight back into `tools_call`, which re-runs the profile
//! gate, the read-only check, the policy, the approval path and the hooks for
//! the inner tool exactly as if it had been called directly.
//!
//! Dispatching by recursion is the point. A shortcut that called
//! `kg_tools::call` directly would let `kg_more` reach a tool that the policy
//! denies, or one that a host asked to be read-only, and the difference would
//! only show up in production.

use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};

use crate::{Profile, visible};

/// What a `kg_more` call asks for.
///
/// `Ok(None)` is the listing call: no `tool` was named. A named tool has to be
/// one this profile can reach only through here — naming a primary tool is an
/// error rather than a silent success, because a model that learned the wrong
/// spelling should be told which spelling is right.
pub(crate) fn resolve(requested: Option<&str>, profile: Profile) -> Result<Option<String>> {
    let hidden = visible::kg_more_names(profile);
    match requested.map(str::trim).filter(|tool| !tool.is_empty()) {
        None => Ok(None),
        Some(tool) if hidden.iter().any(|name| name == tool) => Ok(Some(tool.to_string())),
        Some("kg_more") => anyhow::bail!(
            "kg_more does not call itself; name one of: {}",
            hidden.join(", ")
        ),
        Some(tool) if visible::KG_PRIMARY.contains(&tool) => anyhow::bail!(
            "`{tool}` is advertised directly — call it as a tool, not through kg_more"
        ),
        Some(tool) => anyhow::bail!(
            "`{tool}` is not a code-graph tool this profile can reach; kg_more carries: {}",
            hidden.join(", ")
        ),
    }
}

/// Answer a `kg_more` call: list, or run the named tool through the real gate.
pub(crate) fn call(
    engine: &crate::MemoryEngine,
    coord: &crate::Coord,
    profile: Profile,
    args: &Value,
    principal: Option<&str>,
    vault: Option<&Path>,
) -> Result<Value> {
    let requested = args.get("tool").and_then(Value::as_str);
    let Some(tool) = resolve(requested, profile)? else {
        let names = visible::kg_more_names(profile);
        return Ok(json!({
            "tools": names,
            "note": "call kg_more with {\"tool\": \"<name>\", \"args\": {..}} to run one; the primary kg_* tools are advertised directly",
        }));
    };
    let inner = args.get("args").cloned().unwrap_or_else(|| json!({}));
    crate::tools_call(
        engine,
        coord,
        profile,
        &json!({"name": tool, "arguments": inner}),
        principal,
        vault,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listing_call_names_every_reachable_tool() {
        let names = visible::kg_more_names(Profile::Full);
        assert!(
            names.iter().any(|name| name == "kg_coverage"),
            "a hidden tool must be reachable: {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "kg_search"),
            "an advertised tool must not be listed twice: {names:?}"
        );
    }

    #[test]
    fn every_profile_advertises_at_most_twelve_kg_tools() {
        // W7-13's cap: eleven primaries plus the escape hatch. The count is
        // read from the same list the server serves, so this cannot pass while
        // the wire carries more.
        for profile in [
            Profile::Micro,
            Profile::Memory,
            Profile::Coord,
            Profile::Code,
            Profile::Full,
        ] {
            let advertised = visible::visible_tools(profile)
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .filter(|name| name.starts_with("kg_"))
                .count();
            assert!(
                advertised <= 12,
                "{profile:?} advertises {advertised} kg tools, over the cap of 12"
            );
        }
    }

    #[test]
    fn the_escape_hatch_is_advertised_only_when_it_has_something_to_reach() {
        let full: Vec<String> = visible::visible_tools(Profile::Full)
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect();
        assert!(full.iter().any(|name| name == "kg_more"));

        // A profile that carries no kg tool at all must not gain one just
        // because the hatch could exist.
        assert!(
            !visible::kg_more_names(Profile::Micro)
                .iter()
                .any(|name| name == "kg_more"),
            "the hatch is never in its own list"
        );
    }

    #[test]
    fn a_primary_tool_is_redirected_instead_of_run_twice() {
        let error = resolve(Some("kg_search"), Profile::Full)
            .expect_err("a primary tool does not need the hatch");
        assert!(error.to_string().contains("advertised directly"), "{error}");
    }

    #[test]
    fn a_name_the_hatch_cannot_reach_is_refused_with_the_list() {
        let error = resolve(Some("kg_nonesuch"), Profile::Full)
            .expect_err("an unknown tool must be refused");
        let message = error.to_string();
        assert!(
            message.contains("kg_coverage"),
            "the refusal lists what works: {message}"
        );

        let recursion = resolve(Some("kg_more"), Profile::Full).expect_err("no recursion");
        assert!(recursion.to_string().contains("does not call itself"));
    }

    #[test]
    fn a_named_hidden_tool_resolves() {
        assert_eq!(
            resolve(Some("kg_doctor"), Profile::Full).unwrap(),
            Some("kg_doctor".to_string())
        );
        assert_eq!(resolve(None, Profile::Full).unwrap(), None);
        assert_eq!(resolve(Some("   "), Profile::Full).unwrap(), None);
    }
}
