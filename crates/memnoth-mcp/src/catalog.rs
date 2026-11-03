//! The wire catalogue: every tool's name, description and input schema.
//!
//! Split from `lib.rs`, which owns dispatch and transport: these are pure data,
//! and a 600-line `vec![...]` of schemas made every unrelated edit to the server
//! touch the file that also owns the JSON-RPC loop. `tools_agent` holds the
//! second half of the catalogue for the same reason.

use serde_json::Value;

use crate::{
    Annotations, READ_ONLY, WRITE, coord_join_schema, coord_ok_schema, coord_task_create_schema,
    one_of, opt, opt_list, req, tool, typed_tool,
};

pub(crate) fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "mem_search",
            "Search the memory vault by relevance: ranked entities with the best-matching fact text for each hit",
            &[
                req(
                    "query",
                    "string",
                    "Search text matched against entity names and graph content",
                ),
                opt(
                    "limit",
                    "integer",
                    "Maximum number of results to return (default 20)",
                ),
                opt(
                    "text_chars",
                    "integer",
                    "Characters of matched fact text to include per hit, taken from the observations that best match this query (default 1200; 0 returns names and scores only, which is much cheaper)",
                ),
                one_of(
                    "rerank",
                    "string",
                    "How to reorder the fused candidates before returning; the response names the method that actually ran under `ranking`. `off` keeps fusion order (default). `mmr` is a model-free MMR pass (lexical relevance + redundancy penalty, vault-native). `cross_encoder` rescores each candidate against the query jointly with a local ONNX cross-encoder — it can promote a passage that shares little vocabulary with the question, and silently falls back to fusion order when no model is installed. `llm` is an **assisted** listwise rerank: an external model orders the candidate passages and the response's `ranking` says `assisted` and names the provider and model, so a result produced with a model is never mistaken for a model-free one. Every mode except `off` is fail-open: a missing model or an unavailable provider returns the fused order and says so, never an error. A boolean is still accepted for compatibility: `true` means `mmr`, `false` means `off`.",
                    &["off", "mmr", "cross_encoder", "llm"],
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "mem_find_facts",
            "Full-text search over atomic facts (observations); returns matching fact contents with scores",
            &[
                req(
                    "query",
                    "string",
                    "Full-text query matched against stored fact contents",
                ),
                opt(
                    "limit",
                    "integer",
                    "Maximum number of facts to return (default 20)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "mem_add_fact",
            "Attach an atomic fact to an entity, creating the entity if it does not exist",
            &[
                req(
                    "entity_name",
                    "string",
                    "Name of the entity to attach the fact to; auto-created when missing",
                ),
                req(
                    "content",
                    "string",
                    "Atomic fact text to store as an observation on the entity",
                ),
                opt(
                    "kind",
                    "string",
                    "Observation kind label, e.g. \"fact\", \"decision\", \"gotcha\" (default \"fact\")",
                ),
            ],
            WRITE,
        ),
        tool(
            "mem_supersede",
            "Revise an entity through time: closes the current revision and opens its successor with the new name/summary/confidence, moving its facts and graph edges across and journaling why. Use for renames and re-framings instead of creating a second entity — history stays readable via vault_history and vault_asof.",
            &[
                req("name", "string", "Current entity name to revise"),
                req(
                    "reason",
                    "string",
                    "Why the belief changed; journaled and returned by vault_history",
                ),
                opt(
                    "new_name",
                    "string",
                    "New name for the successor (omit to keep the name and revise only summary/confidence)",
                ),
                opt("summary", "string", "New summary for the successor"),
                opt(
                    "confidence",
                    "number",
                    "New confidence 0..1 for the successor",
                ),
            ],
            WRITE,
        ),
        tool(
            "mem_forget",
            "Retract a belief: closes the entity with a journaled reason and no successor, so it stops appearing in searches while every earlier belief state stays readable through vault_asof. Use when something is wrong rather than merely outdated — mem_supersede is the tool for replacement.",
            &[
                req("name", "string", "Entity name to retract"),
                req(
                    "reason",
                    "string",
                    "Why it is no longer believed; journaled and surfaced by vault_contradictions",
                ),
            ],
            WRITE,
        ),
        tool(
            "mem_related",
            "Graph neighbors of an entity via typed relations",
            &[
                req(
                    "entity",
                    "string",
                    "Entity name whose relation edges to expand",
                ),
                opt(
                    "hops",
                    "integer",
                    "Number of graph hops to walk, clamped to a maximum of 3 (default 1)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "mem_stats",
            "Vault counters: current and total entities, relations, observations",
            &[],
            READ_ONLY,
        ),
        tool(
            "mem_duplicates",
            "Vault hygiene: groups of *current* entities that are the same name in different dresses (case, separators, word order) and facts recorded more than once. Each group carries every member's id, kind, fact count and access count, so the keeper is chosen from evidence instead of guessing. Deterministic and model-free; explicitly not fuzzy (\"AuthService\" and \"AuthSvc\" are not grouped) — a wrong merge is worse than a missed one.",
            &[
                opt(
                    "limit",
                    "integer",
                    "Max groups per section, clamped to 200 (default 20)",
                ),
                opt(
                    "min_group",
                    "integer",
                    "Smallest group to report, at least 2 (default 2)",
                ),
                opt(
                    "fuzzy",
                    "boolean",
                    "Also report stem-similarity candidates (shared 4+ char token stem, e.g. AuthService/AuthSvc); never merged automatically (default false)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "mem_merge",
            "Merge a duplicate vault entity into a keeper: moves its current facts (skipping ones the keeper already has), rewires its live relations to the keeper, and closes the duplicate with the reason journaled — the whole merge is visible in vault_history and reversible as history. The keeper keeps its identity; facts and graph edges are what move.",
            &[
                req(
                    "keeper_id",
                    "integer",
                    "Entity id that survives the merge and accumulates the facts",
                ),
                req(
                    "merge_id",
                    "integer",
                    "Entity id to merge away; it is closed (revoked with reason), never deleted",
                ),
                opt(
                    "reason",
                    "string",
                    "Why the merge is happening; journaled on the closed entity (default: 'merged duplicate')",
                ),
            ],
            WRITE,
        ),
        tool(
            "mem_ingest",
            "Ingest a directory into the vault: code structure (tree-sitter), markdown headings, and the full text of every document as searchable facts",
            &[req(
                "path",
                "string",
                "Filesystem path of the directory to ingest",
            )],
            Annotations {
                read_only: false,
                destructive: false,
                idempotent: false,
                open_world: true,
            },
        ),
        typed_tool(
            "coord_join",
            "Join the coordination room for this vault; returns your session_id for all other coord_* calls",
            &[req(
                "name",
                "string",
                "Display name for your session in the coordination room",
            )],
            WRITE,
            coord_join_schema(),
        ),
        typed_tool(
            "coord_heartbeat",
            "Keep your coordination session alive; stale sessions are reported as inactive",
            &[req(
                "session_id",
                "string",
                "Session id returned by coord_join",
            )],
            Annotations {
                read_only: false,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
            coord_ok_schema(),
        ),
        typed_tool(
            "coord_leave",
            "Leave the coordination room and release all held path claims",
            &[req(
                "session_id",
                "string",
                "Session id returned by coord_join",
            )],
            WRITE,
            coord_ok_schema(),
        ),
        tool(
            "coord_claim",
            "Claim a path glob before editing; first-writer-wins with TTL auto-release",
            &[
                req("session_id", "string", "Session id returned by coord_join"),
                req(
                    "path_glob",
                    "string",
                    "Path glob to claim before editing files under it",
                ),
                opt(
                    "ttl_secs",
                    "integer",
                    "Seconds before the claim is automatically released (default 1800)",
                ),
            ],
            WRITE,
        ),
        tool(
            "coord_release",
            "Release one of your claimed path globs early",
            &[
                req("session_id", "string", "Session id returned by coord_join"),
                req(
                    "path_glob",
                    "string",
                    "Path glob previously claimed via coord_claim",
                ),
            ],
            WRITE,
        ),
        tool(
            "coord_status",
            "Who is in the coordination room, which path globs each session holds, and staleness",
            &[],
            READ_ONLY,
        ),
        typed_tool(
            "coord_task_create",
            "Create a task on the shared board",
            &[
                req(
                    "title",
                    "string",
                    "Short task title shown on the shared board",
                ),
                opt(
                    "description",
                    "string",
                    "Longer task details (defaults to empty)",
                ),
                opt(
                    "depends_on",
                    "string",
                    "Task id this task depends on; it stays blocked until that task completes",
                ),
            ],
            WRITE,
            coord_task_create_schema(),
        ),
        tool(
            "coord_task_claim",
            "Atomically claim a task so only one session owns it",
            &[
                req("task_id", "string", "Task id from coord_task_list to claim"),
                req(
                    "session_id",
                    "string",
                    "Session id returned by coord_join claiming the task",
                ),
            ],
            WRITE,
        ),
        tool(
            "coord_task_list",
            "List all tasks on the shared board with status and claimant",
            &[],
            READ_ONLY,
        ),
        tool(
            "coord_send",
            "Message another coordination session, or broadcast to the whole room",
            &[
                req(
                    "from_session",
                    "string",
                    "Your session id returned by coord_join",
                ),
                opt(
                    "to_session",
                    "string",
                    "Recipient session id; omit to broadcast to every session in the room",
                ),
                req("body", "string", "Message body to deliver"),
            ],
            WRITE,
        ),
        tool(
            "coord_inbox",
            "Read direct messages addressed to you plus room broadcasts",
            &[
                req(
                    "session_id",
                    "string",
                    "Session id returned by coord_join whose inbox to read",
                ),
                opt(
                    "include_broadcast",
                    "boolean",
                    "Whether to include broadcast messages alongside direct messages (default true)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_index",
            "(Re)index a workspace into the code graph; omit incremental for full index, or pass a relative file path for a single-file refresh. When the workspace holds several codebases the result lists them — pass codebases to index only some. Symbols are also embedded when the server has an embedding runtime, so kg_search can rank with FTS+vector RRF; pass embed:false to skip that for this run.",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                opt(
                    "incremental",
                    "string",
                    "Relative path within workspace to re-index incrementally; omit for full re-index",
                ),
                opt_list(
                    "codebases",
                    "Index only these codebases (names or paths, as the result's `codebases` lists them); omit to index all of them",
                ),
                opt(
                    "embed",
                    "boolean",
                    "Embed symbols for semantic search (default true when the server has an embedding runtime). Set false to build structure only — much faster, and `kg_search` stays lexical until a later run embeds.",
                ),
            ],
            Annotations {
                read_only: false,
                destructive: false,
                idempotent: false,
                open_world: true,
            },
        ),
        tool(
            "kg_impact",
            "Blast radius: direct and transitive callers of a symbol in the code graph. Use BEFORE a refactor or risky change to see what your edit would break.",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                req(
                    "symbol",
                    "string",
                    "Symbol qname or unique short name to trace from",
                ),
                opt("depth", "integer", "Maximum hops to walk (default 2)"),
                opt(
                    "include_tests",
                    "boolean",
                    "Include test callers (default false)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_trace",
            "Paginated direct callers of a symbol with a generation-stamped cursor. For the full flow picture, prefer kg_explore; use kg_trace when there are too many callers to show at once.",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                req(
                    "symbol",
                    "string",
                    "Symbol qname or unique short name to trace from",
                ),
                opt(
                    "page_size",
                    "integer",
                    "Direct callers per page, clamped to 5000 (default 20)",
                ),
                opt(
                    "cursor",
                    "string",
                    "Opaque cursor from a previous page's next_cursor",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_status",
            "Code graph staleness and generation counters for a workspace",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                opt(
                    "max_checks",
                    "integer",
                    "Cap on stat+hash comparisons (default 64)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_explore",
            "PRIMARY CODE TOOL — call FIRST for almost any code question OR before an edit: how does X work, architecture, a bug, where/what is X, surveying an area, or the symbols you are about to change. Returns the verbatim line-numbered source of the relevant symbols grouped by file in ONE capped call (Read-equivalent — treat the shown source as already Read; do NOT re-open those files), plus the call-path flow among them and staleness info. Query can be a natural-language question OR a bag of symbol/file names. Usually the ONLY call you need — far fewer tokens and round-trips than a grep/read loop.",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                req(
                    "query",
                    "string",
                    "Symbol qname, short name, FTS query, natural-language question, or a bag of symbol/file names (e.g. \"AuthService loginUser session-manager\")",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_search",
            "Symbol search over the code graph: qname, kind, file, start line, signature. Uses FTS+vector hybrid RRF when embeddings are available (vector_ranked=true), plain FTS otherwise. Use when you want a compact ranked list of matching symbols BEFORE deciding what to read in full (kg_explore returns sources; kg_search returns the index).",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                req(
                    "query",
                    "string",
                    "FTS query over symbol names/signatures (camelCase tokens split automatically)",
                ),
                opt("limit", "integer", "Max hits, clamped to 200 (default 20)"),
                opt(
                    "hybrid",
                    "boolean",
                    "Use vector+RRF when embeddings exist (default true)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_coverage",
            "Index health: files grouped by parse status (ok/partial/unsupported/skipped/failed), generation, and the stale file count. Use to diagnose why a symbol is missing from results.",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                opt(
                    "max_checks",
                    "integer",
                    "Cap on stat+hash comparisons for staleness (default 64)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_communities",
            "Architecture map: the code graph's communities (deterministic label propagation over live CALLS edges), each with size, cohesion and representative symbols, plus the count of symbols outside any community. Use to answer \"what are the main subsystems and how internally coherent are they\" — a stable, index-derived view that does not change with directory layout. Same index, same clusters. Nothing is sent to a model to build it.",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                opt(
                    "limit",
                    "integer",
                    "Max communities to return, clamped to 200 (default 12)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_dead",
            "Dead code scan: functions with zero live callers. Optional min loop-depth filter to surface deeply-nested orphaned logic first. Excludes test modules and public/exported API.",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                opt(
                    "min_loop_depth",
                    "integer",
                    "Only report bodies nested at least this deep (default 0 = all dead fns)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_unresolved",
            "Resolution residue: every call/ref the code graph could NOT bind, with the tail name for retry-on-new-symbol. High unresolved counts mean the index missed imports or needs re-index.",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                opt(
                    "status",
                    "string",
                    "Filter: pending or failed (default all)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_complexity",
            "Functions ranked by transitive loop depth (deepest nested loop anywhere in their call subtree), with recursion flags. Use to find the riskiest functions to touch or test first.",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                opt("limit", "integer", "Max rows (default 50, clamped to 1000)"),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_changes",
            "Git-diff impact: maps uncommitted changes (git diff --name-only) to the symbols they define, then computes the blast radius of each affected symbol — what your in-progress edit would break. Use BEFORE committing or pushing.",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                opt("base", "string", "Git base ref for the diff (default HEAD)"),
                opt(
                    "depth",
                    "integer",
                    "Blast-radius hops per affected symbol (default 1)",
                ),
            ],
            READ_ONLY,
        ),
        // The three engine capabilities that had no agent surface at all. The
        // graph could answer "what services exist", "what can an untrusted entry
        // point reach" and "how does A reach B" for the CLI and the web UI, and
        // an agent had no way to ask. Parity matters most in this direction:
        // agents write the code, so they benefit most from the answers.
        tool(
            "kg_services",
            "Deployment shape of the workspace: services declared in compose/Dockerfiles with their images and dependencies, calls that cross a service boundary (marked with whether the callee is served inside this repository), and the environment keys that are declared versus read. Answers \"what talks to what, and what env does it need\".",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                opt(
                    "undeclared_only",
                    "boolean",
                    "Report only env keys that are read but never declared (default false)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_risk",
            "Taint reach: from every entry point (routes, mains, roots nothing calls), which dangerous callees (shell, SQL, filesystem, network, deserialization) its code can reach, with the hop depth of each sink. This is a triage list of where untrusted input meets a sink — not a proof of exploitability.",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                opt("depth", "integer", "Call-edge traversal depth (default 6)"),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_why",
            "Explain how one symbol reaches another along call edges — the shortest call path, step by step. Use when you need to know whether a change to A can affect B, or how a request gets from an entry point to a function. Reports \"unreachable within N hops\" as an answer rather than an error, and names which of the two symbols is missing or ambiguous when no path can be computed.",
            &[
                opt(
                    "workspace",
                    "string",
                    "Workspace root (default: server cwd)",
                ),
                req("from", "string", "Symbol the path starts at (caller side)"),
                req("to", "string", "Symbol the path should reach (callee side)"),
                one_of(
                    "direction",
                    "string",
                    "Which way to walk the edges (default down)",
                    &["down", "up", "both"],
                ),
                opt("depth", "integer", "Maximum hops to search (default 6)"),
            ],
            READ_ONLY,
        ),
        tool(
            "kg_doctor",
            "One-screen health check for the code graph: completeness (coverage, files that failed to parse or were over the size cap), freshness (a full staleness sweep, not a sample), unresolved call-site rate, and whether semantic search is usable. Answers \"why does the graph miss the thing I am looking for?\" with a list of findings, each naming its own fix.",
            &[opt(
                "workspace",
                "string",
                "Workspace root (default: the directory the server was started in)",
            )],
            READ_ONLY,
        ),
    ]
}
