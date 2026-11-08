#![warn(missing_docs)]
// The E2E report is one `json!({...})` literal with ~85 keys, and `json!` expands each key through a
// nested macro call — so the recursion depth grows with the *number of fields*, not with the nesting
// a reader sees. At the default limit the literal compiles until a field is added, then fails with
// "recursion limit reached while expanding `$crate::json_internal!`" at the report, which reads like a
// problem in the field that was added rather than in the size of the object. Raised here, once, with
// headroom, because the report is the harness's record: adding a field to it must never be the change
// that breaks the build.
//
// `256` did not have headroom after all: adding four map-stage fields in `iter11` reached it, so the
// claim above is corrected rather than left standing. Raised to `512`, which is the value the compiler
// itself names in the diagnostic. If this is ever hit again the fix is not a third bump — it is to
// split the report into named sub-objects, because the depth is a function of the field count and the
// field count only grows.
#![recursion_limit = "512"]
// TODO: Change to #![deny(missing_docs)] after all public items are documented

//! memnoth-mcp — the memnoth MCP stdio server.
//!
//! # Overview
//!
//! Drop-in memory + coordination layer for any MCP client (Claude Code,
//! Cursor, Codex, opencode, ...). Newline-delimited JSON-RPC 2.0 over stdio.
//!
//! # Tool Profiles
//!
//! memnoth exposes 51 tools on the full profile (plus 6 agent-mode tools)
//! across 6 profiles:
//!
//! | Profile | Tools | Use Case |
//! |---------|-------|----------|
//! | `micro` | 4 | Minimal context (coding agents) |
//! | `memory` | 21 | Memory-focused workflows, incl. the temporal quartet |
//! | `code` | 26 | Code intelligence: KG navigation + fs/git toolbox |
//! | `coord` | 11 | Multi-agent coordination |
//! | `full` | 51 | Complete feature set (no allowlist: Full means all) |
//! | `agent` | 6 | Agent session management (HTTP transport only) |
//!
//! Membership is declared once per tool in `TOOL_PROFILES`; the counts above
//! are derived from it by `Profile::tool_names`.
//!
//! Set `MEMNOTH_MCP_PROFILE` to select a profile.
//!
//! # Key Tools
//!
//! - `mem_search` — Hybrid FTS+graph search over the memory vault
//! - `mem_add_fact` — Store facts with auto-entity creation
//! - `kg_impact` — Blast radius analysis for code changes
//! - `kg_explore` — One-call symbol discovery with source snippets
//! - `coord_claim` — Path locking for multi-agent workflows

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use memnoth_capability::{
    Approval, CacheClass, Call, CostClass, Profile as CapProfile, Registry, Tool, ToolContext,
    ToolDef,
};
use rusqlite::params;
use serde_json::{Value, json};

mod agent;
mod catalog;
mod cli_graph;
// The `tools/list` projection, so the two anti-drift rules (membership from the
// shared table, stable order) live in one file rather than in the middle of the
// dispatcher.
mod visible;

use catalog::tool_definitions;
pub use visible::{compose_toolset, declared_names, visible_tools};
pub(crate) mod cloud_ingest;
// W0.1 — the dense (embedding) retrieval arm. A sibling module rather than more
// functions here, because it owns a process-wide embedding cache and its own cost
// model, and none of that belongs in the dispatcher.
//
// The module name is identical in both builds; only the file differs, so every
// call site stays put and the no-ONNX build answers the same question (`is a
// dense arm available?`) with `None` rather than failing to compile.
#[cfg(feature = "semantic")]
#[path = "dense.rs"]
mod dense;
#[cfg(not(feature = "semantic"))]
#[path = "dense_stub.rs"]
mod dense;
#[cfg(test)]
mod eval;
// The memory gauntlet: scoring instruments + ingest shapes for every benchmark
// outside LoCoMo/LongMemEval (BEAM, PersonaMem, MemoryArena, HaluMem, FAMA/Memora).
// Written and tested *before* those benchmarks are run, so their classes are known
// rather than discovered by failing them.
/// M3 compat aliases: rival tool names → native tools, full profile only.
mod compat;
mod gauntlet;
mod http;
/// M3 hub surface: claim-gated routing to N downstream servers, trust receipts,
/// npm hygiene. Public so `memnoth mcp doctor` can report the same facts the
/// server would enforce.
pub mod hub;
mod kg_index;
mod kg_tools;
/// Assisted retrieval (opt-in LLM listwise rerank). Off unless a caller asks for
/// `rerank=llm` and a key is present; labelled `assisted` when it answers.
mod llm;
mod mrtr;
mod oauth;
pub mod plugin_hooks;
mod policy;
/// S1 fixed-cost retrieval: per-arm recall budgets, rerank cap, token-budget
/// packing, exact per-stage accounting (scale lane).
mod recall_budget;
mod resource_templates;
mod symbol_resource;
mod tasks;
mod tools_agent;

pub use agent::AgentBridge;
pub use http::run_http_server;
/// Canonical leader-socket path (session routing rendezvous). Re-exported for
/// the same reason as [`harness_runtime_dir`].
pub use memnoth_harness_api::leader_socket_path as harness_leader_socket_path;
/// Re-exported so binaries that depend on `memnoth-mcp` (e.g. the `memnoth`
/// CLI's ACP bridge) resolve socket paths from the same definition the
/// harness API clients use, without adding a second dependency edge.
pub use memnoth_harness_api::runtime_dir as harness_runtime_dir;
pub use plugin_hooks::{
    PluginHookPoint, PluginHookSpec, dispatch_plugin_hook, matching_plugin_hooks,
    parse_plugin_hooks,
};

use memnoth_coord::{ClaimResult, Coord, MessageRow, SessionStatus, TaskRow};

#[cfg(test)]
use memnoth_capability::Receipt;
/// The unnarrated entry point is what the tests index fixtures with; every
/// production call goes through [`index_full_with_progress`] so that a client
/// which opted into progress actually receives it.
#[cfg(test)]
use memnoth_codegraph::index_full;
use memnoth_codegraph::{IndexPhase, IndexProgress, StalenessReport, Store, open_store};
#[cfg(test)]
use memnoth_coord::TaskStatus;
use memnoth_engine::{Entity, EntityKind, MemoryEngine};

/// Protocol revision this server speaks natively: stateless, with the version
/// and the client's capabilities carried on **every request** in `_meta`
/// (SEP-2575, the 2026-07-28 revision).
const LATEST_PROTOCOL_VERSION: &str = "2026-07-28";

/// Revisions a stateless request may declare. Anything else is refused with
/// `UnsupportedProtocolVersionError` and the supported list, so the client can
/// retry instead of mistaking a silent downgrade for success.
const MODERN_PROTOCOL_VERSIONS: &[&str] = &[LATEST_PROTOCOL_VERSION];

/// The revision our `initialize` handshake implements.
///
/// A handshake must never select a *modern* revision: those revisions have no
/// handshake, so answering one would tell the client to switch to per-request
/// semantics — and to a `resultType`/`_meta` contract — on the strength of a
/// negotiation that does not exist in them. A dual-era server picks its era
/// from *how the client opens* (spec 2026-07-28, `basic/versioning`), which is
/// why this stays legacy-only and why `server/discover` is what modern clients
/// probe.
const LEGACY_PROTOCOL_VERSION: &str = "2024-11-05";

/// `_meta` keys defined by the 2026-07-28 revision (SEP-2575).
const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// Spec-reserved error codes. The 2026-07-28 error-code allocation policy
/// partitions the JSON-RPC server-error range: `-32000..-32019` is
/// implementation-defined (existing usage grandfathered), `-32020..-32099`
/// belongs to MCP. These two are ours to use because they are in the reserved
/// half, not the grandfathered one.
const ERR_HEADER_MISMATCH: i64 = -32020;
const ERR_UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// SEP-2549 makes `ttlMs` + `cacheScope` **required** on list and read results.
/// The values differ because the answers have different lifetimes — and for
/// anything derived from a workspace the parse ledger, not a constant, decides:
///
/// * Tool, prompt and template *definitions* are compiled into the binary and
///   can only change on restart, so those lists are stable for the life of the
///   process.
/// * Vault-derived content (`resources/list`) reads live vault and coordination
///   counts, which move on every write; a few seconds absorbs a burst from one
///   host without ever serving a stale inventory.
/// * **Index-derived content is the one case where the hint is derived from
///   state**: `mem://index/status` reports the parse ledger, so when the ledger
///   says files changed since the last parse, the answer we just produced is
///   *knowably* incomplete and reuse is forbidden outright (0 ms) rather than
///   shortened. A fresh ledger is the only thing that licenses a host to cache
///   a freshness claim at all.
/// * `resources/read` of *vault* content gets **zero**. The vault is bi-temporal
///   and beliefs are invalidated in place, so a cached belief is a wrong belief
///   — a memory server that lets an intermediary serve a stale `mem://`
///   document has broken its one promise. `cacheScope` is `private` for the
///   same reason on every endpoint: the lists are filtered by this
///   installation's tool profile and the reads return this installation's
///   memory.
const DEFINITION_CACHE_TTL_MS: i64 = 300_000;

/// Ceiling for a workspace-derived answer that nothing has contradicted yet.
/// Short because the ledger's whole job is to notice edits, and a host that
/// caches the notice is caching the thing that just changed.
const WORKSPACE_DERIVED_TTL_MS: i64 = 5_000;

pub(crate) const READ_CACHE_TTL_MS: i64 = 0;

/// How many files the ledger may stat to answer a cache-hint question. Bounded
/// for the same reason `kg_status` bounds it: deriving a hint must never turn a
/// list call into a full filesystem sweep.
const LEDGER_MAX_CHECKS: usize = 64;

/// What a cached result's freshness depends on.
///
/// Naming this is the whole point: it turns three magic numbers into a claim
/// about *why* a host may reuse an answer, and it gives the parse ledger one
/// place to speak instead of scattering an `if stale` through the dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheKind {
    /// The server's own surface: tools, prompts, templates.
    Definitions,
    /// Vault and coordination state: every write moves it.
    Vault,
    /// Code-graph state, carrying what the parse ledger said about it.
    WorkspaceIndex {
        stale_files: u64,
        watcher_pending: u64,
    },
}

/// The cache hint for a kind of content. Pure, so the ledger's effect on a
/// host's caching is testable without a filesystem.
fn cache_ttl(kind: CacheKind) -> i64 {
    match kind {
        CacheKind::Definitions => DEFINITION_CACHE_TTL_MS,
        CacheKind::Vault => WORKSPACE_DERIVED_TTL_MS,
        // A ledger that reports work outstanding is a statement that the answer
        // is already out of date. Zero, not "a little" — there is no honest
        // TTL for a result we have just been told is incomplete.
        CacheKind::WorkspaceIndex {
            stale_files,
            watcher_pending,
        } if stale_files > 0 || watcher_pending > 0 => 0,
        CacheKind::WorkspaceIndex { .. } => WORKSPACE_DERIVED_TTL_MS,
    }
}

/// Tools that answer with a task handle when the client declares the Tasks
/// extension.
///
/// Both long-running tools, and they earn their place differently. `kg_index`
/// needs nothing but a workspace path — it opens its own code-graph store.
/// `mem_ingest` does write through the vault, and its worker therefore opens
/// **its own connection to the same file**: a `rusqlite::Connection` is `Send`
/// but not `Sync`, so the server's engine cannot cross a thread boundary, while
/// a second connection is already this system's model — the schema runs
/// `journal_mode = WAL`, and the daemon and this server routinely write one
/// vault from two processes.
///
/// Naming the set rather than checking one tool inline also keeps the claim
/// honest: a host decides how long it may wait *before* it calls a tool, so the
/// task-backed surface has to be a fact, not a guess (LANE-BUS `L-MCP-16`).
///
/// A server with no vault path — an in-memory engine, an embedder, most tests —
/// cannot start a `mem_ingest` worker, so that call stays synchronous exactly as
/// it was: `spawn_task` reports the tool as not async-eligible rather than
/// failing a call it cannot host.
const TASK_ELIGIBLE_TOOLS: &[&str] = &["kg_index", "mem_ingest"];

/// Agent playbook injected into the `initialize` response as `instructions`.
/// MCP clients surface this in the agent's system prompt automatically, so it
/// steers agent behavior before the first tool call. Modeled on the strongest
/// competitor pattern (codegraph's server instructions) but scoped to memnoth's
/// dual surface: memory vault + coordination + code graph.
const SERVER_INSTRUCTIONS: &str = r#"# Memnoth — memory, coordination, and code intelligence

Memnoth gives you three pre-built layers. Use them BEFORE re-deriving things
by hand — they are cached intelligence you don't pay to re-reason each run.

## 1. Code graph (`kg_*`) — use it instead of reading files

`kg_explore` is the primary code tool. It takes a natural-language question or
a bag of symbol/file names and returns the **verbatim, line-numbered source**
of the relevant symbols grouped by file — the same shape `Read` gives you,
safe to `Edit` from — plus the call path among them and a blast-radius summary
of what depends on them. ONE call usually answers the whole question.

- **"How does X work", architecture, a bug, "what/where is X", surveying an
  area, or editing a symbol you can name** → `kg_explore` first. Treat the
  source it returns as already Read; do NOT re-open those files with Read/Grep.
- **"What breaks if I change X?"** → `kg_impact` (blast radius, transitive
  callers). Use it BEFORE a refactor.
- **"Who calls X?"** → `kg_trace` (paginated direct callers).
- **"Is the index fresh?"** → `kg_status` / `kg_coverage` (parse ledger +
  staleness). When a tool response includes a staleness banner (⚠️), only the
  listed files are pending re-index — Read those specific files for accurate
  content; trust everything else.
- **"Which symbols match X?"** → `kg_search` (compact ranked FTS list) before
  deciding what to read in full.
- **"Is this code dead / too complex / unresolved?"** → `kg_dead` (zero
  callers), `kg_complexity` (transitive loop depth, recursion), `kg_unresolved`
  (refs the graph could not bind — a freshness/import signal).
- **"What would my uncommitted work break?"** → `kg_changes` (git diff →
  affected symbols → blast radius) BEFORE committing or pushing.

Anti-patterns: don't run your own grep + read loop to reconstruct flows the
code graph already resolved; don't hand-walk call chains that `kg_explore`
returns in one call.

## 2. Memory vault (`mem_*`) — what the team/you already know

`mem_search` is hybrid (FTS + graph + score) — search before asking. Facts
are bi-temporal: they are invalidated, never deleted, so `mem_search` can
answer "what was true then" as well as "what is true now".

## 3. Coordination (`coord_*`) — multi-agent safety

Claim path globs before editing (`coord_claim`), create/claim tasks on the
shared board, and message other sessions. First-writer-wins with TTL
auto-release prevents parallel agents from stepping on each other.

## 4. Agent utilities — the toolbox

Which profile exposes these: `fs_*` / `git_*` / `think` and the KG
navigation verbs are in the **`code`** profile; `vault_*` and `now` are in
**`memory`**; every one of them is in `full`. A tool you expect but cannot
see is a profile setting (`MEMNOTH_MCP_PROFILE`), not a missing capability.

- `now` → always check the clock when the task mentions dates/times/"today".
- `think` → for multi-step plans, write the reasoning chain explicitly.
- `fs_list` / `fs_read` → raw file content when you need config, manifests,
  generated code (kg_explore still wins for symbol-level questions).
- `git_status` / `git_diff` / `git_log` → before proposing edits, diff, or
  commits, ground the plan in the actual working tree.
- `vault_history` / `vault_asof` / `vault_contradictions` → temporal
  reasoning: what changed, what was believed at time T, and which current
  beliefs were contradicted (with reasons). No competitor memory layer
  exposes this — use it before trusting a belief.

When in doubt, one `kg_explore` or `mem_search` call beats a dozen
Read/Grep/file-reading round-trips."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Tool profile for the MCP server.
///
/// Profiles control which tools are exposed to the client. Membership is
/// **data**, declared once per tool in `TOOL_PROFILES` — see that table's
/// doc comment for why the shape matters.
pub enum Profile {
    /// Minimal profile with 4 tools (for coding agents)
    Micro,
    /// Memory profile with 21 tools: the vault surface, the **temporal
    /// quartet**, vault hygiene (`mem_duplicates` / `mem_merge`), the
    /// index-health verbs that let a host check its own index, and the clock.
    Memory,
    /// Coordination profile with 11 tools
    Coord,
    /// Code-intelligence profile: the KG navigation verbs, the same index
    /// health set, and the filesystem/git toolbox.
    Code,
    /// Full profile: every non-agent tool (51). There is deliberately NO
    /// allowlist for Full — `allows` is unconditional for it, so a new tool
    /// is reachable the moment it is defined; never a second list.
    Full,
    /// Agent profile with 6 tools for session management. The agent family is
    /// gated on the HTTP transport and is not part of Full.
    Agent,
}

impl Profile {
    /// The name this profile is selected by, e.g. `code`.
    pub fn name(self) -> &'static str {
        match self {
            Profile::Micro => "micro",
            Profile::Memory => "memory",
            Profile::Coord => "coord",
            Profile::Code => "code",
            Profile::Full => "full",
            Profile::Agent => "agent",
        }
    }

    /// Load the profile from the `MEMNOTH_MCP_PROFILE` environment variable.
    ///
    /// **Refuses an unrecognised value.** This process decides which tools a host
    /// is even told about, so a typo must not widen the surface: a machine whose
    /// operator meant `memory` (21 tools) used to expose `full` (51 tools), and
    /// the only signal was one stderr line nobody reads.
    pub fn from_env() -> anyhow::Result<Self> {
        let raw = std::env::var("MEMNOTH_MCP_PROFILE").ok();
        Self::parse_strict(raw.as_deref())
    }

    /// Strict parse: the documented default when unset, an error when unknown.
    ///
    /// The default is `memory`, not `full` — `main.rs`'s own help text says
    /// `(default: memory)`, and an unset variable is a decision to take that
    /// default rather than to take the widest surface available.
    pub fn parse_strict(value: Option<&str>) -> anyhow::Result<Self> {
        match value.map(str::trim).filter(|name| !name.is_empty()) {
            None => Ok(Profile::Memory),
            Some(name) => Self::parse_known(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown MEMNOTH_MCP_PROFILE value {name:?}; expected one of \
                     micro, memory, coord, code, agent, full. Refusing to start rather \
                     than falling back to 'full', which would widen the tool surface."
                )
            }),
        }
    }

    /// Lenient parse for values that are already stored (a connection row): an
    /// unknown name narrows to `memory` and never widens to `full`. The
    /// environment goes through [`Profile::parse_strict`], where an error is the
    /// correct answer.
    pub fn parse(value: Option<&str>) -> Self {
        Self::parse_strict(value).unwrap_or(Profile::Memory)
    }

    /// The name map, with no default of any kind.
    fn parse_known(name: &str) -> Option<Self> {
        Some(match name {
            "micro" => Profile::Micro,
            "memory" => Profile::Memory,
            "coord" => Profile::Coord,
            "code" => Profile::Code,
            "agent" => Profile::Agent,
            "full" => Profile::Full,
            // Spelled out so a profile added later must be given an arm here
            // rather than inheriting one by falling through.
            _ => return None,
        })
    }

    /// Whether `parse` accepts the name without falling back to `Full`.
    /// `mcp doctor` reads this instead of re-listing the names, so the two
    /// can never disagree about what counts as a typo.
    pub fn is_known_name(name: &str) -> bool {
        matches!(
            name,
            "micro" | "memory" | "coord" | "code" | "agent" | "full"
        )
    }

    /// Every tool name this profile exposes, in definition order.
    ///
    /// The seam hosts, docs and the schema-cost measurement read instead of
    /// hard-coding a count: the resolved list is a pure function of
    /// `TOOL_PROFILES` plus the two family rules in `Profile::allows`.
    ///
    /// Owned strings on purpose. The names are resolved from the live tool
    /// definitions (`visible_tools`) rather than re-read from the membership
    /// table, so a test comparing this list against the table is a real
    /// cross-check instead of a restatement of it.
    pub fn tool_names(self) -> Vec<String> {
        visible_tools(self)
            .iter()
            .filter_map(|t| t["name"].as_str())
            .map(str::to_owned)
            .collect()
    }

    /// Whether a tool call is permitted under this profile.
    ///
    /// Two rules, both about *families*, and **neither enumerating individual
    /// tools** — that is the shape that keeps the moat reachable:
    ///
    /// * `Full` is allow-all, so it can never rot: a new tool is reachable the
    ///   moment it is defined, with no second list to update.
    /// * `Agent` is the transport-gated agent-session family (`agent::AGENT_TOOLS`),
    ///   which needs the HTTP bridge and therefore must not be advertised by
    ///   `Full` over stdio.
    ///
    /// Membership for everything else comes from `TOOL_PROFILES` — one row per
    /// tool, so a profile can no longer lose a tool to a control-flow special
    /// case.
    fn allows(self, tool: &str) -> bool {
        match self {
            Profile::Full => true,
            Profile::Agent => agent::AGENT_TOOLS.contains(&tool),
            narrow => profiles_of(tool).contains(&narrow),
        }
    }
}

/// Profile membership lives in `memnoth-capability`.
///
/// It used to live here, and the runtime had a second list of eleven tools that
/// nothing forced to agree with it. The table is now in the crate both surfaces
/// link, so `tools/list` and the agent's router answer from one dataset; this
/// module keeps only the enum and the two family rules that are genuinely local
/// to the server (`Full` is allow-all, `Agent` is the HTTP-gated session family).
///
/// The tier sizes are still prose worth keeping honest, and the same evidence
/// applies: `memorix` ships micro 9 / lite 20 / team 28 / full 40+, `codegraph`
/// advertises one tool by default, `codebase-memory-mcp` ships 15. `Memory` is
/// what a memory-focused host loads *instead of* a bigger profile, so it is
/// held at 21 — one over memorix lite's solo-memory tier. The shape rule behind
/// that ceiling outlived the number: code navigation stays in `code` rather than
/// being absorbed, so `Memory` grows only when the memory category itself does.
const _: () = ();

/// The profiles a tool belongs to, from `TOOL_PROFILES`.
///
/// An unknown name returns an empty slice: a typo hides a tool from every
/// narrow profile rather than inventing exposure for it.
/// `tests::profile_membership_is_a_pure_function_of_metadata` fails when a
/// defined tool is missing a row, so that empty slice is a test failure and
/// never a silent Full-only tool.
fn profiles_of(tool: &str) -> Vec<Profile> {
    match memnoth_capability::TOOL_PROFILES
        .iter()
        .find(|(name, _)| *name == tool)
    {
        Some((_, profiles)) => profiles
            .iter()
            .filter_map(|profile| Profile::from_capability(*profile))
            .collect(),
        // No row: the tool is reachable from `Full` only, which the parity test
        // treats as a failure rather than as a silent policy.
        None => Vec::new(),
    }
}

impl Profile {
    /// Map the shared enum into this crate's.
    ///
    /// The two enums carry the same six names today; keeping the conversion
    /// explicit means a new profile has to be handled here rather than
    /// arriving as a silently ignored variant.
    fn from_capability(profile: memnoth_capability::Profile) -> Option<Self> {
        use memnoth_capability::Profile as Shared;
        match profile {
            Shared::Micro => Some(Self::Micro),
            Shared::Memory => Some(Self::Memory),
            Shared::Code => Some(Self::Code),
            Shared::Coord => Some(Self::Coord),
            Shared::Full => Some(Self::Full),
            Shared::Agent => Some(Self::Agent),
        }
    }
}

/// Run the MCP server over stdio for the active profile. Agent profile is
/// rejected here: it requires the HTTP transport.
pub async fn run_stdio_server(db_path: PathBuf) -> Result<()> {
    use tokio::io::BufReader;

    let profile = Profile::from_env()?;
    if profile == Profile::Agent {
        anyhow::bail!(
            "MEMNOTH_MCP_PROFILE=agent requires the HTTP transport: set MEMNOTH_MCP_PROFILE=agent \
             plus MEMNOTH_MCP_HTTP_TOKEN (and optionally MEMNOTH_MCP_HTTP_BIND)"
        );
    }
    eprintln!(
        "memnoth-mcp: serving tool profile '{}' (set MEMNOTH_MCP_PROFILE=micro|memory|coord|code|full)",
        profile_name(profile)
    );

    ensure_vault_parent(&db_path)?;
    let engine = memnoth_engine::open_vault(&db_path)?;
    let coord = Coord::open(&db_path)?;

    serve(
        engine,
        coord,
        profile,
        Some(&db_path),
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        None,
    )
    .await
}
/// Create the directory a vault path lives in.
///
/// SQLite reports a missing *directory* as "unable to open database file"
/// (error 14), which is indistinguishable from a permissions problem — a first
/// run from a fresh workspace looked like a broken install rather than an
/// uncreated `.memnoth/`. Every entry point creates it here.
fn ensure_vault_parent(db: &Path) -> Result<()> {
    if let Some(parent) = db.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    Ok(())
}

/// Run one tool by name with no transport in front of it.
///
/// This is the **same dispatcher** `tools/call` goes through: the profile gate,
/// the multi-round-trip confirmation, the task-eligible path and every tool body
/// are the ones a stdio host reaches. Only the JSON-RPC envelope is removed.
/// `memnoth mcp call` uses it, which is what lets the CLI reach every tool the
/// server advertises instead of the eleven the runtime router happens to own.
///
/// `db` is the vault the server would have been pointed at. It is created when
/// absent, exactly as `mcp serve` creates it, so a call from a fresh workspace
/// reports an empty vault rather than "no such file".
pub fn call(
    db: &Path,
    profile: memnoth_capability::Profile,
    name: &str,
    args: &Value,
) -> Result<Value> {
    let Some(profile) = Profile::from_capability(profile) else {
        anyhow::bail!("unknown tool profile");
    };
    if profile == Profile::Agent {
        anyhow::bail!("the agent profile needs the HTTP transport; use `memnoth mcp serve --http`");
    }
    ensure_vault_parent(db)?;
    let engine = memnoth_engine::open_vault(db)?;
    let coord = Coord::open(db)?;
    let args = crate::kg_tools::with_default_workspace(name, args, Some(db));
    let params = json!({ "name": name, "arguments": args });
    tools_call(&engine, &coord, profile, &params, None, Some(db))
}

/// Prove the MCP wire: spawn the stdio server under the active profile, perform the
/// `initialize` handshake, run a cursor-paginated `tools/list`, and compare the returned
/// count against the profile promise. This is the function `memnoth mcp doctor` reaches
/// into, so a CLI flag and the crate's own probe can never disagree about what a healthy
/// server looks like.
///
/// `db` is the vault the spawned server serves. A temporary path is fine — the server only
/// needs a writable database, not a populated one.
pub async fn doctor(db: &Path) -> Result<DoctorReport> {
    use std::io::{BufRead, BufReader, Write};

    let profile = Profile::from_env()?;
    let promise = visible_tools(profile).len();

    let mut command = std::process::Command::new("memnoth-mcp");
    command.arg("--db").arg(db);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| "failed to spawn memnoth-mcp server")?;
    let mut stdin = child.stdin.take().expect("stdin piped");
    let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut lines = stdout.lines();

    let mut next_id: u64 = 1;
    let mut cursor: Option<String> = None;
    let mut tools = 0usize;
    let mut handshake = false;
    let mut error: Option<String> = None;

    // initialize
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": next_id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "memnoth-mcp doctor", "version": env!("CARGO_PKG_VERSION")}
        }
    });
    next_id += 1;
    let initialize_text =
        serde_json::to_string(&initialize).expect("serializing a static JSON value cannot fail");
    if writeln!(stdin, "{initialize_text}").is_err() || stdin.flush().is_err() {
        error = Some("server closed stdin during initialize".to_owned());
    } else {
        let reply_text = match lines.next() {
            Some(Ok(line)) => line,
            Some(Err(e)) => bail!("failed to read initialize reply: {e}"),
            None => bail!("stream closed before reply to initialize"),
        };
        let reply: Value = serde_json::from_str(&reply_text)
            .with_context(|| format!("initialize reply was not JSON: {reply_text}"))?;
        handshake = reply.get("error").is_none();
        if !handshake {
            error = Some(format!(
                "initialize failed: {}",
                reply
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
            ));
        }
    }

    // cursor-paginated tools/list
    if error.is_none() {
        loop {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let request = json!({
                "jsonrpc": "2.0",
                "id": next_id,
                "method": "tools/list",
                "params": params
            });
            let request_text = serde_json::to_string(&request)
                .expect("serializing a static JSON value cannot fail");
            next_id += 1;
            if writeln!(stdin, "{request_text}").is_err() || stdin.flush().is_err() {
                error = Some("server closed stdin during tools/list".to_owned());
                break;
            }
            match lines.next() {
                Some(Ok(line)) => {
                    let reply: Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(_) => {
                            error = Some("tools/list reply was not JSON".to_owned());
                            break;
                        }
                    };
                    if let Some(err) = reply.get("error") {
                        error = Some(format!("tools/list failed: {err}"));
                        break;
                    }
                    let page = reply
                        .pointer("/result/tools")
                        .and_then(Value::as_array)
                        .map(|array| array.len())
                        .unwrap_or(0);
                    tools += page;
                    cursor = reply
                        .pointer("/result/nextCursor")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    if cursor.is_none() {
                        break;
                    }
                }
                Some(Err(_)) => {
                    error = Some("failed to read tools/list reply".to_owned());
                    break;
                }
                None => {
                    error = Some("stream closed before reply to tools/list".to_owned());
                    break;
                }
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    let report = DoctorReport {
        profile: profile.name().to_owned(),
        promise,
        tools,
        handshake,
        error,
    };
    Ok(report)
}

/// The result of `memnoth-mcp` proving its own wire.
#[derive(Debug, Clone)]
pub struct DoctorReport {
    /// The profile the server ran under.
    pub profile: String,
    /// The tool count the profile promises.
    pub promise: usize,
    /// The tool count `tools/list` actually returned.
    pub tools: usize,
    /// Whether `initialize` completed without an error field.
    pub handshake: bool,
    /// Any error encountered while probing, if any.
    pub error: Option<String>,
}

impl DoctorReport {
    /// Whether the wire is healthy: handshake succeeded and tool count matches the promise.
    pub fn is_healthy(&self) -> bool {
        self.handshake && self.tools == self.promise && self.error.is_none()
    }
}

pub mod selftest;

fn profile_name(profile: Profile) -> &'static str {
    match profile {
        Profile::Micro => "micro",
        Profile::Memory => "memory",
        Profile::Coord => "coord",
        Profile::Code => "code",
        Profile::Full => "full",
        Profile::Agent => "agent",
    }
}

/// NDJSON JSON-RPC loop over any byte transport. Split out of
/// `run_stdio_server` so tests can drive the real loop over in-memory pipes.
pub(crate) async fn serve<R, W>(
    engine: MemoryEngine,
    coord: Coord,
    profile: Profile,
    vault: Option<&Path>,
    stdin: R,
    mut stdout: W,
    agent: Option<std::sync::Arc<AgentBridge>>,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut lines = BufReader::new(stdin).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(resp) = maybe_agent_response(profile, agent.as_deref(), &msg).await {
            stdout.write_all(resp.to_string().as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
            // Drained here too, and this `continue` is exactly why: the agent bridge handles the
            // *longest*-running calls in the server, so it is the path where progress matters most.
            // Returning to the top of the loop without draining would leave those notifications
            // queued until some unrelated later request — which is the same silence the feature
            // exists to remove.
            for notification in drain_notifications() {
                stdout
                    .write_all(notification.to_string().as_bytes())
                    .await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            continue;
        }
        // stdio has no transport-level principal to bind to: the process *is*
        // the user, so the multi-round-trip state omits the claim rather than
        // inventing an identity (see `mrtr`).
        if let Some(resp) = handle_message_for_profile(&engine, &coord, profile, msg, None, vault) {
            stdout.write_all(resp.to_string().as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
        // Anything a tool queued while answering goes out **after** its response. The order is
        // load-bearing: a `progressToken` is the client's handle on a request, so progress that
        // arrived before the client had seen the request would have nothing to attach to.
        for notification in drain_notifications() {
            stdout
                .write_all(notification.to_string().as_bytes())
                .await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

/// Intercept `agent_*` tools/call requests before the synchronous dispatcher:
/// agent tools need the async bridge and only exist under `Profile::Agent`.
async fn maybe_agent_response(
    profile: Profile,
    agent: Option<&AgentBridge>,
    msg: &Value,
) -> Option<Value> {
    if profile != Profile::Agent {
        return None;
    }
    let is_agent_call = msg.get("method").and_then(Value::as_str) == Some("tools/call")
        && msg
            .get("params")
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .map(|n| n.starts_with("agent_"))
            .unwrap_or(false);
    if !is_agent_call {
        return None;
    }
    let id = msg.get("id")?.clone();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match agent {
        None => Err(anyhow::anyhow!(
            "agent tools require MEMNOTH_MCP_PROFILE=agent so the harness API bridge is attached"
        )),
        Some(bridge) => {
            let name = params["name"].as_str().unwrap_or_default().to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            bridge.call(&name, &args).await
        }
    };
    let response = match result {
        Ok(v) => json!({"content": [{"type": "text", "text": v.to_string()}]}),
        Err(e) => json!({
            "content": [{"type": "text", "text": e.to_string()}],
            "isError": true,
        }),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": response}))
}

/// Handle one JSON-RPC message at the default (full) profile.
#[cfg(test)]
pub(crate) fn handle_message(engine: &MemoryEngine, coord: &Coord, msg: Value) -> Option<Value> {
    handle_message_for_profile(engine, coord, Profile::Full, msg, None, None)
}

/// Handle one JSON-RPC message under a tool-profile restriction: `tools/list`
/// only advertises in-profile tools and `tools/call` rejects out-of-profile
/// calls with a pointer to `MEMNOTH_MCP_PROFILE`.
/// `principal` is the authenticated identity when the transport has one (an
/// HTTP bearer's `sub`), and `None` on stdio. It is threaded in rather than read
/// from a global so the multi-round-trip state can be bound to whoever actually
/// made the request (SEP-2322).
///
/// `vault` is where this server's vault lives, and it exists for one caller:
/// a task worker that needs its **own** connection to that vault (see
/// [`spawn_task`]). `None` means "this server has no vault file" — an in-memory
/// engine, an embedder — and only narrows what can run in the background; no
/// tool changes its answer because of it.
pub(crate) fn handle_message_for_profile(
    engine: &MemoryEngine,
    coord: &Coord,
    profile: Profile,
    msg: Value,
    principal: Option<&str>,
    vault: Option<&Path>,
) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method")?.as_str()?.to_string();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    // Modern (2026-07-28 and later) requests declare their protocol version on
    // every request instead of handshaking. An unknown revision is a hard
    // error carrying the list we do speak: the spec's whole point is that the
    // client retries with a mutually supported version rather than silently
    // living with a downgrade. Absent `_meta` means a legacy request, which is
    // served exactly as it was before this revision existed.
    if let Some(requested) = params
        .get("_meta")
        .and_then(|meta| meta.get(META_PROTOCOL_VERSION))
        .and_then(Value::as_str)
        && !MODERN_PROTOCOL_VERSIONS.contains(&requested)
    {
        let id = id?;
        return Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": ERR_UNSUPPORTED_PROTOCOL_VERSION,
                "message": "Unsupported protocol version",
                "data": {
                    "supported": MODERN_PROTOCOL_VERSIONS,
                    "requested": requested,
                },
            },
        }));
    }

    // Plugin hook point (fail-open, detached): every inbound method notifies
    // on_command subscribers with the method name; tool calls additionally
    // notify on_tool_call subscribers with the tool name (see tools_call).
    if !method.starts_with("notifications/") {
        dispatch_plugin_hook(PluginHookPoint::OnCommand, &method);
    }

    let response = match method.as_str() {
        // The spec's liveness utility. Without this arm a host that pings us — which is exactly
        // what a host does before deciding whether to keep a server in a session — received
        // `-32601 method not found`, i.e. "this is not an MCP server". It is a request/response
        // no-op, and the only correct answer is an empty result.
        "ping" => Ok(json!({})),
        "initialize" => {
            Ok(json!({
                "protocolVersion": LEGACY_PROTOCOL_VERSION,
                "capabilities": legacy_capabilities(),
                "serverInfo": server_info(),
                // Agent playbook: MCP clients surface this in the system
                // prompt, steering the agent to explore-first behavior
                // before it sees individual tool descriptions.
                "instructions": SERVER_INSTRUCTIONS,
            }))
        }
        // REQUIRED by the 2026-07-28 revision: the one RPC every server must
        // implement, advertising the revisions it speaks, its capabilities and
        // its identity. It doubles as the stdio backward-compatibility probe,
        // so a dual-era client can tell us apart from a legacy-only server
        // before it commits to an era.
        //
        // `protocolVersions` lists every revision this server speaks, newest
        // first. The version *error* lists only the modern ones, because only
        // those are selectable on a stateless request.
        "server/discover" => Ok(json!({
            "protocolVersions": supported_protocol_versions(),
            "capabilities": server_capabilities(),
            "serverInfo": server_info(),
        })),
        "tools/list" => Ok(cacheable(
            json!({ "tools": visible_tools(profile) }),
            cache_ttl(CacheKind::Definitions),
        )),
        "tools/call" => {
            // Whether this tool promised a typed result, read from the request
            // rather than from what came back: the promise and the payload are
            // then two independent facts, and only their agreement is checked.
            let typed = params
                .get("name")
                .and_then(Value::as_str)
                .map(declares_output_schema)
                .unwrap_or(false);
            // Progress for **every** tool, emitted from one place instead of per tool. The tools
            // that need it most are the bulk ones (`kg_index`, `mem_ingest`), whose failure mode is
            // a host that cannot tell a thirty-second ingest from a hang — and a per-tool hook is a
            // hook the next bulk tool will not have.
            //
            // A start/finish pair is deliberately the minimum that is *true* here: this server
            // knows when a call begins and when it ends, and does not invent a percentage it has no
            // way to know. A fabricated `progress: 0.5` would be a lie the client renders.
            let token = progress_token(&params);
            if let Some(token) = token.as_ref() {
                enqueue_notification(progress_notification(token, 0.0, "started"));
            }
            // Measured here, at the one place every tool call passes through, so a host can
            // show what the call cost without asking a second question.
            let started = std::time::Instant::now();
            let outcome = match tools_call(engine, coord, profile, &params, principal, vault) {
                // `input_required` is an *interim* result, not a tool result: the
                // client gathers the answers and retries the same call. Wrapping
                // it in `content` would hide the prompt from every host that
                // looks for `resultType`, which is every host that implemented
                // MRTR (SEP-2322). A `resultType: "task"` handle is likewise an
                // *interim* result in place of the tool result, not a payload to
                // serialize into `content` — the extension mandates the flat
                // `CreateTaskResult`.
                Ok(result)
                    if result["resultType"].as_str() == Some(mrtr::INPUT_REQUIRED)
                        || result["resultType"].as_str() == Some(tasks::TASK_RESULT_TYPE) =>
                {
                    Ok(result)
                }
                Ok(result) => {
                    let receipt = memnoth_capability::Receipt::measured(
                        params
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown"),
                        profile.name(),
                        // A tool answer is not the definition list: no cache class is claimed
                        // here, because the server has none to claim on the tool's behalf.
                        memnoth_capability::CacheClass::NoStore,
                        result.to_string().len() as u64,
                        started,
                    );
                    Ok(with_receipt(call_tool_result(result, typed), receipt))
                }
                Err(e) => Ok(json!({
                    "content": [{"type": "text", "text": e.to_string()}],
                    "isError": true,
                })),
            };
            // Not every response is a *completion*. An `input_required` result is waiting for the
            // client to answer a question and a `resultType: "task"` handle means the work is still
            // running elsewhere — announcing either as "finished" would tell the host the opposite
            // of what is true, and a progress message is the part of a protocol a UI renders
            // without checking. So the announcement is tied to the result type, not to the fact
            // that a response was produced.
            if let Some(token) = token.as_ref() {
                let interim = outcome
                    .as_ref()
                    .ok()
                    .and_then(|result| result["resultType"].as_str())
                    .map(|kind| kind == mrtr::INPUT_REQUIRED || kind == tasks::TASK_RESULT_TYPE)
                    .unwrap_or(false);
                if !interim {
                    enqueue_notification(progress_notification(token, 1.0, "finished"));
                }
            }
            outcome
        }
        "prompts/list" => Ok(cacheable(
            json!({ "prompts": prompt_definitions() }),
            cache_ttl(CacheKind::Definitions),
        )),
        "prompts/get" => prompts_get(&params),
        // Autocompletion for prompt arguments and resource templates (spec
        // `server/utilities/completion`), declared in the capabilities above.
        "completion/complete" => completions_complete(engine, coord, profile, &params),
        "resources/list" => {
            // Whether this server serves an indexed workspace decides both the
            // list's contents and its lifetime, so the ledger is read once. A
            // single index-derived entry makes the whole list a derived answer:
            // it may only be reused at the pace of its shortest-lived member.
            let ledger = workspace_root_for_vault(vault)
                .and_then(|root| workspace_freshness(&root, LEDGER_MAX_CHECKS));
            match page_cursor(&params) {
                Err(error) => Err(error),
                Ok(cursor) => {
                    let (resources, next) = resource_list(engine, coord, ledger.is_some(), cursor);
                    let mut result = json!({ "resources": resources });
                    if let Some(next) = next {
                        result["nextCursor"] = json!(next);
                    }
                    Ok(cacheable(
                        result,
                        match ledger {
                            Some(freshness) => {
                                cache_ttl(freshness.kind).min(cache_ttl(CacheKind::Vault))
                            }
                            None => cache_ttl(CacheKind::Vault),
                        },
                    ))
                }
            }
        }
        "resources/templates/list" => Ok(cacheable(
            json!({ "resourceTemplates": resource_templates::all() }),
            cache_ttl(CacheKind::Definitions),
        )),
        "resources/read" => {
            resources_read(engine, coord, &params, vault).map(|(read, ttl)| cacheable(read, ttl))
        }
        // Tasks extension (`io.modelcontextprotocol/tasks`). Clients poll with
        // `tasks/get`; we never push `notifications/tasks`, which the extension
        // makes optional and gates behind `subscriptions/listen`.
        "tasks/get" => tasks_get(&params),
        "tasks/update" => tasks_update(&params),
        "tasks/cancel" => tasks_cancel(&params),
        m if m.starts_with("notifications/") => return None,
        other => Err(json!({"code": -32601, "message": format!("method not found: {other}")})),
    };

    let id = id?;
    Some(match response {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": complete_result(result)}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    })
}

/// Identity reported in `initialize`, in `server/discover`, and in the `_meta`
/// of every result — the revision asks servers to identify themselves per
/// result, which is what lets a host (or a proxy in the middle) see which
/// server actually answered.
fn server_info() -> Value {
    // `title` is the human-readable half of `Implementation` (2025-06-18+); `name` stays the
    // stable machine identifier. `websiteUrl` and `icons` are deliberately absent rather than
    // filled with a plausible-looking URL: both must point at assets that actually exist, and a
    // broken icon URL is worse for a host than no icon at all.
    json!({
        "name": "memnoth",
        "title": "Memnoth",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// Capabilities as advertised by `server/discover`. `extensions` is the
/// negotiation channel added by 2026-07-28, and it is where the Tasks
/// extension is declared: the extension spec has the *server* advertise here
/// while the *client* declares support per request in `_meta`.
fn server_capabilities() -> Value {
    // Assembled rather than written as one literal so the extension identifier
    // exists in exactly one place: `tasks::TASKS_EXTENSION`, the same string the
    // client-side negotiation check reads. Declaring the extension in one spot
    // and gating on a literal in another is how a server ends up advertising an
    // extension it then refuses to honour.
    let mut capabilities = legacy_capabilities();
    capabilities["extensions"] = json!({});
    capabilities["extensions"][tasks::TASKS_EXTENSION] = json!({});
    capabilities
}

/// Capabilities returned by the legacy `initialize` handshake. Deliberately no
/// `extensions` map: extensions are a 2026-07-28 concept, and a legacy client
/// has no vocabulary for one.
fn legacy_capabilities() -> Value {
    // `completions` is the one capability a host must be *told* about before it
    // will ever ask, so declaring it is the whole difference between an
    // autocompleting host and a host that never tries (see
    // `completions_complete`).
    // The `listChanged` flags are stated explicitly rather than left absent. Absent means "not
    // supported" too, so this is not a behaviour change — it is a change from an omission to a
    // claim. A host that has to distinguish "this server does not push tool-list changes" from
    // "this server forgot to say" will assume the worse of the two, and it should: `tools/list`
    // here is already served with a `ttlMs`, which is how this server answers the same problem the
    // notification would solve.
    json!({
        "tools": {"listChanged": false},
        "prompts": {"listChanged": false},
        "resources": {"subscribe": false, "listChanged": false},
        "completions": {},
    })
}

/// Every revision this server speaks, newest first. Modern revisions are
/// selectable per request; the legacy one is selected by the handshake.
fn supported_protocol_versions() -> Vec<&'static str> {
    MODERN_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .chain(std::iter::once(LEGACY_PROTOCOL_VERSION))
        .collect()
}

/// Attach the SEP-2549 caching hints a list/read result is required to carry.
fn cacheable(mut result: Value, ttl_ms: i64) -> Value {
    result["ttlMs"] = json!(ttl_ms);
    result["cacheScope"] = json!("private");
    result
}

/// Upper bound on queued notifications. See [`enqueue_notification`] for why a bound is required
/// rather than merely prudent.
const MAX_PENDING_NOTIFICATIONS: usize = 64;

/// Notifications the server sends on its **own** initiative: progress, and the resource/tool-list
/// change signals.
///
/// **Why a queue instead of a direct write.** `handle_message_for_profile` is deliberately
/// synchronous and transport-agnostic — stdio, the HTTP handler and every test call it — and only
/// stdio has a live outbound stream. A tool that wrote a notification itself would couple every tool
/// to the transport. So tools *enqueue*, and each transport drains what applies to it: stdio writes
/// them out after the response they belong to, and HTTP — which serves no stream at all by design
/// (see the header of `http.rs`) — lets them go.
///
/// That asymmetry is why this buffer is **bounded and drops the oldest**. On stdio it never fills,
/// because every response drains it. On HTTP nothing ever drains it, and an unbounded queue would
/// quietly turn "progress is unsupported on this transport" into a memory leak on a long-lived
/// server. Dropping the oldest entry is the right end to lose: a superseded progress update has no
/// value once a newer one exists.
///
/// A process-global rather than a per-session handle, which is correct for the transports that
/// exist: a stdio server *is* one session, and the HTTP handler drops its notifications anyway, so
/// there is nothing to cross-talk. If a future transport both multiplexes sessions and pushes, this
/// must become session-scoped — the comment is here so that change is a decision, not an accident.
static PENDING_NOTIFICATIONS: std::sync::Mutex<Vec<Value>> = std::sync::Mutex::new(Vec::new());

fn enqueue_notification(notification: Value) {
    if let Ok(mut queue) = PENDING_NOTIFICATIONS.lock() {
        if queue.len() >= MAX_PENDING_NOTIFICATIONS {
            queue.remove(0);
        }
        queue.push(notification);
    }
}

/// Take everything queued since the last drain.
fn drain_notifications() -> Vec<Value> {
    PENDING_NOTIFICATIONS
        .lock()
        .map(|mut queue| std::mem::take(&mut *queue))
        .unwrap_or_default()
}

/// A `notifications/progress` payload, bound to the client's own token.
///
/// No token, no notification: the spec makes `progressToken` the client's opt-in, and a progress
/// notification that carries no token cannot be associated with the request that caused it.
fn progress_notification(token: &Value, progress: f64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {
            "progressToken": token,
            "progress": progress,
            "message": message,
        },
    })
}

/// The `progressToken` a client opted into for this request, if any.
fn progress_token(params: &Value) -> Option<Value> {
    params
        .get("_meta")
        .and_then(|meta| meta.get("progressToken"))
        .filter(|token| !token.is_null())
        .cloned()
}

/// How often a `kg_index` run may report. The engine's callback fires per file —
/// thousands of times on a real repository — and the client renders each one, so
/// flooding it would cost more than the indexing saved.
const KG_INDEX_PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);

thread_local! {
    /// The progress sink for a `kg_index` running on **this** thread, if the
    /// client opted into progress.
    ///
    /// Thread-local rather than global, and the reason is the reason a global
    /// would be wrong: the engine's callback fires on the thread that started the
    /// index — the request thread for an inline call, the worker thread for a
    /// task-backed one — so a shared slot would let two concurrent indexes report
    /// their file counts into each other's token.
    static KG_INDEX_SINK: RefCell<Option<KgIndexSink>> = const { RefCell::new(None) };
}

/// A live progress sink: the client's token plus the throttle state.
struct KgIndexSink {
    token: Value,
    last: Instant,
}

impl KgIndexSink {
    fn new(token: Value) -> Self {
        // Backdated so the first snapshot is emitted immediately: the point of
        // the feature is the first honest sign of life, and holding it back for
        // an interval makes the start look identical to the silence it replaces.
        let last = Instant::now()
            .checked_sub(KG_INDEX_PROGRESS_INTERVAL)
            .unwrap_or_else(Instant::now);
        Self { token, last }
    }

    fn report(&mut self, progress: &IndexProgress) {
        let fraction = match progress.phase {
            // No ratio exists until discovery has finished: `files_total` is
            // still growing and `files_processed` is legitimately zero. Sending
            // 0.0 here is honest — it is the phase start, not a percentage.
            IndexPhase::Discovering => 0.0,
            // Embedding has its own denominator. Every file is read by the time
            // it starts, so the file ratio is pinned at 1.0 for the whole phase
            // — a host would render that as "finished" while the actual work,
            // which on a large repository is the longest part of the run, is
            // still going.
            IndexPhase::Embedding => {
                if progress.symbols_to_embed == 0 {
                    1.0
                } else {
                    (progress.symbols_embedded as f64 / progress.symbols_to_embed as f64)
                        .clamp(0.0, 1.0)
                }
            }
            _ if progress.files_total == 0 => 0.0,
            _ => (progress.files_processed as f64 / progress.files_total as f64).clamp(0.0, 1.0),
        };
        let finished = matches!(progress.phase, IndexPhase::Complete) || fraction >= 1.0;
        if !finished && self.last.elapsed() < KG_INDEX_PROGRESS_INTERVAL {
            return;
        }
        self.last = Instant::now();
        enqueue_notification(progress_notification(
            &self.token,
            fraction,
            &kg_index_progress_message(progress, fraction),
        ));
    }
}

/// The human half of a `kg_index` progress update: what is running and how far.
fn kg_index_progress_message(progress: &IndexProgress, fraction: f64) -> String {
    let phase = match progress.phase {
        IndexPhase::Discovering => "scanning",
        IndexPhase::Parsing => "parsing",
        IndexPhase::Extracting => "parsing",
        IndexPhase::Relations => "resolving calls",
        IndexPhase::Resolving => "resolving calls",
        IndexPhase::Publishing => "writing the graph",
        IndexPhase::Embedding => "embedding symbols",
        IndexPhase::Complete => "finishing",
    };
    let seconds = progress.elapsed_ms as f64 / 1000.0;
    if matches!(progress.phase, IndexPhase::Discovering) {
        return format!(
            "{phase} · {} files to check · {seconds:.0}s",
            progress.files_total
        );
    }
    if matches!(progress.phase, IndexPhase::Embedding) {
        return format!(
            "{phase} · {}% · {}/{} symbols · {seconds:.0}s",
            (fraction * 100.0).round(),
            progress.symbols_embedded,
            progress.symbols_to_embed,
        );
    }
    format!(
        "{phase} · {}% · {}/{} files · {} symbols · {seconds:.0}s",
        (fraction * 100.0).round(),
        progress.files_processed,
        progress.files_total,
        progress.symbols_extracted,
    )
}

/// Arm the sink for a `kg_index` about to run on this thread, returning a guard
/// that disarms it however the call ends.
///
/// Arming is scoped to the one tool that can honestly report: the engine exposes
/// a progress callback for indexing and for nothing else, so announcing a
/// percentage for any other tool would be inventing a number, which is exactly
/// what the start/finish pair exists to avoid.
fn arm_kg_index_progress(name: &str, token: Option<&Value>) -> Option<KgIndexSinkGuard> {
    if name != "kg_index" {
        return None;
    }
    let token = token?.clone();
    KG_INDEX_SINK.with(|slot| *slot.borrow_mut() = Some(KgIndexSink::new(token)));
    Some(KgIndexSinkGuard)
}

struct KgIndexSinkGuard;

impl Drop for KgIndexSinkGuard {
    fn drop(&mut self) {
        KG_INDEX_SINK.with(|slot| *slot.borrow_mut() = None);
    }
}

/// How often a `mem_ingest` run may report. The ingest callback fires once per
/// file the walk sees; a client renders each update, so the interval is the
/// same order as the indexer's.
const MEM_INGEST_PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);

thread_local! {
    /// Progress sink for a `mem_ingest` on this thread. Same thread-local
    /// reasoning as [`KG_INDEX_SINK`]: a shared slot would let two concurrent
    /// ingests report into each other's token.
    static MEM_INGEST_SINK: RefCell<Option<MemIngestSink>> = const { RefCell::new(None) };
}

struct MemIngestSink {
    token: Value,
    last: Instant,
}

impl MemIngestSink {
    fn new(token: Value) -> Self {
        let last = Instant::now()
            .checked_sub(MEM_INGEST_PROGRESS_INTERVAL)
            .unwrap_or_else(Instant::now);
        Self { token, last }
    }

    /// `seen` is a **count**, not a fraction: the walk discovers files as it
    /// goes, so there is no honest denominator until it ends. `total` is
    /// therefore omitted on the running updates and supplied once, at the end,
    /// where the report knows it.
    fn report(&mut self, seen: u64, rel: &str, force: bool) {
        if !force && self.last.elapsed() < MEM_INGEST_PROGRESS_INTERVAL {
            return;
        }
        self.last = Instant::now();
        enqueue_notification(json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": {
                "progressToken": &self.token,
                "progress": seen as f64,
                "message": format!("ingesting · {seen} files · {rel}"),
            },
        }));
    }

    fn finish(&mut self, report: &memnoth_ingest::IngestReport) {
        enqueue_notification(json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": {
                "progressToken": &self.token,
                "progress": report.files_seen as f64,
                "total": report.files_seen as f64,
                "message": format!(
                    "ingested {} of {} files ({} unchanged) · {} entities · {} observations",
                    report.files_ingested,
                    report.files_seen,
                    report.files_unchanged,
                    report.entities_created,
                    report.observations_created,
                ),
            },
        }));
    }
}

/// Arm the `mem_ingest` sink for this thread; `None` when the client did not
/// opt in, in which case the ingest runs exactly as before.
fn arm_mem_ingest_progress(name: &str, token: Option<&Value>) -> Option<MemIngestSinkGuard> {
    if name != "mem_ingest" {
        return None;
    }
    let token = token?.clone();
    MEM_INGEST_SINK.with(|slot| *slot.borrow_mut() = Some(MemIngestSink::new(token)));
    Some(MemIngestSinkGuard)
}

struct MemIngestSinkGuard;

impl Drop for MemIngestSinkGuard {
    fn drop(&mut self) {
        MEM_INGEST_SINK.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Forward one file-walk tick into the armed `mem_ingest` sink, if any.
fn report_mem_ingest_progress(seen: u64, rel: &str) {
    MEM_INGEST_SINK.with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut()
            && let Some(sink) = slot.as_mut()
        {
            sink.report(seen, rel, false);
        }
    });
}

/// Whether a client armed a progress sink for this call.
///
/// `kg_index` uses it to decide who indexes: a client that asked for progress
/// wants the per-file snapshots of the bundled indexer, and the CLI cannot
/// narrate its own.
pub(crate) fn kg_index_progress_armed() -> bool {
    KG_INDEX_SINK.with(|slot| {
        slot.try_borrow()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
    })
}

/// Forward one engine snapshot into the armed sink, if any.
pub(crate) fn report_kg_index_progress(progress: &IndexProgress) {
    KG_INDEX_SINK.with(|slot| {
        // A panicking or re-entrant callback must not poison the index run: the
        // worst case here is a missed progress update.
        if let Ok(mut slot) = slot.try_borrow_mut()
            && let Some(sink) = slot.as_mut()
        {
            sink.report(progress);
        }
    });
}

/// Wrap a tool's payload as a `CallToolResult`. Shared by the synchronous path
/// and the task worker, so a completed task's `result` field is byte-for-byte
/// what the same call would have returned inline — which is exactly what the
/// tasks extension promises ("the structure matches the result type of the
/// original request").
/// `structured: true` additionally publishes the payload as
/// `structuredContent` — the typed half a host can read without parsing the text
/// (the revision's `outputSchema`/`structuredContent` pair). Both halves are sent
/// deliberately: the spec says a server returning structured content SHOULD also
/// return the serialized JSON, so a client that predates the field keeps working.
/// Attach a call's receipt to its result's `_meta`.
///
/// `_meta` is the protocol's extension point: a host that does not know `receipt` ignores it, and
/// one that does can render `6 ms · 1.8 KB` from the same response instead of issuing a second
/// call to find out what the first one cost.
fn with_receipt(mut out: Value, receipt: memnoth_capability::Receipt) -> Value {
    if let Some(object) = out.as_object_mut() {
        let meta = object.entry("_meta").or_insert_with(|| json!({}));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert("receipt".to_string(), json!(receipt));
        }
    }
    out
}

fn call_tool_result(result: Value, structured: bool) -> Value {
    let text = result.to_string();
    let mut out = json!({"content": [{"type": "text", "text": text}]});
    if structured && result.is_object() {
        out["structuredContent"] = result;
    }
    out
}

/// Stamp the two fields every 2026-07-28 result carries: the `resultType`
/// discriminator and the server's identity in `_meta` (SEP-2322, SEP-2575).
/// Stamping them unconditionally is safe in both eras — older clients ignore
/// unknown fields, and the revision instructs clients to read a *missing*
/// `resultType` as `"complete"` — so there is one envelope, never two.
fn complete_result(mut result: Value) -> Value {
    if let Some(object) = result.as_object_mut() {
        object
            .entry("resultType")
            .or_insert_with(|| json!("complete"));
        let meta = object.entry("_meta").or_insert_with(|| json!({}));
        if let Some(meta) = meta.as_object_mut() {
            meta.entry(META_SERVER_INFO).or_insert_with(server_info);
        }
    }
    result
}

fn task_id_param(params: &Value) -> std::result::Result<&str, Value> {
    params
        .get("taskId")
        .and_then(Value::as_str)
        .ok_or_else(|| json!({"code": -32602, "message": "missing taskId"}))
}

fn tasks_get(params: &Value) -> std::result::Result<Value, Value> {
    let task_id = task_id_param(params)?;
    tasks::get(task_id).ok_or_else(
        || json!({"code": -32602, "message": format!("unknown or expired task: {task_id}")}),
    )
}

/// Accept input responses for a task waiting on them. Nothing here reaches
/// `input_required` yet (no task declares `inputRequests`), but the RPC is
/// acknowledged per the extension so a client that sends one is not left with a
/// method-not-found — the failure mode that makes a host switch off an
/// extension it cannot fully exercise.
fn tasks_update(params: &Value) -> std::result::Result<Value, Value> {
    let task_id = task_id_param(params)?;
    if tasks::get(task_id).is_none() {
        return Err(
            json!({"code": -32602, "message": format!("unknown or expired task: {task_id}")}),
        );
    }
    Ok(json!({}))
}

fn tasks_cancel(params: &Value) -> std::result::Result<Value, Value> {
    let task_id = task_id_param(params)?;
    if tasks::cancel(task_id) {
        Ok(json!({}))
    } else {
        Err(json!({"code": -32602, "message": format!("unknown or expired task: {task_id}")}))
    }
}

// ── document prose capture ──────────────────────────────────────────────────
//
// `memnoth-ingest` keeps *structure*: a file node named by its path, Rust
// symbols, markdown headings. It does not keep the prose. A `.txt` file yields
// nothing at all beyond a `"N lines, hash"` note, and a markdown body is
// discarded the moment its headings become concept entities. The consequence is
// that ingesting a directory of documents produces a set of filenames you cannot
// search — the one thing a memory server exists to prevent, and exactly what the
// first external-dataset run measured as zero recall.
//
// So the tool stores the body itself, chunked, as observations on the file's own
// entity. `mem_search` reaches those through its fact fusion, and because every
// observation is prefixed with the document path, a retrieved fact can always be
// traced back to the file that said it.
//
// The right home for this is `memnoth-ingest::ingest_file`, which already holds
// both the file entity and the text (filed as L-MCP-31, KG lane). It lives here
// because `mem_ingest` is the tool whose contract is "remember this directory",
// and the duplication of `SKIP_DIRS` / `MAX_FILE_BYTES` below is the smell that
// ask is meant to remove.

/// Characters of document body per stored observation. Sized to the retrieval
/// window a host actually reads: big enough that a chunk stands on its own,
/// small enough that a hit is not mostly padding.
const TEXT_CHUNK_CHARS: usize = 1200;
/// Overlap between consecutive chunks, so a sentence on a boundary still lives
/// wholly inside one of them.
const TEXT_CHUNK_OVERLAP: usize = 200;
/// Most chunks stored per file. A bound, not a quality setting: one enormous
/// file must not be able to fill the vault, and the count is reported.
const MAX_CHUNKS_PER_FILE: usize = 400;
/// Mirrors `memnoth-ingest`'s ceiling so both halves agree on what a document is.
const MAX_TEXT_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Extensions whose *text* is worth storing. Prose first — that is the half the
/// ingest crate drops completely — then source and config, because "recall what
/// this file says" is the same request whatever the file is called.
const TEXT_EXTENSIONS: &[&str] = &[
    "md", "markdown", "mdx", "txt", "rst", "org", "adoc", "text", "log", "csv", "tsv", "rs", "py",
    "ts", "tsx", "js", "jsx", "mjs", "go", "java", "rb", "kt", "cs", "c", "h", "cpp", "hpp",
    "swift", "toml", "yaml", "yml", "ini", "cfg", "json", "jsonl", "sql", "sh", "bash", "ps1",
    "psm1", "html", "css", "scss", "vue", "svelte", "ex", "exs", "erl", "hs", "lua", "php", "pl",
    "r", "scala", "clj",
];
/// Kept in step with `memnoth-ingest::SKIP_DIRS`.
const TEXT_SKIP_DIRS: &[&str] = &["target", ".git", "node_modules", ".playwright-mcp", "dist"];
/// Off switch, matching the shape of the other opt-outs (`mrtr::enabled`,
/// `tasks::enabled`). Default is **on**: a memory server that does not remember
/// the text it was handed is the defect this exists to fix.
fn ingest_text_enabled() -> bool {
    !matches!(
        std::env::var("MEMNOTH_MCP_INGEST_TEXT").as_deref(),
        Ok("0") | Ok("off") | Ok("false") | Ok("no")
    )
}

fn is_text_document(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| TEXT_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Split on character boundaries so a multi-byte character is never cut in half
/// (which would corrupt the stored text, not merely shorten it).
fn chunk_text(text: &str, size: usize, overlap: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= size {
        let only = text.trim();
        return if only.is_empty() {
            Vec::new()
        } else {
            vec![only.to_string()]
        };
    }
    let step = size.saturating_sub(overlap).max(1);
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + size).min(chars.len());
        let chunk: String = chars[start..end].iter().collect();
        let chunk = chunk.trim();
        if !chunk.is_empty() {
            out.push(chunk.to_string());
        }
        if end == chars.len() {
            break;
        }
        start += step;
    }
    out
}

/// Store document bodies as observations. Returns `(files, observations)`.
fn store_document_text(root: &Path, engine: &MemoryEngine) -> Result<(usize, usize)> {
    let now = memnoth_engine::now_ms();
    let mut files = 0usize;
    let mut stored = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if !TEXT_SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(path);
                }
                continue;
            }
            if !is_text_document(&path) {
                continue;
            }
            let meta = match path.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.len() > MAX_TEXT_FILE_BYTES {
                continue;
            }
            let body = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let text = String::from_utf8_lossy(&body);
            let chunks = chunk_text(&text, TEXT_CHUNK_CHARS, TEXT_CHUNK_OVERLAP);
            if chunks.is_empty() {
                continue;
            }
            // The file node `ingest_dir` already created, so prose lands on the
            // entity a host already sees for this document rather than beside it.
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let entity_id = match engine.find_entities_by_name(&rel, now)?.first() {
                Some(e) => e.id,
                None => engine.add_entity(EntityKind::File, &rel, 1.0, now)?,
            };
            for (idx, chunk) in chunks.into_iter().take(MAX_CHUNKS_PER_FILE).enumerate() {
                engine.add_observation(
                    entity_id,
                    &format!("{rel}#{idx}: {chunk}"),
                    "text",
                    0.9,
                    now,
                )?;
                stored += 1;
            }
            files += 1;
        }
    }
    Ok((files, stored))
}

/// `mem_ingest` body, shared by the synchronous path and the task worker.
///
/// It does need the vault, unlike `run_kg_index` — which is why the worker hands
/// it a freshly opened engine of its own rather than a borrow of the server's.
fn run_mem_ingest(args: &Value, engine: &MemoryEngine) -> Result<Value> {
    let path = arg_str(args, "path")?;
    let root = Path::new(&path);
    // Counted progress: the sink, when armed, reports files *seen* as the walk
    // finds them. With no `progressToken` the sink is absent and the ingest runs
    // exactly as it did before.
    let report = memnoth_ingest::ingest_dir_with_progress(root, engine, |seen, rel| {
        report_mem_ingest_progress(seen, rel)
    })?;
    MEM_INGEST_SINK.with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut()
            && let Some(sink) = slot.as_mut()
        {
            sink.finish(&report);
        }
    });
    let (text_files, text_observations) = if ingest_text_enabled() {
        store_document_text(root, engine)?
    } else {
        (0, 0)
    };
    Ok(json!({
        "files_seen": report.files_seen,
        "files_ingested": report.files_ingested,
        "files_unchanged": report.files_unchanged,
        "entities_created": report.entities_created,
        "relations_created": report.relations_created,
        "observations_created": report.observations_created,
        "calls_created": report.calls_created,
        "calls_unresolved": report.calls_unresolved,
        // Structure-only was the old contract; these two say that the prose was
        // captured too, so an operator can see what the vault now holds.
        "text_files": text_files,
        "text_observations": text_observations,
    }))
}

/// Register the task, start the work on a worker thread, and return the
/// `CreateTaskResult` in place of the tool's result — or `None` for "run this
/// one synchronously".
///
/// Ordering is load-bearing: the task exists in the registry before the handle
/// goes out, so a client that polls the instant it receives the id always finds
/// it (the extension requires the task to be durably created before the
/// response is sent).
///
/// `None` is a fallback, never a failure: either the tool has no async body, or
/// it needs the vault and this server never had a path to open its own
/// connection with. A deploy that cannot be served in the background keeps
/// exactly the behaviour it had before tasks existed, which is what makes this
/// safe to turn on for every transport.
fn spawn_task(
    name: &str,
    args: &Value,
    vault: Option<&Path>,
    progress_token: Option<&Value>,
) -> Option<Value> {
    match name {
        "kg_index" => {
            // The token has to cross the thread boundary with the work: the
            // worker runs after this call has already returned the task handle,
            // so it arms its own sink rather than inheriting one.
            let token = progress_token.cloned();
            Some(spawn_worker(name, args, move |owned: &Value| {
                let _guard = arm_kg_index_progress("kg_index", token.as_ref());
                crate::kg_index::run_kg_index(owned)
            }))
        }
        // The worker opens its own connection to the same vault file. WAL means
        // readers never block and writers serialize per transaction, so the
        // worst case for a concurrent `mem_add_fact` is waiting on a short lock
        // rather than reading a torn vault.
        "mem_ingest" => {
            // The token crosses the thread boundary with the work, same as the
            // indexer's: the worker runs after the handle has been returned.
            let token = progress_token.cloned();
            let vault = vault?.to_path_buf();
            Some(spawn_worker(name, args, move |owned: &Value| {
                let _guard = arm_mem_ingest_progress("mem_ingest", token.as_ref());
                let engine = memnoth_engine::open_vault(&vault)?;
                run_mem_ingest(owned, &engine)
            }))
        }
        _ => None,
    }
}

/// The common tail of every task-backed tool: register, spawn, hand back the
/// handle. The worker owns its inputs, so nothing borrowed from the request
/// outlives it.
fn spawn_worker<F>(name: &str, args: &Value, work: F) -> Value
where
    F: FnOnce(&Value) -> Result<Value> + Send + 'static,
{
    let task = tasks::create(name, Some(tasks::DEFAULT_TASK_TTL_MS));
    let worker_id = task.task_id.clone();
    let owned = args.clone();
    // Resolved before the thread starts: the task result must be enveloped the
    // same way the synchronous path envelopes it, or `tasks/get` would hand back
    // a payload shaped differently from the identical inline call.
    let structured = declares_output_schema(name);
    std::thread::spawn(move || match work(&owned) {
        Ok(result) => tasks::complete(&worker_id, call_tool_result(result, structured)),
        Err(e) => tasks::fail(
            &worker_id,
            json!({"code": -32603, "message": e.to_string()}),
        ),
    });
    task.to_create_result()
}

/// Record that a search returned these entities, saying so when the write fails.
///
/// The search has already succeeded by the time this runs, and usage accounting
/// must not turn a good answer into an error — so the failure goes to stderr
/// instead of back up the call chain. What it replaces is silence: on a vault
/// that refuses the write, every ranking still looked correct and the access
/// counts simply stopped moving.
fn note_access(engine: &MemoryEngine, entity_id: i64, at: i64) {
    if let Err(error) = engine.record_access(entity_id, at) {
        eprintln!("memnoth-mcp: access not recorded for entity {entity_id}: {error}");
    }
}

/// The MCP surface carried through [`ToolContext::extra`] on every
/// [`Registry::execute`] call.
///
/// [`rusqlite::Connection`] is `Send` but not `Sync`, so neither [`MemoryEngine`]
/// nor [`Coord`] can be held behind `Arc<dyn Any + Send + Sync>` directly. Raw
/// pointers are `Send + Sync` regardless of their referent, and the surface is
/// only ever dereferenced synchronously within a single [`execute`] call, so
/// borrowing the caller's engine/coord for the duration of the call is sound.
#[derive(Debug)]
struct McpSurface {
    engine: *const MemoryEngine,
    coord: *const Coord,
}

impl McpSurface {
    fn new(engine: &MemoryEngine, coord: &Coord) -> Self {
        Self {
            engine: engine as *const MemoryEngine,
            coord: coord as *const Coord,
        }
    }

    fn engine(&self) -> &MemoryEngine {
        unsafe { &*self.engine }
    }

    fn coord(&self) -> &Coord {
        unsafe { &*self.coord }
    }
}
/// Raw pointers are `Send` but not `Sync`; the surface is only ever
/// dereferenced synchronously inside a single [`Registry::execute`] call, so
/// `Sync` is safe here.
unsafe impl Sync for McpSurface {}

unsafe impl Send for McpSurface {}
///
/// The two enums carry the same six names; the conversion is explicit so a new
/// profile cannot silently vanish between surfaces.
fn mcp_profile_to_capability(profile: Profile) -> CapProfile {
    match profile {
        Profile::Micro => CapProfile::Micro,
        Profile::Memory => CapProfile::Memory,
        Profile::Code => CapProfile::Code,
        Profile::Coord => CapProfile::Coord,
        Profile::Full => CapProfile::Full,
        Profile::Agent => CapProfile::Agent,
    }
}
/// Shared schema stub for MCP tools shimmed into the capability registry.
///
/// [`Registry::execute`] never calls [`ToolDef::schema`] — it uses `name` and
/// `cache` for the profile gate and receipt. The wire `tools/list` payload
/// keeps coming from this crate's [`tool_definitions`], so an empty schema is
/// correct here and keeps one schema source of truth.
fn tool_schema_stub() -> Value {
    json!({})
}

macro_rules! tool_def {
    ($name:ident, $wire_name:expr, $desc:expr, $approval:expr, $cost:expr, $cache:expr $(,)?) => {
        const $name: ToolDef = ToolDef {
            name: $wire_name,
            description: $desc,
            schema: tool_schema_stub,
            approval: $approval,
            cost: $cost,
            cache: $cache,
        };
    };
}
// ── vault surface ────────────────────────────────────────────────────────
tool_def!(
    TOOL_MEM_SEARCH,
    "mem_search",
    "Search the memory vault by relevance",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Persistent,
);
tool_def!(
    TOOL_MEM_ADD_FACT,
    "mem_add_fact",
    "Attach an atomic fact to an entity",
    Approval::MutatesWorkspace,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_MEM_FIND_FACTS,
    "mem_find_facts",
    "Full-text search over atomic facts",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Persistent,
);
tool_def!(
    TOOL_MEM_RELATED,
    "mem_related",
    "Graph neighbors of an entity",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Persistent,
);
tool_def!(
    TOOL_MEM_STATS,
    "mem_stats",
    "Vault counters",
    Approval::ReadOnly,
    CostClass::Cheap,
    CacheClass::Session,
);

tool_def!(
    TOOL_MEM_INGEST,
    "mem_ingest",
    "Ingest a directory into the vault",
    Approval::MutatesWorkspace,
    CostClass::Expensive,
    CacheClass::Session,
);
tool_def!(
    TOOL_MEM_DUPLICATES,
    "mem_duplicates",
    "Vault hygiene duplicate groups",
    Approval::ReadOnly,
    CostClass::Expensive,
    CacheClass::Session,
);
tool_def!(
    TOOL_MEM_MERGE,
    "mem_merge",
    "Merge a duplicate vault entity",
    Approval::MutatesWorkspace,
    CostClass::Expensive,
    CacheClass::Session,
);
tool_def!(
    TOOL_MEM_SUPERSEDE,
    "mem_supersede",
    "Revise an entity through time",
    Approval::MutatesWorkspace,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_MEM_FORGET,
    "mem_forget",
    "Retract a belief",
    Approval::MutatesWorkspace,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_VAULT_HISTORY,
    "vault_history",
    "Entity history chain",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_VAULT_ASOF,
    "vault_asof",
    "Belief state at a point in time",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_VAULT_CONTRADICTIONS,
    "vault_contradictions",
    "Contradicted beliefs",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_VAULT_TOP,
    "vault_top",
    "Top entities by access",
    Approval::ReadOnly,
    CostClass::Cheap,
    CacheClass::Session,
);

// ── clock / scratchpad / toolbox ─────────────────────────────────────────
tool_def!(
    TOOL_NOW,
    "now",
    "Wall clock",
    Approval::ReadOnly,
    CostClass::Cheap,
    CacheClass::NoStore,
);
tool_def!(
    TOOL_THINK,
    "think",
    "Sequential thinking scratchpad",
    Approval::ReadOnly,
    CostClass::Cheap,
    CacheClass::NoStore,
);
tool_def!(
    TOOL_FS_LIST,
    "fs_list",
    "Bounded directory listing",
    Approval::ReadOnly,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_FS_READ,
    "fs_read",
    "Read a file",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_GIT_STATUS,
    "git_status",
    "Git working tree status",
    Approval::ReadOnly,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_GIT_DIFF,
    "git_diff",
    "Git diff",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_GIT_LOG,
    "git_log",
    "Git log",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);

// ── coordination surface ─────────────────────────────────────────────────
tool_def!(
    TOOL_COORD_JOIN,
    "coord_join",
    "Join the coordination room",
    Approval::MutatesWorkspace,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_COORD_HEARTBEAT,
    "coord_heartbeat",
    "Keep session alive",
    Approval::MutatesWorkspace,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_COORD_LEAVE,
    "coord_leave",
    "Leave the coordination room",
    Approval::MutatesWorkspace,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_COORD_CLAIM,
    "coord_claim",
    "Claim a path glob",
    Approval::MutatesWorkspace,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_COORD_RELEASE,
    "coord_release",
    "Release a path glob",
    Approval::MutatesWorkspace,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_COORD_STATUS,
    "coord_status",
    "Coordination room status",
    Approval::ReadOnly,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_COORD_TASK_CREATE,
    "coord_task_create",
    "Create a task",
    Approval::MutatesWorkspace,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_COORD_TASK_CLAIM,
    "coord_task_claim",
    "Claim a task",
    Approval::MutatesWorkspace,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_COORD_TASK_LIST,
    "coord_task_list",
    "List tasks",
    Approval::ReadOnly,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_COORD_SEND,
    "coord_send",
    "Send a coordination message",
    Approval::MutatesWorkspace,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_COORD_INBOX,
    "coord_inbox",
    "Read coordination inbox",
    Approval::ReadOnly,
    CostClass::Cheap,
    CacheClass::Session,
);

// ── code-graph surface ───────────────────────────────────────────────────
tool_def!(
    TOOL_KG_INDEX,
    "kg_index",
    "Index a workspace into the code graph",
    Approval::MutatesWorkspace,
    CostClass::Expensive,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_IMPACT,
    "kg_impact",
    "Blast radius of a symbol",
    Approval::ReadOnly,
    CostClass::Expensive,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_TRACE,
    "kg_trace",
    "Paginated direct callers",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_CALLEES,
    "kg_callees",
    "What a symbol calls, as the names mentioned in its body",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_DEF,
    "kg_def",
    "Where a symbol is defined, with its snippet",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_REFS,
    "kg_refs",
    "Every place a symbol is mentioned",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_STATUS,
    "kg_status",
    "Code graph staleness",
    Approval::ReadOnly,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_EXPLORE,
    "kg_explore",
    "Symbol discovery with source snippets",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_SEARCH,
    "kg_search",
    "Symbol search",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_COVERAGE,
    "kg_coverage",
    "Index health by parse status",
    Approval::ReadOnly,
    CostClass::Cheap,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_COMMUNITIES,
    "kg_communities",
    "Code graph communities",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_DEAD,
    "kg_dead",
    "Dead code scan",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_UNRESOLVED,
    "kg_unresolved",
    "Unresolved call sites",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_COMPLEXITY,
    "kg_complexity",
    "Functions by loop depth",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_CHANGES,
    "kg_changes",
    "Git-diff impact",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_DOCTOR,
    "kg_doctor",
    "Code graph health check",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_SERVICES,
    "kg_services",
    "Deployment shape",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_RISK,
    "kg_risk",
    "Taint reach",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);
tool_def!(
    TOOL_KG_WHY,
    "kg_why",
    "Shortest call path",
    Approval::ReadOnly,
    CostClass::Moderate,
    CacheClass::Session,
);

/// Build the [`Registry`] that backs [`tools/call`] for the full tool set.
///
/// Every [`Tool`] impl is a thin shim: argument parsing and the actual work
/// live in this crate's existing bodies, which the shim invokes through
/// [`McpSurface`]. The static definition (schema, approval, cost, cache class)
/// is declared once per tool here so [`Registry::execute`] can apply the
/// profile gate and mint a receipt without the MCP crate maintaining a second
/// table.
/// The one registry every MCP tool call goes through.
///
/// Module-level rather than a function-local static so the transport can ask
/// the same table what a tool requires. A second registry would be a second
/// answer to "what scope gates this tool".
static MCP_REGISTRY: LazyLock<Registry> = LazyLock::new(mcp_registry);

/// The scope an authenticated transport must present to call `tool`.
///
/// Read-only tools need none; a mutating tool needs `mcp:write`, a destructive
/// one `mcp:admin`. `None` also means "not a registry tool" — the transport
/// lets the dispatcher answer that with its own unknown-tool error.
pub fn required_scope(tool: &str) -> Option<&'static str> {
    MCP_REGISTRY.required_scope(tool)
}

fn mcp_registry() -> Registry {
    let mut registry = Registry::empty();

    // ── vault surface ────────────────────────────────────────────────────
    registry.add(McpTool {
        def: &TOOL_MEM_SEARCH,
        call: |args, surface| {
            let query = arg_str(args, "query")?;
            let limit = recall_budget::clamp_limit(arg_usize(args, "limit", 20));
            let rerank_mode: &str = match args.get("rerank") {
                Some(Value::Bool(true)) => "mmr",
                Some(Value::Bool(false)) | None => "off",
                Some(Value::String(s)) => match s.as_str() {
                    "cross_encoder" | "ce" => "cross_encoder",
                    "llm" | "assisted" => "llm",
                    "mmr" => "mmr",
                    _ => "off",
                },
                _ => "off",
            };
            let rerank = rerank_mode != "off";
            let fusion = active_fusion(args.get("fusion").and_then(|v| v.as_str()));
            let over = recall_budget::clamp_rerank_pool(match rerank_mode {
                "cross_encoder" => (limit * ce_pool_factor()).clamp(limit, CE_MAX_POOL),
                "llm" => (limit * LLM_RERANK_POOL_FACTOR).clamp(limit, LLM_RERANK_MAX_POOL),
                "mmr" => (limit * 2).min(100),
                _ => limit,
            });
            let now = memnoth_engine::now_ms();
            let exact = surface.engine().hybrid_search(&query, over, now)?;
            let relax = args
                .get("relaxed")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let (hits, relaxed) = if !relax || exact.len() >= over {
                (exact, false)
            } else {
                let mut merged = exact;
                let before = merged.len();
                let seen: HashSet<i64> = merged.iter().map(|(e, _)| e.id).collect();
                merged.extend(
                    relaxed_search(surface.engine(), &query, over, now, fusion)?
                        .into_iter()
                        .filter(|(e, _)| !seen.contains(&e.id)),
                );
                let relaxed = merged.len() > before;
                (merged, relaxed)
            };
            let mut hits = hits;
            let mut preference_boosted = 0usize;
            if fusion.boosts_preference_docs() && preference_intent(&query) {
                let marked = preference_marked_entities(surface.engine(), now)?;
                if !marked.is_empty() {
                    let factor = preference_boost();
                    for (e, score) in hits.iter_mut() {
                        if marked.contains(&e.id) {
                            *score *= factor;
                            preference_boosted += 1;
                        }
                    }
                    hits.sort_by(|a, b| {
                        b.1.partial_cmp(&a.1)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| a.0.id.cmp(&b.0.id))
                    });
                }
            }
            // The arm that produced this order, never a constant. `"rrf"` was
            // written for every non-relaxed search whatever the fusion was, so
            // a `dense_hybrid` result set was attributed to an arm it had not
            // used, and yesterday's answer became indistinguishable from
            // today's once the default moved.
            let base_ranking = if relaxed {
                format!("{} (relaxed: per-token fusion)", fusion.label())
            } else {
                fusion.label().to_string()
            };
            let base_ranking = if preference_boosted > 0 {
                format!(
                    "{base_ranking} + preference boost x{:.2} on {preference_boosted} candidate(s)",
                    preference_boost()
                )
            } else {
                base_ranking
            };
            let (results, ranking) = if rerank_mode == "cross_encoder" && hits.len() > 1 {
                match rerank_cross_encoder(surface.engine(), &query, &hits, limit, now) {
                    Some((ranked, method)) => {
                        let items: Vec<Value> = ranked
                            .into_iter()
                            .map(|(e, score)| {
                                note_access(surface.engine(), e.id, now);
                                json!({"id": e.id, "name": e.name, "kind": e.kind.as_str(), "score": score})
                            })
                            .collect();
                        let method = if relaxed {
                            format!("{method}; over relaxed ({} per-token fusion)", fusion.label())
                        } else {
                            method.to_string()
                        };
                        (items, method)
                    }
                    None => {
                        let items: Vec<Value> = hits
                            .into_iter()
                            .map(|(e, score)| {
                                note_access(surface.engine(), e.id, now);
                                json!({"id": e.id, "name": e.name, "kind": e.kind.as_str(), "score": score})
                            })
                            .collect();
                        (items, format!("{base_ranking} (cross-encoder unavailable)"))
                    }
                }
            } else if rerank_mode == "llm" && hits.len() > 1 {
                match rerank_llm(surface.engine(), &query, &hits, limit, now) {
                    Some((ranked, method)) => {
                        let items: Vec<Value> = ranked
                            .into_iter()
                            .map(|(e, score)| {
                                note_access(surface.engine(), e.id, now);
                                json!({"id": e.id, "name": e.name, "kind": e.kind.as_str(), "score": score})
                            })
                            .collect();
                        (items, method)
                    }
                    None => {
                        let items: Vec<Value> = hits
                            .into_iter()
                            .map(|(e, score)| {
                                note_access(surface.engine(), e.id, now);
                                json!({"id": e.id, "name": e.name, "kind": e.kind.as_str(), "score": score})
                            })
                            .collect();
                        let reason = if llm::client().is_some() {
                            "llm rerank failed"
                        } else {
                            "llm rerank unavailable: set KILOCODE_API_KEY or GEMINI_API_KEY"
                        };
                        (items, format!("{base_ranking} ({reason})"))
                    }
                }
            } else if rerank && hits.len() > 1 {
                let (ranked, method) = rerank_candidates(surface.engine(), &query, hits, limit, now)?;
                let items: Vec<Value> = ranked
                    .into_iter()
                    .map(|(e, score)| {
                        note_access(surface.engine(), e.id, now);
                        json!({"id": e.id, "name": e.name, "kind": e.kind.as_str(), "score": score})
                    })
                    .collect();
                let method = if relaxed {
                    format!(
                        "mmr over relaxed ({} per-token fusion); no model",
                        fusion.label()
                    )
                } else {
                    method.to_string()
                };
                (items, method)
            } else {
                let items: Vec<Value> = hits
                    .into_iter()
                    .map(|(e, score)| {
                        note_access(surface.engine(), e.id, now);
                        json!({"id": e.id, "name": e.name, "kind": e.kind.as_str(), "score": score})
                    })
                    .collect();
                (items, base_ranking)
            };
            let text_chars = arg_usize(args, "text_chars", DEFAULT_SNIPPET_CHARS);
            let query_tokens = content_tokens(&query);
            let results = attach_evidence(surface.engine(), &query_tokens, results, text_chars, now)?;
            let (results, packing) = pack_search_results(results);
            Ok(json!({"results": results, "ranking": ranking, "packing": packing}))
        },
    });

    registry.add(McpTool {
        def: &TOOL_MEM_ADD_FACT,
        call: |args, surface| {
            let entity_name = arg_str(args, "entity_name")?;
            let content = arg_str(args, "content")?;
            let kind = args
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("fact")
                .to_string();
            let now = memnoth_engine::now_ms();
            let entity_id = match surface
                .engine()
                .find_entities_by_name(&entity_name, now)?
                .first()
            {
                Some(e) => e.id,
                None => surface
                    .engine()
                    .add_entity(EntityKind::Other, &entity_name, 0.9, now)?,
            };
            let observation_id = surface
                .engine()
                .add_observation(entity_id, &content, &kind, 0.9, now)?;
            Ok(json!({"entity_id": entity_id, "observation_id": observation_id}))
        },
    });

    registry.add(McpTool {
        def: &TOOL_MEM_FIND_FACTS,
        call: |args, surface| {
            let query = arg_str(args, "query")?;
            let limit = recall_budget::clamp_limit(arg_usize(args, "limit", 20));
            let now = memnoth_engine::now_ms();
            let hits = surface.engine().search_observations(&query, limit, now)?;
            let items: Vec<Value> = hits
                .into_iter()
                .map(|(o, score)| {
                    note_access(surface.engine(), o.entity_id, now);
                    json!({"entity_id": o.entity_id, "content": o.content, "score": score})
                })
                .collect();
            Ok(json!({"facts": items}))
        },
    });

    registry.add(McpTool {
        def: &TOOL_MEM_RELATED,
        call: |args, surface| {
            let entity = arg_str(args, "entity")?;
            let hops = arg_usize(args, "hops", 1).min(3);
            let now = memnoth_engine::now_ms();
            let found = surface
                .engine()
                .find_entities_by_name(&entity, now)?
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("entity not found: {entity}"))?;
            let mut edges = surface.engine().expand_frontier(&[found.id], now)?;
            for _ in 1..hops {
                let frontier: Vec<i64> = edges.iter().map(|r| r.target_id).collect();
                if frontier.is_empty() {
                    break;
                }
                edges.extend(surface.engine().expand_frontier(&frontier, now)?);
            }
            let items: Vec<Value> = edges
                .into_iter()
                .map(|r| json!({"source": r.source_id, "target": r.target_id, "kind": r.kind}))
                .collect();
            Ok(json!({"edges": items}))
        },
    });

    registry.add(McpTool {
        def: &TOOL_MEM_STATS,
        call: |_args, surface| {
            let s = surface.engine().stats()?;
            Ok(serde_json::to_value(s)?)
        },
    });

    registry.add(McpTool {
        def: &TOOL_MEM_INGEST,
        call: |args, surface| run_mem_ingest(args, surface.engine()),
    });

    registry.add(McpTool {
        def: &TOOL_MEM_DUPLICATES,
        call: |args, surface| {
            let limit = recall_budget::clamp_limit(arg_usize(args, "limit", 20));
            let min_group = arg_usize(args, "min_group", 2);
            let fuzzy = args.get("fuzzy").and_then(|v| v.as_bool()).unwrap_or(false);
            mem_duplicates(surface.engine(), limit, min_group, fuzzy)
        },
    });

    registry.add(McpTool {
        def: &TOOL_MEM_MERGE,
        call: |args, surface| {
            let keeper_id = args
                .get("keeper_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| {
                    anyhow::anyhow!("mem_merge requires a `keeper_id` integer argument")
                })?;
            let merge_id = args
                .get("merge_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| {
                    anyhow::anyhow!("mem_merge requires a `merge_id` integer argument")
                })?;
            let reason = args
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("merged duplicate")
                .to_string();
            let now = memnoth_engine::now_ms();
            mem_merge(surface.engine(), keeper_id, merge_id, &reason, now)
        },
    });

    registry.add(McpTool {
        def: &TOOL_MEM_SUPERSEDE,
        call: |args, surface| {
            let name = arg_str(args, "name")?;
            let reason = arg_str(args, "reason")?;
            let new_name = args
                .get("new_name")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let summary = args
                .get("summary")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let confidence = args.get("confidence").and_then(|v| v.as_f64());
            let now = memnoth_engine::now_ms();
            mem_supersede(
                surface.engine(),
                &name,
                new_name.as_deref(),
                summary.as_deref(),
                confidence,
                &reason,
                now,
            )
        },
    });

    registry.add(McpTool {
        def: &TOOL_MEM_FORGET,
        call: |args, surface| {
            let name = arg_str(args, "name")?;
            let reason = arg_str(args, "reason")?;
            let now = memnoth_engine::now_ms();
            mem_forget(surface.engine(), &name, &reason, now)
        },
    });

    registry.add(McpTool {
        def: &TOOL_VAULT_HISTORY,
        call: |args, surface| tools_agent::vault_history(surface.engine(), args),
    });

    registry.add(McpTool {
        def: &TOOL_VAULT_ASOF,
        call: |args, surface| {
            let now = memnoth_engine::now_ms();
            tools_agent::vault_asof(surface.engine(), args, now)
        },
    });

    registry.add(McpTool {
        def: &TOOL_VAULT_CONTRADICTIONS,
        call: |args, surface| tools_agent::vault_contradictions(surface.engine(), args),
    });

    registry.add(McpTool {
        def: &TOOL_VAULT_TOP,
        call: |args, surface| {
            let now = memnoth_engine::now_ms();
            tools_agent::vault_top(surface.engine(), args, now)
        },
    });

    registry.add(McpTool {
        def: &TOOL_NOW,
        call: |_args, _surface| {
            let now = memnoth_engine::now_ms();
            Ok(json!({"now_ms": now, "epoch_ms": now}))
        },
    });

    registry.add(McpTool {
        def: &TOOL_THINK,
        call: |args, _surface| Ok(tools_agent::think(args)),
    });

    registry.add(McpTool {
        def: &TOOL_FS_LIST,
        call: |args, _surface| tools_agent::fs_list(args),
    });

    registry.add(McpTool {
        def: &TOOL_FS_READ,
        call: |args, _surface| tools_agent::fs_read(args),
    });

    registry.add(McpTool {
        def: &TOOL_GIT_STATUS,
        call: |_args, _surface| tools_agent::git_status(),
    });

    registry.add(McpTool {
        def: &TOOL_GIT_DIFF,
        call: |args, _surface| tools_agent::git_diff(args),
    });

    registry.add(McpTool {
        def: &TOOL_GIT_LOG,
        call: |args, _surface| tools_agent::git_log(args),
    });

    // ── coordination surface ─────────────────────────────────────────────
    registry.add(McpTool {
        def: &TOOL_COORD_JOIN,
        call: |args, surface| {
            let name = arg_str(args, "name")?;
            let root = db_parent_display(args);
            let session_id = surface.coord().join(&name, std::process::id(), &root)?;
            Ok(json!({"session_id": session_id}))
        },
    });

    registry.add(McpTool {
        def: &TOOL_COORD_HEARTBEAT,
        call: |args, surface| {
            surface.coord().heartbeat(&arg_str(args, "session_id")?)?;
            Ok(json!({"ok": true}))
        },
    });

    registry.add(McpTool {
        def: &TOOL_COORD_LEAVE,
        call: |args, surface| {
            surface.coord().leave(&arg_str(args, "session_id")?)?;
            Ok(json!({"ok": true}))
        },
    });

    registry.add(McpTool {
        def: &TOOL_COORD_CLAIM,
        call: |args, surface| {
            let sid = arg_str(args, "session_id")?;
            let glob = arg_str(args, "path_glob")?;
            let ttl = args
                .get("ttl_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(1800);
            match surface.coord().claim(&sid, &glob, ttl)? {
                ClaimResult::Granted { claim_id } => {
                    Ok(json!({"granted": true, "claim_id": claim_id}))
                }
                ClaimResult::Refused {
                    holder_session,
                    holder_glob,
                } => Ok(json!({
                    "granted": false,
                    "holder_session": holder_session,
                    "holder_glob": holder_glob,
                })),
            }
        },
    });

    registry.add(McpTool {
        def: &TOOL_COORD_RELEASE,
        call: |args, surface| {
            surface
                .coord()
                .release(&arg_str(args, "session_id")?, &arg_str(args, "path_glob")?)?;
            Ok(json!({"ok": true}))
        },
    });

    registry.add(McpTool {
        def: &TOOL_COORD_STATUS,
        call: |_args, surface| {
            let sessions: Vec<SessionStatus> = surface.coord().status()?;
            Ok(serde_json::to_value(sessions)?)
        },
    });

    registry.add(McpTool {
        def: &TOOL_COORD_TASK_CREATE,
        call: |args, surface| {
            let title = arg_str(args, "title")?;
            let description = args
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let depends_on = args.get("depends_on").and_then(|v| v.as_str());
            let task_id = surface
                .coord()
                .task_create(&title, description, depends_on)?;
            Ok(json!({"task_id": task_id}))
        },
    });

    registry.add(McpTool {
        def: &TOOL_COORD_TASK_CLAIM,
        call: |args, surface| {
            surface
                .coord()
                .task_claim(&arg_str(args, "task_id")?, &arg_str(args, "session_id")?)?;
            Ok(json!({"ok": true}))
        },
    });

    registry.add(McpTool {
        def: &TOOL_COORD_TASK_LIST,
        call: |_args, surface| {
            let tasks: Vec<TaskRow> = surface.coord().task_list()?;
            Ok(serde_json::to_value(tasks)?)
        },
    });

    registry.add(McpTool {
        def: &TOOL_COORD_SEND,
        call: |args, surface| {
            let from = arg_str(args, "from_session")?;
            let to = args.get("to_session").and_then(|v| v.as_str());
            let body = arg_str(args, "body")?;
            let message_id = surface.coord().send_message(&from, to, &body)?;
            Ok(json!({"message_id": message_id}))
        },
    });

    registry.add(McpTool {
        def: &TOOL_COORD_INBOX,
        call: |args, surface| {
            let sid = arg_str(args, "session_id")?;
            let include_broadcast = args
                .get("include_broadcast")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let messages: Vec<MessageRow> = surface.coord().inbox(&sid, include_broadcast)?;
            Ok(serde_json::to_value(messages)?)
        },
    });

    // ── code-graph surface ───────────────────────────────────────────────
    registry.add(McpTool {
        def: &TOOL_KG_INDEX,
        call: |args, _surface| crate::kg_index::run_kg_index(args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_IMPACT,
        call: |args, _surface| crate::kg_tools::call("kg_impact", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_TRACE,
        call: |args, _surface| crate::kg_tools::call("kg_trace", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_CALLEES,
        call: |args, _surface| crate::kg_tools::call("kg_callees", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_DEF,
        call: |args, _surface| crate::kg_tools::call("kg_def", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_REFS,
        call: |args, _surface| crate::kg_tools::call("kg_refs", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_STATUS,
        call: |args, _surface| crate::kg_tools::call("kg_status", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_EXPLORE,
        call: |args, _surface| crate::kg_tools::call("kg_explore", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_SEARCH,
        call: |args, _surface| crate::kg_tools::call("kg_search", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_COVERAGE,
        call: |args, _surface| crate::kg_tools::call("kg_coverage", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_COMMUNITIES,
        call: |args, _surface| crate::kg_tools::call("kg_communities", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_DEAD,
        call: |args, _surface| crate::kg_tools::call("kg_dead", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_UNRESOLVED,
        call: |args, _surface| crate::kg_tools::call("kg_unresolved", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_COMPLEXITY,
        call: |args, _surface| crate::kg_tools::call("kg_complexity", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_CHANGES,
        call: |args, _surface| crate::kg_tools::call("kg_changes", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_DOCTOR,
        call: |args, _surface| crate::kg_tools::call("kg_doctor", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_SERVICES,
        call: |args, _surface| crate::kg_tools::call("kg_services", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_RISK,
        call: |args, _surface| crate::kg_tools::call("kg_risk", args),
    });

    registry.add(McpTool {
        def: &TOOL_KG_WHY,
        call: |args, _surface| crate::kg_tools::call("kg_why", args),
    });

    registry
}

/// One MCP tool: a static definition plus the body that runs it.
///
/// The body runs synchronously on the caller's thread and borrows the engine
/// and coord through [`McpSurface`] for the duration of the call only.
struct McpTool {
    def: &'static ToolDef,
    call: fn(&Value, &McpSurface) -> Result<Value>,
}

// `McpTool` is `Send + Sync` automatically: its fields are `&'static ToolDef`
// and a `fn` pointer, both of which are `Send + Sync`.

impl Tool for McpTool {
    fn definition(&self) -> &'static ToolDef {
        self.def
    }

    fn call(&self, args: &Value, context: &ToolContext) -> Result<Value> {
        let surface = context
            .extra::<McpSurface>()
            .ok_or_else(|| anyhow::anyhow!("MCP surface not bound to tool context"))?;
        (self.call)(args, surface)
    }
}

/// Run one tool body through [`Registry::execute`] and convert the [`Call`]
/// into the MCP `tools/call` result envelope.
///
/// The receipt from [`Registry::execute`] is attached to `_meta` by the
/// caller (`with_receipt`); this returns the bare payload plus, on an
/// `input_required`/`task` interim result, the payload as-is so MRTR and
/// Tasks keep their own envelopes.
fn run_via_registry(
    registry: &Registry,
    name: &str,
    args: &Value,
    engine: &MemoryEngine,
    coord: &Coord,
    vault: Option<&Path>,
    profile: Profile,
) -> Result<Value> {
    let surface = McpSurface::new(engine, coord);
    let workspace = vault.unwrap_or(Path::new(""));
    let context = ToolContext::new(workspace).with_extra(surface);
    let cap_profile = mcp_profile_to_capability(profile);
    match registry.execute(name, args, &context, cap_profile) {
        Call::Ok { value, .. } => Ok(value),
        Call::Refused { reason, .. } => {
            anyhow::bail!("{reason}")
        }
        Call::Failed { error, .. } => anyhow::bail!("{error}"),
    }
}

fn tools_call(
    engine: &MemoryEngine,
    coord: &Coord,
    profile: Profile,
    params: &Value,
    principal: Option<&str>,
    vault: Option<&Path>,
) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing tool name"))?;
    // `memnoth mcp tools --counts`: per-profile tool counts and bytes, derived
    // from `memnoth_capability::surface_report` so every surface agrees. This
    // pseudo-tool is not in `TOOL_PROFILES` and bypasses the profile gate below.
    if name == "counts" {
        let rows: Vec<Value> = memnoth_capability::profile::surface_report()
            .into_iter()
            .map(|row| {
                let profile = crate::Profile::parse(Some(row.profile));
                let definitions = visible_tools(profile);
                json!({
                    "profile": row.profile,
                    "tools": row.tool_count,
                    "bytes": serde_json::to_string(&definitions)
                        .map(|s| s.len())
                        .unwrap_or(0),
                })
            })
            .collect();
        return Ok(Value::Array(rows));
    }

    // M3 compat aliases: a host configured for a rival memory server keeps
    // calling `retain`/`recall`/`create_entities`/`search_nodes`. In the full
    // profile those resolve to the native tool *before* the gate below, so a
    // migration needs no config rewrite. Narrow profiles are deliberately not
    // aliased, so their `tools/list` never carries rival vocabulary.
    let is_full_profile = crate::profile_name(profile).eq_ignore_ascii_case("full");
    let name = if is_full_profile {
        compat::resolve(name).unwrap_or(name)
    } else {
        name
    };
    if !profile.allows(name) {
        anyhow::bail!(
            "tool '{name}' is not available in the '{}' tool profile; restart with \
              MEMNOTH_MCP_PROFILE=full to enable every memory/coordination/kg tool",
            crate::profile_name(profile)
        );
    }
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    // Policy gate: blocking and fail-closed for mutating tools, deliberately
    // separate from the fail-open observability hooks below. A refusal is
    // synchronous, before any approval flow, and names the file and the exact
    // entry that produced it — the smallest request that can widen the rule.
    let policy_path = policy::path_from_env_or_vault(vault);
    let policy = policy::load(policy_path.as_deref());
    let mutating = required_scope(name).is_some();
    if let policy::Decision::Deny { rule } = policy.decide(name, mutating) {
        anyhow::bail!(
            "tool `{name}` is denied by {rule}. The smallest change that would allow it is \
             removing that rule; the agent cannot change the policy itself."
        );
    }

    // Plugin hook point (fail-open, detached): lets plugin authors observe or
    // audit tool calls without touching the daemon hook engine.
    dispatch_plugin_hook(PluginHookPoint::OnToolCall, name);

    // Multi-round-trip confirmation for bulk writes (SEP-2322). Sits ahead of
    // dispatch so no code path can reach a bulk write without passing it, and
    // the only way past is the explicit env switch, a host that cannot be
    // asked, or a retry carrying a verified single-use approval.
    match mrtr::decision(name, &args, params, principal) {
        mrtr::Decision::Proceed => {}
        mrtr::Decision::Ask(result) => return Ok(result),
        mrtr::Decision::Deny(reason) => anyhow::bail!(reason),
    }

    // Long-running tools answer with a task handle instead of blocking past the
    // host's tool-call timeout — but only for a client that declared the Tasks
    // extension in its per-request capabilities. Everyone else (every legacy
    // client, and any modern host that has not opted in) gets the synchronous
    // answer it has always had, which is why enabling this cannot break a
    // caller that never asked for it.
    let progress = progress_token(params);
    if tasks::enabled()
        && TASK_ELIGIBLE_TOOLS.contains(&name)
        && tasks::client_declared(params)
        && let Some(handle) = spawn_task(name, &args, vault, progress.as_ref())
    {
        return Ok(handle);
    }
    // Every path below runs the tool inline on this thread, so `kg_index` and
    // `mem_ingest` can report real progress from here. The guards cover the
    // whole dispatch, so no early return inside them can leave a sink armed
    // for the next call.
    let _progress_guard = arm_kg_index_progress(name, progress.as_ref());
    let _ingest_progress_guard = arm_mem_ingest_progress(name, progress.as_ref());

    run_via_registry(&MCP_REGISTRY, name, &args, engine, coord, vault, profile)
}

#[cfg(test)]
mod tool_bus_tests {
    use super::*;

    /// A temporary vault path unique enough to run alongside the suite.
    fn temp_vault() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memnoth-mcp-bus-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("vault.db")
    }

    #[test]
    fn bus_executes_every_full_tool_through_registry() {
        let db = temp_vault();
        let _engine = memnoth_engine::open_vault(&db).unwrap();
        let _coord = Coord::open(&db).unwrap();
        let registry = mcp_registry();

        // Every row in the shared table must be runnable by the registry under
        // `Full`, and every tool the registry holds must have a row.
        let names: Vec<&str> = registry.names();
        assert!(!names.is_empty(), "registry holds no tools");
        for (name, profiles) in memnoth_capability::TOOL_PROFILES {
            if profiles.contains(&memnoth_capability::Profile::Full) {
                assert!(
                    names.contains(name),
                    "`{name}` is in the shared table under `full` but absent from the MCP registry"
                );
            }
        }
        for name in &names {
            assert!(
                memnoth_capability::TOOL_PROFILES
                    .iter()
                    .any(|(row, profiles)| row == name
                        && profiles.contains(&memnoth_capability::Profile::Full)),
                "`{name}` is in the MCP registry but has no `full` row in the shared table"
            );
        }
    }

    #[test]
    fn bus_receipt_shape_matches_capability_receipt() {
        // [`Receipt`] in `memnoth-capability/src/receipt.rs:64` carries exactly
        // { tool, ms, bytes_out, tokens_out, tokens_saved_est, cache, profile,
        // refused, reason } — this test pins that shape by serializing a
        // measured receipt and asserting on the key set.
        let receipt = Receipt::measured(
            "mem_stats",
            "full",
            CacheClass::Session,
            1234,
            std::time::Instant::now(),
        );
        let serialized = serde_json::to_value(&receipt).unwrap();
        let expected: Vec<&str> = vec![
            "tool",
            "ms",
            "bytes_out",
            "tokens_out",
            "tokens_saved_est",
            "cache",
            "profile",
            "refused",
            "reason",
        ];
        for key in &expected {
            assert!(
                serialized.get(*key).is_some(),
                "receipt missing `{key}`: {serialized}"
            );
        }
        assert_eq!(serialized.as_object().unwrap().len(), expected.len());
    }

    #[test]
    fn bus_mcp_and_runtime_profile_parity_for_shared_names() {
        // Cross-surface parity: for every tool name that appears in both the
        // MCP server's exposure table (`memnoth_capability::TOOL_PROFILES`) and
        // the runtime router's table (`RUNTIME_TOOL_PROFILES`), the set of
        // profiles exposing it must be identical on both sides — the symmetric
        // difference is empty.
        //
        // Deliberately excluded: `mem_search` (MCP) vs `vault_recall` (runtime)
        // name the same capability under different names, so they never share a
        // row and are not compared here; likewise agent-family tools live only
        // in `agent::AGENT_TOOLS` and are transport-gated, not in either table.
        let mcp: std::collections::BTreeMap<&str, Vec<memnoth_capability::Profile>> =
            memnoth_capability::TOOL_PROFILES
                .iter()
                .map(|(name, profiles)| (*name, profiles.to_vec()))
                .collect();
        let runtime: std::collections::BTreeMap<&str, Vec<memnoth_capability::Profile>> =
            memnoth_capability::RUNTIME_TOOL_PROFILES
                .iter()
                .map(|(name, profiles)| (*name, profiles.to_vec()))
                .collect();

        let mut mismatches = Vec::new();
        for name in mcp.keys() {
            if let Some(runtime_profiles) = runtime.get(name) {
                let mcp_profiles = mcp.get(name).unwrap();
                if mcp_profiles != runtime_profiles {
                    mismatches.push(format!(
                        "{name}: mcp={mcp_profiles:?} runtime={runtime_profiles:?}"
                    ));
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "shared-name profile disagreement (symmetric difference non-empty):\n{}",
            mismatches.join("\n")
        );
    }

    #[test]
    fn bus_call_delegates_through_registry() {
        let db = temp_vault();
        let engine = memnoth_engine::open_vault(&db).unwrap();
        let coord = Coord::open(&db).unwrap();
        let registry = mcp_registry();

        // `now` is an MCP-family tool the runtime router does not own; reaching
        // it through `run_via_registry` proves the delegation lands.
        let value = run_via_registry(
            &registry,
            "now",
            &json!({}),
            &engine,
            &coord,
            None,
            Profile::Full,
        )
        .unwrap();
        assert!(
            value.get("now_ms").is_some() || value.get("epoch_ms").is_some(),
            "unexpected `now` payload: {value}"
        );

        // Profile gate still applies through the bus.
        let err = run_via_registry(
            &registry,
            "git_status",
            &json!({}),
            &engine,
            &coord,
            None,
            Profile::Memory,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("git_status") || err.to_string().contains("profile"),
            "refusal must name the tool/profile: {err}"
        );
    }
}

pub(crate) fn arg_str(args: &Value, key: &str) -> Result<String> {
    match args.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => anyhow::bail!("argument `{key}` must be a string, got: {other}"),
        None => anyhow::bail!("missing string argument: {key}"),
    }
}

pub(crate) fn arg_usize(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(default)
}

/// The parse ledger for one workspace: the **single** answer to "how out of date
/// is the code graph, and does that permit caching?".
///
/// Every consumer comes through here — the `mem://index/status` resource,
/// `kg_status`, `kg_coverage`, and the in-band `staleness_notice` on the other
/// `kg_*` reads. Before this existed, seven call sites each ran their own
/// `staleness_report` and each opened the code graph a *second* time to do it:
/// one workspace could be described two different ways in one session, and every
/// `kg_*` read paid for an extra connection to say so.
pub(crate) struct Freshness {
    /// What the ledger implies for a host's caching (see [`cache_ttl`]).
    kind: CacheKind,
    /// The ledger itself: generation, stale file count, coverage.
    pub(crate) report: StalenessReport,
}

impl Freshness {
    /// The notice an agent sees attached to a result it is already reading.
    /// `None` when nothing is outstanding — absence is the signal, so a fresh
    /// index adds no field at all.
    fn notice(&self) -> Option<String> {
        if self.report.stale_files == 0 {
            return None;
        }
        Some(format!(
            "⚠️ {} file(s) were edited since the last index sync and are pending \
             re-index. Read those files directly for accurate content; everything \
             else is fresh. Run kg_index (incremental) to refresh them.\n",
            self.report.stale_files
        ))
    }
}

/// Read the ledger off a store the caller **already has open**, so answering
/// "is this stale?" never costs a second connection to the same database.
pub(crate) fn freshness_of(store: &Store, root: &Path, max_checks: usize) -> Option<Freshness> {
    let report = memnoth_codegraph::staleness_report(store, root, max_checks).ok()?;
    Some(Freshness {
        kind: CacheKind::WorkspaceIndex {
            stale_files: report.stale_files,
            watcher_pending: report.watcher_pending,
        },
        report,
    })
}

/// The same answer for a caller with no store open (the resource paths).
/// `None` when there is no code graph to read — never a fabricated "fresh".
fn workspace_freshness(root: &Path, max_checks: usize) -> Option<Freshness> {
    let db = root.join(".memnoth/kg.db");
    if !db.exists() {
        return None;
    }
    freshness_of(&open_store(&db).ok()?, root, max_checks)
}

/// Attach the in-band notice to a `kg_*` read, from the ledger that tool already
/// opened. One call per tool, one sentence for all of them.
/// Communities at a requested width.
///
/// Deliberately bypasses `memnoth_codegraph::load_or_compute`, which serves a
/// generation-stamped cache, because that cache cannot answer this tool
/// honestly. `load_clusters` rebuilds the report with `truncated: false` and
/// never persists the flag, so a map clipped to a narrower width is
/// indistinguishable from a genuinely small graph — a host would be told it had
/// seen every community when it had not. The cache is also *shared*: it is the
/// same `clusters` table the `kg map` CLI reads, so persisting a narrow map
/// here would silently shrink another consumer's answer. Label propagation is
/// deterministic and cheap ("same index, same clusters"), so the tool computes
/// it directly and writes nothing. Filed for the KG lane (LANE-BUS `L-MCP-27`):
/// persist `truncated`, after which this can go back to the cached path.
pub(crate) fn load_communities(
    store: &Store,
    limit: usize,
) -> Result<memnoth_codegraph::MapReport> {
    memnoth_codegraph::compute_clusters(store, limit.clamp(1, 200))
}

/// Query relaxation: turn a natural-language ask into something the vault's
/// FTS can actually answer.
///
/// The vault's entity index holds names and summaries, and its query builder
/// quotes every whitespace token into a single implicit-AND expression. So
/// "what happens with the deploy pipeline cache that we discussed" requires
/// `what`, `happens`, `with`, `the` and `discussed` to all appear in a *name* —
/// and returns nothing, while the answer sits in a fact two rows away. Measured
/// before this existed: R@1 = R@5 = R@10 = 0.0% on a corpus that contained every
/// answer (see `eval.rs`). A retriever that only works when the caller already
/// speaks FTS is not a memory.
///
/// So: drop stopwords and short tokens, then fuse **per-token** searches with
/// RRF instead of AND-ing them into one query. That is OR-ish behaviour built
/// out of the public API, and it is the difference between "found nothing" and
/// "ranked the right memory". Fact hits are aggregated up to the entities that
/// own them, so a query phrased like prose still lands on a memory.
///
/// Bounded on purpose: at most `RELAX_MAX_TOKENS` single-token searches, each
/// capped, and the fusion reuses the same RRF constant the engine uses so the
/// two orderings are comparable.
const RELAX_MAX_TOKENS: usize = 8;

/// Words that carry no retrieval signal and are the usual reason an AND query
/// matches nothing. Deliberately small: this is a recall aid, not an NLP stack,
/// and an unknown word is better searched than dropped.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "what", "when", "where", "which", "who", "why",
    "how", "does", "did", "was", "were", "are", "is", "has", "have", "had", "into", "from",
    "about", "there", "their", "its", "you", "your", "our", "us", "we", "it", "of", "in", "on",
    "to", "at", "by", "as", "or", "an", "be", "not", "all", "any", "can",
];

/// Every searchable term in a text, in order, deduplicated — **uncapped**.
///
/// Split out from [`content_tokens`] so that document text and query text are
/// tokenized by one function. They were about to diverge: a query is capped at
/// `RELAX_MAX_TOKENS` because each token costs two bounded index queries, but a
/// *document* being mined for expansion terms must not be capped, and two copies of
/// "split, lowercase, drop stopwords, drop unlexable" is two chances to disagree
/// about what a term is.
fn text_terms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(|t| t.to_lowercase())
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        // W4.4 — a token the index cannot lexeme is not a token. FTS5 drops a term
        // it cannot tokenize, so searching one costs a query and can contribute
        // nothing; dropping it here keeps the fusion's arithmetic about the terms
        // that actually exist in the index. `sanitize_fts_query` is the same
        // function the engine uses to decide what a query term *is*, so this filter
        // cannot drift from the index's own answer.
        .filter(|t| !MemoryEngine::sanitize_fts_query(t).is_empty())
        .collect();
    out.dedup();
    out
}

fn content_tokens(query: &str) -> Vec<String> {
    let mut out = text_terms(query);
    // Order by *discriminating power*, not length. An identifier like `007` or
    // `v2` is short and almost always the token that pins the answer, so
    // ranking by length alone dropped exactly the token that mattered (which is
    // what the first evidence run measured). Tokens carrying a digit lead, then
    // longer tokens — a cheap proxy for rarity that needs no index statistics.
    out.sort_by(|a, b| {
        let a_digit = a.chars().any(|c| c.is_ascii_digit());
        let b_digit = b.chars().any(|c| c.is_ascii_digit());
        b_digit
            .cmp(&a_digit)
            .then(b.len().cmp(&a.len()))
            .then(a.cmp(b))
    });
    // A total order, so duplicates are adjacent and the cap can never split a pair.
    out.dedup();
    out.truncate(RELAX_MAX_TOKENS);
    out
}

/// How deep each token is searched in the relaxed pass.
///
/// This is a **recall ceiling, not a tuning knob**: a token whose best hits sit
/// below this depth cannot contribute at all, so a vault larger than the window
/// silently loses documents that the engine itself ranked. It was 20, which the
/// external LoCoMo run showed costs real recall once documents are chunked
/// (272 documents, ~1000 chunks, and every token competing inside a window that
/// held a fiftieth of them). It was then 50, raised to 150 after the **flat**
/// haystack run — the shape the third-party harness uses, where all 272 documents
/// are one corpus and a common first name saturates the window with chunks of a
/// single long session before another document's chunk is ever seen. Bounded
/// work, not a per-vault cost: two limited FTS queries per token.
/// Raised 150 → 400 on 2026-09-17, and the reason is a **budget nobody was
/// spending**. The lane's own latency measurement is p50 **1.94 ms** against a
/// ≤10 ms budget, so the depth window was the cheapest source of ceiling left: a
/// token whose best hits sit below the window cannot contribute at all, which means
/// every document ranked 151–400 for a query token was *invisible* to fusion while
/// the engine had already ranked it. `any@1 ≤ any@10`, and the tier-1 targets are
/// `any@5 ≥ 95%` — so lifting the ceiling is the only route, and this is bounded
/// work rather than a per-vault cost: two `LIMIT`ed FTS queries per token.
///
/// It is a knob, not a constant to defend: `MEMNOTH_MCP_TOKEN_DEPTH` sweeps it, and
/// the sweep is what should fix the value at the measured knee rather than this
/// comment.
const PER_TOKEN_DEPTH: usize = 400;

/// `PER_TOKEN_DEPTH` for this process, overridable by `MEMNOTH_MCP_TOKEN_DEPTH`.
///
/// W1.4 asks for a recall-versus-latency curve rather than a defended constant,
/// and a sweep is only honest if every point is produced by the *shipping* code
/// path. An environment knob is how the arena produces those points without a
/// second retriever living beside the one we ship — and it is undocumented in the
/// tool schema for the same reason `fusion` is: a knob no host sets must not cost
/// context bytes.
fn per_token_depth() -> usize {
    std::env::var("MEMNOTH_MCP_TOKEN_DEPTH")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(PER_TOKEN_DEPTH)
}

/// Largest length divisor the bounded normalization may apply (W2.2).
///
/// Rank fusion over a 60-constant spans `1/61 .. 1/360` — a **5.9× spread** — so
/// any adjustment inside that range can only reorder what fusion found, while an
/// adjustment *outside* it decides the order by itself and lets the adjusted arm
/// fill the candidate cap. `sqrt(150) = 12.2`, so the unbounded divisor leaves the
/// range at the top end, and 3.0 keeps the correction strictly inside it.
const NORM_MAX_DIVISOR: f64 = 3.0;

/// How far down a ranked list the temporal-intent boost may reach, and how much
/// it counts for (W4.1).
///
/// Bounded because the pass reads each candidate's facts, and a boost is a
/// tie-breaker between memories that are already comparably relevant — not a
/// licence for a date mention to outrank a better answer.
const TEMPORAL_INTENT_WINDOW: usize = 256;
const TEMPORAL_DATE_BOOST: f64 = 1.5;

/// Which tokens the per-token pass will actually search, in `probed` order.
///
/// Extracted so the contract is a function rather than an accident of loop
/// ordering, and so it can be tested. The contract is **selection can never block
/// recall**: however many tokens are skipped as ubiquitous or unlexable, the
/// rarest one is always searched, so a query can be made slow by its own filters
/// but never empty. hindsight documents the same rule after a term-selection bug
/// hung production for 60 seconds; here it is stated where a test can force it.
fn select_searchable_tokens(probed: &[(String, f64, bool)], total_docs: f64) -> Vec<usize> {
    let mut chosen: Vec<usize> = Vec::new();
    for (index, (_, df, df_capped)) in probed.iter().enumerate() {
        // A capped probe means ≥DF_PROBE_LIMIT documents carry this token: it cannot
        // discriminate inside a depth window, and searching it costs a posting-list
        // walk the probe was bounded to avoid. Skip it whenever a cheaper token
        // exists — which is exactly `chosen` being non-empty.
        if !chosen.is_empty()
            && total_docs > 0.0
            && (*df_capped || df / total_docs > COMMON_TOKEN_DF_RATIO)
        {
            continue;
        }
        chosen.push(index);
    }
    // Unreachable while the guard above is written as it is, and kept anyway: this
    // is the fallback that makes the contract hold even after the rule is edited,
    // which is the difference between a property and a coincidence.
    if chosen.is_empty() && !probed.is_empty() {
        chosen.push(0);
    }
    chosen
}

/// Dates the *question* names, as lowercase tokens (W4.1).
///
/// A four-digit year in range, or a full month name. Two rules keep this from
/// becoming a filter that deletes answers, and both were earned by a test failure:
///
/// * **Short month names are excluded entirely.** `mar`, `dec` and `jun` are
///   ordinary word fragments far more often than they are dates, and this list
///   gates a filter that must match *every* candidate's facts — so a false positive
///   removes the answer rather than merely re-ranking it.
/// * **`may` is accepted only where the question capitalised it.** It is a real
///   month name and an extremely common modal verb, so it cannot be decided
///   case-insensitively. A host that sends `march 2024` and one that sends
///   `May 2024` both keep their date; one that sends `you may delete that` keeps
///   its answer. When the month is lost, the year — the token that actually pins a
///   LongMemEval question — is still there.
fn query_date_tokens(query: &str) -> Vec<String> {
    const MONTHS: &[&str] = &[
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
        "sept",
    ];
    let mut out: Vec<String> = Vec::new();
    for raw in query.split(|c: char| !c.is_alphanumeric()) {
        let token = raw.to_lowercase();
        if token.is_empty() {
            continue;
        }
        let is_year = token.len() == 4
            && token.chars().all(|c| c.is_ascii_digit())
            && token
                .parse::<u32>()
                .map(|year| (1900..2100).contains(&year))
                .unwrap_or(false);
        // `may` is the one month name that is also a common verb, so it is decided
        // by the question's own capitalisation rather than by the word alone.
        let is_month = MONTHS.contains(&token.as_str())
            && (token != "may" || raw.chars().next().is_some_and(char::is_uppercase));
        if is_year || is_month {
            out.push(token);
        }
    }
    out.dedup();
    out
}

/// Promote candidates whose facts carry the date the question named (W4.1).
fn apply_temporal_intent(
    engine: &MemoryEngine,
    query: &str,
    ranked: &mut [(Entity, f64)],
    now: i64,
) -> Result<()> {
    let dates = query_date_tokens(query);
    if dates.is_empty() || ranked.len() < 2 {
        return Ok(());
    }
    let window = ranked.len().min(TEMPORAL_INTENT_WINDOW);
    for (entity, score) in ranked.iter_mut().take(window) {
        let mut text = String::new();
        for observation in engine.observations_for(entity.id, now)? {
            text.push_str(&observation.content);
            text.push(' ');
        }
        let haystack = text.to_lowercase();
        // Every named date must be present. Requiring all of them is what keeps
        // this a filter on the *question's* time anchor rather than a popularity
        // contest between documents that mention any year at all.
        if dates.iter().all(|date| haystack.contains(date.as_str())) {
            *score *= TEMPORAL_DATE_BOOST;
        }
    }
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(())
}

/// Pseudo-relevance feedback (W1.6) — the **model-free ceiling lever**.
///
/// Every class whose ceiling is the binding constraint fails the same way: the
/// question's vocabulary is not the document's. Open-domain sits at a **73.9%**
/// `any@10`, and `any@1` can never exceed it, so no reranker can reach 95 until
/// candidates come from somewhere new. Dense retrieval attacks that with a model;
/// this attacks it with the corpus itself — the classic Rocchio fix:
///
/// 1. rank once with the shipping arm;
/// 2. mine the terms the top hits **share** and the query lacks;
/// 3. rank again with those terms added;
/// 4. fuse the two rankings, weighting the original higher.
///
/// It needs no model, so its number can be published as `raw`. It is bounded at
/// every step (seeds, facts per seed, terms, and a fixed pool factor) because a
/// feedback pass that grows with the corpus would be a new scaling problem instead
/// of a new ranking.
///
/// The failure mode it must avoid is **query drift** — feedback terms pulled from an
/// irrelevant top hit send the second pass somewhere else entirely. Two guards, and
/// both are deliberate: a term must appear in **more than one** seed document
/// (agreement, not a single document's vocabulary), and the original ranking keeps
/// weight 1.0 against `PRF_EXPANSION_WEIGHT`, so a drifted second pass cannot
/// outvote a confident first one.
const PRF_SEED_DOCS: usize = 8;
const PRF_FACTS_PER_SEED: usize = 8;
const PRF_MAX_TERMS: usize = 6;
const PRF_EXPANSION_WEIGHT: f64 = 0.6;
const PRF_POOL_FACTOR: usize = 3;

/// The dense arm's share of the fused rank contribution.
///
/// `DenseHybrid` keeps **1.0** — the arm that was measured, measured with that
/// value, and changing it in place would make its recorded number unattributable.
/// `DenseWeighted` defaults to **0.5**, so the dense arm only outranks a lexical hit
/// when it is more than twice as confident. `MEMNOTH_DENSE_WEIGHT` overrides either,
/// which is how the weight itself becomes a measurable knob instead of a belief.
fn dense_weight(fusion: Fusion) -> f64 {
    let default = match fusion {
        Fusion::DenseWeighted => 0.5,
        _ => 1.0,
    };
    std::env::var("MEMNOTH_DENSE_WEIGHT")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|w| *w >= 0.0)
        .unwrap_or(default)
}

/// A PRF pass: rank, mine, rank again, fuse. See [`PRF_SEED_DOCS`].
fn prf_search(
    engine: &MemoryEngine,
    query: &str,
    limit: usize,
    now: i64,
) -> Result<Vec<(Entity, f64)>> {
    const K: f64 = 60.0;
    // A deeper pool than the caller asked for, so a candidate the first pass ranked
    // just outside the cut is still available to be promoted by the second.
    let pool = (limit * PRF_POOL_FACTOR).max(limit);
    let base = relaxed_search(engine, query, pool, now, Fusion::Bm25Graph)?;
    let terms = expansion_terms(engine, query, &base, now)?;
    if terms.is_empty() {
        return Ok(base.into_iter().take(limit).collect());
    }
    // The same arm, one query later. Using `Bm25Graph` for both passes is what keeps
    // this an experiment about *feedback* rather than about the retriever.
    let expanded_query = format!("{query} {}", terms.join(" "));
    let second = relaxed_search(engine, &expanded_query, pool, now, Fusion::Bm25Graph)?;

    let mut fused: HashMap<i64, (Entity, f64)> = HashMap::with_capacity(base.len());
    for (rank, (e, _)) in base.iter().enumerate() {
        let slot = fused.entry(e.id).or_insert_with(|| (e.clone(), 0.0));
        slot.1 += 1.0 / (K + rank as f64);
    }
    for (rank, (e, _)) in second.iter().enumerate() {
        let contribution = PRF_EXPANSION_WEIGHT / (K + rank as f64);
        match fused.get_mut(&e.id) {
            Some(slot) => slot.1 += contribution,
            // A document only the expanded query found is the entire point of this
            // arm — it is admitted, and it is admitted with the *lower* weight.
            None => {
                fused.insert(e.id, (e.clone(), contribution));
            }
        }
    }
    let mut out: Vec<(Entity, f64)> = fused.into_values().collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(limit);
    Ok(out)
}

/// Terms the top hits agree on and the query does not already contain.
///
/// Ranked by **how many seed documents hold the term** first and rarity second, in
/// that order on purpose: a term in five of the eight top hits is evidence about the
/// topic, while a term in one hit may be that hit's own noise, and rarity alone would
/// prefer exactly those singleton oddities.
fn expansion_terms(
    engine: &MemoryEngine,
    query: &str,
    base: &[(Entity, f64)],
    now: i64,
) -> Result<Vec<String>> {
    let query_terms: HashSet<String> = text_terms(query).into_iter().collect();
    let mut agreement: HashMap<String, usize> = HashMap::new();
    for (entity, _) in base.iter().take(PRF_SEED_DOCS) {
        let mut text = entity.name.clone();
        if let Some(summary) = entity.summary.as_deref() {
            text.push(' ');
            text.push_str(summary);
        }
        for observation in engine
            .observations_for(entity.id, now)?
            .into_iter()
            .take(PRF_FACTS_PER_SEED)
        {
            text.push(' ');
            text.push_str(&observation.content);
        }
        for term in text_terms(&text) {
            if !query_terms.contains(&term) {
                *agreement.entry(term).or_insert(0) += 1;
            }
        }
    }
    let totals = corpus_totals(engine);
    let mut ranked: Vec<(String, f64, usize)> = agreement
        .into_iter()
        .map(|(term, count)| {
            let (df, _) = token_df(engine, &term);
            (term, idf_from_df(df, totals), count)
        })
        // Agreement, then rarity, then the term itself: a total order, so two runs on
        // one corpus cannot pick different expansions.
        .collect();
    ranked.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.0.cmp(&b.0))
    });
    Ok(ranked
        .into_iter()
        .take(PRF_MAX_TERMS)
        .map(|(term, _, _)| term)
        .collect())
}

/// How the per-token pass combines evidence into one ranking.
///
/// Selectable because "this fusion is better" is a claim to be *measured*
/// against its own alternatives on real data, not asserted in a comment.
/// `mem_search` accepts an (undocumented) `fusion` argument so the retrieval
/// arena in `eval.rs` can score every variant through the public dispatch path,
/// and `MEMNOTH_MCP_FUSION` lets an operator pin one if a default ever regresses
/// them. It is deliberately absent from the tool schema: a knob no host sets must
/// not cost context bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Fusion {
    /// A — rank fusion (`1/(60+rank)`) with the graph fused inline, every token
    /// equal. This is what shipped before the arena ran, and it is kept under its
    /// own name so the improvement is attributable rather than asserted.
    Rrf,
    /// A minus the graph — the arm that separates *removing the traversal* from
    /// *changing the scoring*, which no other single arm can do.
    RrfNoGraph,
    /// A plus per-token IDF weights.
    RrfIdf,
    /// A plus IDF weights, minus the inline graph.
    RrfIdfNoGraph,
    /// IDF-weighted BM25 scores summed per token, no graph. Scores rather than
    /// ranks: ranks throw away how much better the best hit was, and on both real
    /// datasets that information turns out to be worth more than the fusion rule.
    Bm25,
    /// Bm25 plus the graph, *appended below every scored hit* instead of fused
    /// into the ranking. Keeps the traversal capability without letting it
    /// outrank a text match.
    Bm25Graph,
    /// IDF + rank fusion + a length normalization (worst single-token chunk count,
    /// sqrt). **Measured negative on both real datasets** — LoCoMo any@1 63.1% to
    /// 57.2%, LongMemEval 79.2% to 45.6% — because the long sessions are exactly
    /// the ones that hold the answer. Kept, named, and not the default.
    RrfIdfNorm,
    /// RrfIdfNorm plus a recency multiplier. On these two corpora every document is
    /// ingested at the same instant, so the multiplier is constant and this
    /// reproduces `RrfIdfNorm` exactly — a fact about the corpora, not evidence for
    /// or against recency.
    RrfIdfTemporal,
    /// `Bm25Graph`'s lexical half, rank-fused with a whole-corpus dense (embedding)
    /// arm. Every other variant here is a different way of ranking *the same lexical
    /// evidence*, and the arena says that space is exhausted — the eight lexical arms
    /// span 63.0–64.3% any@1 on LoCoMo while open-domain sits at a **73.9% any@10
    /// ceiling**. `any@1` cannot exceed `any@10`, so no reranker and no fusion rule
    /// can lift that ceiling; only retrieval that does not depend on token overlap
    /// can. See `dense.rs`.
    DenseHybrid,
    /// **W1.2 — best-chunk scoring.** The identical candidate set and per-token
    /// evidence as `Bm25`, combined with `max` over tokens instead of `sum`.
    ///
    /// A document is a session of ~30 turns, and it is ranked today by an aggregate
    /// over every chunk it owns — so a session that mentions the query word forty
    /// times outranks the one session that *answers* it once. Ranking by the single
    /// strongest match is a different combiner on identical evidence, which is what
    /// makes this pair an ablation instead of two retrievers.
    BestChunk,
    /// **W3.1 — facet coverage.** Multi-hop is the worst class by far (`all@3`
    /// **16.0%**, against 77–90% everywhere else), because a multi-hop question
    /// needs *two* pieces of evidence inside the same cut and a per-token sum cannot
    /// tell "matched one facet strongly" from "matched both facets". This multiplies
    /// a candidate's fused score by the number of **distinct query facets it
    /// answered** — the cheapest honest form of set assembly. The expensive one
    /// (MMR) was measured harmful.
    FacetCoverage,
    /// **W4.1 — query-side temporal intent.** A date in the question (a year, a
    /// month) is evidence that the answering memory *mentions that date*, so
    /// candidates whose facts carry it are promoted. Deliberately scoring the date a
    /// match carries rather than blacklisting temporal words: `"we"`, `"me"` and
    /// `"did"` are read as weekdays by naive parsers, and blacklisting them costs
    /// the answer rather than sharpening it.
    TemporalIntent,
    /// **W2.2 — the cap hypothesis for the written-off normalization.**
    /// Byte-for-byte `RrfIdfNorm` except that the length divisor is clamped inside
    /// RRF's own dynamic range.
    ///
    /// We recorded `rrf_idf_norm` as 82.1% → 53.6% and refused it, without a
    /// diagnosis. hindsight's `recall_boost.py` documents the same shape as a
    /// measured defect: an adjustment whose range exceeds the flat rank fusion's
    /// span does not reorder, it *replaces* the ordering, and the adjusted arm then
    /// fills the candidate cap. `sqrt(150) = 12.2×` exceeds RRF's 5.9×, so the
    /// unbounded divide is a candidate cause — and the only way to tell a real
    /// effect from a cap interaction is to measure both side by side.
    RrfIdfNormBounded,
    /// **The ceiling lever, with the displacement risk removed by construction.**
    ///
    /// `DenseHybrid` fuses the dense arm *into* the lexical ranking, and on the
    /// synthetic corpus that cost 35 pp of `any@1` — a document only the dense arm
    /// found was admitted *above* documents lexical retrieval had already ranked
    /// correctly. But the ceiling argument has not gone away: `any@1 ≤ any@10`, and
    /// open-domain's ceiling is **73.9%**, so nothing can reach 95 without new
    /// candidates. This arm gets the new candidates without paying the displacement
    /// cost: every lexical hit keeps its position, and dense-only entities are
    /// **appended below** them, exactly as `Bm25Graph` appends the graph.
    ///
    /// It therefore **cannot lower `any@1`** and can only raise `any@10` — which is
    /// what makes it a clean experiment rather than another hopeful one.
    DenseTail,
    /// `DenseHybrid` with the dense arm's rank contribution **scaled**, because
    /// equal-weight rank fusion is an assumption and the synthetic run suggests it
    /// is the wrong one. `MEMNOTH_DENSE_WEIGHT` sets the share (default 0.5, i.e.
    /// dense outranks lexical only when it is more than twice as confident). This is
    /// hindsight's documented fix — weight in *rank* space, never by multiplying
    /// scores — applied to the arm that regressed.
    DenseWeighted,
    /// **Pseudo-relevance feedback — the model-free ceiling lever.**
    ///
    /// The one thing every class's ceiling failure has in common is a *vocabulary*
    /// gap: the question says "how did we decide to ship" and the document says
    /// "release gate review". Dense retrieval attacks that with a model;
    /// `Relaxed`/PRF attacks it with the corpus itself: rank once, mine the terms
    /// that the top hits share and the query lacks, then rank again with them. It is
    /// the classic Rocchio fix, it needs no model, and it can therefore be published
    /// as `raw`.
    Prf,
    /// **The candidate *union* — the recall stage, separated from the ranking stage.**
    ///
    /// Every arm above answers one question ("what is the best way to rank what
    /// lexical retrieval found") and the arena says that question is settled: the
    /// lexical arms span 63.0–64.3% any@1 while open-domain's **any@10 ceiling is
    /// 73.9%**. `any@1 ≤ any@5 ≤ any@10`, so no ordering rule can reach the Tier
    /// targets — and that is true on LoCoMo too, where `any@5` needs a ceiling above
    /// 95 while the measured ceiling is **93.6**. A reranker reorders a pool; it
    /// cannot add a document the pool never held.
    ///
    /// `DenseHybrid` and `Prf` attack the ceiling with a *better single generator*.
    /// This arm attacks it with a **union of generators that fail differently**: the
    /// shipping arm's own list, plus facet coverage (multi-hop), temporal intent
    /// (dates), PRF (vocabulary gap) and the dense tail (paraphrase). Half the pool is
    /// the shipping arm's own top hits, **in its own order**, so the pool's first half
    /// can never be worse than the shipping arm's; the other half is drawn round-robin
    /// from the other four so no single generator can fill it — a pool whose novelty
    /// half came from one source would not be a union.
    ///
    /// It is a **pool for a reranker, not a ranking.** A union's score column is not
    /// comparable across sources (a `0.0` graph-tail hit and a BM25 score are different
    /// units), so a union read *without* a reranker is order-of-arrival, not relevance.
    /// That is why it is measured as `union + cross_encoder` / `union + llm` and why it
    /// is not the default.
    Union,
    /// **Spine 1's graph arm.** `Bm25` — the measured best single ranking — with the 1-hop
    /// traversal given **slots reserved inside the cut**, instead of being appended below an
    /// already-full candidate pool.
    ///
    /// WHY THIS EXISTS, and it is a measurement rather than a preference: on the full 500-group
    /// LongMemEval arena run (2026-09-18), `bm25_graph` scored **identical to `bm25` on every
    /// metric, to the digit**. The graph tail is appended after `pool` lexical hits and then
    /// truncated away at `limit`, so the traversal runs, finds neighbours, and cannot enter the
    /// result. The capability is exercised and *invisible* — which means "the graph does not help
    /// on LongMemEval" has never actually been tested, only its invisibility has.
    ///
    /// `GraphReserve` prices the trade explicitly: the last `graph_reserve()` slots of the cut are
    /// filled from the traversal rather than from lexical ranks. That costs at most that many
    /// lexical positions and **guarantees** the graph is visible, which is what turns the next run
    /// into a measurement of the graph instead of of its invisibility.
    GraphReserve,
    /// **The AMA-Bench recipe: causal edges, and retrieval that follows them.**
    ///
    /// `GraphReserve` proved a traversal can be made *visible* inside the cut; this asks the
    /// different question of whether the **kind** of edge matters. It differs from `bm25_graph` in
    /// two ways at once, and both are load-bearing: it selects only `causes` / `enables` /
    /// `prevents` rather than every edge, and it gives a neighbour a **score** rather than a 0.0
    /// tail — a fraction of the seed's own score, so a causal neighbour of the top hit can enter
    /// the cut instead of sitting below every lexical match.
    ///
    /// The null for this arm is `bm25_graph`, not `MEMNOTH_GRAPH_RESERVE=0`.
    CausalGraph,
    /// **The preference channel.** `Bm25Graph` plus a bounded score boost for documents
    /// that *state a preference*, applied only when the question asks about one.
    ///
    /// Why this is not another ranking tweak: on the LongMemEval-S arena the
    /// `single-session-preference` class sits at any@1 **33.3%** against a 100% ceiling
    /// (MRR .513) — 66.7 pp of its ceiling lost at rank 1, the largest per-class gap in
    /// that run — while any@10 is already 100%. So recall is not the problem. The
    /// question ("what do I prefer for X") is phrased in the vocabulary of the *value*,
    /// while the document that answers it is phrased as a preference statement ("I'd
    /// rather…", "my favourite…", "I usually…"). Lexical overlap therefore selects
    /// topic documents over the one expressing the preference, and no rank-fusion rule
    /// can repair a list whose ordering ignored the document type the question wanted.
    ///
    /// The boost is intent-gated (only a preference question pays for it), marker-gated
    /// (only documents carrying a preference lexeme are touched) and bounded
    /// (`MEMNOTH_PREFERENCE_BOOST`, default 1.25) so it can reorder near-ties rather
    /// than promote a weak document past a strong one.
    PreferenceBoost,
    /// **The no-tie variant of [`Fusion::CausalGraph`].** Same causal edges, same per-kind
    /// ordering, same seed selection — the only difference is that every inherited weight is
    /// scaled to stay strictly below its own seed's score (`causal_kind_weight_soft`).
    ///
    /// Why the distinction is a different arm and not a tweak to `causal_graph`: the measured
    /// failure of `causal_graph` on LongMemEval-S (−4.0 pp any@1, worse in every shard) has a
    /// named candidate cause — a `Causes` neighbour inherits at weight 1.0 and can *tie the seed
    /// it came from*, so a neighbour of the top hit is interchangeable with the top hit. Making
    /// the arm's weights stop tying is a one-line change; proving it fixes the LME-S regression
    /// without giving back the LoCoMo gain is a measurement, and a measurement needs both arms to
    /// stay separately selectable or the before/after cannot be attributed.
    CausalSoft,
}

impl Fusion {
    /// **Every arm, in one place.** `label()` cannot be forgotten (the compiler
    /// enforces it) but a hand-maintained *list* of arms can be — and that is how a
    /// new arm once stayed untested while the arena reported it as measured. Any test
    /// that needs "all arms" iterates this, so adding a variant is one edit here.
    ///
    /// `#[cfg(test)]` because that is its whole audience: the eval harness lives in a
    /// test-only module (`#[cfg(test)] mod eval`), so in a release build nothing reads
    /// this list. Publishing it in the artifact would be carrying a test fixture.
    #[cfg(test)]
    pub(crate) const ALL: &'static [Fusion] = &[
        Self::Rrf,
        Self::RrfNoGraph,
        Self::RrfIdf,
        Self::RrfIdfNoGraph,
        Self::Bm25,
        Self::Bm25Graph,
        Self::RrfIdfNorm,
        Self::RrfIdfTemporal,
        Self::DenseHybrid,
        Self::BestChunk,
        Self::FacetCoverage,
        Self::TemporalIntent,
        Self::RrfIdfNormBounded,
        Self::DenseTail,
        Self::DenseWeighted,
        Self::Prf,
        Self::Union,
        Self::GraphReserve,
        Self::CausalGraph,
        Self::CausalSoft,
        Self::PreferenceBoost,
    ];

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "rrf" => Some(Self::Rrf),
            "rrf_no_graph" => Some(Self::RrfNoGraph),
            "rrf_idf" => Some(Self::RrfIdf),
            "rrf_idf_no_graph" => Some(Self::RrfIdfNoGraph),
            "bm25" => Some(Self::Bm25),
            "bm25_graph" => Some(Self::Bm25Graph),
            "rrf_idf_norm" => Some(Self::RrfIdfNorm),
            "rrf_idf_temporal" => Some(Self::RrfIdfTemporal),
            "dense_hybrid" => Some(Self::DenseHybrid),
            "best_chunk" => Some(Self::BestChunk),
            "facet_coverage" => Some(Self::FacetCoverage),
            "temporal_intent" => Some(Self::TemporalIntent),
            "rrf_idf_norm_bounded" => Some(Self::RrfIdfNormBounded),
            "dense_tail" => Some(Self::DenseTail),
            "dense_weighted" => Some(Self::DenseWeighted),
            "prf" => Some(Self::Prf),
            "union" => Some(Self::Union),
            "graph_reserve" => Some(Self::GraphReserve),
            // `causal` alone as well, so a bare `causal` needle reaches this arm. The selector no
            // longer matches only by substring — it has `=exact` and `!exclude` forms, because a
            // `fusion1` sweep pulled 16 arms instead of the 9 it named — but this alias still
            // earns its line: a substring need would also drag in anything else carrying
            // "causal", and the label is the thing a report is attributed by.
            "causal_graph" | "causal" => Some(Self::CausalGraph),
            "causal_soft" | "causal_no_tie" => Some(Self::CausalSoft),
            "preference_boost" | "preference" => Some(Self::PreferenceBoost),
            _ => None,
        }
    }

    /// One line a host can read in the response, naming what produced the order.
    /// Every variant keeps its own name even where it shares an ingredient with
    /// another, so a result set produced yesterday can still be attributed after
    /// the default has moved on.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Rrf => "rrf",
            Self::RrfNoGraph => "rrf_no_graph",
            Self::RrfIdf => "rrf_idf",
            Self::RrfIdfNoGraph => "rrf_idf_no_graph",
            Self::Bm25 => "bm25",
            Self::Bm25Graph => "bm25_graph",
            Self::RrfIdfNorm => "rrf_idf_norm",
            Self::RrfIdfTemporal => "rrf_idf_temporal",
            Self::DenseHybrid => "dense_hybrid",
            Self::BestChunk => "best_chunk",
            Self::FacetCoverage => "facet_coverage",
            Self::TemporalIntent => "temporal_intent",
            Self::RrfIdfNormBounded => "rrf_idf_norm_bounded",
            Self::DenseTail => "dense_tail",
            Self::DenseWeighted => "dense_weighted",
            Self::Prf => "prf",
            Self::Union => "union",
            Self::GraphReserve => "graph_reserve",
            Self::CausalGraph => "causal_graph",
            Self::CausalSoft => "causal_soft",
            Self::PreferenceBoost => "preference_boost",
        }
    }

    fn uses_idf(self) -> bool {
        !matches!(self, Self::Rrf | Self::RrfNoGraph)
    }

    /// Whether the primary list is the raw BM25 **score** list
    /// (`search_entities`) rather than a fused **rank** list — expressed as the
    /// positive case deliberately.
    ///
    /// It was written as `uses_ranks()` with a `matches!` list of rank variants,
    /// and when two variants were added to the enum they fell out of that list and
    /// silently changed arm: the same arm name measured two different retrievers
    /// across arena runs. That is exactly the failure a benchmark exists to catch,
    /// and it is why the answer is now stated as "does it use scores" — a new
    /// variant has to opt *in* to scores instead of being opted out by omission.
    fn uses_scores(self) -> bool {
        matches!(
            self,
            Self::Bm25
                | Self::Bm25Graph
                | Self::DenseHybrid
                | Self::BestChunk
                | Self::FacetCoverage
                | Self::TemporalIntent
                | Self::DenseTail
                | Self::DenseWeighted
                | Self::Prf
                | Self::Union
                | Self::GraphReserve
                | Self::PreferenceBoost
        )
    }

    /// Whether the 1-hop traversal is fused *into* the ranked list. The two
    /// `*_no_graph` arms ask the index directly instead.
    fn fuses_graph_inline(self) -> bool {
        matches!(
            self,
            Self::Rrf
                | Self::RrfIdf
                | Self::RrfIdfNorm
                | Self::RrfIdfTemporal
                | Self::RrfIdfNormBounded
        )
    }

    /// Whether this variant carries the lexical arm's **score** list into the dense
    /// stage. All three dense variants do, because they are all "the measured lexical
    /// ranking, plus a second arm" — which is what makes their comparison an
    /// ablation of the placement rule rather than of the retriever.
    fn has_dense_arm(self) -> bool {
        matches!(
            self,
            Self::DenseHybrid | Self::DenseTail | Self::DenseWeighted
        )
    }

    /// Whether the 1-hop traversal is given **reserved slots inside the cut** rather than being
    /// appended below an already-full pool.
    ///
    /// A predicate rather than a comment inside the placement code, for the same reason
    /// `uses_scores` is one: the only thing distinguishing this arm from `bm25` is the placement
    /// rule, so if that difference is not expressible as a property then nothing can assert it —
    /// and an arm whose difference from its control is untestable is an arm that can silently
    /// become a copy of it (which is precisely what happened to `bm25_graph`).
    ///
    /// `#[cfg(test)]`: it exists to be *asserted*, and its assertions are tests.
    #[cfg(test)]
    fn reserves_graph_slots(self) -> bool {
        matches!(self, Self::GraphReserve)
    }

    /// Whether this arm follows **causal** edges rather than any edge at all.
    ///
    /// A predicate for the same reason `reserves_graph_slots` is one: the entire difference between
    /// this arm and `bm25_graph` is which relations it reads and how it scores them, so if that
    /// difference is not expressible as a property then no test can assert it — and an arm whose
    /// difference from its control is untestable can silently become a copy of it, which is exactly
    /// what happened to `bm25_graph`.
    fn follows_causal_edges(self) -> bool {
        matches!(self, Self::CausalGraph | Self::CausalSoft)
    }

    /// Whether inherited causal scores must stay **strictly below** their seed
    /// (`CausalSoft`) rather than being allowed to tie it (`CausalGraph`).
    ///
    /// Expressed as its own predicate so the two arms' difference is a named property
    /// with a test, not an `if fusion == …` buried in the expansion loop: the LME-S
    /// regression this exists to fix was a *weight* difference, and a difference that
    /// cannot be asserted is a difference that can silently disappear.
    fn caps_causal_inheritance(self) -> bool {
        matches!(self, Self::CausalSoft)
    }

    /// Whether this arm re-scores **preference-stating** documents when the question
    /// asks about preferences.
    ///
    /// A predicate for the same reason as the others: the entire difference between
    /// this arm and `bm25_graph` is the boost, so if that difference is not expressible
    /// as a property, no test can assert the arm is not a copy of its control.
    fn boosts_preference_docs(self) -> bool {
        matches!(self, Self::PreferenceBoost)
    }
}

/// Slots [`Fusion::GraphReserve`] holds for the 1-hop traversal **inside** the cut.
///
/// Default 5, because the metric that arm is judged on is `any@5`: reserving five of a ten-slot
/// cut is the largest change that can move a question's outcome without rewriting the whole
/// ordering. `MEMNOTH_GRAPH_RESERVE=0` makes the arm identical to `bm25` again, which is the null
/// the comparison needs — an arm with no null cannot show that its ingredient did anything.
fn graph_reserve() -> usize {
    std::env::var("MEMNOTH_GRAPH_RESERVE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5)
}

/// Lexemes that mark a passage as *stating a preference* rather than describing a
/// topic — the document type the preference class asks for and lexical overlap does not
/// select. Single lowercase tokens only: they are searched as quoted FTS terms, so a
/// multi-word entry would be escaped into a phrase the marker search could never match
/// (asserted in the tests).
const PREFERENCE_MARKERS: &[&str] = &[
    "prefer",
    "prefers",
    "preferred",
    "favourite",
    "favorite",
    "rather",
    "usually",
    "enjoy",
    "enjoys",
    "love",
    "loves",
    "hate",
    "hates",
    "dislike",
    "dislikes",
    "tend",
    "tends",
];

/// Whether a question asks about the user's **preferences/opinions** rather than a fact.
///
/// Deliberately phrase-based instead of matching a bare `like`: "what was the trip
/// like" is not a preference question, and boosting preference chatter into its answer
/// would trade one class's ranking for another's. Every phrase here is one a
/// preference question actually uses (the LME-S class's own wording plus the
/// equivalents rivals publish), and the negative cases live in the tests.
fn preference_intent(query: &str) -> bool {
    const PHRASES: &[&str] = &[
        "prefer",
        "favourite",
        "favorite",
        "rather",
        "recommend",
        "opinion",
        "should i",
        "would i",
        "do i like",
        "like best",
        "my kind of",
        "interested in",
        "tend to",
    ];
    let q = query.to_ascii_lowercase();
    PHRASES.iter().any(|p| q.contains(p))
}

/// Score multiplier for marker-bearing documents on a preference question.
///
/// Default 1.25 — enough to swap an adjacent pair, far too little to jump a document
/// past a hit that leads by a wide margin. Clamped to [1.0, 4.0] so a typo in the
/// environment cannot turn a ranking experiment into a rank-shuffling one.
fn preference_boost() -> f64 {
    std::env::var("MEMNOTH_PREFERENCE_BOOST")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|b| (1.0..=4.0).contains(b))
        .unwrap_or(1.25)
}

/// Depth of each marker search. Bounded because the union only has to name candidate
/// entities near the top; a marker that matches a thousand passages is not evidence that
/// the *document* states a preference.
fn preference_mark_depth() -> usize {
    std::env::var("MEMNOTH_PREFERENCE_DEPTH")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|d| *d > 0)
        .unwrap_or(50)
}

/// Entity ids whose observations carry a preference marker, as the union over the
/// marker lexicon.
///
/// One FTS pass per marker, paid **only** for a preference-intent question — the boost
/// exists for one class, so the class pays and every other search is untouched. The
/// union (rather than an AND over markers) is the right shape because any single
/// preference statement marks its document: a passage saying "I'd rather" need not also
/// say "favourite".
fn preference_marked_entities(engine: &MemoryEngine, now: i64) -> Result<HashSet<i64>> {
    let mut out: HashSet<i64> = HashSet::new();
    for marker in PREFERENCE_MARKERS {
        for (obs, _) in engine.search_observations(marker, preference_mark_depth(), now)? {
            out.insert(obs.entity_id);
        }
    }
    Ok(out)
}

/// Score multiplier per causal kind, applied to the **seed's own score**.
///
/// Ordered rather than equal because the three kinds answer different questions and the agentic
/// benchmarks grade them differently: `causes` names what produced the outcome that was asked
/// about, `enables` names a precondition (a cause one step removed), and `prevents` names something
/// pointing *away* from the outcome. Equal weights would leave the arm unable to say which kind
/// carried a question it won — and if one weight carried every win, the arm would be measuring
/// "edges exist" rather than "causality helps".
fn causal_kind_weight(kind: crate::gauntlet::CausalKind) -> f64 {
    match kind {
        crate::gauntlet::CausalKind::Causes => 1.0,
        crate::gauntlet::CausalKind::Enables => 0.8,
        crate::gauntlet::CausalKind::Prevents => 0.6,
    }
}

/// [`causal_kind_weight`] for the no-tie arm: identical proportions, every value strictly below
/// 1.0. A neighbour reached from the top hit therefore scores *less* than that hit — it still
/// surfaces where its seed was strong (which is the arm's whole mechanism) but can no longer be
/// interchangeable with the seed, or displace a different direct hit that happens to score the
/// same as the seed. `Prevents` is scaled too, not exempted: a `prevents` edge pointing away from
/// the outcome is the kind most likely to displace a correct hit, so exempting it would leave the
/// arm's most suspicious edges at full strength.
fn causal_kind_weight_soft(kind: crate::gauntlet::CausalKind) -> f64 {
    causal_kind_weight(kind) * CAUSAL_SOFT_CAP
}

/// Ceiling on every inherited causal score. `0.9` is a deliberate headroom, not a tuned value:
/// it must be below 1.0 for any kind, and the first measurement of this arm is what decides
/// whether the gap is wide enough.
const CAUSAL_SOFT_CAP: f64 = 0.9;

/// Causal neighbours of the ranked seeds, each carrying its **inherited** score.
///
/// Returns `(target_id, score, kind_name)`. The kind is returned rather than folded away because a
/// `causes` edge that wins a question and a `prevents` edge that wins one are different findings
/// about the same arm, and a single number could not tell them apart.
///
/// Reads `outgoing_relations` per seed rather than `expand_frontier` over all of them for one
/// reason: `expand_frontier` returns a flat list, so the seed an edge came from would be lost, and
/// the inherited score is exactly what makes this arm score rather than tail. A causal neighbour of
/// the top hit must outrank one reached from the tenth, or an edge is worth no more than its seed.
fn causal_frontier(
    engine: &MemoryEngine,
    seeds: &[(i64, f64)],
    now: i64,
    soft: bool,
) -> Result<Vec<(i64, f64, &'static str)>> {
    let mut out: Vec<(i64, f64, &'static str)> = Vec::new();
    let mut seen: HashSet<i64> = seeds.iter().map(|(id, _)| *id).collect();
    for (seed_id, seed_score) in seeds {
        for rel in engine.outgoing_relations(*seed_id, now)? {
            // One filter for the whole vocabulary, called rather than re-listed. A traversal that
            // spelled the three kinds out again is how an alias accepted at write time becomes
            // invisible at read time: an edge that exists, is counted, and cannot be followed.
            if !crate::gauntlet::is_causal_relation_kind(&rel.kind) {
                continue;
            }
            let Some(kind) = crate::gauntlet::CausalKind::from_relation_kind(&rel.kind) else {
                // Unreachable while the predicate above and this parse share one alias table. Kept
                // as a `continue` rather than an `unwrap` so a future alias added to only one of
                // them degrades to "this edge is skipped" instead of panicking mid-retrieval.
                continue;
            };
            if rel.target_id == 0 || !seen.insert(rel.target_id) {
                continue;
            }
            let weight = if soft {
                causal_kind_weight_soft(kind)
            } else {
                causal_kind_weight(kind)
            };
            out.push((rel.target_id, weight * seed_score, kind.name()));
        }
    }
    Ok(out)
}

/// Resolve an ingested **document id** to the entity `mem_ingest` created for it.
///
/// Two forms are tried in order, and the second is the one that exists today:
///
/// 1. the bare id (`task#s3`) — what a caller naturally holds, and what an ingest that named
///    entities by document would produce;
/// 2. the relative path (`task#s3.md`) — what `store_document_text` actually creates, because
///    `ingest_dir` names file nodes by their path relative to the ingest root.
///
/// **Why the lookup is fixed here instead of by renaming the file entity to the document id.**
/// The rename would be the obvious implementation and it has two costs that are not visible from
/// the causal arm at all:
///
/// - `EntityKind::File` nodes are named by **path on purpose**. The code graph needs the directory
///   to tell `src/api/client.rs` from `src/db/client.rs`; naming them by stem collapses two
///   distinct files into one entity, and that is the KG lane's contract, not a detail here.
/// - It would **silently change every retrieval score**. This harness normalises a ranked name
///   with `doc_id_of` (`Path::file_stem`), which strips an extension but truncates at the first
///   dot — so any document id containing a `.` would stop matching its own gold, on every
///   question, with no error anywhere. Today's ids (`<task>#s<index>`) happen to be dot-free, and
///   a fix that is correct only because of a property of today's data is a trap for tomorrow's.
///
/// So the resolver absorbs the difference and neither the graph nor the metric is touched.
///
/// `#[cfg(test)]` with its caller below: this is the eval harness's ingest path, and the
/// harness compiles only under `cfg(test)`.
#[cfg(test)]
pub(crate) fn resolve_document_entity(
    engine: &MemoryEngine,
    doc_id: &str,
    now: i64,
) -> Result<Option<i64>> {
    for candidate in [doc_id.to_string(), format!("{doc_id}.md")] {
        if let Some(e) = engine
            .find_entities_by_name(&candidate, now)?
            .into_iter()
            .next()
        {
            return Ok(Some(e.id));
        }
    }
    Ok(None)
}

/// Write causal edges into the vault, resolving both endpoints **by name**.
///
/// Returns `(written, unresolved)`, and the split is the point. A run that resolves nothing must
/// say so rather than report a successful ingest of zero edges — this lane has already paid for
/// that lesson once, when `bm25_graph` executed, scored identically to `bm25` on all 500 groups,
/// and nobody noticed because nothing counted it.
///
/// Resolution is by name through `find_entities_by_name` because that is the only identity the
/// vault exposes after `mem_ingest` has run: the ingest creates entities from what it read, and the
/// caller's own handles (a document id, a step index) do not survive it. An endpoint that resolves
/// to nothing is counted, not guessed.
///
/// `#[cfg(test)]` for the same reason as [`resolve_document_entity`]: the only caller is
/// `eval::write_group_causal_edges`, which exists only in the test build.
#[cfg(test)]
pub(crate) fn write_causal_relations_by_name(
    engine: &MemoryEngine,
    edges: &[(String, String, &'static str)],
    now: i64,
) -> Result<(usize, usize)> {
    let mut written = 0usize;
    let mut unresolved = 0usize;
    for (from, to, kind) in edges {
        // The same predicate the traversal filters on, asked at the write end too, so an edge this
        // function accepts is one retrieval can actually follow. A kind the reader cannot parse
        // would produce a relation that exists, is counted, and is invisible.
        if !crate::gauntlet::is_causal_relation_kind(kind) {
            unresolved += 1;
            continue;
        }
        let a = resolve_document_entity(engine, from, now)?;
        let b = resolve_document_entity(engine, to, now)?;
        match (a, b) {
            // A self-loop is not a causal claim, and `add_relation` would happily store one.
            (Some(a), Some(b)) if a != b => {
                engine.add_relation(a, b, kind, 1.0, now)?;
                written += 1;
            }
            _ => unresolved += 1,
        }
    }
    Ok((written, unresolved))
}

/// The variant `mem_search` uses when the caller does not name one.
///
/// Set from the arena, not from preference. On the full LoCoMo set (10
/// conversations, 1536 questions) and on a 250-group stride sample of
/// LongMemEval-S (11,952 documents), score-ranked retrieval beat rank-fusion on
/// every aggregate that matters, and this variant is score-ranked with the graph
/// kept as a tail:
///
/// | arm | LoCoMo any@1 / any@10 / MRR | LongMemEval-S any@1 / any@10 / MRR |
/// |---|---|---|
/// | `rrf` (shipped before 2026-09-26) | 60.0 / — / 0.716 | 80.4 / — / 0.857 |
/// | `bm25_graph` (**default**) | 63.5 / **92.5** / 0.733 | **85.0** / **99.0** / **0.897** |
/// | `facet_coverage` | 65.8 / 92.3 / 0.748 | not measured |
/// | `temporal_intent` | 64.8 / 92.4 / 0.741 | not measured |
/// | `causal_graph` (selectable) | **66.4** / 92.0 / **0.752** | 81.0 / 98.0 / 0.874 |
///
/// **Why the default did *not* move to `causal_graph`, after it won on LoCoMo.** Both rows above
/// are measured full-set-style: LoCoMo 1536 scored questions, N=1, shipping path, no model; and
/// LongMemEval-S capped at 100/500 groups with a 3-shard split that reproduced the single-process
/// capped row to the digit (85.0 / 94.0 / 96.0 / 99.0 / .897) before being used for the comparison.
/// `causal_graph` trades datasets: **+2.9 pp any@1 on LoCoMo but −4.0 pp on LongMemEval-S**, and it
/// is worse in *every* LME-S shard (85.3 vs 91.2, 75.8 vs 81.8, tie) while giving back any@10 on
/// both (92.0 vs 92.5; 98.0 vs 99.0). A default has to be the arm that is best across datasets,
/// not one that wins the smaller corpus by losing the larger one — so the win is kept as a
/// selectable arm and the trade is published instead of the highlight.
///
/// The mechanism is visible in `causal_kind_weight`: a `Causes` neighbour inherits its seed's score
/// at weight **1.0**, i.e. it can tie its own seed. On LoCoMo (abstract questions) that conversion
/// is worth 2.9 pp at rank 1; on LongMemEval (questions that often quote the fact verbatim) it
/// displaces lexical hits that were already correct. `causal_soft` is the no-tie variant of the
/// same arm — identical except that every inherited weight is scaled below 1.0, so a neighbour can
/// never tie or outrank the seed that produced it. It is being measured on both datasets before
/// anyone touches the default again. MMR rerank stays net-negative on top of both
/// (LoCoMo any@1 66.4 → 65.0).
///
/// `MEMNOTH_MCP_FUSION` is the escape hatch: `causal_graph` opts a corpus into the LoCoMo profile.
const DEFAULT_FUSION: Fusion = Fusion::Bm25Graph;

/// Resolve the fusion for one call: caller argument, then environment, then the
/// built-in default. An unparseable value falls through rather than erroring — a
/// typo must not turn a search into a failure.
pub(crate) fn active_fusion(requested: Option<&str>) -> Fusion {
    requested
        .and_then(Fusion::parse)
        .or_else(|| {
            std::env::var("MEMNOTH_MCP_FUSION")
                .ok()
                .and_then(|v| Fusion::parse(&v))
        })
        .unwrap_or(DEFAULT_FUSION)
}

/// Ceiling on the IDF weight. Without it a single identifier that appears in one
/// document out of a million would outvote the rest of the query; with it the
/// ratio between the rarest and the commonest term stays bounded — which is what
/// keeps "rare tokens win" from becoming *rare-token tyranny*.
const MAX_IDF_WEIGHT: f64 = 6.0;

/// Approximate number of rows in each index — the `N` of `idf` and the denominator
/// of the common-token rule.
///
/// **`MAX(rowid)`, not `COUNT(*)`**, and the difference is the difference between a
/// vault that scales and one that does not. `COUNT(*)` over an FTS5 table walks the
/// whole content table, so it costs O(corpus) *per search* — the scale curve showed
/// a perfectly selective query at 84 ms p50 on 50K entities with almost all of that
/// time in the two counts, because every search paid a full scan of the corpus
/// before it read a single post. `MAX(rowid)` is a rightmost-descent on the primary
/// key: O(log n), and it is the *better* denominator anyway, because a closed
/// revision still sits in the FTS index, so the highest assigned rowid is exactly
/// the population that `df` is counted against.
///
/// Nothing in this vault is ever deleted (revisions are closed, merges revoke), so
/// the estimate does not drift downward as memory ages — the failure mode that would
/// eventually misclassify a rare token as common. It is documented as an estimate
/// because it is one: a vault that does delete rows would need the exact count back.
fn corpus_totals(engine: &MemoryEngine) -> (i64, i64) {
    let conn = engine.connection();
    let entities = conn
        .query_row(
            "SELECT COALESCE(MAX(rowid), 0) FROM entities_fts",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    let observations = conn
        .query_row(
            "SELECT COALESCE(MAX(rowid), 0) FROM observations_fts",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    (entities, observations)
}

/// Smoothed inverse document frequency of one token over both FTS indexes:
/// `1 + ln((N+1)/(df+1))`, capped at [`MAX_IDF_WEIGHT`]. A token in every document
/// keeps weight ~1, so this can only ever *demote* the ubiquitous — never silence
/// the rare, and never divide by zero for a term no document holds.
///
/// `df` is read straight from the index with a bounded `COUNT(*) ... MATCH`
/// ([`token_df`]), because that is the vault's only record of *how many documents
/// contain a term*: the engine exposes ranked search, not corpus statistics. The
/// proper home is an
/// engine-level `document_frequency()` (KG lane, `L-MCP-34`); until it exists this
/// is the smallest possible read-only probe. It counts closed revisions too, on
/// purpose — term rarity is a property of the corpus, not of which revision
/// currently holds.
/// Upper bound on rows walked by one document-frequency probe.
///
/// The df probe is a `COUNT(*) ... MATCH`, and FTS5 answers it by walking the
/// token's entire posting list — so on a large corpus the *probe itself* is the
/// linear-in-N cost: at 50K entities the ubiquitous query paid seven full-corpus
/// walks per search (p50 106 ms), and even selective queries paid two. Bounding
/// the count at [`DF_PROBE_LIMIT`] rows makes the probe O(1) instead: past the
/// bound we do not know the exact df, and we do not need it — a token in >4096
/// documents is by definition common, contributes little discriminative value
/// to a fusion over `PER_TOKEN_DEPTH` candidates, and is skipped whenever the
/// query holds a rarer alternative. Datasets searched in this repo top out at
/// ~24K documents with no token above ~8% df, so the bound is never hit there;
/// the full re-baseline on LoCoMo + LongMemEval guards that claim.
const DF_PROBE_LIMIT: i64 = 4096;

/// Document frequency of one token over both FTS indexes, plus whether the
/// probe hit [`DF_PROBE_LIMIT`] and the true count is unknown ("common").
fn token_df(engine: &MemoryEngine, token: &str) -> (f64, bool) {
    let expr = MemoryEngine::sanitize_fts_query(token);
    if expr.is_empty() {
        return (0.0, false);
    }
    let conn = engine.connection();
    // COUNT over a LIMITed subquery lets SQLite stop scanning at the bound.
    let bounded = |fts: &str| -> std::result::Result<(i64, bool), rusqlite::Error> {
        conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM (SELECT 1 FROM {fts} WHERE {fts} MATCH ?1 LIMIT {DF_PROBE_LIMIT})"
            ),
            params![expr],
            |row| {
                let n: i64 = row.get(0)?;
                Ok((n, n >= DF_PROBE_LIMIT))
            },
        )
    };
    let entity_df = bounded("entities_fts").unwrap_or((0, false));
    let observation_df = bounded("observations_fts").unwrap_or((0, false));
    (
        (entity_df.0 + observation_df.0) as f64,
        entity_df.1 || observation_df.1,
    )
}

fn idf_from_df(df: f64, totals: (i64, i64)) -> f64 {
    let total = (totals.0 + totals.1) as f64;
    (1.0 + ((total + 1.0) / (df + 1.0)).ln()).min(MAX_IDF_WEIGHT)
}

/// Share of the corpus above which a query token is treated as carrying no
/// evidence and is not searched at all.
///
/// This is a **latency** rule, and it is the difference between a vault that scales
/// and one that does not. FTS5 answers `ORDER BY bm25(...) LIMIT k` by walking every
/// document that matches, so a token costs in proportion to its document frequency,
/// not to `k`. Measured on the scale curve: with one token present in every fact,
/// p50 went 26 ms → 203 ms → 981 ms from 1K → 10K → 50K entities — linear in the
/// corpus, for a query that was otherwise perfectly selective.
///
/// The threshold is not a tuning knob either. A term held by a third of the corpus
/// cannot discriminate between the documents that contain it, so searching it buys
/// a small, IDF-discounted amount of score for a large amount of I/O. At least one
/// token is always searched: a query made entirely of ubiquitous words is still a
/// query, and silently answering nothing would be worse than being slow.
const COMMON_TOKEN_DF_RATIO: f64 = 0.30;

/// Recency multiplier for the temporal variant: 1.5 for a belief added now,
/// tending to 1.0 as it ages, 30-day half-life. Bounded at both ends on purpose —
/// this is a tie-breaker between comparably relevant memories, not a licence for a
/// fresh wrong answer to outrank an old right one.
fn recency_multiplier(valid_from: i64, now: i64) -> f64 {
    const HALF_LIFE_MS: f64 = 30.0 * 24.0 * 60.0 * 60.0 * 1000.0;
    let age = (now - valid_from).max(0) as f64;
    1.0 + 0.5 * 0.5f64.powf(age / HALF_LIFE_MS)
}

/// Record one hit on one token.
///
/// `raw` counts *every* matching chunk while `best` keeps only the best one, and
/// that difference is the length-bias fix: a long document that mentions a token
/// in ten chunks contributes **once** to the score, and the inflated chunk count
/// is visible only to the normalization that is supposed to know about length.
fn note(
    order: &mut Vec<i64>,
    best: &mut HashMap<i64, (f64, usize)>,
    raw: &mut HashMap<i64, usize>,
    id: i64,
    rank: usize,
    score: f64,
    by_score: bool,
) {
    *raw.entry(id).or_insert(0) += 1;
    let better = match best.get(&id) {
        Some((best_score, best_rank)) => {
            if by_score {
                score > *best_score
            } else {
                rank < *best_rank
            }
        }
        None => true,
    };
    if better {
        best.insert(id, (score, rank));
    }
    if !order.contains(&id) {
        order.push(id);
    }
}

/// Share of a union pool reserved for the shipping arm's own top hits.
///
/// A knob rather than a constant because the trade-off it encodes is empirical: 1.0
/// makes the union an expensive alias for the primary arm (no novelty), 0.0 makes it
/// a four-way novelty pool with the best lexical hits pushed out of the rerank
/// window. 0.5 ("half the pool is the ordering we already trust") is the hypothesis,
/// and `MEMNOTH_UNION_PRIMARY_SHARE` exists so the arena can falsify it.
const UNION_PRIMARY_SHARE_DEFAULT: f64 = 0.5;

fn union_primary_share() -> f64 {
    std::env::var("MEMNOTH_UNION_PRIMARY_SHARE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0 && *v <= 1.0)
        .unwrap_or(UNION_PRIMARY_SHARE_DEFAULT)
}

/// The generators a union pools, **the shipping arm first** (it defines the pool's
/// top half, so it must not move).
///
/// Each generator after the first is here because it fails differently from the
/// primary, not because it is good: facet coverage returns multi-hop sets the
/// per-token sum cannot assemble, temporal intent returns candidates whose *dates*
/// match, PRF returns documents in the corpus's vocabulary rather than the
/// question's, and the dense tail returns paraphrases with no token overlap at all.
/// A generator that is individually worse than the primary can still contribute the
/// one document the primary could not reach — which is the entire point of a union.
const UNION_SOURCES: &[Fusion] = &[
    Fusion::Bm25Graph,
    Fusion::FacetCoverage,
    Fusion::TemporalIntent,
    Fusion::Prf,
    Fusion::DenseTail,
];

/// Union the candidate sets of [`UNION_SOURCES`] into one rerankable pool.
///
/// The contract, in order:
///
/// 1. the primary's own top `limit * share` hits, in the primary's own order;
/// 2. the other generators, **round-robin**, each contributing one unseen candidate
///    per round, so the novelty half is genuinely shared;
/// 3. the primary's remainder, if the others ran dry — a short pool wastes the
///    reranker's budget and cannot be justified by "diversity" when there is no
///    diversity left to add.
///
/// Bounded by `limit` and de-duplicated, because a pool that admits the same entity
/// twice spends a rerank slot on nothing. A member that errors contributes an empty
/// list: a union is defined by what its members *do* return, and one unavailable
/// optional model (the dense arm needs one) must not fail a search.
fn union_search(
    engine: &MemoryEngine,
    query: &str,
    limit: usize,
    now: i64,
) -> Result<Vec<(Entity, f64)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let lists: Vec<Vec<(Entity, f64)>> = UNION_SOURCES
        .iter()
        .map(|source| relaxed_search(engine, query, limit, now, *source).unwrap_or_default())
        .collect();
    let primary_quota = ((limit as f64) * union_primary_share()).round() as usize;
    let primary_quota = primary_quota.clamp(1, limit);
    let mut out: Vec<(Entity, f64)> = Vec::with_capacity(limit);
    let mut seen: HashSet<i64> = HashSet::with_capacity(limit * 2);
    for (e, s) in lists[0].iter().take(primary_quota) {
        if seen.insert(e.id) {
            out.push((e.clone(), *s));
        }
    }
    let mut cursors = vec![0usize; lists.len()];
    let mut progressed = true;
    while out.len() < limit && progressed {
        progressed = false;
        for (i, list) in lists.iter().enumerate().skip(1) {
            if out.len() >= limit {
                break;
            }
            while cursors[i] < list.len() {
                let (e, s) = &list[cursors[i]];
                cursors[i] += 1;
                if seen.insert(e.id) {
                    out.push((e.clone(), *s));
                    progressed = true;
                    break;
                }
            }
        }
    }
    for (e, s) in lists[0].iter().skip(primary_quota) {
        if out.len() >= limit {
            break;
        }
        if seen.insert(e.id) {
            out.push((e.clone(), *s));
        }
    }
    Ok(out)
}

/// Search each content token separately and fuse the results — the pass that turns
/// a prose question into a rankable set of memories.
///
/// The fusion is pluggable ([`Fusion`]) because the ranking *is* the product: a
/// ranking claimed to be best has to beat its own alternatives on real data. Every
/// variant shares the plumbing — same tokens, same per-token depth, same
/// one-contribution-per-entity rule — so the arena compares fusion strategies
/// rather than six different retrievers wearing one name.
fn relaxed_search(
    engine: &MemoryEngine,
    query: &str,
    limit: usize,
    now: i64,
    fusion: Fusion,
) -> Result<Vec<(Entity, f64)>> {
    const K: f64 = 60.0;
    // PRF is a *second pass over the same machinery*, not a fourth retriever: it
    // ranks with the shipping arm, mines expansion terms from those hits, and ranks
    // again. Returning here keeps that recursion in one place and out of the hot
    // path every other arm walks.
    if fusion == Fusion::Prf {
        return prf_search(engine, query, limit, now);
    }
    // The union is a *pool builder*, not a ranker: it runs several arms and merges
    // their membership. Returning here keeps that fan-out out of the path every
    // other arm walks, and keeps the recursion one level deep by construction
    // (`UNION_SOURCES` never contains `Union`).
    if fusion == Fusion::Union {
        return union_search(engine, query, limit, now);
    }
    let tokens = content_tokens(query);
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    // Corpus totals are fetched for every fusion now, not only the IDF ones: the
    // common-token skip needs the same denominator, and a query that costs a second
    // of I/O to save two COUNT queries is not a trade anyone would take.
    let totals = corpus_totals(engine);
    let total_docs = (totals.0 + totals.1) as f64;
    // Probe every token's df up front, then search rarest-first. The df probes
    // are bounded ([`token_df`]), so this costs O(tokens) flat — and it buys two
    // things the old digit/length ordering could not: the always-searched *first*
    // token is now the most selective one the query has (an "about {topic}
    // record" query seeds on {topic}, not on the ubiquitous "record"), and the
    // skip rule applies to a fully informed list instead of whatever the length
    // sort happened to put first.
    let mut probed: Vec<(String, f64, bool)> = tokens
        .into_iter()
        .map(|t| {
            let (df, capped) = token_df(engine, &t);
            (t, df, capped)
        })
        .collect();
    probed.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    let mut scores: HashMap<i64, f64> = HashMap::new();
    // The per-token *maximum* raw hit count, deliberately not a sum: a sum would
    // punish exactly the multi-token matches the accumulation is rewarding.
    let mut chunks: HashMap<i64, usize> = HashMap::new();
    // How many *distinct* query facets a candidate answered (W3.1). Counted here,
    // where one increment is exactly one token, rather than derived later from a
    // score that has already been weighted.
    let mut facets: HashMap<i64, usize> = HashMap::new();
    let mut order: Vec<i64> = Vec::new();
    // Read once per search, not per token: the arena's depth sweep sets this, and
    // a per-token environment read would make the knob cost more than it measures.
    let depth = per_token_depth();

    for index in select_searchable_tokens(&probed, total_docs) {
        let (token, df, _) = &probed[index];
        let weight = if fusion.uses_idf() {
            idf_from_df(*df, totals)
        } else {
            1.0
        };
        let mut best: HashMap<i64, (f64, usize)> = HashMap::new();
        let mut raw: HashMap<i64, usize> = HashMap::new();
        let mut token_order: Vec<i64> = Vec::new();

        // The primary list, and the arm's single most consequential choice: ask
        // `hybrid_search` (BM25 text matches with the 1-hop graph fused in) or ask
        // the entity index directly (text matches only, raw scores).
        let by_score = fusion.uses_scores();
        if fusion.fuses_graph_inline() {
            for (rank, (e, score)) in engine.hybrid_search(token, depth, now)?.iter().enumerate() {
                note(
                    &mut token_order,
                    &mut best,
                    &mut raw,
                    e.id,
                    rank,
                    *score,
                    false,
                );
            }
        } else {
            for (rank, (e, score)) in engine
                .search_entities(token, depth, now)?
                .iter()
                .enumerate()
            {
                note(
                    &mut token_order,
                    &mut best,
                    &mut raw,
                    e.id,
                    rank,
                    *score,
                    by_score,
                );
            }
        }
        // Fact hits carry the prose, so a query word that only appears in a fact
        // still lands on the entity that owns it — and on the external datasets
        // this is the list that matched anything at all when the entity index had
        // nothing to say about a prose question.
        for (rank, (o, score)) in engine
            .search_observations(token, depth, now)?
            .iter()
            .enumerate()
        {
            note(
                &mut token_order,
                &mut best,
                &mut raw,
                o.entity_id,
                rank,
                *score,
                by_score,
            );
        }

        for id in token_order {
            let (score, rank) = best.get(&id).copied().unwrap_or((0.0, 0));
            let contribution = if by_score {
                weight * score
            } else {
                weight / (K + rank as f64)
            };
            if !scores.contains_key(&id) {
                order.push(id);
            }
            // W1.2 — the combiner, and the *only* difference between `Bm25` and
            // `best_chunk`. Both arms see this identical candidate set, these
            // identical per-token scores and these identical caps; one sums the
            // contributions and one keeps the strongest.
            let slot = scores.entry(id).or_default();
            *slot = if fusion == Fusion::BestChunk {
                (*slot).max(contribution)
            } else {
                *slot + contribution
            };
            *facets.entry(id).or_insert(0) += 1;
            let raw_for_token = raw.get(&id).copied().unwrap_or(0);
            let slot = chunks.entry(id).or_insert(0);
            *slot = (*slot).max(raw_for_token);
        }
    }

    let mut out: Vec<(Entity, f64)> = Vec::new();
    for id in order {
        let Some(e) = engine.entity_at(id, now)? else {
            continue;
        };
        let mut score = scores.get(&id).copied().unwrap_or(0.0);
        if matches!(
            fusion,
            Fusion::RrfIdfNorm | Fusion::RrfIdfTemporal | Fusion::RrfIdfNormBounded
        ) {
            // Length normalization. Contribution is already *per token*, but a
            // document that matched one token in forty chunks still enjoyed forty
            // chances to be inside the window, so its worst single-token hit count
            // is the length proxy this divide wants. `sqrt` because length
            // discounting in BM25-class scoring is sub-linear, not proportional.
            //
            // W2.2: the *bounded* arm clamps the divisor inside the range rank
            // fusion can express (see [`NORM_MAX_DIVISOR`]), which is the one thing
            // separating it from the arm we refused without diagnosis.
            let matched = chunks.get(&id).copied().unwrap_or(1).max(1) as f64;
            score /= if fusion == Fusion::RrfIdfNormBounded {
                matched.sqrt().clamp(1.0, NORM_MAX_DIVISOR)
            } else {
                matched.sqrt()
            };
        }
        if fusion == Fusion::RrfIdfTemporal {
            score *= recency_multiplier(e.valid_from, now);
        }
        if fusion == Fusion::FacetCoverage {
            // W3.1 — set assembly, priced as a multiply rather than a second pass
            // over the corpus. `+1` so a single-facet candidate keeps its score
            // instead of being zeroed, and the count is of *distinct* facets, which
            // is what a multi-hop question actually requires.
            let covered = facets.get(&id).copied().unwrap_or(0) as f64;
            score *= 1.0 + covered;
        }
        out.push((e, score));
    }
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // W4.1 — query-side temporal intent, applied **after** ranking and inside a
    // bounded window, because it reads fact text per candidate and an unbounded
    // pass would turn one date word into a scan of every candidate's evidence.
    if fusion == Fusion::TemporalIntent {
        apply_temporal_intent(engine, query, &mut out, now)?;
    }

    // Graph capability, kept but *demoted* to the tail.
    //
    // `hybrid_search` fuses 1-hop neighbours into the ranked list, and both real
    // datasets say that costs precision (the arena's `rrf` vs `rrf_no_graph`).
    // Appending neighbours below every scored hit keeps the capability — a query
    // with few lexical matches still reaches related entities — without letting a
    // traversal outrank a text match. Score 0.0 is honest: these entities earned no
    // lexical score, they were reached by following an edge from one that did.
    if matches!(fusion, Fusion::Bm25Graph | Fusion::GraphReserve) || fusion.has_dense_arm() {
        let seeds: Vec<i64> = out
            .iter()
            .take(GRAPH_SEED_COUNT)
            .map(|(e, _)| e.id)
            .collect();
        let mut seen: HashSet<i64> = out.iter().map(|(e, _)| e.id).collect();
        let mut tail: Vec<(Entity, f64)> = Vec::new();
        for edge in engine.expand_frontier(&seeds, now)? {
            for id in [edge.target_id, edge.source_id] {
                if id != 0
                    && seen.insert(id)
                    && let Some(e) = engine.entity_at(id, now)?
                {
                    tail.push((e, 0.0));
                }
            }
        }
        out.extend(tail);
    }

    // Causal expansion (the AMA-Bench arm): `Fusion::CausalGraph`.
    //
    // Deliberately **separate** from the generic tail above rather than layered on top of it. This
    // arm's question is whether the *kind* of an edge carries information, and it can only answer
    // that if it reads causal edges and nothing else — a causal neighbour appended after an
    // undifferentiated tail could not be attributed to either mechanism. `bm25_graph` is therefore
    // this arm's **control**, and this arm is not a variant of it.
    //
    // Neighbours **score** here instead of tailing. The convention above parks neighbours at 0.0,
    // which is honest for a traversal that makes no claim about relevance, but fatal for this one:
    // a 0.0 neighbour sorts below every lexical hit and cannot enter the cut, which is exactly the
    // invisibility that made `bm25_graph` score identical to `bm25` to the digit on all 500 groups.
    // A causal neighbour inherits a fraction of its seed's score instead, so it surfaces where its
    // seed was strong — and the fraction is per-kind, so a run can say which kind carried it.
    if fusion.follows_causal_edges() {
        let seeds: Vec<(i64, f64)> = out
            .iter()
            .take(GRAPH_SEED_COUNT)
            .map(|(e, s)| (e.id, *s))
            .collect();
        let mut seen: HashSet<i64> = out.iter().map(|(e, _)| e.id).collect();
        let mut causal: Vec<(Entity, f64)> = Vec::new();
        let soft = fusion.caps_causal_inheritance();
        for (id, score, _kind) in causal_frontier(engine, &seeds, now, soft)? {
            if !seen.insert(id) {
                continue;
            }
            if let Some(e) = engine.entity_at(id, now)? {
                causal.push((e, score));
            }
        }
        out.extend(causal);
    }

    // The dense arm (W0.1). It runs *here*, after the lexical ranking and its graph
    // tail are complete, because it does not replace lexical retrieval — it fuses
    // with it. `out` is a rank list by this point, which is exactly what RRF wants,
    // and the two lists are chosen to fail differently: lexical misses a question
    // phrased in words the document never used, dense misses exact identifiers, and
    // rank fusion keeps whichever list actually found the document. Neither list
    // outranks the other by construction — contribution depends only on rank.
    if fusion.has_dense_arm()
        && let Ok(Some((dense, truncated))) = dense::dense_ranking(engine, query, now)
    {
        if truncated {
            warn_dense_truncated_once();
        }
        match fusion {
            // Placement rule 1 — interleaved by rank. This is the arm that
            // measured 35 pp of `any@1` loss on the synthetic corpus, and it is
            // kept under its own name so that regression stays attributable rather
            // than becoming folklore. `DenseWeighted` is the same rule with the
            // dense arm's share scaled, because equal weight was an assumption.
            Fusion::DenseHybrid | Fusion::DenseWeighted => {
                let weight = dense_weight(fusion);
                let mut fused: HashMap<i64, (Entity, f64)> = HashMap::with_capacity(out.len());
                for (rank, (e, _)) in out.iter().enumerate() {
                    let slot = fused.entry(e.id).or_insert_with(|| (e.clone(), 0.0));
                    slot.1 += 1.0 / (K + rank as f64);
                }
                for (rank, (id, _)) in dense.iter().enumerate() {
                    let contribution = weight / (K + rank as f64);
                    match fused.get_mut(id) {
                        Some(slot) => slot.1 += contribution,
                        None => {
                            // A document only the dense arm found is the whole point
                            // of these arms, so it is admitted rather than filtered —
                            // that is what raises the ceiling instead of reordering.
                            if let Some(e) = engine.entity_at(*id, now)? {
                                fused.insert(*id, (e, contribution));
                            }
                        }
                    }
                }
                out = fused.into_values().collect();
                out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            }
            // Placement rule 2 — appended below every lexical hit, exactly as
            // `Bm25Graph` appends the graph. **A lexical hit cannot be displaced by
            // construction, so `any@1` cannot fall, while a dense-only document
            // still enters the cut, so the ceiling can rise.** That is the whole
            // point: it separates the two effects the interleaved arm conflates,
            // and it makes the next run decisive instead of another hopeful one.
            Fusion::DenseTail => {
                let lexical = out.len();
                let mut seen: HashSet<i64> = out.iter().map(|(e, _)| e.id).collect();
                for (id, _) in dense.iter() {
                    if out.len() >= lexical + limit {
                        break;
                    }
                    if seen.insert(*id)
                        && let Some(e) = engine.entity_at(*id, now)?
                    {
                        // 0.0 is honest: no lexical score earned the placement.
                        out.push((e, 0.0));
                    }
                }
            }
            _ => {}
        }
    }

    // `GraphReserve` — the traversal's slots **inside** the cut.
    //
    // The graph tail above is appended after a candidate pool that already holds `pool` lexical
    // hits, so on its own it is truncated away and the arm measures whatever `bm25` measures
    // (verified: identical to the digit on all 500 groups). Truncating the lexical half first and
    // then re-appending the reserved number of traversal hits is what makes the graph visible, at
    // a cost of at most `graph_reserve()` lexical positions — which is exactly the trade this arm
    // exists to price.
    if matches!(fusion, Fusion::GraphReserve) {
        let reserve = graph_reserve();
        let lexical_keep = limit.saturating_sub(reserve);
        let lexical: Vec<(Entity, f64)> = out.iter().take(lexical_keep).cloned().collect();
        let mut present: HashSet<i64> = lexical.iter().map(|(e, _)| e.id).collect();
        let mut reserved: Vec<(Entity, f64)> = Vec::new();
        for (entity, score) in out.iter().skip(lexical_keep) {
            if reserved.len() >= reserve {
                break;
            }
            // Only genuinely traversal-reached entities qualify. A lexical hit that merely fell
            // below the cut is not the capability under test, and admitting it would make the arm
            // a no-op that still reported a graph effect.
            if *score == 0.0 && present.insert(entity.id) {
                reserved.push((entity.clone(), 0.0));
            }
        }
        out = lexical;
        out.extend(reserved);
    }

    out.truncate(limit);
    Ok(out)
}

/// Warn once per process — not per query — when the dense index was cut short by
/// `MEMNOTH_DENSE_MAX_ENTITIES`.
///
/// A truncated index lowers the ceiling **silently**, which is precisely the failure
/// mode this arm exists to remove, so it cannot be allowed to pass unmentioned. It is
/// a deployment condition rather than a per-search event, so once is enough.
fn warn_dense_truncated_once() {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if WARNED.set(()).is_ok() {
        eprintln!(
            "memnoth: dense index truncated at MEMNOTH_DENSE_MAX_ENTITIES — recall above that \
             bound is not measured. Raise the limit or expect a lower ceiling."
        );
    }
}

/// Seed count for the graph tail. Matches the seeds `hybrid_search` uses for its
/// own inline expansion, so the tail is the same traversal — only its *placement*
/// changed.
const GRAPH_SEED_COUNT: usize = 10;

/// Facts per candidate examined by the rerank, and tokens per fact.
///
/// These two bounds are the rerank's cost model. The pass is opt-in and measured to
/// *cost recall* on both real datasets (`eval::evidence_fusion_arena`: LongMemEval
/// all@3 73.6% to 56.0%), so it exists for callers who want a diverse list rather
/// than a relevant one — and a caller using it for diversity should not be charged
/// for reading the entire corpus to get it.
const MAX_RERANK_FACTS: usize = 32;
const MAX_RERANK_TOKENS_PER_FACT: usize = 64;

/// Characters of evidence text attached to each `mem_search` hit by default.
///
/// Set at the knee of a measured frontier, not picked for roundness. Swept on the
/// third-party LoCoMo lane (272 documents, 1986 queries, one provider per budget,
/// `MEMNOTH_TEXT_CHARS`), content-hit against characters returned per query:
///
/// | budget | content-hit | chars/query | ~tokens/query |
/// |---|---|---|---|
/// | 0 (names only) | 0.000 | 0 | 0 |
/// | 320 | 0.060 | 1 601 | 400 |
/// | **1 200** | **0.328** | **5 877** | **1 469** |
/// | unlimited | 0.332 | 5 973 | 1 493 |
/// | filesystem `grep` (baseline) | 0.297 | 9 960 | 2 490 |
///
/// So 1200 is where the answer starts actually being *in* the response: past it the
/// curve is flat to within 0.004, and below it the server hands back pointers
/// instead of memories. It also clears the `grep` baseline on the metric while
/// using 41% fewer characters to do it — which is the only form of the context-tax
/// claim that survives a like-for-like comparison. A caller who only wants
/// candidates sets `text_chars: 0`.
///
/// Assembling several facts to fill the budget was measured against clipping the
/// single best one: content-hit 0.328 either way at 1200 (+2% characters). It is
/// kept because it is the version that degrades gracefully — a small budget still
/// yields the best fact, and a document chunked differently than LoCoMo's is not
/// forced to report an arbitrary slice — but it is **not** claimed as an
/// improvement, because on the one corpus that could measure it, it was not one.
const DEFAULT_SNIPPET_CHARS: usize = 1200;

/// Attach the evidence itself to each ranked hit.
///
/// A hit that names a document but does not carry it is a *pointer*, not a memory:
/// the third-party retrieval harness scores a `content_hit` (does the returned
/// text contain the expected answer) and this server scored **0.000** on it while
/// a filesystem `grep` scored 0.297 — because every result was a path and a score.
/// A host given only names has to spend a second round trip per hit to learn
/// anything, which is exactly the context plumbing this server exists to remove.
///
/// `text_chars: 0` restores the name-and-score payload for callers that only want
/// candidates, and keeps the smaller context cost measurable either way.
fn attach_evidence(
    engine: &MemoryEngine,
    query_tokens: &[String],
    mut results: Vec<Value>,
    text_chars: usize,
    now: i64,
) -> Result<Vec<Value>> {
    if text_chars == 0 {
        return Ok(results);
    }
    for row in results.iter_mut() {
        let Some(id) = row.get("id").and_then(Value::as_i64) else {
            continue;
        };
        if let Some(text) = evidence_for(engine, id, query_tokens, now, text_chars)? {
            row["text"] = json!(text);
        }
    }
    Ok(results)
}

/// S1 fixed-cost output packing: keep the ranked results, cap the context.
///
/// This is the call site the S1 contract was written for. `attach_evidence`
/// fills each row to the *character* budget the host asked for, and a 20-row
/// answer at the default snippet size is unbounded in *tokens* — one row per
/// session, each session a different size. `pack_top_down` walks the ranked
/// rows once, keeps what fits in [`recall_budget::TOP200_PACK_TOKEN_BUDGET`],
/// skips an oversize row rather than truncating the answer, and is never empty:
/// if nothing fits, the head is kept alone and `over_budget` says so. Rank
/// order is preserved — packing never reorders a ranking.
///
/// The report rides along with the answer so a breach is visible to the caller
/// (and to the receipt) instead of being absorbed by the harness. It is four
/// small integers, and the tokens it costs are counted in the same pass.
/// The S1 pack budget actually applied, overridable for measurement.
///
/// **Why the override exists — a measured ceiling, three layers down.** `mem_search`'s total payload
/// was capped twice over and neither cap was visible from the caller. The answer lane asked for a
/// 60 000-character window and got **3 020 tokens**; it asked for 116 000 and got **6 801** — the same
/// number it got for 60 000, to four significant figures, which is what a *ceiling* looks like and not
/// what a *budget* looks like. The ceilings were: the answer lane's own per-session cap (now a knob,
/// [`PerSessionKey`] in `eval.rs`), and this one — `TOP200_PACK_TOKEN_BUDGET` = **7 000 tokens**, of
/// which the observed 6 801 is 97 %.
///
/// That matters because the reference point is not 7 000. BEAM's own harness uses a **29 000-token**
/// context window, so three consecutive arms were scored at 23 % of the reference's context while
/// being read as statements about prompting, retrieval and depth.
///
/// The default is unchanged, so the S1 contract ("single-pass top-200 packed into at most 7K
/// tokens/query") still holds and every figure recorded before this knob existed stays reproducible.
/// Reading the value once per search rather than caching it in a `OnceLock` is deliberate: a `OnceLock`
/// would fix the budget at whatever the first search in the process saw, which makes a run that changes
/// it mid-flight silently inconsistent with its own report.
fn pack_token_budget() -> u64 {
    std::env::var("MEMNOTH_PACK_TOKEN_BUDGET")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(recall_budget::TOP200_PACK_TOKEN_BUDGET)
}

fn pack_search_results(results: Vec<Value>) -> (Vec<Value>, Value) {
    let items: Vec<(Value, u64)> = results
        .into_iter()
        .map(|row| {
            let tokens = recall_budget::exact_tokens(&row.to_string());
            (row, tokens)
        })
        .collect();
    let budget = pack_token_budget();
    let report = recall_budget::pack_top_down(items, budget);
    let packing = json!({
        "kept": report.kept.len(),
        "skipped_oversize": report.skipped_oversize,
        "tokens": report.tokens(),
        "budget": report.token_budget,
        "over_budget": report.over_budget,
        // Which ceiling produced this answer, in the answer itself. A caller reading `tokens` beside
        // `budget` can now tell "the ranking had nothing more" from "the server cut me off", which is
        // the distinction every figure in this lane's ledger depends on and could not previously make.
        "budget_source": if std::env::var("MEMNOTH_PACK_TOKEN_BUDGET").is_ok() {
            "MEMNOTH_PACK_TOKEN_BUDGET"
        } else {
            "TOP200_PACK_TOKEN_BUDGET"
        },
    });
    (report.kept.into_iter().map(|p| p.item).collect(), packing)
}

/// A partial fact is only worth including if there is room for a readable slice;
/// below this the remainder is noise that costs tokens without carrying a phrase.
const MIN_PARTIAL_CHARS: usize = 40;

/// The evidence for *this* query on one entity, assembled to the character budget.
///
/// Facts are ordered by **query-token overlap**, then by recency — the point is to
/// answer the question asked, and an entity with forty facts would otherwise hand
/// back whichever happened to be written first. Ties going to the newer fact means
/// a corrected belief is quoted in preference to the one it replaced.
///
/// Then, crucially, it **fills the budget with several facts rather than clipping
/// one**. A session is stored as several chunks, so the answer is frequently in a
/// sibling chunk of the fact that matched best: returning one clipped chunk spends
/// the same tokens and reports less. Assembling until the budget is spent is what
/// turns a snippet into evidence. One query per returned row, bounded by the
/// caller's own `limit` — no model, and no second pass over the corpus.
fn evidence_for(
    engine: &MemoryEngine,
    entity_id: i64,
    query_tokens: &[String],
    now: i64,
    max_chars: usize,
) -> Result<Option<String>> {
    let mut ranked: Vec<(usize, i64, String)> = engine
        .observations_for(entity_id, now)?
        .into_iter()
        .map(|o| {
            let lower = o.content.to_lowercase();
            let overlap = query_tokens
                .iter()
                .filter(|t| lower.contains(t.as_str()))
                .count();
            (overlap, o.valid_from, o.content)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

    let mut out = String::new();
    for (_, _, content) in ranked {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }
        let used = out.chars().count();
        let separator = usize::from(!out.is_empty());
        if used + separator + trimmed.chars().count() > max_chars {
            let remaining = max_chars.saturating_sub(used + separator);
            if remaining >= MIN_PARTIAL_CHARS {
                if separator == 1 {
                    out.push(' ');
                }
                out.push_str(&truncate_chars(trimmed, remaining));
            }
            break;
        }
        if separator == 1 {
            out.push(' ');
        }
        out.push_str(trimmed);
    }
    Ok(if out.is_empty() { None } else { Some(out) })
}

/// Truncate on a *character* boundary, not a byte one, and mark the cut — so a
/// multi-byte character can never be split into invalid UTF-8, and a reader can
/// never mistake a clipped fact for a complete one.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", head.trim_end())
}

/// Rerank a fused candidate list with vault-native signals — the pass between
/// RRF fusion and the answer that mem0/hindsight/graphiti all perform and we
/// used to skip.
///
/// Where the cross-encoder lives, if it is installed.
///
/// `MEMNOTH_CROSS_ENCODER_DIR` wins so a deployment can point at its own copy;
/// otherwise the conventional bench location is used. Resolution is separated from
/// loading so a missing model costs one `stat`, not an ONNX parse.
///
/// Only the `semantic` build can load a model, so the resolver is compiled for that
/// build alone; in a build without the ONNX stack nothing would call it.
#[cfg(feature = "semantic")]
fn cross_encoder_dir() -> Option<std::path::PathBuf> {
    let looks_like_a_model =
        |p: &std::path::Path| p.join("model.onnx").exists() && p.join("tokenizer.json").exists();
    if let Some(dir) = std::env::var_os("MEMNOTH_CROSS_ENCODER_DIR") {
        let p = std::path::PathBuf::from(dir);
        if looks_like_a_model(&p) {
            return Some(p);
        }
    }
    // Both `HOME` and `USERPROFILE` are candidates rather than one-or-the-other: a
    // shell that exports a Git-style `HOME` (`/c/Users/...`) leaves the joined path
    // unopenable to a Windows process, and the model is then reported missing while
    // sitting on disk. Trying both and keeping whichever actually holds the files
    // costs one `stat` on the wrong candidate and removes that whole class of
    // silent fallback.
    for home in ["HOME", "USERPROFILE"] {
        let Some(base) = std::env::var_os(home) else {
            continue;
        };
        let p = std::path::PathBuf::from(base).join("memnoth-memory-bench/models/ce-minilm-l6");
        if looks_like_a_model(&p) {
            return Some(p);
        }
    }
    None
}

/// The reranker type.
///
/// With the `semantic` feature this is the real ONNX cross-encoder. Without it
/// there is no runtime that could build one, so a local stand-in keeps the
/// signature; [`cross_encoder`] never hands one out, and the call sites fall
/// back to the fused order exactly as they do when no model is installed.
#[cfg(feature = "semantic")]
use memnoth_embedding::CrossEncoder;

#[cfg(not(feature = "semantic"))]
struct CrossEncoder;

#[cfg(not(feature = "semantic"))]
#[allow(dead_code)]
impl CrossEncoder {
    /// Mirrors `memnoth_embedding::CrossEncoder::rerank`. Unreachable in a
    /// build without the semantic stack — the guard, not a code path.
    fn rerank(
        &self,
        _query: &str,
        _candidates: &[(String, String)],
    ) -> anyhow::Result<Vec<(String, f32)>> {
        unreachable!("no cross-encoder without the `semantic` feature")
    }
}

/// The process-wide cross-encoder, loaded at most once.
///
/// Loading parses and optimises a ~90 MB ONNX graph, which is far too expensive to
/// do per search, so it is cached behind a `OnceLock`. **A missing model is cached
/// too** — every search in a process without the model pays one filesystem check
/// and nothing else, and the caller falls back to the fused order rather than
/// failing.
pub(crate) fn cross_encoder() -> Option<&'static CrossEncoder> {
    #[cfg(not(feature = "semantic"))]
    {
        // No ONNX runtime is linked in, so no model could be loaded even if one is on disk.
        None
    }
    #[cfg(feature = "semantic")]
    {
        static CE: std::sync::OnceLock<Option<CrossEncoder>> = std::sync::OnceLock::new();
        CE.get_or_init(|| {
            let dir = match cross_encoder_dir() {
                Some(d) => d,
                None => {
                    // **The one fallback that used to be silent, and it is the expensive one.**
                    //
                    // A missing cross-encoder turns every `rerank: cross_encoder` arm into the
                    // arm it was supposed to differ from — so a sweep reports "the reranker
                    // changes nothing" when in fact the reranker never ran, and the negative
                    // result gets attributed to the technique. Observed for real: the model had
                    // been downloaded under a previous home directory, and this process's
                    // `HOME` had moved to a new profile, so the path was correct in the docs and
                    // empty on disk.
                    warn_cross_encoder_missing_once();
                    return None;
                }
            };
            match memnoth_embedding::CrossEncoder::load_from_dir(&dir) {
                Ok(ce) => Some(ce),
                Err(e) => {
                    eprintln!(
                        "memnoth: cross-encoder at {} failed to load: {e}",
                        dir.display()
                    );
                    None
                }
            }
        })
        .as_ref()
    }
}

/// Warn once per process when a rerank was requested and **no cross-encoder model is
/// installed**.
///
/// Once, because this is a deployment condition and not a per-search event — but not
/// never, because a silent `None` here is indistinguishable from "the reranker was
/// measured and did not help". The message names every path that was checked, since the
/// usual cause is a moved `HOME` rather than a missing download.
#[cfg(feature = "semantic")]
fn warn_cross_encoder_missing_once() {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if WARNED.set(()).is_ok() {
        eprintln!(
            "memnoth: cross-encoder rerank requested but NO model was found, so every \
             `cross_encoder` result in this process is the fused order UNCHANGED. Checked \
             MEMNOTH_CROSS_ENCODER_DIR, then \
             <home>/memnoth-memory-bench/models/ce-minilm-l6 for both HOME and USERPROFILE."
        );
    }
}

/// Cross-encoder rerank (W2.1): score each candidate's passage **jointly** with the
/// query, then order by that score.
///
/// This is the semantic reranker `rerank_candidates` explicitly is not. The MMR
/// pass re-reads the *same* lexical evidence RRF already used and adds a diversity
/// penalty; a cross-encoder reads the query and the passage together and can
/// therefore promote a candidate that shares little vocabulary with the question —
/// which is the measured failure mode of the open-domain class (a 73.9% `any@10`
/// ceiling at the time of writing).
///
/// Cost model, deliberately bounded the way the MMR bound was: one forward pass per
/// candidate over a truncated passage, so the total is O(candidates x PASSAGE_CHARS)
/// and independent of how long the candidate's document is. Candidates are taken
/// from the already-fused pool, so the pass can only *reorder* what fusion found —
/// it does not raise the ceiling, and must not be claimed to.
///
/// Returns `None` when no model is installed, so the caller keeps the fused order.
///
/// Takes the candidate pool **by reference**: the caller still needs `hits` in the
/// fallback arm when no model is installed, and cloning the pool at every call site
/// to satisfy ownership would undo the cost bound this pass exists to respect.
/// Bounded text for one candidate: name, summary, then facts until the budget.
///
/// Shared by both rerankers on purpose. A cross-encoder and an LLM must be given
/// **the same view of a candidate**, or a comparison between them measures the
/// passage builder instead of the ranker — and the passage rules (name first, facts
/// carry the answer, stop at the budget) are a real modelling decision that should
/// exist once.
fn candidate_passage(engine: &MemoryEngine, entity: &Entity, now: i64, chars: usize) -> String {
    let mut passage = String::with_capacity(chars);
    passage.push_str(&entity.name);
    if let Some(summary) = entity.summary.as_deref() {
        passage.push(' ');
        passage.push_str(summary);
    }
    // Facts carry the document body, so they are where a question's answer actually
    // lives; stop as soon as the budget is spent.
    if let Ok(obs) = engine.observations_for(entity.id, now) {
        for o in obs {
            if passage.chars().count() >= chars {
                break;
            }
            passage.push(' ');
            passage.push_str(&o.content);
        }
    }
    passage.chars().take(chars).collect()
}

/// How deep a pool the cross-encoder is allowed to reorder.
///
/// This is the **ceiling of the conversion lever**: a reranker can only promote what
/// candidate generation returned, so a pool of `limit × 8` caps how far down the
/// lexical ranking a correct answer can be rescued from. The pass is local and cheap
/// (no network, no cost per candidate beyond one forward run), so a deeper pool buys
/// recall that a shallower one cannot — and `MEMNOTH_CE_POOL_FACTOR` is how the depth
/// becomes a measured choice rather than a constant nobody revisited.
///
/// Bounded at `CE_MAX_POOL` because the cost is linear in candidates and this pass is
/// opt-in per call: an unbounded pool would make `rerank=cross_encoder` on a large
/// vault a slower search than the caller asked for.
const CE_MAX_POOL: usize = 400;

fn ce_pool_factor() -> usize {
    std::env::var("MEMNOTH_CE_POOL_FACTOR")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(8)
}

/// Candidate pool for the assisted rerank, and the characters of each candidate
/// that reach the prompt. Both bounded because this pass spends money.
const LLM_RERANK_POOL_FACTOR: usize = 4;
const LLM_RERANK_MAX_POOL: usize = 80;
const LLM_RERANK_PASSAGE_CHARS: usize = 480;

/// Assisted listwise rerank (W2.4) — the lane every high published number sits in.
///
/// Returns `None` when there is no provider, when the pool is too small to reorder,
/// or when the provider fails — and the caller keeps the fused order and says why.
/// The score is **rank-derived and normalized to 0..1**, so `score` means the same
/// thing in `off`, `mmr`, `cross_encoder` and `llm` modes; a raw logit or a raw
/// position in one mode and a fraction in another is how a host ends up comparing
/// incomparable numbers.
fn rerank_llm(
    engine: &MemoryEngine,
    query: &str,
    candidates: &[(Entity, f64)],
    limit: usize,
    now: i64,
) -> Option<(Vec<(Entity, f64)>, String)> {
    let client = llm::client()?;
    if candidates.len() < 2 {
        return None;
    }
    let passages: Vec<(i64, String)> = candidates
        .iter()
        .map(|(e, _)| {
            (
                e.id,
                candidate_passage(engine, e, now, LLM_RERANK_PASSAGE_CHARS),
            )
        })
        .collect();
    let order = match llm::rerank(&client, query, &passages, limit) {
        Ok(order) => order,
        Err(e) => {
            eprintln!("memnoth: assisted rerank failed, keeping fused order: {e}");
            return None;
        }
    };
    let by_id: HashMap<i64, &(Entity, f64)> = candidates.iter().map(|c| (c.0.id, c)).collect();
    let total = order.len().max(1) as f64;
    let mut out: Vec<(Entity, f64)> = Vec::with_capacity(order.len());
    for (rank, id) in order.iter().enumerate() {
        if let Some((entity, _)) = by_id.get(id) {
            out.push(((*entity).clone(), 1.0 - rank as f64 / total));
        }
    }
    out.truncate(limit);
    // The label is the disclosure: `assisted` and the model name travel in the
    // response, so a number produced with a model can never be quoted as a
    // model-free one — the failure that makes an assisted benchmark untrustworthy.
    let method = format!(
        "llm listwise rerank — assisted (provider={}, model={})",
        client.provider_label(),
        client.model()
    );
    Some((out, method))
}

/// Cross-encoder rerank over code-graph hits.
///
/// The same model and the same contract as the memory path's
/// `rerank_cross_encoder`: score each candidate jointly with the query, and on a
/// missing model or a failure return `None` so the caller keeps the fused order.
/// A reranker is an optional model and must never turn a working search into a
/// failed one.
///
/// Passages are `kind qname signature` — the text the vector arm embeds — so the
/// reranker and the retriever are judging the same representation rather than
/// two subtly different ones.
pub(crate) fn rerank_cross_encoder_symbols(
    query: &str,
    candidates: &[memnoth_codegraph::ScoredHit],
    limit: usize,
) -> Option<(Vec<memnoth_codegraph::ScoredHit>, &'static str)> {
    let ce = cross_encoder()?;
    if candidates.len() < 2 {
        return None;
    }
    // This model truncates at 256 tokens, so the head of the text is what can
    // move the score; bounding here also keeps the pass linear in the pool.
    const PASSAGE_CHARS: usize = 512;
    let passages: Vec<String> = candidates
        .iter()
        .map(|scored| {
            let hit = &scored.hit;
            let text = format!(
                "{} {} {}",
                hit.kind,
                hit.qname,
                hit.signature.as_deref().unwrap_or("")
            );
            text.chars().take(PASSAGE_CHARS).collect()
        })
        .collect();
    let scored = match ce.rerank(query, &passages) {
        Ok(Some(scores)) => scores,
        Ok(None) => return None,
        Err(e) => {
            eprintln!(
                "memnoth: cross-encoder rerank failed on kg_search, keeping fused order: {e}"
            );
            return None;
        }
    };
    let by_id: HashMap<i64, &memnoth_codegraph::ScoredHit> =
        candidates.iter().map(|c| (c.symbol_id, c)).collect();
    let mut out: Vec<memnoth_codegraph::ScoredHit> = Vec::with_capacity(candidates.len());
    // The reranker's own order is what reorders these hits; the scores are not
    // read again, so only the position matters here.
    for (idx, _score) in scored.into_iter().enumerate() {
        if idx >= candidates.len() {
            break;
        }
        let symbol_id = candidates[idx].symbol_id;
        if let Some(hit) = by_id.get(&symbol_id) {
            out.push((*hit).clone());
        }
    }
    out.truncate(limit);
    if out.is_empty() {
        return None;
    }
    Some((out, "cross_encoder"))
}

fn rerank_cross_encoder(
    engine: &MemoryEngine,
    query: &str,
    candidates: &[(Entity, f64)],
    limit: usize,
    now: i64,
) -> Option<(Vec<(Entity, f64)>, &'static str)> {
    let ce = cross_encoder()?;
    if candidates.len() < 2 {
        return None;
    }
    // A cross-encoder is trained on short pairs and this model truncates at 256
    // tokens, so the head of an entity's text is what can move the score. Bounding
    // here also keeps the pass linear in the pool rather than in the corpus.
    const PASSAGE_CHARS: usize = 512;
    let passages: Vec<String> = candidates
        .iter()
        .map(|(e, _)| candidate_passage(engine, e, now, PASSAGE_CHARS))
        .collect();
    let scored = match ce.rerank(query, &passages) {
        Ok(Some(s)) => s,
        Ok(None) => return None,
        Err(e) => {
            // A rerank failure must not fail the search: fall back, and say so.
            eprintln!("memnoth: cross-encoder rerank failed, keeping fused order: {e}");
            return None;
        }
    };
    // Borrow the candidates in place; building `&(e.clone(), *s)` temporaries here
    // does not compile, because the borrowed tuple would be dropped at the end of
    // the collecting statement.
    let by_id: HashMap<i64, &(Entity, f64)> = candidates.iter().map(|c| (c.0.id, c)).collect();
    let mut out: Vec<(Entity, f64)> = Vec::with_capacity(candidates.len());
    for (idx, score) in scored.into_iter().enumerate() {
        if idx >= candidates.len() {
            break;
        }
        let id = candidates[idx].0.id;
        if let Some((e, _)) = by_id.get(&id) {
            out.push(((*e).clone(), score as f64));
        }
    }
    out.truncate(limit);
    // Cross-encoder scores are raw logits and can be negative. Normalize to the same
    // 0..1 convention MMR already uses (best = 1.0) so a host that reads `score` is
    // not silently comparing a logit in one mode against a normalised value in
    // another.
    if out.len() > 1 {
        let min = out.iter().map(|(_, s)| *s).fold(f64::INFINITY, f64::min);
        let max = out
            .iter()
            .map(|(_, s)| *s)
            .fold(f64::NEG_INFINITY, f64::max);
        if max > min {
            for (_, s) in &mut out {
                *s = (*s - min) / (max - min);
            }
        }
    }
    Some((
        out,
        "cross-encoder (query-passage joint scoring); no lexical fusion re-read",
    ))
}

/// There is **no embedding model** driving this pass, so it is not a semantic
/// reranker and must not be sold as one. (A real cross-encoder is reachable via
/// `rerank=cross_encoder`; this is the model-free alternative.) It is a Maximal
/// Marginal Relevance pass whose two signals come from data the vault already holds:
///
/// 1. **Lexical relevance** — the share of the query's word tokens that appear
///    in the entity's name/summary and its current facts. RRF already ranks by
///    *which list* a hit made; this re-reads *the text* and can demote a hit
///    that fused well but shares no vocabulary with the question.
/// 2. **Redundancy penalty** — MMR's core: once an entity is picked, later
///    candidates that share many of its facts are pushed down, so ten variants
///    of the same belief stop crowding out the fifth distinct one.
///
/// Output score is normalized 0..1 within the result (rank-preserving on the
/// final order; the raw fused score is gone, so a caller comparing across
/// calls should compare ids, not scores). `ranking` in the response names the
/// method, so a host is never left guessing which ranker produced an answer.
fn rerank_candidates(
    engine: &MemoryEngine,
    query: &str,
    candidates: Vec<(Entity, f64)>,
    limit: usize,
    now: i64,
) -> Result<(Vec<(Entity, f64)>, &'static str)> {
    let tokenize = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() >= 3)
            .map(|t| t.to_lowercase())
            .collect()
    };
    let query_tokens: std::collections::HashSet<String> = tokenize(query).into_iter().collect();

    // One fact-set per candidate, used for both relevance and redundancy —
    // **bounded**, which is the whole difference between an affordable rerank and
    // the 20x tax this pass used to charge. It previously tokenized *every* fact of
    // *every* candidate: on LongMemEval that is 53 sessions of chat per vault, so the
    // rerank spent its time building sets nobody reads past the first few of. A
    // candidate's signature is its identity for diversity purposes, and identity
    // does not need all 400 of an entity's facts — the first few carry it, ties go to
    // the most recent, and the bound makes the cost O(candidates x facts_cap) instead
    // of O(candidates x corpus).
    let mut facts_of: HashMap<i64, std::collections::HashSet<String>> = HashMap::new();
    for (e, _) in &candidates {
        let mut set = std::collections::HashSet::new();
        for o in engine
            .observations_for(e.id, now)?
            .into_iter()
            .take(MAX_RERANK_FACTS)
        {
            for t in tokenize(&o.content)
                .into_iter()
                .take(MAX_RERANK_TOKENS_PER_FACT)
            {
                set.insert(t);
            }
        }
        for t in tokenize(e.name.as_str()) {
            set.insert(t);
        }
        if let Some(summary) = &e.summary {
            for t in tokenize(summary) {
                set.insert(t);
            }
        }
        facts_of.insert(e.id, set);
    }
    let lexical = |id: i64| -> f64 {
        let set = facts_of.get(&id);
        if query_tokens.is_empty() {
            return 0.0;
        }
        match set {
            Some(s) => {
                query_tokens.iter().filter(|q| s.contains(*q)).count() as f64
                    / query_tokens.len() as f64
            }
            None => 0.0,
        }
    };

    // Greedy MMR: relevance minus the max overlap with what is already picked.
    let mut pool: Vec<(Entity, f64)> = candidates;
    // Fused-RRF order is a relevance prior too: normalize it 0..1 so it can
    // compete with the lexical share instead of being discarded.
    let max_fused = pool.iter().map(|(_, s)| *s).fold(0.0f64, f64::max);
    pool.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut picked: Vec<(Entity, f64)> = Vec::new();
    let mut picked_ids: Vec<i64> = Vec::new();
    const LAMBDA: f64 = 0.7; // relevance-heavy MMR; 1.0 = pure relevance
    while !pool.is_empty() && picked.len() < limit {
        let mut best: Option<(usize, f64)> = None;
        for (idx, (e, fused)) in pool.iter().enumerate() {
            let rel = LAMBDA * lexical(e.id) + (1.0 - LAMBDA) * (fused / max_fused);
            let redundancy = picked_ids
                .iter()
                .map(|pid| {
                    let a = facts_of.get(&e.id);
                    let b = facts_of.get(pid);
                    match (a, b) {
                        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => {
                            let inter = a.intersection(b).count() as f64;
                            inter / (a.len() as f64).min(b.len() as f64)
                        }
                        _ => 0.0,
                    }
                })
                .fold(0.0f64, f64::max);
            let mmr = rel - redundancy;
            if best.map(|(_, s)| mmr > s).unwrap_or(true) {
                best = Some((idx, mmr));
            }
        }
        let Some((idx, mmr)) = best else { break };
        let (e, _) = pool.remove(idx);
        picked_ids.push(e.id);
        picked.push((e, mmr.max(0.0)));
    }
    // Normalize the MMR values 0..1 for the response.
    let max_mmr = picked.iter().map(|(_, s)| *s).fold(0.0f64, f64::max);
    if max_mmr > 0.0 {
        for (_, s) in &mut picked {
            *s /= max_mmr;
        }
    }
    Ok((
        picked,
        "mmr (lexical relevance + redundancy penalty over rrf fusion; no model)",
    ))
}

/// Merge a duplicate entity into a keeper — the write half the leaderboard
/// said the category needs and only the finder existed for.
///
/// Journal discipline (the property that makes this safe on a bi-temporal
/// vault):
///
/// * The keeper is never mutated — `supersede_entity` is not used, because a
///   merge adds facts, it does not change what the keeper *was*.
/// * The duplicate is closed with `revoke_entity`, which journals the old row
///   with the merge reason. Nothing is deleted; `vault_history` still shows the
///   closed entity, and the journal is the undo path.
/// * Facts are **moved, not copied**: current observations re-point to the
///   keeper, skipping ones whose normalized text the keeper already has (that
///   dedup is the point of the operation). Closed observations are left alone —
///   history does not get rewritten by a merge.
/// * Live relations are rewired source- or target-wise, deduplicated on
///   (keeper, other, kind) so rewiring two parallel edges does not double them.
///
/// MRTR-protected like `mem_ingest`: this is a bulk write decided by argument
/// values, so a host that declares elicitation is asked first. Callers are
/// expected to get their ids from `mem_duplicates` — the response suggests a
/// keeper precisely so this tool's arguments are not guesses.
fn mem_merge(
    engine: &MemoryEngine,
    keeper_id: i64,
    merge_id: i64,
    reason: &str,
    now: i64,
) -> Result<Value> {
    if keeper_id == merge_id {
        return Err(anyhow::anyhow!(
            "keeper_id and merge_id are the same entity ({}); a merge needs two different ids",
            keeper_id
        ));
    }
    let keeper = engine
        .entity_at(keeper_id, now)?
        .with_context(|| format!("keeper entity {keeper_id} does not exist at the current time"))?;
    if keeper.valid_to.is_some() {
        return Err(anyhow::anyhow!(
            "keeper entity {keeper_id} is closed (revoked/superseded); it cannot absorb a merge"
        ));
    }
    let dup = engine
        .entity_at(merge_id, now)?
        .with_context(|| format!("entity {merge_id} does not exist at the current time"))?;
    if dup.valid_to.is_some() {
        return Err(anyhow::anyhow!(
            "entity {merge_id} is already closed; nothing to merge"
        ));
    }
    let conn = engine.connection();

    // 1. Move current facts, skipping texts the keeper already holds.
    let norm = |s: &str| duplicate_key(s);
    let keeper_facts: std::collections::HashSet<String> = {
        let mut set = std::collections::HashSet::new();
        for o in engine.observations_for(keeper_id, now)? {
            set.insert(norm(&o.content));
        }
        set
    };
    let mut moved: Vec<i64> = Vec::new();
    let mut skipped: usize = 0;
    {
        let mut stmt = conn.prepare(
            "SELECT id, content FROM observations \
             WHERE entity_id = ?1 AND valid_from <= ?2 AND (valid_to IS NULL OR valid_to > ?2)",
        )?;
        let rows = stmt.query_map(params![merge_id, now], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for (obs_id, content) in rows.filter_map(|x| x.ok()) {
            if keeper_facts.contains(&norm(&content)) {
                skipped += 1;
                continue;
            }
            conn.execute(
                "UPDATE observations SET entity_id = ?1 WHERE id = ?2",
                params![keeper_id, obs_id],
            )?;
            moved.push(obs_id);
        }
    }

    // 2. Rewire live relations onto the keeper, deduplicated.
    let mut rewired: usize = 0;
    {
        let mut stmt = conn.prepare(
            "SELECT id, source_id, target_id, kind FROM relations \
                 WHERE valid_to IS NULL AND revoked_at IS NULL \
                 AND (source_id = ?1 OR target_id = ?1)",
        )?;
        let rows = stmt.query_map(params![merge_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let edges: Vec<(i64, i64, i64, String)> = rows.filter_map(|x| x.ok()).collect();
        // Collect first, mutate after — the same statement cannot safely write
        // while its cursor is open.
        let mut seen: std::collections::HashSet<(i64, i64, String)> =
            std::collections::HashSet::new();
        for (rel_id, source, target, kind) in edges {
            let new_source = if source == merge_id {
                keeper_id
            } else {
                source
            };
            let new_target = if target == merge_id {
                keeper_id
            } else {
                target
            };
            if new_source == new_target {
                // The merge would create a self-loop; drop the edge instead of
                // recording a meaningless relation.
                conn.execute(
                    "UPDATE relations SET valid_to = ?1 WHERE id = ?2",
                    params![now, rel_id],
                )?;
                rewired += 1;
                continue;
            }
            if !seen.insert((new_source, new_target, kind.clone())) {
                conn.execute(
                    "UPDATE relations SET valid_to = ?1 WHERE id = ?2",
                    params![now, rel_id],
                )?;
                rewired += 1;
                continue;
            }
            conn.execute(
                "UPDATE relations SET source_id = ?1, target_id = ?2 WHERE id = ?3",
                params![new_source, new_target, rel_id],
            )?;
            rewired += 1;
        }
    }

    // 3. Close the duplicate — journaled, not deleted.
    engine.revoke_entity(merge_id, &format!("merged into {keeper_id}: {reason}"), now)?;

    Ok(json!({
        "keeper": {"entity_id": keeper.id, "name": keeper.name},
        "merged": {"entity_id": dup.id, "name": dup.name},
        "facts_moved": moved.len(),
        "facts_skipped_duplicate": skipped,
        "relations_rewired": rewired,
        "journaled": true,
        "note": "The duplicate is closed with the reason journaled — vault_history shows it, and the journal is the undo path. Facts were moved (not copied) and deduplicated against the keeper's existing facts.",
    }))
}

/// Revise an entity's identity through time — the write half of the temporal
/// vocabulary.
///
/// The vault has had the *read* half since the start (`vault_asof`,
/// `vault_history`, `vault_contradictions`) and no way to write one: a host could
/// see that a belief had been superseded and could not supersede anything. That
/// is a moat with one bank missing, and it is the first thing the temporal
/// benchmark reported.
///
/// `supersede_entity` closes the current row, journals the reason and opens a
/// successor — but moves nothing, so the successor would carry the name and none
/// of the facts, and would then rank for the name while answering nothing.
///
/// Facts are therefore **copied**, not moved, and this is the whole reason the
/// temporal claim holds together. Moving them (the first version of this function,
/// which the benchmark caught) emptied the predecessor: `vault_asof` at an instant
/// before the revision still resolved to the old row and found no facts on it, so
/// the one question a history is for — "what did we believe then?" — answered
/// nothing, at 0% over 40 scenarios. A copy keeps the earlier revision readable
/// while the successor answers at the current instant.
///
/// Graph edges *are* moved: an edge is a statement about the live entity, and
/// leaving a stale edge pointing at a closed row would keep a retired revision in
/// the traversal. Edges are the present; facts are history.
///
/// A `supersedes` relation from successor to predecessor is recorded as well, so
/// the chain is a fact in the graph rather than an inference from two rows that
/// happen to share a name — `entity_chain` follows it, which is what makes
/// `vault_history` reach a renamed entity's past by its *new* name.
fn mem_supersede(
    engine: &MemoryEngine,
    name: &str,
    new_name: Option<&str>,
    summary: Option<&str>,
    confidence: Option<f64>,
    reason: &str,
    now: i64,
) -> Result<Value> {
    let target = engine
        .find_entities_by_name(name, now)?
        .into_iter()
        .next()
        .with_context(|| format!("no current entity named `{name}` to revise"))?;
    let successor_id =
        engine.supersede_entity(target.id, None, new_name, summary, confidence, reason, now)?;
    let conn = engine.connection();

    // Facts are copied. The predecessor keeps its own for `vault_asof`; the
    // successor gets its own so the current belief is answerable. No dedup pass is
    // needed — the successor is brand new and cannot already hold any of them.
    let mut copied = 0usize;
    {
        let mut stmt = conn.prepare(
            "SELECT content, kind, confidence, valid_from FROM observations \
             WHERE entity_id = ?1 AND valid_from <= ?2 AND (valid_to IS NULL OR valid_to > ?2)",
        )?;
        let facts: Vec<(String, String, f64, i64)> = stmt
            .query_map(params![target.id, now], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .filter_map(|x| x.ok())
            .collect();
        for (content, kind, confidence, valid_from) in facts {
            engine.add_observation(successor_id, &content, &kind, confidence, valid_from)?;
            copied += 1;
        }
    }

    // The chain edge. Without it, `entity_chain` can only find revisions by a name
    // both rows happen to share, so `vault_history(new_name)` listed one version
    // and the rename was unreachable from the name the host actually has.
    engine.add_relation(successor_id, target.id, "supersedes", 1.0, now)?;

    let mut rewired = 0usize;
    {
        // `kind <> 'supersedes'` is load-bearing: the chain edge was just written by
        // this very function, and rewiring it turns `successor -> predecessor` into a
        // self-loop — which is exactly what happened first, silently, leaving
        // `vault_history` reporting one revision and no way to see the rename. A
        // history edge must survive the operation that creates it.
        let mut stmt = conn.prepare(
            "SELECT id, source_id, target_id FROM relations \
             WHERE valid_to IS NULL AND revoked_at IS NULL AND kind <> 'supersedes' \
             AND (source_id = ?1 OR target_id = ?1)",
        )?;
        let edges: Vec<(i64, i64, i64)> = stmt
            .query_map(params![target.id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .filter_map(|x| x.ok())
            .collect();
        for (rel_id, source, target_side) in edges {
            let new_source = if source == target.id {
                successor_id
            } else {
                source
            };
            let new_target = if target_side == target.id {
                successor_id
            } else {
                target_side
            };
            conn.execute(
                "UPDATE relations SET source_id = ?1, target_id = ?2 WHERE id = ?3",
                params![new_source, new_target, rel_id],
            )?;
            rewired += 1;
        }
    }

    let successor = engine.entity_at(successor_id, now)?;
    Ok(json!({
        "revised_from": {"entity_id": target.id, "name": target.name},
        "revised_to": {
            "entity_id": successor_id,
            "name": successor.map(|e| e.name).unwrap_or_default(),
        },
        "facts_copied": copied,
        "relations_rewired": rewired,
        "reason": reason,
        "at_ms": now,
        "journaled": true,
        "note": "The previous revision is closed, not deleted: vault_asof at any instant before at_ms still returns the old belief with its facts, vault_history lists both revisions with this reason (reachable by either name), and searches from now on see only the successor.",
    }))
}

/// Retract a belief with a reason and no replacement.
///
/// Distinct from `mem_supersede` on purpose: a revision says "this replaced that",
/// a retraction says "this was wrong". `vault_contradictions` reports both, but only
/// this one leaves the successor slot empty, which is what makes a wrong memory
/// *gone* from search while still *explainable* afterwards.
fn mem_forget(engine: &MemoryEngine, name: &str, reason: &str, now: i64) -> Result<Value> {
    let target = engine
        .find_entities_by_name(name, now)?
        .into_iter()
        .next()
        .with_context(|| format!("no current entity named `{name}` to retract"))?;
    engine.revoke_entity(target.id, reason, now)?;
    Ok(json!({
        "revoked": {"entity_id": target.id, "name": target.name},
        "reason": reason,
        "at_ms": now,
        "journaled": true,
        "note": "Retracted, not deleted: the entity and its facts stay readable through vault_asof at any instant before at_ms, and vault_contradictions reports it with this reason. Searches at the current instant no longer return it.",
    }))
}

/// Upper bound on observations examined by `mem_duplicates`. Grouping needs the
/// whole current belief state, but a scan without a ceiling would let one call
/// on a huge vault balloon the server's memory; the cap is declared in the
/// response rather than applied silently.
const DUPLICATE_FACT_SCAN_CAP: usize = 20_000;

/// Normalization key for duplicate detection: lowercase, every run of
/// non-alphanumeric characters collapsed away. `AuthService`, `auth_service`,
/// `Auth-Service` and `auth service` all land on `authservice`.
fn duplicate_key(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_alphanumeric() {
            for lower in c.to_lowercase() {
                out.push(lower);
            }
        }
    }
    out
}

/// The same normalization with word order removed, so `session manager` and
/// `manager session` collide. Empty when there are fewer than two words — a
/// single word is already covered by [`duplicate_key`], and returning it again
/// would report one collision twice.
fn duplicate_token_key(text: &str) -> String {
    let mut tokens: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();
    if tokens.len() < 2 {
        return String::new();
    }
    tokens.sort();
    tokens.join(" ")
}

/// Near-duplicate detection over the vault's **current** belief state.
///
/// Deterministic and model-free, in two passes:
///
/// 1. **Entity groups.** Members share a normalization key — case, separators
///    and (as a second key) word order are ignored, so `AuthService`,
///    `auth_service` and `service auth` land together. A second key is only
///    consulted for entities the first did not already group, so a collision is
///    reported once.
/// 2. **Fact groups.** Current observations that are the same text once
///    punctuation and case are normalized — the same claim recorded against two
///    entities, or twice against one.
///
/// Each member carries its fact count and access count, so a keeper is chosen
/// from evidence (`suggested_keeper`) instead of by guessing.
///
/// Deliberately **not** fuzzy: there is no edit-distance or embedding pass, so
/// `AuthService` and `AuthSvc` are not grouped. That is a stated limit, not an
/// oversight — a fuzzy pass needs a threshold, a threshold needs tuning against
/// real vaults, and a wrong merge is worse than a missed one.
fn mem_duplicates(
    engine: &MemoryEngine,
    limit: usize,
    min_group: usize,
    fuzzy: bool,
) -> Result<Value> {
    let limit = limit.clamp(1, 200);
    let min_group = min_group.max(2);

    let entities = engine.entities_current()?;
    let conn = engine.connection();

    // Fact and access counts per entity, one aggregate query each — a dedup
    // decision needs "which of these two is the real one", and that is what a
    // fact count and an access count answer.
    let mut facts_of: HashMap<i64, i64> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT entity_id, COUNT(*) FROM observations WHERE valid_to IS NULL GROUP BY entity_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        for (id, count) in rows.filter_map(|x| x.ok()) {
            facts_of.insert(id, count);
        }
    }
    let mut access_of: HashMap<i64, i64> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT entity_id, access_count FROM access_stats")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        for (id, count) in rows.filter_map(|x| x.ok()) {
            access_of.insert(id, count);
        }
    }

    let member = |e: &Entity| {
        json!({
            "entity_id": e.id,
            "name": e.name,
            "kind": e.kind.as_str(),
            "facts": facts_of.get(&e.id).copied().unwrap_or(0),
            "access_count": access_of.get(&e.id).copied().unwrap_or(0),
            "confidence": e.confidence,
        })
    };
    // Keepers are ordered by evidence: most facts, then most accessed, then the
    // lowest id (the oldest row) — fully deterministic, no tie left to chance.
    let rank = |e: &Entity| {
        (
            facts_of.get(&e.id).copied().unwrap_or(0),
            access_of.get(&e.id).copied().unwrap_or(0),
            -e.id,
        )
    };

    let mut strict: BTreeMap<String, Vec<&Entity>> = BTreeMap::new();
    let mut reordered: BTreeMap<String, Vec<&Entity>> = BTreeMap::new();
    for e in &entities {
        strict.entry(duplicate_key(&e.name)).or_default().push(e);
        let key = duplicate_token_key(&e.name);
        if !key.is_empty() {
            reordered.entry(key).or_default().push(e);
        }
    }

    let mut entity_groups: Vec<Value> = Vec::new();
    let mut grouped: HashSet<i64> = HashSet::new();
    let build = |reason: &str, key: &str, members: &[&Entity]| {
        let mut sorted: Vec<&Entity> = members.to_vec();
        sorted.sort_by_key(|e| std::cmp::Reverse(rank(e)));
        let keeper = sorted[0];
        json!({
            "reason": reason,
            "key": key,
            "size": sorted.len(),
            "suggested_keeper": { "entity_id": keeper.id, "name": keeper.name },
            "members": sorted.iter().map(|e| member(e)).collect::<Vec<_>>(),
        })
    };
    for (key, members) in &strict {
        if members.len() < min_group {
            continue;
        }
        for m in members {
            grouped.insert(m.id);
        }
        entity_groups.push(build(
            "same name once case and separators are normalized",
            key,
            members,
        ));
    }
    for (key, members) in &reordered {
        if members.len() < min_group {
            continue;
        }
        // Only entities the strict pass did not already report: otherwise a
        // three-way collision on case alone would also appear as a word-order
        // group of the same rows.
        let fresh: Vec<&Entity> = members
            .iter()
            .copied()
            .filter(|e| !grouped.contains(&e.id))
            .collect();
        if fresh.len() < min_group {
            continue;
        }
        entity_groups.push(build("same words, different order", key, &fresh));
    }

    // Revisions are the same identity at two instants, so a fact present on both
    // sides of a `supersedes` edge is history, not duplication. Without this, every
    // revision would appear in the hygiene report as "the same fact is recorded
    // against more than one entity" — and a host acting on that report would merge
    // a revision into its own successor, deleting exactly the past the revision
    // exists to keep. The edge `mem_supersede` writes is what tells them apart.
    let revised_pairs: HashSet<(i64, i64)> = {
        let mut stmt =
            conn.prepare("SELECT source_id, target_id FROM relations WHERE kind = 'supersedes'")?;
        let pairs: Vec<(i64, i64)> = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
            .filter_map(|x| x.ok())
            .collect();
        pairs
            .into_iter()
            .flat_map(|(a, b)| [(a, b), (b, a)])
            .collect()
    };

    // Facts: same text once punctuation and case are normalized.
    let mut occurrences: BTreeMap<String, Vec<(i64, i64, String)>> = BTreeMap::new();
    let mut facts_scanned = 0usize;
    let mut facts_capped = false;
    {
        let mut stmt = conn.prepare(
            "SELECT id, entity_id, content FROM observations WHERE valid_to IS NULL ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for (obs_id, entity_id, content) in rows.filter_map(|x| x.ok()) {
            if facts_scanned >= DUPLICATE_FACT_SCAN_CAP {
                facts_capped = true;
                break;
            }
            facts_scanned += 1;
            let key = duplicate_key(&content);
            // Two- or three-character contents collide constantly and carry no
            // signal; the point is repeated *claims*, not repeated short tokens.
            if key.len() < 8 {
                continue;
            }
            occurrences
                .entry(key)
                .or_default()
                .push((obs_id, entity_id, content));
        }
    }
    let mut fact_groups: Vec<Value> = Vec::new();
    for (key, mut rows) in occurrences {
        if rows.len() < min_group {
            continue;
        }
        rows.sort_by_key(|(obs_id, _, _)| *obs_id);
        let mut entity_ids: Vec<i64> = rows.iter().map(|(_, e, _)| *e).collect();
        entity_ids.sort_unstable();
        entity_ids.dedup();
        // Skip groups that are entirely one identity across revisions.
        let revision_only = entity_ids.len() > 1
            && entity_ids.iter().all(|a| {
                entity_ids
                    .iter()
                    .all(|b| a == b || revised_pairs.contains(&(*a, *b)))
            });
        if revision_only {
            continue;
        }
        let cross_entity = entity_ids.len() > 1;
        fact_groups.push(json!({
            "reason": if cross_entity {
                "the same fact is recorded against more than one entity"
            } else {
                "the same fact is recorded twice on one entity"
            },
            "key": key,
            "occurrences": rows.len(),
            "entities": entity_ids,
            "sample": rows[0].2,
        }));
    }

    // Fuzzy pass, opt-in: token-stem similarity, reported separately so it can
    // never be mistaken for the deterministic groups above. Two names are
    // candidates when they share a 4+-character token stem ("authservice" and
    // "authservices" share "authservic"). Reported, never merged — the
    // threshold is a heuristic and the response says so.
    let mut fuzzy_groups: Vec<Value> = Vec::new();
    let mut fuzzy_scanned = 0usize;
    if fuzzy {
        // Bucket key is the first 4 characters of a normalized word token, so
        // `authservice` and `authsvc` share the `auth` stem. Deliberately crude
        // and opt-in: it over-reports by design (a 4-char prefix is not proof
        // of relatedness), which is why these are candidates and never merges.
        let stem = |t: &str| -> String {
            let norm: String = t
                .chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(|c| c.to_lowercase())
                .collect();
            norm.chars().take(4).collect()
        };
        let stems_of = |name: &str| -> Vec<String> {
            let mut out: Vec<String> = name
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| t.chars().count() >= 4)
                .map(stem)
                .filter(|s| s.chars().count() == 4)
                .collect();
            out.sort();
            out.dedup();
            out
        };
        let mut by_stem: BTreeMap<String, Vec<&Entity>> = BTreeMap::new();
        for e in &entities {
            for s in stems_of(&e.name) {
                by_stem.entry(s).or_default().push(e);
            }
        }
        for (stem_key, members) in by_stem {
            if members.len() < min_group {
                continue;
            }
            // One name may appear under several stems; de-duplicate the
            // reported group by (lowest_id, joined names).
            let mut ids: Vec<i64> = members.iter().map(|e| e.id).collect();
            ids.sort_unstable();
            ids.dedup();
            if ids.len() < min_group {
                continue;
            }
            let mut uniq: Vec<&Entity> = Vec::new();
            for id in &ids {
                if let Some(e) = members.iter().find(|e| e.id == *id)
                    && !uniq.iter().any(|u| u.id == *id)
                {
                    uniq.push(e);
                }
            }
            fuzzy_scanned += 1;
            let build2 = |members: &[&Entity]| {
                let mut sorted: Vec<&Entity> = members.to_vec();
                sorted.sort_by_key(|e| std::cmp::Reverse(rank(e)));
                let keeper = sorted[0];
                json!({
                    "stem": stem_key,
                    "size": sorted.len(),
                    "suggested_keeper": {"entity_id": keeper.id, "name": keeper.name},
                    "members": sorted.iter().map(|e| member(e)).collect::<Vec<_>>(),
                })
            };
            fuzzy_groups.push(build2(&uniq));
        }
    }

    // Biggest groups first: a 4-way entity collision deserves the attention a
    // 2-way one does not.
    entity_groups.sort_by_key(|g| std::cmp::Reverse(g["size"].as_u64().unwrap_or(0)));
    fact_groups.sort_by_key(|g| std::cmp::Reverse(g["occurrences"].as_u64().unwrap_or(0)));
    let entity_truncated = entity_groups.len() > limit;
    let fact_truncated = fact_groups.len() > limit;
    entity_groups.truncate(limit);
    fact_groups.truncate(limit);
    let entities_involved: usize = entity_groups
        .iter()
        .map(|g| g["size"].as_u64().unwrap_or(0) as usize)
        .sum();

    let fuzzy_truncated = fuzzy_groups.len() > limit;
    fuzzy_groups.truncate(limit);
    Ok(json!({
        "entity_groups": entity_groups,
        "fact_groups": fact_groups,
        "fuzzy_groups": fuzzy_groups,
        "counts": {
            "entity_groups": entity_groups.len(),
            "entities_involved": entities_involved,
            "fact_groups": fact_groups.len(),
            "fuzzy_groups": fuzzy_groups.len(),
            // The pre-truncation count, so a host can tell "there are none" from
            // "there are more than you were shown" without re-running with a
            // bigger limit.
            "fuzzy_candidates": fuzzy_scanned,
            "entities_current": entities.len(),
            "facts_scanned": facts_scanned,
        },
        "truncated": { "entity_groups": entity_truncated, "fact_groups": fact_truncated, "fuzzy_groups": fuzzy_truncated, "facts_scan": facts_capped },
        "guidance": "Merge only what you can justify: a group is candidate evidence, not a verdict. Use vault_history/vault_asof on the keeper before retiring a member — a duplicate name can be two genuinely different things that share spelling. mem_merge performs the merge when you have decided; fuzzy_groups are heuristics and must be human-justified before any merge.",
        "note": "Deterministic and model-free: case, separators and word order are normalized, and facts are compared after punctuation/case normalization. The opt-in fuzzy pass reports stem-similarity candidates (shared 4+ char token stem) — a heuristic, reported separately and never auto-merged.",
    }))
}

/// Serialise a tool payload with its staleness notice — the pairing every graph
/// tool needs, since counts without a freshness stamp are a bug, not a variant.
pub(crate) fn stamped<T: serde::Serialize>(value: T, store: &Store, root: &Path) -> Result<Value> {
    let mut value = serde_json::to_value(value)?;
    attach_staleness_notice(&mut value, store, root);
    Ok(value)
}

pub(crate) fn attach_staleness_notice(value: &mut Value, store: &Store, root: &Path) {
    if let Some(notice) = freshness_of(store, root, LEDGER_MAX_CHECKS).and_then(|f| f.notice()) {
        value["staleness_notice"] = json!(notice);
    }
}

/// The notice for a result that carries its **own** referenced-file count
/// (`kg_explore`, `kg_impact`): a narrower question than the ledger's — *these*
/// files, not the whole graph — so it keeps its own count and shares only the
/// sentence, which is the part that must not drift.
pub(crate) fn referenced_staleness_notice(stale_files: u64) -> String {
    format!(
        "⚠️ {stale_files} file(s) referenced below were edited since the last index \
         sync and are pending re-index. Read those specific files for \
         accurate content; every file NOT affected is fresh — still \
         trust this response for the rest."
    )
}

/// The workspace this server reports an index for: a vault at
/// `<root>/.memnoth/vault.db` means `<root>`.
///
/// The layout is the only thing that makes this knowable — the code graph is
/// keyed by an *argument* (`kg_*` take `workspace`), so a server has no ambient
/// workspace to speak for unless the vault's own path says where it lives.
/// `None` for a vault outside a `.memnoth` directory (a hand-picked path, an
/// embedder, most tests), which is precisely when there is no index to report.
pub(crate) fn workspace_root_for_vault(vault: Option<&Path>) -> Option<PathBuf> {
    let dir = vault?.parent()?;
    if dir.file_name()? != ".memnoth" {
        return None;
    }
    Some(dir.parent()?.to_path_buf())
}

fn db_parent_display(_args: &Value) -> String {
    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

struct Arg {
    name: &'static str,
    ty: &'static str,
    desc: &'static str,
    required: bool,
    /// Allowed values, emitted as JSON Schema `enum`. Empty means "no restriction".
    /// A mode argument that is only *described* in prose is not reachable by a host
    /// that reads the schema — which is exactly how `rerank=cross_encoder` was
    /// unreachable until this existed.
    values: &'static [&'static str],
    /// Element type when `ty` is `array`. Without it the schema said
    /// `"type": "array"` and stopped, which tells a host the shape of nothing.
    items: Option<&'static str>,
}

const fn req(name: &'static str, ty: &'static str, desc: &'static str) -> Arg {
    Arg {
        name,
        ty,
        desc,
        required: true,
        values: &[],
        items: None,
    }
}

const fn opt(name: &'static str, ty: &'static str, desc: &'static str) -> Arg {
    Arg {
        name,
        ty,
        desc,
        required: false,
        values: &[],
        items: None,
    }
}

/// An optional list-of-strings argument.
const fn opt_list(name: &'static str, desc: &'static str) -> Arg {
    Arg {
        name,
        ty: "array",
        desc,
        required: false,
        values: &[],
        items: Some("string"),
    }
}

/// An optional argument with a closed set of allowed values.
const fn one_of(
    name: &'static str,
    ty: &'static str,
    desc: &'static str,
    values: &'static [&'static str],
) -> Arg {
    Arg {
        name,
        ty,
        desc,
        required: false,
        values,
        items: None,
    }
}

fn schema(args: &[Arg]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for arg in args {
        let mut prop = json!({ "type": arg.ty, "description": arg.desc });
        if !arg.values.is_empty() {
            prop["enum"] = json!(arg.values);
        }
        if let Some(items) = arg.items {
            prop["items"] = json!({ "type": items });
        }
        properties.insert(arg.name.to_string(), prop);
        if arg.required {
            required.push(Value::from(arg.name));
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

struct Annotations {
    read_only: bool,
    destructive: bool,
    idempotent: bool,
    open_world: bool,
}

const READ_ONLY: Annotations = Annotations {
    read_only: true,
    destructive: false,
    idempotent: true,
    open_world: false,
};

const WRITE: Annotations = Annotations {
    read_only: false,
    destructive: false,
    idempotent: false,
    open_world: false,
};

/// The five agent-mode tools (strategy §4 P1.2). Semantics are shaped exactly
/// against Session C's scenario contract: prompts submit like typing plus
/// Enter, waits are deadline-bounded conditions (never blind sleeps), and
/// events carry the harness lifecycle vocabulary.
fn agent_tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "agent_session_new",
            "Start a new memnoth agent session in the given working directory; returns the session id used by every other agent_* tool",
            &[
                req(
                    "cwd",
                    "string",
                    "Absolute working directory for the session",
                ),
                opt(
                    "cols",
                    "integer",
                    "Terminal grid width for event formatting (default 120)",
                ),
                opt(
                    "rows",
                    "integer",
                    "Terminal grid height for event formatting (default 40)",
                ),
            ],
            WRITE,
        ),
        tool(
            "agent_session_close",
            "Close a finished session's connection early; its final events stay readable via agent_events, and idle sessions are reaped automatically after the TTL",
            &[req(
                "session",
                "string",
                "Session id returned by agent_session_new",
            )],
            WRITE,
        ),
        tool(
            "agent_prompt",
            "Submit a user message to the session, like typing it and pressing Enter",
            &[
                req(
                    "session",
                    "string",
                    "Session id returned by agent_session_new",
                ),
                req("text", "string", "Message text to submit to the agent"),
            ],
            WRITE,
        ),
        tool(
            "agent_wait",
            "Wait for a condition with a hard deadline — never a blind sleep: wait until text appears in the transcript or until output has been stable for N ms",
            &[
                req(
                    "session",
                    "string",
                    "Session id returned by agent_session_new",
                ),
                opt(
                    "text",
                    "string",
                    "Transcript substring to wait for (use this OR stable_ms)",
                ),
                opt(
                    "stable_ms",
                    "integer",
                    "Milliseconds of no new output to consider the turn settled (use this OR text)",
                ),
                opt(
                    "timeout_ms",
                    "integer",
                    "Hard deadline in milliseconds (default 5000); timeouts return found/stable=false instead of erroring",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "agent_events",
            "Read the tail of the session's event stream plus current state",
            &[
                req(
                    "session",
                    "string",
                    "Session id returned by agent_session_new",
                ),
                opt(
                    "tail_n",
                    "integer",
                    "Number of recent events to return (default 20)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "agent_set_model",
            "Switch the session's model (and optionally provider)",
            &[
                req(
                    "session",
                    "string",
                    "Session id returned by agent_session_new",
                ),
                opt(
                    "provider",
                    "string",
                    "Provider name; combined as provider/model when the model id is not already prefixed",
                ),
                req(
                    "model",
                    "string",
                    "Model id from the provider catalog, e.g. claude-opus-5",
                ),
            ],
            WRITE,
        ),
    ]
}

fn prompt_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "recall",
            "description": "Walk through recalling a topic from the memory vault with mem_search and mem_find_facts",
            "arguments": [
                {"name": "topic", "description": "Subject to recall from the vault", "required": true}
            ],
        }),
        json!({
            "name": "ingest",
            "description": "Ingest a directory into the vault and summarize what was learned",
            "arguments": [
                {"name": "path", "description": "Directory to ingest", "required": true}
            ],
        }),
        json!({
            "name": "handoff",
            "description": "Compose a session handoff for the next agent from coordination-room state",
            "arguments": [],
        }),
        json!({
            "name": "skill",
            "description": "Load a registry skill by name (skills-as-commands over MCP: the /<skill> equivalent for hosts without slash commands)",
            "arguments": [
                {"name": "name", "description": "Skill name as shown in the skills registry", "required": true}
            ],
        }),
    ]
}

fn prompts_get(params: &Value) -> std::result::Result<Value, Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| json!({"code": -32602, "message": "missing prompt name"}))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let text = match name {
        "recall" => {
            let topic = prompt_arg(&args, "topic")?;
            format!(
                "Use the mem_search and mem_find_facts tools against the memory vault to recall \
                 everything relevant to: {topic}\n\nSummarize the key facts and cite the entity \
                 names they came from. If the vault returns nothing, say so explicitly and list \
                 what related queries you tried."
            )
        }
        "ingest" => {
            let path = prompt_arg(&args, "path")?;
            format!(
                "Run the mem_ingest tool on this directory: {path}\n\nThen report files seen vs \
                 ingested, entities/relations/observations created, and any unresolved call edges. \
                 Finish with the three most important things the vault just learned."
            )
        }
        "handoff" => "Prepare a handoff for the next agent working in this project:\n\n\
             1. Read coord_status and coord_task_list to see who is active, which path globs are \
             claimed, and which tasks are open or claimed.\n\
             2. Check coord_inbox for unread direct and broadcast messages.\n\
             3. Write a concise handoff: current claims (with owners), in-flight tasks, unread \
             messages that need action, and recommended next steps. Flag any stale sessions."
            .to_string(),
        "skill" => {
            let name = prompt_arg(&args, "name")?;
            format!(
                "Load the registry skill named: {name}\n\n\
                 1. Read the skill definition from ~/.memnoth/skills/{name}/SKILL.md, then from \
                 any Claude Code plugin skills directories (~/.claude/plugins) that provide a \
                 skills/{name}/SKILL.md entry.\n\
                 2. If no definition exists under that name, say so explicitly and list the \
                 closest names you can find — never invent skill content.\n\
                 3. Follow the loaded SKILL.md instructions exactly; this is the MCP equivalent \
                 of invoking /{name} in the TUI."
            )
        }
        other => {
            return Err(json!({"code": -32602, "message": format!("unknown prompt: {other}")}));
        }
    };
    Ok(json!({
        "description": prompt_definitions()
            .into_iter()
            .find(|p| p["name"] == name)
            .and_then(|p| p["description"].as_str().map(str::to_string))
            .unwrap_or_default(),
        "messages": [
            {"role": "user", "content": {"type": "text", "text": text}}
        ],
    }))
}

fn prompt_arg(args: &Value, key: &str) -> std::result::Result<String, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(
            || json!({"code": -32602, "message": format!("missing required argument '{key}'")}),
        )
}

const ENTITY_LIST_CAP: usize = 100;

fn json_resource(uri: String, name: &str, description: &str) -> Value {
    json!({
        "uri": uri,
        "name": name,
        "description": description,
        "mimeType": "application/json",
    })
}

fn invalid_params(message: impl std::fmt::Display) -> Value {
    json!({"code": -32602, "message": message.to_string()})
}

/// How many suggestions `completion/complete` may return. The spec caps a
/// response at 100 and requires `hasMore` when there are more, so the cap is
/// never a truncation a host can miss.
const COMPLETION_MAX_VALUES: usize = 100;

/// `completion/complete` — autocompletion for prompt arguments and resource
/// templates.
///
/// Two things make this worth more than the spec text suggests. First, **no
/// server in this category answers it at all** (checked across every rival
/// checkout in `research/`), so a host that offers argument completion has no
/// other memory server to fall back on. Second, the spec's security section
/// applies with force: a completion list is a *read* of the vault, so every
/// source here is gated by the same profile that gates the equivalent tool. It
/// must not become a side channel that spells out data the caller cannot
/// otherwise read.
fn completions_complete(
    engine: &MemoryEngine,
    _coord: &Coord,
    profile: Profile,
    params: &Value,
) -> std::result::Result<Value, Value> {
    let argument = params
        .get("argument")
        .ok_or_else(|| invalid_params("missing argument"))?;
    let name = argument
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_params("missing argument.name"))?;
    let typed = argument.get("value").and_then(Value::as_str).unwrap_or("");
    let reference = params
        .get("ref")
        .ok_or_else(|| invalid_params("missing ref"))?;

    let candidates = match reference.get("type").and_then(Value::as_str) {
        // A URI template: the argument is one of its variables, and both of ours
        // place an entity, so the vocabulary is the vault's entity names.
        Some("ref/resource") => {
            let uri = reference
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match (uri, name) {
                ("mem://entity/{id}", "id") | ("mem://related/{id}?hops=n", "id") => {
                    entity_names(engine, profile)
                }
                ("mem://entity/{id}" | "mem://related/{id}?hops=n", other) => {
                    return Err(invalid_params(format!(
                        "template '{uri}' has no argument '{other}'"
                    )));
                }
                (other, _) => {
                    return Err(invalid_params(format!(
                        "unknown resource template: {other}"
                    )));
                }
            }
        }
        Some("ref/prompt") => {
            let prompt = reference
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            prompt_argument_values(engine, profile, prompt, name)?
        }
        other => {
            return Err(invalid_params(format!(
                "unknown completion reference type: {}",
                other.unwrap_or("<missing>")
            )));
        }
    };
    Ok(rank_completions(candidates, typed))
}

/// The result shape the spec defines: values, the **full** match count, and
/// whether more exist. `total` cannot be the number returned — a host has to be
/// able to tell "3 matches" from "3 of 900", which is what `hasMore` reports.
fn rank_completions(candidates: Vec<String>, typed: &str) -> Value {
    let needle = typed.trim().to_lowercase();
    let mut unique = candidates;
    unique.sort();
    unique.dedup();
    let mut prefix = Vec::new();
    let mut contains = Vec::new();
    for candidate in unique {
        let lower = candidate.to_lowercase();
        if lower.starts_with(&needle) {
            prefix.push(candidate);
        } else if !needle.is_empty() && lower.contains(&needle) {
            contains.push(candidate);
        }
    }
    let total = prefix.len() + contains.len();
    let mut values = prefix;
    values.extend(contains);
    let has_more = total > values.len().min(COMPLETION_MAX_VALUES);
    values.truncate(COMPLETION_MAX_VALUES);
    json!({"completion": {"values": values, "total": total, "hasMore": has_more}})
}

/// Entity names the caller may see, in a stable order.
fn entity_names(engine: &MemoryEngine, profile: Profile) -> Vec<String> {
    if !profile.allows("mem_search") {
        return Vec::new();
    }
    engine
        .entities_current()
        .map(|entities| entities.into_iter().map(|e| e.name).collect())
        .unwrap_or_default()
}

/// The values a prompt's argument can hold, from real state — never a static
/// list, which would teach a host a vocabulary this server does not have.
///
/// An argument we do not know is `-32602` (the spec's rule for an invalid prompt
/// name or missing argument), not an empty suggestion.
fn prompt_argument_values(
    engine: &MemoryEngine,
    profile: Profile,
    prompt: &str,
    argument: &str,
) -> std::result::Result<Vec<String>, Value> {
    match (prompt, argument) {
        // `recall.topic` is a thing in the vault, so the vault is the dictionary.
        ("recall", "topic") => Ok(entity_names(engine, profile)),
        // `skill.name` must name a skill that exists: the prompt itself instructs
        // the agent never to invent one, so the suggestions come from the same
        // directory that prompt tells it to read.
        ("skill", "name") => Ok(skill_names(profile)),
        // `ingest.path` is a directory on this machine.
        ("ingest", "path") => Ok(directory_names(profile)),
        (known, other) => Err(invalid_params(format!(
            "prompt '{known}' has no argument '{other}'"
        ))),
    }
}

/// Skill names from the registry the `skill` prompt points at.
fn skill_names(profile: Profile) -> Vec<String> {
    if !profile.allows("fs_list") {
        return Vec::new();
    }
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else {
        return Vec::new();
    };
    directory_names_in(std::path::Path::new(&home).join(".memnoth").join("skills"))
}

/// Directory names under the working directory: what `ingest` can be pointed at.
fn directory_names(profile: Profile) -> Vec<String> {
    if !profile.allows("fs_list") {
        return Vec::new();
    }
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    directory_names_in(cwd)
}

/// Directory names in one directory, sorted. Deliberately **not** recursive and
/// dot-directories are skipped: a completion is a menu, not a filesystem walk a
/// caller never asked for.
fn directory_names_in(dir: std::path::PathBuf) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// The `cursor` a host sent, if any.
///
/// A malformed cursor is invalid params, not a silent restart from the
/// beginning: a host that paginates would otherwise loop over page one forever
/// and never notice that it is doing so.
fn page_cursor(params: &Value) -> std::result::Result<Option<i64>, Value> {
    match params.get("cursor").and_then(Value::as_str) {
        Some(raw) => raw
            .parse::<i64>()
            .map(Some)
            .map_err(|_| invalid_params(format!("invalid cursor: {raw}"))),
        None => Ok(None),
    }
}

/// One page of the resource list, plus the cursor for the next page.
///
/// Entities are the only unbounded part of this list, so they are the only part
/// paginated — and the cursor is the **last id emitted**, which is stable
/// because `entities_current` orders by id. The header entries (the three static
/// resources, and the index resource when this server has one) appear on the
/// first page only: repeating them on every page would inflate the inventory a
/// host is trying to enumerate.
fn resource_list(
    engine: &MemoryEngine,
    _coord: &Coord,
    has_index: bool,
    cursor: Option<i64>,
) -> (Vec<Value>, Option<String>) {
    let mut out = Vec::new();
    if cursor.is_none() {
        out.push(json_resource(
            "mem://stats".into(),
            "Vault statistics",
            "Entity/relation/observation counters for this vault",
        ));
        out.push(json_resource(
            "mem://tasks".into(),
            "Task board",
            "All shared tasks with status and claimant",
        ));
        out.push(json_resource(
            "mem://room".into(),
            "Coordination room",
            "Active sessions and their held path claims",
        ));
    }
    let mut next = None;
    if let Ok(entities) = engine.entities_current() {
        // One extra row is read purely to learn whether another page exists,
        // so `nextCursor` is never a guess.
        let page: Vec<_> = entities
            .into_iter()
            .filter(|entity| cursor.is_none_or(|last| entity.id > last))
            .take(ENTITY_LIST_CAP + 1)
            .collect();
        let has_more = page.len() > ENTITY_LIST_CAP;
        let mut last_id = None;
        for entity in page.iter().take(ENTITY_LIST_CAP) {
            last_id = Some(entity.id);
            out.push(json_resource(
                format!("mem://entity/{}", entity.id),
                &entity.name,
                "Entity page: facts and outgoing relations",
            ));
        }
        if has_more {
            next = last_id.map(|id| id.to_string());
        }
    }
    // Listed only when a code graph actually exists behind this vault: a
    // freshness resource for an index that was never built would be a lie with
    // a URI, and worse than the `kg_status` tool it mirrors — a host can read a
    // resource without deciding to call anything.
    if has_index && cursor.is_none() {
        out.push(json_resource(
            "mem://index/status".into(),
            "Index freshness",
            "The parse ledger for this workspace: generation, stale file count and coverage — how much of the code graph is out of date right now",
        ));
    }
    (out, next)
}

/// Resource errors are `Invalid Params`. The 2026-07-28 revision changed this
/// from the removed `-32002` to `-32602` to align with JSON-RPC, and the rest of
/// this file's resource errors already used `-32602` — so this helper was the
/// last `-32002` in the process.
pub(crate) fn resource_not_found(what: String) -> Value {
    json!({"code": -32602, "message": what})
}

fn to_json<T: serde::Serialize>(value: anyhow::Result<T>) -> std::result::Result<Value, Value> {
    let inner = value.map_err(|e| resource_not_found(e.to_string()))?;
    serde_json::to_value(inner).map_err(|e| resource_not_found(e.to_string()))
}

/// Read a resource, returning the payload **and its cache hint** — a pair rather
/// than a value because this is the one read whose hint is derived: the vault
/// documents are always 0 ms, while the index ledger answers with its own
/// freshness and says so in the same breath as the data.
fn resources_read(
    engine: &MemoryEngine,
    coord: &Coord,
    params: &Value,
    vault: Option<&Path>,
) -> std::result::Result<(Value, i64), Value> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| json!({"code": -32602, "message": "missing resource uri"}))?;
    let (body, ttl) = match uri {
        "mem://stats" => (to_json(engine.stats())?, READ_CACHE_TTL_MS),
        "mem://tasks" => (to_json(coord.task_list())?, READ_CACHE_TTL_MS),
        "mem://room" => (to_json(coord.status())?, READ_CACHE_TTL_MS),
        rest if rest.starts_with("mem://entity/") => {
            (read_entity_resource(engine, rest)?, READ_CACHE_TTL_MS)
        }
        rest if rest.starts_with("mem://related/") => {
            (read_related_resource(engine, rest)?, READ_CACHE_TTL_MS)
        }
        "mem://index/status" => read_index_status(vault)?,
        rest if rest.starts_with("kg://symbol/") => symbol_resource::read(rest, vault)?,
        other => {
            return Err(
                json!({"code": -32602, "message": format!("unknown resource uri: {other}")}),
            );
        }
    };
    let text =
        serde_json::to_string_pretty(&body).map_err(|e| resource_not_found(e.to_string()))?;
    Ok((
        json!({
            "contents": [{"uri": uri, "mimeType": "application/json", "text": text}],
        }),
        ttl,
    ))
}

/// `mem://index/status` — the parse ledger, plus the cache hint the ledger
/// itself justifies.
///
/// A read that cannot be answered is an *error*, never a fabricated "fresh":
/// "no index for this workspace" and "the index is current" are different
/// facts, and a host cannot tell them apart if both look like 0 stale files.
fn read_index_status(vault: Option<&Path>) -> std::result::Result<(Value, i64), Value> {
    let root = workspace_root_for_vault(vault).ok_or_else(|| {
        resource_not_found(
            "this server is not serving a workspace vault, so it has no index to report".into(),
        )
    })?;
    let freshness = workspace_freshness(&root, LEDGER_MAX_CHECKS).ok_or_else(|| {
        resource_not_found(format!(
            "no code graph at {} — run kg_index for this workspace first",
            root.join(".memnoth/kg.db").display()
        ))
    })?;
    let report =
        serde_json::to_value(&freshness.report).map_err(|e| resource_not_found(e.to_string()))?;
    Ok((report, cache_ttl(freshness.kind)))
}

fn read_entity_resource(engine: &MemoryEngine, uri: &str) -> std::result::Result<Value, Value> {
    let id: i64 = uri["mem://entity/".len()..].trim().parse().map_err(
        |_| json!({"code": -32602, "message": format!("invalid entity id in uri: {uri}")}),
    )?;
    let now = memnoth_engine::now_ms();
    let entity = engine
        .entity_at(id, now)
        .map_err(|e| resource_not_found(e.to_string()))?
        .ok_or_else(|| resource_not_found(format!("entity not found: {id}")))?;
    let observations = engine
        .observations_for(id, now)
        .map_err(|e| resource_not_found(e.to_string()))?;
    let outgoing = engine
        .outgoing_relations(id, now)
        .map_err(|e| resource_not_found(e.to_string()))?;
    Ok(json!({
        "entity": entity,
        "observations": observations,
        "outgoing_relations": outgoing,
    }))
}

fn read_related_resource(engine: &MemoryEngine, uri: &str) -> std::result::Result<Value, Value> {
    let suffix = &uri["mem://related/".len()..];
    let (id_part, query) = match suffix.split_once('?') {
        Some((id, query)) => (id, Some(query)),
        None => (suffix, None),
    };
    let id: i64 = id_part.parse().map_err(
        |_| json!({"code": -32602, "message": format!("invalid entity id in uri: {uri}")}),
    )?;
    let hops = match query {
        None => 1usize,
        Some(query) => {
            let raw = query
                .split('&')
                .filter_map(|kv| kv.strip_prefix("hops="))
                .next()
                .ok_or_else(|| json!({"code": -32602, "message": format!("unsupported query in uri: {query}")}))?;
            raw.parse::<usize>().map(|h| h.clamp(1, 3)).map_err(
                |_| json!({"code": -32602, "message": format!("invalid hops value: {raw}")}),
            )?
        }
    };
    let now = memnoth_engine::now_ms();
    engine
        .entity_at(id, now)
        .map_err(|e| resource_not_found(e.to_string()))?
        .ok_or_else(|| resource_not_found(format!("entity not found: {id}")))?;
    let mut edges = engine
        .expand_frontier(&[id], now)
        .map_err(|e| resource_not_found(e.to_string()))?;
    for _ in 1..hops {
        let frontier: Vec<i64> = edges.iter().map(|r| r.target_id).collect();
        if frontier.is_empty() {
            break;
        }
        edges.extend(
            engine
                .expand_frontier(&frontier, now)
                .map_err(|e| resource_not_found(e.to_string()))?,
        );
    }
    Ok(json!({"edges": edges}))
}

/// Tools whose result is a typed object: they declare an `outputSchema`, and a
/// call answers with `structuredContent` as well as text.
///
/// The set is named rather than derived so the two halves can be checked against
/// each other — a tool that declares a schema but returns no structured content,
/// or one that returns structured content nothing declared, is exactly the drift
/// `typed_outputs_match_their_declared_schemas` fails on.
/// # W8.3 — why the *cheap* tools were typed and the expensive ones were not
///
/// A tool with an `outputSchema` answers twice: once as `structuredContent` and
/// once as serialized text, because the spec asks a server that returns structured
/// content to also return the JSON. That duplication is **free** for a payload of
/// three integers and **ruinous** for `mem_search`, whose contract is 1,200
/// characters of evidence per hit across 20 hits — typing it would double the
/// single largest thing this server sends, which is precisely the context-tax
/// axis the dominance plan treats as a moat.
///
/// So the set grows by *shape*, not by coverage: small, fixed-envelope answers are
/// typed, and the large prose payloads stay text-only on purpose. `mem_stats` is
/// left out for a different reason — its envelope is the engine's own stat struct,
/// and pinning the engine's columns here would make any engine change a protocol
/// change.
const TYPED_OUTPUT_TOOLS: &[&str] = &[
    "vault_history",
    "vault_asof",
    "vault_contradictions",
    "vault_top",
    "coord_join",
    "coord_heartbeat",
    "coord_leave",
    "coord_task_create",
];

/// Does this tool promise a typed result? (see [`TYPED_OUTPUT_TOOLS`])
fn declares_output_schema(name: &str) -> bool {
    TYPED_OUTPUT_TOOLS.contains(&name)
}

/// A tool that declares its result shape.
///
/// A host reads `outputSchema` to decide whether our answers may be indexed as
/// data instead of prose — which is the whole reason the temporal quartet
/// returns objects rather than sentences.
fn typed_tool(
    name: &str,
    description: &str,
    args: &[Arg],
    annotations: Annotations,
    output_schema: Value,
) -> Value {
    let mut definition = tool(name, description, args, annotations);
    definition["outputSchema"] = output_schema;
    definition
}

/// The result envelope shape the typed tools share: a closed object.
///
/// Property internals are typed but deliberately not pinned field-by-field —
/// this promises the envelope a host depends on, not a frozen copy of every
/// engine column, which would make any engine change a protocol change.
fn output_object(properties: Value, required: &[&str]) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// `coord_join` → the one field every later `coord_*` call needs.
fn coord_join_schema() -> Value {
    output_object(
        json!({
            "session_id": {"type": "string", "description": "Pass to every other coord_* call"},
        }),
        &["session_id"],
    )
}

/// The acknowledgement envelope: heartbeat, leave. A typed `ok` is what lets a
/// host branch on the outcome instead of pattern-matching prose.
fn coord_ok_schema() -> Value {
    output_object(
        json!({
            "ok": {"type": "boolean", "description": "True when the call took effect"},
        }),
        &["ok"],
    )
}

fn coord_task_create_schema() -> Value {
    output_object(
        json!({
            "task_id": {"type": "string", "description": "Board task id, usable as depends_on or coord_task_claim"},
        }),
        &["task_id"],
    )
}

fn tool(name: &str, description: &str, args: &[Arg], annotations: Annotations) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema(args),
        "annotations": {
            "readOnlyHint": annotations.read_only,
            "destructiveHint": annotations.destructive,
            "idempotentHint": annotations.idempotent,
            "openWorldHint": annotations.open_world,
        },
    })
}

#[cfg(test)]
mod tests;
