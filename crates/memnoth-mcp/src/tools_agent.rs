//! Agent-utility tool family: filesystem, git, time, sequential thinking, and
//! temporal vault queries.
//!
//! Research (2026 market survey): the tool categories agents actually lean on
//! are filesystem, git, time, and reasoning aids (Sequential Thinking MCP),
//! while the *uncontested* category is temporal memory — as-of recall and
//! contradiction detection — which the memory vault (memnoth-engine) already
//! stores via its entity_versions / relation_versions journal. No competitor
//! MCP (server-memory, Mem0, Graphiti, codebase-memory-mcp) exposes temporal
//! reasoning as a tool surface. That is the moat; these tools make it
//! callable.
//!
//! Design follows the codegraph lesson: keep the dense flagship (kg_explore)
//! first in steering; these are narrow, well-described verbs the agent picks
//! deliberately. All bounded (no unbounded reads), read-only where possible.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use memnoth_engine::MemoryEngine;
use rusqlite::params;
use serde_json::{Value, json};

use crate::{READ_ONLY, WRITE, arg_str, arg_usize, opt, output_object, req, tool, typed_tool};

/// Definitions for the agent-utility tools. Deliberate, narrow verbs — not
/// context-tax padding.
///
/// Membership is **not** declared here: it lives in `lib.rs`'s `TOOL_PROFILES`
/// table, one row per tool, so the same table answers `tools/list` and
/// `tools/call`. These used to be Full-only, which is why the temporal quartet
/// below (`vault_history` / `vault_asof` / `vault_contradictions` / `vault_top`)
/// was unreachable from the profile named for memory: the quartet is now in
/// `memory`, and the toolbox verbs are in `code`.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "now",
            "Current wall-clock time (ms epoch + ISO-ish local string). Call when the task mentions 'now', 'today', dates, or relative time — models drift without a clock.",
            &[],
            READ_ONLY,
        ),
        tool(
            "think",
            "Sequential thinking scratchpad: append one reasoning step; returns the full chain so far (step numbers + text). Use for multi-step plans, tradeoff analysis, or any problem the model wants to reason about explicitly instead of in one shot.",
            &[req(
                "step",
                "string",
                "One reasoning step to append (e.g. 'option A fails because X')",
            )],
            WRITE,
        ),
        tool(
            "fs_list",
            "List a directory: one line per entry with kind (dir/file) and size. Bounded to 500 entries.",
            &[
                req(
                    "path",
                    "string",
                    "Directory path, relative to the workspace or absolute",
                ),
                opt(
                    "depth",
                    "integer",
                    "Recursion depth (0 = top level only, default 0)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "fs_read",
            "Read a file with line numbers, bounded to 2000 lines / 120 KB. Prefer kg_explore for symbol-level questions; use this when you need exact raw file content (config, manifests, generated code).",
            &[
                req(
                    "path",
                    "string",
                    "File path, relative to the workspace or absolute",
                ),
                opt("start_line", "integer", "0-based start line (default 0)"),
                opt("max_lines", "integer", "Line count cap (default 2000)"),
            ],
            READ_ONLY,
        ),
        tool(
            "git_status",
            "Git working-tree status: branch, staged/unstaged file list, ahead/behind. Call before proposing edits so the change plan matches reality.",
            &[],
            READ_ONLY,
        ),
        tool(
            "git_diff",
            "Unified diff of the working tree (unstaged + staged), bounded to 8000 lines. Call to see exactly what changed before reviewing or committing.",
            &[
                opt("path", "string", "Restrict to this path"),
                opt(
                    "staged",
                    "boolean",
                    "Show only staged changes (default false)",
                ),
            ],
            READ_ONLY,
        ),
        tool(
            "git_log",
            "Recent commit history: hash, author, date, subject. Bounded to 50 commits.",
            &[
                opt("n", "integer", "Number of commits (default 20, max 50)"),
                opt("path", "string", "Restrict to commits touching this path"),
            ],
            READ_ONLY,
        ),
        typed_tool(
            "vault_history",
            "Full temporal history of an entity: every birth/rename/revoke/supersede with validity windows and change reasons, from the engine journal. Use to answer 'what happened to X over time' or 'why did X change'.",
            &[req("name", "string", "Entity name (current or historical)")],
            READ_ONLY,
            vault_history_schema(),
        ),
        typed_tool(
            "vault_asof",
            "Recall the vault's belief state about an entity at a point in time: its name/kind/confidence then, plus facts and relations believed at that instant. Temporal reasoning no competitor MCP offers.",
            &[
                req("name", "string", "Entity name"),
                req(
                    "as_of_ms",
                    "integer",
                    "Epoch millis; use the 'now' tool to get the current instant",
                ),
            ],
            READ_ONLY,
            vault_asof_schema(),
        ),
        typed_tool(
            "vault_contradictions",
            "Find contradicted facts about an entity: revoked/superseded facts and relations with the reason and when they were invalidated. Use before trusting a current belief — if the vault changed its mind, the reason is recorded here.",
            &[req("name", "string", "Entity name")],
            READ_ONLY,
            vault_contradictions_schema(),
        ),
        typed_tool(
            "vault_top",
            "Most-referenced entities right now, ranked by access frequency then recency (every mem_search / mem_find_facts / mem_related read feeds the counters). Use to answer 'what is the team actually working on' or to scope context injection to what matters. Pass decay_ms to rank by exponential half-life decay instead, so stale hits age out and recent ones dominate.",
            &[
                opt(
                    "limit",
                    "integer",
                    "Max entities (default 10, clamped to 100)",
                ),
                opt(
                    "decay_ms",
                    "integer",
                    "Half-life in ms for exponential decay ranking (e.g. 604800000 = 7 days); omits to keep raw-count order. MEMNOTH_MCP_VAULT_TOP_DECAY_MS sets a deployment-wide default.",
                ),
            ],
            READ_ONLY,
            vault_top_schema(),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Declared result shapes for the temporal quartet.
//
// These are the schemas a host validates `structuredContent` against, so each
// one describes what its handler returns *exactly* — the envelope's fields and
// their types. Item internals stay loose on purpose: pinning every journal
// column would turn any engine change into a protocol change. The guard test
// `typed_outputs_match_their_declared_schemas` walks the live payloads against
// these, because a schema that does not match what we send is worse than none.

fn vault_history_schema() -> Value {
    output_object(
        json!({
            "entity": {"type": "string", "description": "The entity name as asked"},
            "versions": {
                "type": "array",
                "items": {"type": "object"},
                "description": "Every version of the entity, oldest first, each with its validity window and change reason"
            },
            "count": {"type": "integer", "description": "Number of versions returned"},
        }),
        &["entity", "versions", "count"],
    )
}

fn vault_asof_schema() -> Value {
    output_object(
        json!({
            "entity": {"type": "string"},
            "as_of_ms": {"type": "integer", "description": "The instant asked about"},
            "now_ms": {"type": "integer", "description": "Server clock when the answer was computed"},
            "believed_at": {
                "type": "array",
                "items": {"type": "object"},
                "description": "Entity versions whose validity window covers the instant"
            },
            "facts": {"type": "array", "items": {"type": "object"}, "description": "Facts believed at that instant"},
            "relations": {"type": "array", "items": {"type": "object"}, "description": "Outgoing relations believed at that instant"},
            "note": {
                "type": "string",
                "description": "Explains an empty `believed_at` (the entity was not believed then); empty otherwise"
            },
        }),
        &[
            "entity",
            "as_of_ms",
            "now_ms",
            "believed_at",
            "facts",
            "relations",
            "note",
        ],
    )
}

fn vault_contradictions_schema() -> Value {
    output_object(
        json!({
            "entity": {"type": "string"},
            "contradictions": {
                "type": "array",
                "items": {"type": "object"},
                "description": "Journaled versions and revoked relations, each with when and why the vault changed its mind"
            },
            "count": {"type": "integer"},
            "guidance": {"type": "string", "description": "How to treat the current belief given what was found"},
        }),
        &["entity", "contradictions", "count", "guidance"],
    )
}

fn vault_top_schema() -> Value {
    output_object(
        json!({
            "top": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "entity_id": {"type": "integer"},
                        "name": {"type": "string"},
                        "kind": {"type": "string"},
                        "access_count": {"type": "integer"},
                        "decayed_score": {"type": "number", "description": "Present only when half-life decay is active: accesses weighted by exponential decay, so recent hits count more than old ones"}
                    }
                },
                "description": "Entities ranked by access frequency, then recency — or by decayed score when a half-life is configured"
            },
            "note": {"type": "string", "description": "How the ranking is computed"},
        }),
        &["top", "note"],
    )
}

// -------------------------------------------------------------------------
// sequential thinking
// -------------------------------------------------------------------------

static THINK_LOG: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub(crate) fn think(args: &Value) -> Value {
    let step = arg_str(args, "step").unwrap_or_default();
    let mut log = THINK_LOG.lock().unwrap_or_else(|p| p.into_inner());
    log.push(step);
    let chain: Vec<Value> = log
        .iter()
        .enumerate()
        .map(|(i, s)| json!({ "step": i + 1, "text": s }))
        .collect();
    json!({ "chain": chain, "steps": chain.len() })
}

// -------------------------------------------------------------------------
// filesystem (bounded)
// -------------------------------------------------------------------------

fn resolve(path: &str) -> Result<std::path::PathBuf> {
    let p = Path::new(path);
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(p))
    }
}

pub(crate) fn fs_list(args: &Value) -> Result<Value> {
    let path = arg_str(args, "path")?;
    let depth = arg_usize(args, "depth", 0).min(4);
    let root = resolve(&path)?;
    let mut entries: Vec<Value> = Vec::new();
    let mut stack: Vec<(std::path::PathBuf, usize)> = vec![(root, 0)];
    while let Some((dir, d)) = stack.pop() {
        if d > depth {
            continue;
        }
        let mut dir_entries = std::fs::read_dir(&dir)
            .with_context(|| format!("read_dir {}", dir.display()))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        dir_entries.sort_by_key(|e| e.file_name());
        for e in dir_entries {
            if entries.len() >= 500 {
                break;
            }
            let ft = e.file_type()?;
            let name = e.file_name().to_string_lossy().to_string();
            if ft.is_dir() {
                entries.push(json!({ "name": name, "kind": "dir", "size": null }));
                if d < depth {
                    stack.push((e.path(), d + 1));
                }
            } else {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                entries.push(json!({ "name": name, "kind": "file", "size": size }));
            }
        }
    }
    Ok(json!({ "entries": entries, "count": entries.len() }))
}

pub(crate) fn fs_read(args: &Value) -> Result<Value> {
    let path = arg_str(args, "path")?;
    let start = arg_usize(args, "start_line", 0);
    let max_lines = arg_usize(args, "max_lines", 2000).min(2000);
    let root = resolve(&path)?;
    let bytes = std::fs::read(&root).with_context(|| format!("read {}", root.display()))?;
    if bytes.len() > 120 * 1024 {
        return Ok(json!({
            "error": "file exceeds 120 KB bound",
            "size_bytes": bytes.len(),
        }));
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().skip(start).take(max_lines).collect();
    let numbered: Vec<Value> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| json!({ "line": start + i + 1, "text": l }))
        .collect();
    Ok(json!({
        "path": root.display().to_string(),
        "lines_returned": numbered.len(),
        "total_lines": text.lines().count(),
        "content": numbered,
    }))
}

// -------------------------------------------------------------------------
// git (read-only, bounded)
// -------------------------------------------------------------------------

fn git(args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .output()
        .context("failed to run git (is it on PATH?)")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        anyhow::bail!("git {} failed: {}", args.join(" "), err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub(crate) fn git_status() -> Result<Value> {
    let short = git(&["status", "--short", "--branch"])?;
    let lines: Vec<String> = short.lines().map(|l| l.to_string()).collect();
    Ok(json!({ "status": lines }))
}

pub(crate) fn git_diff(args: &Value) -> Result<Value> {
    let path = args.get("path").and_then(|v| v.as_str());
    let staged = args
        .get("staged")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut cmd: Vec<&str> = vec!["diff", "--no-ext-diff", "--unified=3"];
    if staged {
        cmd.push("--cached");
    }
    if let Some(p) = path {
        cmd.push(p);
    }
    let out = git(&cmd)?;
    let lines: Vec<&str> = out.lines().take(8000).collect();
    Ok(json!({
        "diff": lines.join("\n"),
        "truncated": out.lines().count() > 8000,
    }))
}

pub(crate) fn git_log(args: &Value) -> Result<Value> {
    let n = arg_usize(args, "n", 20).min(50);
    let path = args.get("path").and_then(|v| v.as_str());
    let mut cmd: Vec<String> = vec![
        "log".into(),
        format!("--max-count={n}"),
        "--pretty=format:%h %an %ad %s".into(),
        "--date=short".into(),
    ];
    if let Some(p) = path {
        cmd.push("--".into());
        cmd.push(p.into());
    }
    let cmd_refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    let out = git(&cmd_refs)?;
    let commits: Vec<Value> = out
        .lines()
        .map(|l| {
            let parts: Vec<&str> = l.splitn(4, ' ').collect();
            json!({
                "hash": parts.first().copied().unwrap_or(""),
                "author": parts.get(1).copied().unwrap_or(""),
                "date": parts.get(2).copied().unwrap_or(""),
                "subject": parts.get(3).copied().unwrap_or(""),
            })
        })
        .collect();
    Ok(json!({ "commits": commits }))
}

// -------------------------------------------------------------------------
// temporal vault (the moat)
// -------------------------------------------------------------------------

/// How far a `supersedes` chain is followed. A revision chain longer than this is
/// an editing accident, not a history, and an unbounded walk on a corrupted graph
/// is a hang.
const SUPERSEDE_CHAIN_HOPS: usize = 16;

/// All entities that ever carried this name (chain across renames): current
/// row + every journaled snapshot + every revision reachable by a `supersedes`
/// edge, so a rename is reachable from *either* name.
fn entity_chain(engine: &MemoryEngine, name: &str) -> Result<Vec<Value>> {
    let conn = engine.connection();
    // current/live rows with this name
    let mut live = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT id, name, canonical_name, valid_from, valid_to \
                 FROM entities WHERE name = ?1 OR canonical_name = ?1 ORDER BY valid_from",
            )
            .context("prepare live entity query")?;
        let rows = stmt.query_map(params![name], |r| {
            Ok(json!({
                "entity_id": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "canonical_name": r.get::<_, Option<String>>(2)?,
                "valid_from": r.get::<_, i64>(3)?,
                "valid_to": r.get::<_, Option<i64>>(4)?,
            }))
        })?;
        for row in rows {
            live.push(row?);
        }
    }
    if live.is_empty() {
        return Ok(live);
    }
    // One name is not a chain. A revision is linked by the `supersedes` relation
    // `mem_supersede` writes, so follow it in both directions until the chain stops
    // growing — otherwise `vault_history` on a *renamed* entity listed exactly one
    // version, the successor, because the predecessor's row carries the old name and
    // nothing else connected them. The link is an edge in the graph, not an
    // inference from two rows that happen to share a name.
    let mut ids: Vec<i64> = live
        .iter()
        .filter_map(|e| e["entity_id"].as_i64())
        .collect();
    for _ in 0..SUPERSEDE_CHAIN_HOPS {
        let mut grew = false;
        for id in ids.clone() {
            let mut stmt = conn
                .prepare(
                    "SELECT source_id, target_id FROM relations \
                     WHERE kind = 'supersedes' AND (source_id = ?1 OR target_id = ?1)",
                )
                .context("prepare supersede chain query")?;
            let linked: Vec<i64> = stmt
                .query_map(params![id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })?
                .filter_map(|x| x.ok())
                .flat_map(|(s, t)| [s, t])
                .collect();
            for other in linked {
                if other == id || ids.contains(&other) {
                    continue;
                }
                // Read the row without the validity filter: the point of a chain is
                // to include revisions that are *closed* at the current instant.
                let mut row_stmt = conn.prepare(
                    "SELECT id, name, canonical_name, valid_from, valid_to \
                     FROM entities WHERE id = ?1",
                )?;
                let fetched: Vec<Value> = row_stmt
                    .query_map(params![other], |r| {
                        Ok(json!({
                            "entity_id": r.get::<_, i64>(0)?,
                            "name": r.get::<_, String>(1)?,
                            "canonical_name": r.get::<_, Option<String>>(2)?,
                            "valid_from": r.get::<_, i64>(3)?,
                            "valid_to": r.get::<_, Option<i64>>(4)?,
                        }))
                    })?
                    .filter_map(|x| x.ok())
                    .collect();
                ids.push(other);
                live.extend(fetched);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    live.sort_by_key(|e| e["valid_from"].as_i64().unwrap_or(0));
    let mut versions = Vec::new();
    for id in ids {
        let mut stmt = conn
            .prepare(
                "SELECT entity_id, snapshot, valid_from, valid_to, change_reason \
                 FROM entity_versions WHERE entity_id = ?1 ORDER BY valid_from",
            )
            .context("prepare version query")?;
        let rows = stmt.query_map(params![id], |r| {
            let snapshot: String = r.get(1)?;
            let parsed: Value = serde_json::from_str(&snapshot).unwrap_or(Value::Null);
            Ok(json!({
                "entity_id": r.get::<_, i64>(0)?,
                "snapshot": parsed,
                "valid_from": r.get::<_, i64>(2)?,
                "valid_to": r.get::<_, Option<i64>>(3)?,
                "change_reason": r.get::<_, Option<String>>(4)?,
            }))
        })?;
        for row in rows {
            versions.push(row?);
        }
    }
    versions.extend(live);
    versions.sort_by_key(|v| v["valid_from"].as_i64().unwrap_or(0));
    Ok(versions)
}

pub(crate) fn vault_history(engine: &MemoryEngine, args: &Value) -> Result<Value> {
    let name = arg_str(args, "name")?;
    let chain = entity_chain(engine, &name)?;
    Ok(json!({
        "entity": name,
        "versions": chain,
        "count": chain.len(),
    }))
}

pub(crate) fn vault_asof(engine: &MemoryEngine, args: &Value, now: i64) -> Result<Value> {
    let name = arg_str(args, "name")?;
    let as_of = arg_usize(args, "as_of_ms", 0) as i64;
    let chain = entity_chain(engine, &name)?;
    // the live row believed at as_of
    let believed: Vec<Value> = chain
        .iter()
        .filter(|v| {
            let from = v["valid_from"].as_i64().unwrap_or(0);
            let to = v["valid_to"].as_i64();
            from <= as_of && to.is_none_or(|t| t > as_of)
        })
        .cloned()
        .collect();
    let entity_id = believed
        .first()
        .and_then(|v| v["entity_id"].as_i64())
        .or_else(|| chain.first().and_then(|v| v["entity_id"].as_i64()))
        .unwrap_or(0);
    // facts + relations believed at as_of
    let mut facts = Vec::new();
    let mut rels = Vec::new();
    if entity_id > 0 {
        if let Ok(obs) = engine.observations_for(entity_id, as_of) {
            facts = obs
                .into_iter()
                .map(
                    |o| json!({ "content": o.content, "kind": o.kind, "confidence": o.confidence }),
                )
                .collect();
        }
        if let Ok(out) = engine.outgoing_relations(entity_id, as_of) {
            rels = out
                .into_iter()
                .map(|r| json!({ "target_id": r.target_id, "kind": r.kind, "confidence": r.confidence }))
                .collect();
        }
    }
    Ok(json!({
        "entity": name,
        "as_of_ms": as_of,
        "now_ms": now,
        "believed_at": believed,
        "facts": facts,
        "relations": rels,
        "note": if believed.is_empty() { "entity was not believed at this instant" } else { "" },
    }))
}

/// Exponential half-life decay over the engine's access counters.
///
/// The ideal quantity is `effective = Σ_hits 0.5^(age/half_life)` — a hit today
/// counts 1.0, one half-life ago 0.5, two 0.25. That is what "frequency then
/// recency" was approximating by sorting: a raw count lets a 200-hit entity
/// that has been dead for a year outrank a 10-hit entity that is load-bearing
/// *this week*, and the question a ranking answers should be "what matters
/// now", not "what mattered once".
///
/// The engine keeps only aggregate counters (`access_stats`: count +
/// last_access), not a per-hit log, and the engine is another lane's file. So
/// this is the exact integral of the **uniform-spread approximation**: the
/// `access_count` hits are modelled as evenly distributed over
/// `[created_at, last_access]`, which integrates in closed form to
///
/// `count × half/(ln2·W) × (0.5^(age_last/half) − 0.5^(age_created/half))`,
///
/// `W = last_access − created_at` (collapsing to `count × 0.5^(age_last/half)`
/// when W ≤ 0, i.e. a single instantaneous burst). For spread-out traffic this
/// tracks the true sum closely; for a single old burst it is exact. A per-hit
/// access log in the engine would make this exact — filed to the KG lane
/// (`L-MCP-29`), and this function is the only place that changes.
///
/// Read-only and pure: a presentation decision ("rank by what matters now")
/// over data the engine already records. The engine's `top_entities` stays the
/// raw ordering for callers who want it.
fn decayed_access(
    conn: &rusqlite::Connection,
    half_life_ms: i64,
    now: i64,
) -> Result<HashMap<i64, f64>> {
    let mut out: HashMap<i64, f64> = HashMap::new();
    if half_life_ms <= 0 {
        return Ok(out);
    }
    let half = half_life_ms as f64;
    let mut stmt = conn.prepare(
        "SELECT a.entity_id, a.access_count, a.last_access, e.created_at \
         FROM access_stats a JOIN entities e ON e.id = a.entity_id \
         WHERE e.valid_from <= ?1 AND (e.valid_to IS NULL OR e.valid_to > ?1)",
    )?;
    let mut rows = stmt.query(params![now])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let count: i64 = row.get(1)?;
        let last: Option<i64> = row.get(2)?;
        let created: i64 = row.get(3)?;
        if count <= 0 {
            continue;
        }
        // Missing last_access (never read) contributes nothing.
        let Some(last) = last else { continue };
        let window = (last - created).max(0) as f64;
        let age_last = (now - last).max(0) as f64;
        let effective = if window <= 0.0 {
            count as f64 * 0.5f64.powf(age_last / half)
        } else {
            let age_created = (now - created).max(0) as f64;
            let ln2 = std::f64::consts::LN_2;
            count as f64
                * (half / (ln2 * window))
                * (0.5f64.powf(age_last / half) - 0.5f64.powf(age_created / half))
        };
        if effective > 0.0 {
            out.insert(id, effective);
        }
    }
    Ok(out)
}

pub(crate) fn vault_top(engine: &MemoryEngine, args: &Value, now: i64) -> Result<Value> {
    let limit = arg_usize(args, "limit", 10);
    // Per-call `decay_ms` wins; `MEMNOTH_MCP_VAULT_TOP_DECAY_MS` (e.g.
    // 604800000 = 7 days) is the deployment-wide default. Default OFF either
    // way: a half-life is a policy choice, and silently changing an existing
    // ranking's order is not a default a host ever asked for.
    let decay_ms = args.get("decay_ms").and_then(|v| v.as_i64()).or_else(|| {
        std::env::var("MEMNOTH_MCP_VAULT_TOP_DECAY_MS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
    });
    let items: Vec<Value> = if let Some(half_life) = decay_ms.filter(|h| *h > 0) {
        // Decay reorders, so the candidate set is the whole decayed map, not a
        // raw-count page: a recently-hot entity outside the count top-N must be
        // able to surface — that is the entire point of the decay pass.
        let conn = engine.connection();
        let mut decayed: Vec<(i64, f64)> =
            decayed_access(conn, half_life, now)?.into_iter().collect();
        decayed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut items: Vec<Value> = Vec::new();
        for (id, d) in decayed {
            if items.len() >= limit {
                break;
            }
            let Some(e) = engine.entity_at(id, now)? else {
                continue;
            };
            let count: i64 = conn
                .query_row(
                    "SELECT access_count FROM access_stats WHERE entity_id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            items.push(json!({
                "entity_id": e.id,
                "name": e.name,
                "kind": e.kind.as_str(),
                "access_count": count,
                "decayed_score": (d * 1000.0).round() / 1000.0,
            }));
        }
        items
    } else {
        engine
            .top_entities(now, limit)?
            .into_iter()
            .map(|(e, count)| {
                json!({
                    "entity_id": e.id,
                    "name": e.name,
                    "kind": e.kind.as_str(),
                    "access_count": count,
                })
            })
            .collect()
    };
    let note = if decay_ms.is_some() {
        "ranked by exponential half-life decay of per-hit accesses (what matters now); raw access_count kept for comparison — depth of team knowledge, memary-style"
    } else {
        "ranked by access frequency then recency — depth of team knowledge, memary-style"
    };
    Ok(json!({
        "top": items,
        "note": note,
    }))
}

pub(crate) fn vault_contradictions(engine: &MemoryEngine, args: &Value) -> Result<Value> {
    let name = arg_str(args, "name")?;
    let chain = entity_chain(engine, &name)?;
    let ids: Vec<i64> = chain
        .iter()
        .filter_map(|v| v["entity_id"].as_i64())
        .collect();
    let mut contradicted = Vec::new();
    for id in ids {
        let conn = engine.connection();
        // journaled entity versions record renames/revokes with reasons
        let mut stmt = conn
            .prepare(
                "SELECT snapshot, valid_from, valid_to, change_reason \
                 FROM entity_versions WHERE entity_id = ?1 AND change_reason IS NOT NULL \
                 ORDER BY valid_from",
            )
            .context("prepare contradiction query")?;
        let rows = stmt.query_map(params![id], |r| {
            let snapshot: String = r.get(0)?;
            let parsed: Value = serde_json::from_str(&snapshot).unwrap_or(Value::Null);
            Ok(json!({
                "previous": parsed,
                "valid_to": r.get::<_, Option<i64>>(2)?,
                "reason": r.get::<_, Option<String>>(3)?,
            }))
        })?;
        for row in rows {
            contradicted.push(row?);
        }
        // revoked relations
        let mut rstmt = conn
            .prepare(
                "SELECT source_id, target_id, kind, revoked_at \
                 FROM relations WHERE source_id = ?1 AND revoked_at IS NOT NULL \
                 ORDER BY revoked_at",
            )
            .context("prepare revoked relation query")?;
        let rrows = rstmt.query_map(params![id], |r| {
            Ok(json!({
                "source_id": r.get::<_, i64>(0)?,
                "target_id": r.get::<_, i64>(1)?,
                "kind": r.get::<_, String>(2)?,
                "revoked_at": r.get::<_, Option<i64>>(3)?,
            }))
        })?;
        for row in rrows {
            contradicted.push(row?);
        }
    }
    Ok(json!({
        "entity": name,
        "contradictions": contradicted,
        "count": contradicted.len(),
        "guidance": "previous versions carry the change_reason; treat current belief with caution where contradictions exist",
    }))
}
