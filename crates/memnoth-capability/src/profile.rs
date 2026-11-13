//! Profiles: which tools a surface exposes, in one table.
//!
//! A profile is a token lever and a permission lever (see `docs/DECISIONS.md`
//! D7). Forty-eight tool schemas cost roughly six times the bytes of four on
//! every turn, whether the model calls them or not, so narrow profiles are the
//! cheapest saving available — and a narrower profile is also a smaller blast
//! radius for a turn that goes wrong.
//!
//! Membership lives in [`TOOL_PROFILES`], one row per tool. A tool missing from
//! the table is reachable only from [`Profile::Full`]; the crate's tests fail on
//! that, because "silently Full-only" is how the two registries drifted apart in
//! the first place.

/// A named tool set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Profile {
    /// Four-ish tools for a chat or a small edit.
    Micro,
    /// The temporal vault: recall, as-of, contradictions.
    Memory,
    /// Code navigation: graph, fs, git.
    Code,
    /// Multi-agent coordination.
    Coord,
    /// Everything.
    Full,
    /// Session management for the agent-mode HTTP transport.
    Agent,
}

impl Profile {
    /// The name used in `MEMNOTH_TOOL_PROFILE` and in receipts.
    pub fn name(self) -> &'static str {
        match self {
            Self::Micro => "micro",
            Self::Memory => "memory",
            Self::Code => "code",
            Self::Coord => "coord",
            Self::Full => "full",
            Self::Agent => "agent",
        }
    }

    /// Parse a profile name. Unknown names return `None` so a typo cannot widen
    /// exposure by silently falling back to `full`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "micro" => Some(Self::Micro),
            "memory" => Some(Self::Memory),
            "code" => Some(Self::Code),
            "coord" => Some(Self::Coord),
            "full" => Some(Self::Full),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }

    /// Whether this profile exposes `tool`.
    ///
    /// `Full` means all, deliberately without an allowlist: a tool that has to
    /// be listed twice is a tool that will be forgotten once.
    ///
    /// Both surfaces' tables are consulted, because the gate answers "may this
    /// profile see this name" and the name is the only thing a caller has. The
    /// tables stay disjoint — a test enforces it — so consulting both cannot
    /// mean two different answers for one tool.
    pub fn allows(self, tool: &str) -> bool {
        self == Self::Full
            || TOOL_PROFILES
                .iter()
                .chain(RUNTIME_TOOL_PROFILES.iter())
                .any(|(name, profiles)| *name == tool && profiles.contains(&self))
    }

    /// Every tool this profile exposes, in table order.
    pub fn tool_names(self) -> Vec<&'static str> {
        TOOL_PROFILES
            .iter()
            .filter(|(_, profiles)| self == Self::Full || profiles.contains(&self))
            .map(|(name, _)| *name)
            .collect()
    }

    /// Every tool the *runtime router* exposes under this profile, in table
    /// order. What a turn offers the model, as opposed to what the server lists.
    pub fn runtime_tool_names(self) -> Vec<&'static str> {
        RUNTIME_TOOL_PROFILES
            .iter()
            .filter(|(_, profiles)| self == Self::Full || profiles.contains(&self))
            .map(|(name, _)| *name)
            .collect()
    }
}

/// The exposure table: one row per tool, once.
///
/// It grows as tools are ported from the MCP server. A row may name a tool that
/// is not implemented here yet — the table answers "who is allowed to see this",
/// and the registry answers "can this build run it"; keeping them separate is
/// what lets the server adopt the table before adopting every body.
pub const TOOL_PROFILES: &[(&str, &[Profile])] = &[
    // ── vault: the memory surface ───────────────────────────────────────
    (
        "mem_search",
        &[Profile::Micro, Profile::Memory, Profile::Full],
    ),
    (
        "mem_find_facts",
        &[Profile::Micro, Profile::Memory, Profile::Full],
    ),
    (
        "mem_add_fact",
        &[Profile::Micro, Profile::Memory, Profile::Full],
    ),
    ("mem_related", &[Profile::Memory, Profile::Full]),
    ("mem_stats", &[Profile::Memory, Profile::Full]),
    ("mem_ingest", &[Profile::Memory, Profile::Full]),
    // Vault hygiene: the finder half of dedup, which every memory rival ships
    // (cognee/mempalace/hindsight all carry dedup+consolidation) and we did not.
    ("mem_duplicates", &[Profile::Memory, Profile::Full]),
    ("mem_merge", &[Profile::Memory, Profile::Full]),
    // The **write** half of the temporal vocabulary. Reads of the journal existed
    // from the start while nothing could write to it: a host could see that a
    // belief had been superseded and could not supersede one. Every rival's
    // "temporal" surface is read-only for the same reason, so this pair is the
    // part of the moat that is actually defensible.
    ("mem_supersede", &[Profile::Memory, Profile::Full]),
    ("mem_forget", &[Profile::Memory, Profile::Full]),
    // The moat. `vault_asof` answers "what did we believe at time T";
    // `vault_contradictions` answers "what changed its mind, and why".
    // Deleting a row below this line means arguing with the docstrings quoted
    // above — not with a test.
    ("vault_history", &[Profile::Memory, Profile::Full]),
    ("vault_asof", &[Profile::Memory, Profile::Full]),
    ("vault_contradictions", &[Profile::Memory, Profile::Full]),
    ("vault_top", &[Profile::Memory, Profile::Full]),
    // The clock: a temporal profile that cannot tell the time is half a moat.
    ("now", &[Profile::Memory, Profile::Code, Profile::Full]),
    // ── index health — shared: a memory host must be able to check whether
    //    its own index is stale, and the code host needs the same verbs ────
    ("kg_index", &[Profile::Memory, Profile::Code, Profile::Full]),
    (
        "kg_status",
        &[Profile::Memory, Profile::Code, Profile::Full],
    ),
    (
        "kg_coverage",
        &[Profile::Memory, Profile::Code, Profile::Full],
    ),
    (
        "kg_unresolved",
        &[Profile::Memory, Profile::Code, Profile::Full],
    ),
    (
        "kg_search",
        &[Profile::Memory, Profile::Code, Profile::Full],
    ),
    (
        "kg_changes",
        &[Profile::Memory, Profile::Code, Profile::Full],
    ),
    // Code-only on purpose: a memory host has `kg_status`/`kg_coverage` for
    // staleness, and the `memory` profile has a hard 21-tool ceiling
    // (`memory_profile_stays_within_a_defensible_size`). `kg_doctor` is a
    // *code-graph* verdict — parse failures, unresolved call sites, semantic
    // state — so it belongs to the profile that owns the code graph.
    ("kg_doctor", &[Profile::Code, Profile::Full]),
    // ── code navigation — the `code` profile ───────────────────────────
    ("kg_explore", &[Profile::Code, Profile::Full]),
    ("kg_communities", &[Profile::Code, Profile::Full]),
    ("kg_impact", &[Profile::Code, Profile::Full]),
    ("kg_trace", &[Profile::Code, Profile::Full]),
    // The three the agent used to be half-blind without: `kg_impact` answers
    // "who calls this", and editing a signature also needs what it calls
    // (`kg_callees`), the declaration itself (`kg_def`) and the mention list
    // (`kg_refs`). Code-only, like the impact walk they mirror: a memory host
    // has no declaration bodies to read.
    ("kg_callees", &[Profile::Code, Profile::Full]),
    ("kg_def", &[Profile::Code, Profile::Full]),
    ("kg_refs", &[Profile::Code, Profile::Full]),
    ("kg_why", &[Profile::Code, Profile::Full]),
    ("kg_dead", &[Profile::Code, Profile::Full]),
    ("kg_complexity", &[Profile::Code, Profile::Full]),
    ("kg_services", &[Profile::Code, Profile::Full]),
    ("kg_risk", &[Profile::Code, Profile::Full]),
    ("fs_list", &[Profile::Code, Profile::Full]),
    ("fs_read", &[Profile::Code, Profile::Full]),
    ("git_status", &[Profile::Code, Profile::Full]),
    ("git_diff", &[Profile::Code, Profile::Full]),
    ("git_log", &[Profile::Code, Profile::Full]),
    ("think", &[Profile::Code, Profile::Full]),
    // ── coordination ───────────────────────────────────────────────────
    ("coord_join", &[Profile::Coord, Profile::Full]),
    ("coord_heartbeat", &[Profile::Coord, Profile::Full]),
    ("coord_leave", &[Profile::Coord, Profile::Full]),
    ("coord_claim", &[Profile::Coord, Profile::Full]),
    ("coord_release", &[Profile::Coord, Profile::Full]),
    (
        "coord_status",
        &[Profile::Micro, Profile::Coord, Profile::Full],
    ),
    ("coord_task_create", &[Profile::Coord, Profile::Full]),
    ("coord_task_claim", &[Profile::Coord, Profile::Full]),
    ("coord_task_list", &[Profile::Coord, Profile::Full]),
    ("coord_send", &[Profile::Coord, Profile::Full]),
    ("coord_inbox", &[Profile::Coord, Profile::Full]),
    // The team's unattended queue: beliefs agents and teammates proposed that no
    // person has accepted yet. Coordination owns it because acceptance is a team
    // decision; `memory` sits exactly at its 21-tool ceiling, and widening that
    // ceiling to carry a review queue would trade the memory surface's context
    // budget for a workflow it does not own.
    ("vault_proposals", &[Profile::Coord, Profile::Full]),
    // Team-vault reads: the shared vault a team searches and time-travels.
    ("mem_search_team", &[Profile::Coord, Profile::Full]),
    ("vault_asof_team", &[Profile::Coord, Profile::Full]),
];

/// One published row: a profile and how many tools it exposes, plus the
/// serialized `tools/list` byte cost for that profile.
///
/// `tool_count` is a pure function of [`TOOL_PROFILES`]; `tools_list_bytes`
/// is populated by the MCP crate when it assembles the published report, so
/// this crate reports zero here and the MCP crate fills in the real cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileSurface {
    /// The profile this row describes.
    pub profile: &'static str,
    /// How many tools `tools/list` advertises for it.
    pub tool_count: usize,
    /// Bytes of the serialized `tools/list` payload (filled in by the MCP crate).
    pub tools_list_bytes: usize,
}

/// The one place a published tool count comes from.
///
/// Three different numbers were being printed as "the size of the full profile" (48, 44 and 42)
/// because each document counted by hand. A count is a property of [`TOOL_PROFILES`], not of a
/// sentence, so it is derived here and every surface that wants to print one calls this.
///
/// Table order, and **only** the five table-driven profiles: `agent` is a transport-gated family
/// declared in the MCP crate (`agent::AGENT_TOOLS`), not a row in this table, so it is added by the
/// server that owns it rather than guessed at here.
pub fn surface() -> Vec<SurfaceRow> {
    surface_report()
}

/// One published row of the `SURFACE` report: a profile's tool count and the
/// bytes its `tools/list` payload serializes to.
///
/// `tool_count` is the source of truth: a pure function of [`TOOL_PROFILES`],
/// so every published number must derive from it. `tools_list_bytes` is the
/// serialized `tools/list` schema cost; the MCP crate owns the tool
/// definitions (`visible_tools`) and serializes the real byte cost when it
/// assembles the published report, so this crate reports the count that every
/// document agrees on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceRow {
    /// The profile this row describes.
    pub profile: &'static str,
    /// How many tools `tools/list` advertises for it.
    pub tool_count: usize,
    /// Bytes of the serialized `tools/list` payload.
    pub tools_list_bytes: usize,
}

/// The single source of truth for every published tool count.
///
/// Only the five table-driven profiles live here: `agent` is a transport-gated
/// family owned by the MCP crate (`agent::AGENT_TOOLS`) and is added by the
/// server that owns it rather than guessed at in this table.
pub fn surface_report() -> Vec<SurfaceRow> {
    [
        Profile::Micro,
        Profile::Memory,
        Profile::Code,
        Profile::Coord,
        Profile::Full,
    ]
    .into_iter()
    .map(|profile| SurfaceRow {
        profile: profile.name(),
        tool_count: profile.tool_names().len(),
        tools_list_bytes: 0,
    })
    .collect()
}

/// The exposure table for the runtime's own tool router.
///
/// A second table rather than more rows in [`TOOL_PROFILES`], because the two
/// surfaces name the same capability differently — `mem_search` on the server,
/// `vault_recall` inside a turn — and the server's completeness test compares
/// its table against its own definitions. The two tables are kept disjoint by a
/// test, so "one capability, two names, two exposures" cannot happen quietly.
///
/// Absence is Full-only on purpose: `run_command` is not listed, because a
/// profile that executes shell commands should be a decision rather than an
/// inherited default.
pub const RUNTIME_TOOL_PROFILES: &[(&str, &[Profile])] = &[
    // The `kg_*` tools keep their rows in [`TOOL_PROFILES`]: the two surfaces already agree on
    // that vocabulary, and a name gets exactly one row in the whole crate.
    ("vault_stats", &[Profile::Micro, Profile::Memory]),
    ("vault_recall", &[Profile::Micro, Profile::Memory]),
    ("read_file", &[Profile::Micro, Profile::Code]),
    ("glob_search", &[Profile::Code]),
    ("code_search", &[Profile::Code]),
    ("edit_file", &[Profile::Micro, Profile::Code]),
    ("vault_store", &[Profile::Memory, Profile::Code]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_exposes_every_row_and_nothing_needs_a_row() {
        for (name, _) in TOOL_PROFILES {
            assert!(Profile::Full.allows(name), "full must expose {name}");
        }
        // A tool absent from the table is Full-only by constructor; if that ever
        // becomes the intended state, this assertion is the place to say so.
        assert!(!Profile::Micro.allows("fs_list"));
    }

    #[test]
    fn narrow_profiles_do_not_leak_the_whole_registry() {
        assert!(Profile::Code.allows("fs_list"));
        assert!(!Profile::Memory.allows("fs_list"));
        assert!(Profile::Memory.allows("now"));
    }

    #[test]
    fn every_portable_tool_names_a_profile() {
        for tool in crate::registry::Registry::with_builtins().names() {
            let row = TOOL_PROFILES.iter().find(|(name, _)| *name == tool);
            assert!(row.is_some(), "{tool} has no exposure row");
        }
    }

    #[test]
    fn the_two_surface_tables_are_disjoint() {
        for (name, _) in RUNTIME_TOOL_PROFILES {
            assert!(
                !TOOL_PROFILES.iter().any(|(server, _)| server == name),
                "`{name}` is in both tables: one capability would have two exposures"
            );
        }
    }

    #[test]
    fn a_runtime_tool_is_gated_by_its_table() {
        // `kg_impact` is gated by the shared table, `vault_recall` by the runtime one; the gate
        // does not care which, it cares that one of them names the tool.
        assert!(Profile::Code.allows("kg_impact"));
        assert!(!Profile::Memory.allows("kg_impact"));
        assert!(Profile::Memory.allows("vault_recall"));
        assert!(!Profile::Micro.allows("glob_search"));
        // Listed nowhere: a decision, not an oversight.
        assert!(!Profile::Code.allows("run_command"));
        assert!(Profile::Full.allows("run_command"));
    }

    #[test]
    fn a_typo_does_not_widen_exposure() {
        assert_eq!(Profile::from_name("fulll"), None);
        assert_eq!(Profile::from_name("CODE"), Some(Profile::Code));
    }

    /// The counts every published table must agree with.
    ///
    /// Pinned on purpose: a membership edit that changes a profile's size is a documentation
    /// change too, and this test is where the two are forced to happen together. When it fails,
    /// update `docs/MCP.md`, the npm README and the module header in the same commit — that is
    /// the whole point, not an inconvenience.
    #[test]
    fn the_published_counts_are_the_tables_counts() {
        let counts: Vec<(&str, usize)> = surface()
            .into_iter()
            .map(|row| (row.profile, row.tool_count))
            .collect();
        assert_eq!(
            counts,
            vec![
                ("micro", 4),
                ("memory", 21),
                ("code", 26),
                ("coord", 14),
                ("full", 54)
            ],
            "a profile changed size: update the published tables with it"
        );
    }
    #[test]
    fn surface_report_is_the_published_count_source_of_truth() {
        let report = surface_report();
        let counts: Vec<(&str, usize)> = report
            .iter()
            .map(|row| (row.profile, row.tool_count))
            .collect();
        assert_eq!(
            counts,
            vec![
                ("micro", 4),
                ("memory", 21),
                ("code", 26),
                ("coord", 14),
                ("full", 54),
            ],
            "surface_report must match TOOL_PROFILES membership",
        );
        // `agent` is a transport-gated family owned by the MCP crate; it is not
        // a row in this table and is added by the server that owns it.
        assert!(!report.iter().any(|row| row.profile == "agent"));
    }
}
