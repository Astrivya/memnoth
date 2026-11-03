//! M3 compat aliases — the zero-reconfig migration wedge.
//!
//! A host already configured for a rival memory server calls `retain`/`recall`
//! (Hindsight) or `create_entities`/`search_nodes` (the official memory server).
//! Point that host at memnoth and every call fails with "unknown tool", which is
//! the moment adoption dies. These aliases resolve those names to the native
//! tool before the profile gate, so the host keeps working without a config
//! rewrite.
//!
//! Two deliberate restrictions:
//!
//! * Aliases are resolved **only in the full profile** (see the call site), so a
//!   narrow profile's `tools/list` is never polluted with rival vocabulary —
//!   the surface stays ours, the wedge stays available.
//! * Only names whose native target is unambiguous are mapped. An unmapped rival
//!   name fails loudly with the ordinary profile error, which is better than
//!   silently routing `forget_everything` to `mem_search`.
#![allow(dead_code)] // M3 surface: the alias table is consumed through resolve().

use serde_json::{Value, json};

/// Native tools this module is allowed to route to. `alias_targets_exist`
/// fences the table below against a rename: a target that leaves this list must
/// fail a test, not a host.
pub const NATIVE_TARGETS: &[&str] = &["mem_ingest", "mem_search", "mem_add_fact"];

/// `(rival name, native name)`. Sorted by rival name for a stable dump.
const ALIASES: &[(&str, &str)] = &[
    ("add_memory", "mem_add_fact"),      // mem0: add
    ("create_entities", "mem_add_fact"), // official memory server
    ("recall", "mem_search"),            // Hindsight
    ("retain", "mem_ingest"),            // Hindsight
    ("search_memory", "mem_search"),     // mem0: search
    ("search_nodes", "mem_search"),      // official memory server
];

/// The alias table, for the manifest and docs. Deterministic order.
pub fn aliases() -> &'static [(&'static str, &'static str)] {
    ALIASES
}

/// Resolve one rival tool name to the native tool that serves it.
pub fn resolve(name: &str) -> Option<&'static str> {
    ALIASES
        .iter()
        .find(|(rival, _)| *rival == name)
        .map(|(_, native)| *native)
}

/// The alias table as JSON — the shape the manifest publishes so a host can
/// discover the wedge instead of reading source.
pub fn alias_table_json() -> Value {
    json!({
        "aliases": ALIASES
            .iter()
            .map(|(rival, native)| json!({ "rival": rival, "native": native }))
            .collect::<Vec<_>>(),
        "resolvedInProfile": "full",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rival_names_resolve_to_native_tools() {
        assert_eq!(resolve("retain"), Some("mem_ingest"));
        assert_eq!(resolve("recall"), Some("mem_search"));
        assert_eq!(resolve("create_entities"), Some("mem_add_fact"));
        assert_eq!(resolve("search_nodes"), Some("mem_search"));
    }

    #[test]
    fn native_names_and_junk_are_not_aliased() {
        // a native name must pass through untouched, never be rewritten
        for native in ["mem_search", "mem_ingest", "mem_add_fact", "kg_index"] {
            assert_eq!(resolve(native), None, "{native} must not be aliased");
        }
        assert_eq!(resolve("forget_everything"), None);
        assert_eq!(resolve(""), None);
    }

    #[test]
    fn alias_targets_exist_in_the_native_surface() {
        for (rival, native) in aliases() {
            assert!(
                NATIVE_TARGETS.contains(native),
                "alias {rival} → {native} does not target a known native tool"
            );
        }
    }

    #[test]
    fn table_is_sorted_and_has_no_duplicate_rivals() {
        let mut rivals: Vec<&str> = aliases().iter().map(|(r, _)| *r).collect();
        let sorted = {
            let mut s = rivals.clone();
            s.sort_unstable();
            s
        };
        assert_eq!(
            rivals, sorted,
            "alias table must be sorted for a stable dump"
        );
        rivals.dedup();
        assert_eq!(
            rivals.len(),
            aliases().len(),
            "duplicate rival name in table"
        );
    }

    #[test]
    fn alias_table_json_is_stable() {
        let a = alias_table_json();
        let b = alias_table_json();
        assert_eq!(a, b);
        assert_eq!(a["resolvedInProfile"], "full");
        assert_eq!(a["aliases"].as_array().unwrap().len(), aliases().len());
    }
}
