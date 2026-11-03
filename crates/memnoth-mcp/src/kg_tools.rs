//! The code-graph tool handlers: every `kg_*` call body, one module.
//!
//! Split out of `lib.rs` because that file is a tracked oversized one — the
//! ratchet forbids it growing, and this group is a whole subject rather than an
//! arbitrary N lines, which is the same reason `kg_index.rs` already holds the
//! indexing half. `lib.rs` keeps the *catalogue* (names, schemas, profiles, and
//! the arm that gets here); the bodies and the engine calls they make live here.
//!
//! Nothing in this module knows about the transport, the profile gate or the
//! task machinery — a name that arrives here has already been authorised.

use anyhow::Result;
use memnoth_codegraph::{DEFAULT_BUDGET, blast_radius, blast_radius_page, explore, open_store};
use serde_json::{Value, json};

use crate::{
    arg_str, arg_usize, attach_staleness_notice, cross_encoder, freshness_of, load_communities,
    referenced_staleness_notice, rerank_cross_encoder_symbols, stamped, workspace_root_for_vault,
};

/// The workspace a `kg_*` call is about: the argument, `MEMNOTH_MCP_WORKSPACE`, then the
/// directory the server was started in.
///
/// `workspace` stays an argument for a host that serves several checkouts from one server, but it
/// is optional: a stdio host launches the server in the project, and requiring an absolute path on
/// every one of sixteen calls made the model spend tokens reproducing something the server already
/// knows — with a wrong guess surfacing as an error instead of a fallback.
pub(crate) fn workspace_arg(args: &Value) -> Result<String> {
    if let Some(workspace) = args
        .get("workspace")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|workspace| !workspace.is_empty())
    {
        return Ok(workspace.to_string());
    }
    if let Ok(workspace) = std::env::var("MEMNOTH_MCP_WORKSPACE") {
        let workspace = workspace.trim();
        if !workspace.is_empty() {
            return Ok(workspace.to_string());
        }
    }
    match std::env::current_dir() {
        Ok(dir) => Ok(dir.display().to_string()),
        Err(error) => anyhow::bail!(
            "no workspace argument, no MEMNOTH_MCP_WORKSPACE, and no working directory ({error})"
        ),
    }
}

/// Fill in `workspace` for a `kg_*` call from the vault's own location.
///
/// `memnoth mcp call --root` names the checkout in the CLI rather than in the tool arguments, and
/// the vault it opens is `<root>/.memnoth/vault.db` — the same fact [`workspace_root_for_vault`]
/// reads for resources. Any argument the caller did pass is left exactly as it was.
pub(crate) fn with_default_workspace(
    name: &str,
    args: &Value,
    vault: Option<&std::path::Path>,
) -> Value {
    if !name.starts_with("kg_") || args.get("workspace").is_some() {
        return args.clone();
    }
    let Some(root) = workspace_root_for_vault(vault) else {
        return args.clone();
    };
    let mut filled = args.clone();
    filled["workspace"] = json!(root.display().to_string());
    filled
}

/// Answer one `kg_*` tool call.
///
/// The match is exhaustive over the names `tools_call` routes here and bails on
/// anything else, so a name that is added to the catalogue without a body fails
/// loudly at the first call rather than silently falling through.
pub(crate) fn call(name: &str, args: &Value) -> Result<Value> {
    match name {
        "kg_index" => crate::kg_index::run_kg_index(args),
        "kg_impact" => {
            let workspace = workspace_arg(args)?;
            let symbol = arg_str(args, "symbol")?;
            let depth = arg_usize(args, "depth", 2) as u32;
            let include_tests = args
                .get("include_tests")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            let imp = blast_radius(
                &store,
                &symbol,
                depth as usize,
                DEFAULT_BUDGET,
                include_tests,
            )?;
            stamped(&imp, &store, root)
        }
        "kg_trace" => {
            let workspace = workspace_arg(args)?;
            let symbol = arg_str(args, "symbol")?;
            let page_size = arg_usize(args, "page_size", 20);
            let cursor = args
                .get("cursor")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            let page = blast_radius_page(
                &store,
                &symbol,
                2,
                DEFAULT_BUDGET,
                false,
                page_size,
                cursor.as_deref(),
            )?;
            stamped(&page, &store, root)
        }
        "kg_status" => {
            let workspace = workspace_arg(args)?;
            let max_checks = arg_usize(args, "max_checks", 64);
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            // The same accessor the `mem://index/status` resource uses, so the
            // tool and the resource cannot disagree about one workspace. The
            // tool keeps its own `max_checks` because a caller asking for a full
            // verdict is entitled to a wider sweep than a cache-hint question.
            let freshness = freshness_of(&store, root, max_checks).ok_or_else(|| {
                anyhow::anyhow!(
                    "no code graph at {} — run kg_index for this workspace first",
                    root.join(".memnoth/kg.db").display()
                )
            })?;
            let mut value = serde_json::to_value(freshness.report)?;
            // A status probe answers "is the graph current?", and on this
            // repository the per-file list was 139 rows / ~6k tokens that bury
            // that answer. The count stays; the rows are capped.
            let mut rows: Vec<serde_json::Value> = Vec::new();
            let mut omitted = 0usize;
            if let Some(all) = value["file_details"].as_array() {
                omitted = all.len().saturating_sub(all.len().min(10));
                rows.extend(all.iter().take(10).cloned());
            }
            value["file_details"] = json!(rows);
            if omitted > 0 {
                value["file_details_omitted"] = json!(omitted);
            }
            Ok(value)
        }
        "kg_explore" => {
            let workspace = workspace_arg(args)?;
            let query = arg_str(args, "query")?;
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            let resp = explore(&store, root, &query)?;
            let mut value = serde_json::to_value(&resp)?;
            // In-band staleness transparency: the agent sees which files are
            // pending re-index in the same response it uses, without a
            // separate kg_status round-trip. This count is `explore`'s own —
            // scoped to the files it returned, which is the useful question
            // here — and only the wording is shared.
            if resp.stale_files > 0 {
                value["staleness_notice"] = json!(referenced_staleness_notice(resp.stale_files));
            }
            Ok(value)
        }
        "kg_search" => {
            let workspace = workspace_arg(args)?;
            let query = arg_str(args, "query")?;
            let limit = arg_usize(args, "limit", 20);
            let hybrid = args.get("hybrid").and_then(|v| v.as_bool()).unwrap_or(true);
            let rerank_mode: &str = args.get("rerank").and_then(|v| v.as_str()).unwrap_or("off");
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;

            let vectors_available = memnoth_codegraph::vec_count(&store)?;
            let mut value = if hybrid && vectors_available > 0 {
                // Over-fetch deliberately: reranking a set of exactly `limit`
                // candidates can only permute the results the fused order
                // already chose, which is the one thing a reranker is for.
                let over = limit.saturating_mul(4).clamp(limit, 100);
                let report = memnoth_codegraph::hybrid_search_with_report(&store, &query, over)?;
                let pool = report.hits.len();
                let (hits, ranking) = if rerank_mode == "cross_encoder" && pool > 1 {
                    match rerank_cross_encoder_symbols(&query, &report.hits, limit) {
                        Some((ranked, method)) => (ranked, method.to_string()),
                        None => (
                            report.hits.iter().take(limit).cloned().collect(),
                            "rrf (cross-encoder unavailable)".to_string(),
                        ),
                    }
                } else {
                    (
                        report.hits.iter().take(limit).cloned().collect(),
                        "rrf".to_string(),
                    )
                };
                json!({
                    "hits": hits.iter().map(|scored| &scored.hit).collect::<Vec<_>>(),
                    // Both counts, because they answer different questions: how
                    // much of the index could answer this query, and how much of
                    // it this query actually read.
                    "vectors_available": vectors_available,
                    "vectors_scored": report.vectors_scored,
                    // Results no single retriever returned — reached through the
                    // graph. Surfaced so a caller can tell when structure, and
                    // not text, produced an answer.
                    "added_by_proximity": report.added_by_proximity,
                    "truncated": report.truncated,
                    "rerank_pool": pool,
                    "ranking": ranking,
                })
            } else {
                json!({
                    "hits": memnoth_codegraph::search_symbols(&store, &query, limit)?,
                    "vectors_available": vectors_available,
                    "ranking": if hybrid {
                        "lexical (no vectors for this model)"
                    } else {
                        "lexical"
                    },
                })
            };
            // Say when a reranker is sitting there unused, rather than letting
            // the fused order look like the best this tool can do.
            if rerank_mode != "cross_encoder" && cross_encoder().is_some() {
                value["rerank_available"] = json!("cross_encoder");
            }
            attach_staleness_notice(&mut value, &store, root);
            Ok(value)
        }
        "kg_coverage" => {
            let workspace = workspace_arg(args)?;
            let max_checks = arg_usize(args, "max_checks", 64);
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            let ledger_rows = memnoth_codegraph::ledger(&store)?;
            let freshness = freshness_of(&store, root, max_checks).ok_or_else(|| {
                anyhow::anyhow!(
                    "no code graph at {} — run kg_index for this workspace first",
                    root.join(".memnoth/kg.db").display()
                )
            })?;
            let mut value = serde_json::to_value(json!({
                "ledger": ledger_rows,
                "generation": store.generation()?,
                "stale_files": freshness.report.stale_files,
            }))?;
            // The ledger view and the `kg_*` notices now speak with one voice:
            // the previous here-only sentence ("pending re-index; run kg_index")
            // was a third wording for the same fact.
            if let Some(notice) = freshness.notice() {
                value["staleness_notice"] = json!(notice);
            }
            Ok(value)
        }
        "kg_communities" => {
            let workspace = workspace_arg(args)?;
            let limit = arg_usize(args, "limit", memnoth_codegraph::DEFAULT_MAX_COMMUNITIES);
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            let report = load_communities(&store, limit)?;
            stamped(report, &store, root)
        }
        "kg_dead" => {
            let workspace = workspace_arg(args)?;
            let min_depth = arg_usize(args, "min_loop_depth", 0) as i64;
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            // Public API excluded, as the CLI's default is: a `pub` function may
            // be called from a crate this graph never read.
            let dead = memnoth_codegraph::dead_symbols(&store, min_depth as usize, true)?;
            stamped(json!({ "dead_symbols": dead }), &store, root)
        }
        "kg_unresolved" => {
            let workspace = workspace_arg(args)?;
            let status = args.get("status").and_then(|v| v.as_str());
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            let refs = memnoth_codegraph::unresolved_refs(&store, status.unwrap_or("missed"))?;
            // `refs` is a bounded listing; the count is a separate query. An
            // agent that reads `count: 300` as the size of the problem will
            // draw the wrong conclusion about the index.
            let by_status = memnoth_codegraph::unresolved_counts(&store)?;
            let total: u64 = by_status.iter().map(|(_, n)| *n as u64).sum();
            Ok(serde_json::to_value(json!({
                "unresolved": refs,
                "listed": refs.len(),
                "count": total,
                "by_status": by_status,
                "guidance": "`missed` is the only status that means the index is incomplete; external, macro and method sites were never resolvable. Re-run kg_index after fixing imports.",
            }))?)
        }
        "kg_complexity" => {
            let workspace = workspace_arg(args)?;
            let limit = arg_usize(args, "limit", 50);
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            let rows = memnoth_codegraph::complexity_report(&store, limit)?;
            stamped(json!({ "complexity": rows }), &store, root)
        }
        "kg_changes" => {
            let workspace = workspace_arg(args)?;
            let base = args.get("base").and_then(|v| v.as_str()).unwrap_or("HEAD");
            let depth = arg_usize(args, "depth", 1) as u32;
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;

            // changed files via git (codebase-memory-mcp detect_changes pattern)
            let out = std::process::Command::new("git")
                .args(["diff", "--name-only", base])
                .current_dir(root)
                .output()
                .map_err(|e| anyhow::anyhow!("git diff failed: {e}"))?;
            if !out.status.success() {
                anyhow::bail!("git diff failed: {}", String::from_utf8_lossy(&out.stderr));
            }
            let files_raw = String::from_utf8_lossy(&out.stdout);
            let files: Vec<&str> = files_raw
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();

            let symbols = memnoth_codegraph::symbols_in_files(
                &store,
                &files.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            )?;
            let mut impacts = Vec::new();
            for s in symbols.iter().take(30) {
                if let Ok(imp) =
                    blast_radius(&store, &s.qname, depth as usize, DEFAULT_BUDGET, false)
                {
                    impacts.push(json!({
                        "symbol": s.qname,
                        "file": s.rel_path,
                        "direct_callers": imp.direct.len(),
                        "transitive_symbols": imp.transitive_symbols,
                        "files_touched": imp.files_touched,
                        "hubs": imp.hubs,
                    }));
                }
            }
            Ok(serde_json::to_value(json!({
                "changed_files": files.len(),
                "affected_symbols": symbols.len(),
                "impact": impacts,
                "guidance": "each impact row lists how many callers/files the changed symbol would touch — the risk surface of your uncommitted work.",
            }))?)
        }
        "kg_services" => {
            let workspace = workspace_arg(args)?;
            let undeclared_only = args
                .get("undeclared_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            let services = memnoth_codegraph::service_map(&store)?;
            let cross = memnoth_codegraph::cross_service_calls(&store)?;
            let env = memnoth_codegraph::env_report(&store)?;
            let payload = json!({
                "undeclared_only": undeclared_only,
                "services": services
                    .iter()
                    .map(|s| json!({
                        "name": s.name,
                        "rel_path": s.rel_path,
                        "images": s.images,
                        "depends_on": s.depends_on,
                    }))
                    .collect::<Vec<_>>(),
                "cross_service_calls": cross
                    .iter()
                    .map(|c| json!({
                        "caller": c.caller,
                        "endpoint": c.endpoint,
                        "served_here": c.served_here,
                        "rel_path": c.rel_path,
                        "line": c.line,
                    }))
                    .collect::<Vec<_>>(),
                "env_declared": env
                    .declared
                    .iter()
                    .map(|v| json!({
                        "name": v.name,
                        "definitions": v.defined_in.len(),
                        "read_sites": v.read_by.len(),
                        "never_read": v.unread(),
                    }))
                    .collect::<Vec<_>>(),
                "env_undeclared": env
                    .undeclared
                    .iter()
                    .map(|v| json!({"name": v.name}))
                    .collect::<Vec<_>>(),
            });
            stamped(payload, &store, root)
        }
        "kg_risk" => {
            let workspace = workspace_arg(args)?;
            let depth = arg_usize(args, "depth", memnoth_codegraph::RISK_DEFAULT_DEPTH);
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            let report = memnoth_codegraph::untrusted_reach(&store, depth)?;
            stamped(report, &store, root)
        }
        "kg_why" => {
            let workspace = workspace_arg(args)?;
            let from = arg_str(args, "from")?;
            let to = arg_str(args, "to")?;
            let direction_arg = args
                .get("direction")
                .and_then(|v| v.as_str())
                .unwrap_or("down");
            let direction =
                memnoth_codegraph::Direction::parse(direction_arg).ok_or_else(|| {
                    anyhow::anyhow!("unknown direction {direction_arg:?} — use down, up or both")
                })?;
            let depth = arg_usize(
                args,
                "depth",
                memnoth_codegraph::DEFAULT_PATH_DEPTH as usize,
            ) as u32;
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            let result = memnoth_codegraph::path_between(
                &store,
                &from,
                &to,
                direction,
                depth,
                memnoth_codegraph::DEFAULT_PATH_BUDGET,
            )?;
            stamped(result, &store, root)
        }
        "kg_doctor" => {
            let workspace = workspace_arg(args)?;
            let root = std::path::Path::new(&workspace);
            let store = open_store(&root.join(".memnoth/kg.db"))?;
            let ledger_rows = memnoth_codegraph::ledger(&store)?;
            // `0` = sweep every indexed file. A health check that samples is not
            // a health check, so this is deliberately the one place that pays
            // for a full staleness pass.
            let staleness = memnoth_codegraph::staleness_report(&store, root, 0)?;
            let resolution = memnoth_codegraph::resolution_health(&store)?;
            let symbols = store.symbol_count()?;
            let usable = memnoth_codegraph::vec_count(&store)?;
            let foreign = memnoth_codegraph::foreign_vec_models(&store)?;
            let foreign_total: i64 = foreign.iter().map(|(_, n)| n).sum();
            let rate = if symbols > 0 {
                resolution.missed as f64 * 100.0 / symbols as f64
            } else {
                0.0
            };
            let mut findings: Vec<String> = Vec::new();
            if staleness.stale_files > 0 {
                findings.push(format!(
                    "{} file(s) changed since the last index — run kg_index with incremental semantics (a full kg_index call heals them)",
                    staleness.stale_files
                ));
            }
            if staleness.parse_failures > 0 {
                findings.push(format!(
                    "{} file(s) failed to parse — their symbols are missing from the graph",
                    staleness.parse_failures
                ));
            }
            if resolution.missed > 0 {
                findings.push(format!(
                    "{} genuinely unresolved call site(s) ({rate:.1} per 100 symbols) — use kg_unresolved with status=missed",
                    resolution.missed
                ));
            }
            if resolution.ambiguous > 0 {
                findings.push(format!(
                    "{} call site(s) have multiple candidates — usually a missing or wrong import",
                    resolution.ambiguous
                ));
            }
            if foreign_total > 0 {
                findings.push(format!(
                    "{foreign_total} vector(s) were written by another model and cannot be scored by this build"
                ));
            }
            if !memnoth_codegraph::SEMANTIC_COMPILED {
                findings.push(
                    "this build has no embedding runtime, so semantic search is unavailable (lexical only)"
                        .to_string(),
                );
            } else if usable == 0 && symbols > 0 {
                findings.push(
                    "no vectors are stored — semantic search is off until kg_index runs"
                        .to_string(),
                );
            }
            if staleness.oversized_files > 0 {
                findings.push(format!(
                    "{} file(s) are over the size cap and were never read",
                    staleness.oversized_files
                ));
            }
            if findings.is_empty() {
                findings.push("no problems found — the graph is complete and current".to_string());
            }
            let payload = json!({
                "generation": store.generation()?,
                "symbols": symbols,
                "live_edges": store.live_edge_count()?,
                "ledger": ledger_rows,
                "staleness": staleness,
                "resolution": resolution,
                "unresolved_count": resolution.unresolved_total,
                "missed_count": resolution.missed,
                "unresolved_per_100_symbols": rate,
                "semantic": {
                    "compiled": memnoth_codegraph::SEMANTIC_COMPILED,
                    "model": memnoth_codegraph::LOCAL_MODEL_ID,
                    "usable_vectors": usable,
                    "foreign_vectors": foreign_total,
                },
                "findings": findings,
            });
            stamped(payload, &store, root)
        }
        other => anyhow::bail!("unknown tool: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn an_explicit_workspace_wins() {
        let args = json!({"workspace": " /repo "});
        assert_eq!(
            workspace_arg(&args).unwrap(),
            Path::new("/repo").display().to_string()
        );
    }

    #[test]
    fn the_vaults_own_location_fills_in_a_missing_workspace() {
        let filled = with_default_workspace(
            "kg_impact",
            &json!({"symbol": "parse"}),
            Some(Path::new("/repo/.memnoth/vault.db")),
        );
        assert_eq!(
            filled["workspace"],
            Path::new("/repo").display().to_string(),
            "the vault names the checkout it belongs to"
        );
        assert_eq!(filled["symbol"], "parse", "other arguments are untouched");
    }

    #[test]
    fn a_caller_that_named_a_workspace_is_left_alone() {
        let filled = with_default_workspace(
            "kg_status",
            &json!({"workspace": "/other"}),
            Some(Path::new("/repo/.memnoth/vault.db")),
        );
        assert_eq!(filled["workspace"], "/other");
    }

    #[test]
    fn tools_that_do_not_read_the_graph_get_no_workspace() {
        let filled = with_default_workspace(
            "mem_search",
            &json!({"query": "x"}),
            Some(Path::new("/repo/.memnoth/vault.db")),
        );
        assert!(filled.get("workspace").is_none(), "{filled}");
    }
}
