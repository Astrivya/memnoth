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

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use memnoth_codegraph::{DEFAULT_BUDGET, blast_radius, blast_radius_page, explore, open_store};
use serde_json::{Value, json};

use crate::{
    arg_str, arg_usize, cross_encoder, freshness_of, load_communities, referenced_staleness_notice,
    rerank_cross_encoder_symbols, stamped, workspace_root_for_vault,
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

/// An answer for a question this graph has no data to support.
///
/// The alternative — an empty list — reads as "there are none", and a model
/// will act on that. The two fields that matter are `supported: false` and the
/// reason, because a caller can then stop asking instead of concluding.
fn unsupported(tool: &str, reason: &str, instead: Option<&str>) -> Value {
    let mut answer = json!({
        "tool": tool,
        "supported": false,
        "reason": reason,
    });
    if let Some(instead) = instead {
        answer["instead"] = json!(instead);
    }
    answer
}

/// The server's own graph, opened only when this workspace actually has one.
///
/// `Store::open` creates the file it is given, so opening it unconditionally
/// turned "this workspace was never indexed" into an empty graph that answers
/// every question with "nothing" — the most misleading answer a tool can give.
fn legacy_store(root: &Path) -> Result<memnoth_codegraph::Store> {
    let path = root.join(".memnoth/kg.db");
    if !path.is_file() {
        anyhow::bail!(
            "this workspace has no code graph — run `memnoth kg index` (or call kg_index), then ask again"
        );
    }
    open_store(&path)
}

/// What a symbol mentions inside its own body, from the CLI's graph.
///
/// This is the one question on this surface that needs `refs.container` — the
/// innermost declaration a mention sits in — so it is a query rather than a
/// projection of another tool's answer. Duplicated from `kg callees` on
/// purpose, with the same rule, because an agent and a person asking the same
/// checkout must get the same rows; `CliGraph::dead` argues the same case.
///
/// Mention-shaped, not resolved: a mention in a comment inside the body is
/// counted, and a same-named local in another body is not. The answer says so
/// rather than leaving the model to assume call edges.
fn callees_from_cli_graph(
    root: &Path,
    symbol: &str,
    limit: usize,
    indexed: Value,
) -> Result<Value> {
    use rusqlite::{Connection, OpenFlags, params_from_iter};

    let path = root.join(".memnoth").join("graph.db");
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    let has_container = conn
        .prepare("PRAGMA table_info(refs)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .flatten()
        .any(|column| column == "container");
    if !has_container {
        return Ok(unsupported(
            "kg_callees",
            "this graph was built before refs recorded the declaration around a mention, so it cannot say what a symbol calls",
            Some("run kg_index to rebuild it, or use kg_impact for who mentions the symbol"),
        ));
    }

    // Exact name first, then a qualified spelling's last segment — the same
    // fallback every other lookup makes, so all of them resolve to one subject.
    let mut names: Vec<String> = Vec::new();
    {
        let mut statement = conn.prepare("SELECT DISTINCT name FROM symbols WHERE name = ?1")?;
        names.extend(
            statement
                .query_map([symbol], |row| row.get::<_, String>(0))?
                .flatten(),
        );
    }
    if names.is_empty() {
        let segment = symbol
            .rsplit("::")
            .next()
            .unwrap_or(symbol)
            .rsplit('.')
            .next()
            .unwrap_or(symbol);
        let mut statement = conn.prepare(
            "SELECT DISTINCT name FROM symbols
              WHERE name = ?1 OR name LIKE '%::' || ?1 OR name LIKE '%.' || ?1",
        )?;
        names.extend(
            statement
                .query_map([segment], |row| row.get::<_, String>(0))?
                .flatten(),
        );
    }
    if names.is_empty() {
        return Ok(json!({
            "graph": "memnoth kg",
            "symbol": symbol,
            "resolved": false,
            "callees": [],
            "truncated": false,
            "note": "no declaration by that name; `kg_search` finds the spelling",
            "indexed": indexed,
        }));
    }

    let limit = limit.max(1);
    let placeholders = vec!["?"; names.len()].join(",");
    let sql = format!(
        "SELECT name, path, line FROM refs WHERE container IN ({placeholders})
         ORDER BY path, line LIMIT {}",
        limit.saturating_add(1)
    );
    let rows: Vec<(String, String, i64)> = {
        let mut statement = conn.prepare(&sql)?;
        statement
            .query_map(params_from_iter(names.iter()), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .flatten()
            .collect()
    };
    let truncated = rows.len() > limit;
    let rows = &rows[..rows.len().min(limit)];

    // One read per file, as every other row-carrying tool does: the context
    // line is what makes a list of names judgeable without opening an editor.
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    let mut callees: Vec<Value> = Vec::new();
    for (name, path, line) in rows {
        let lines = cache.entry(path.clone()).or_insert_with(|| {
            std::fs::read_to_string(root.join(path))
                .map(|text| text.lines().map(|line| line.trim().to_string()).collect())
                .unwrap_or_default()
        });
        let context = lines
            .get(line.saturating_sub(1) as usize)
            .cloned()
            .unwrap_or_default();
        callees.push(json!({
            "name": name,
            "path": path,
            "line": line,
            "context": context,
            "kind": "callee",
            "call_shaped": context.contains(&format!("{name}(")),
        }));
    }

    Ok(json!({
        "graph": "memnoth kg",
        "symbol": symbol,
        "resolved": true,
        "callees": callees,
        "truncated": truncated,
        "note": "names mentioned inside this symbol's body; `call_shaped` is the line's shape, not a resolved call edge",
        "indexed": indexed,
    }))
}

/// Answer one `kg_*` tool call.
///
/// The match is exhaustive over the names `tools_call` routes here and bails on
/// anything else, so a name that is added to the catalogue without a body fails
/// loudly at the first call rather than silently falling through.
/// Every KG answer carries `supported`.
///
/// `kg_tools` answers in three shapes — the cli-graph object, the legacy store's
/// object, and [`unsupported`] — and a host could not tell "this graph cannot
/// answer" from "the answer is empty" without reading prose. One field at the one
/// place every answer passes makes that a boolean, and lets all eleven primary
/// tools declare the same envelope in their `outputSchema`.
pub(crate) fn call(name: &str, args: &Value) -> Result<Value> {
    dispatch(name, args).map(with_support)
}

/// Stamp `supported: true` on a successful answer. `unsupported` already answers
/// with `false`, and `or_insert` is what keeps it from being overwritten here.
fn with_support(mut answer: Value) -> Value {
    if let Some(object) = answer.as_object_mut() {
        object.entry("supported").or_insert(json!(true));
    }
    answer
}

fn dispatch(name: &str, args: &Value) -> Result<Value> {
    match name {
        "kg_index" => crate::kg_index::run_kg_index(args),
        "kg_impact" => {
            let workspace = workspace_arg(args)?;
            let symbol = arg_str(args, "symbol")?;
            let depth = arg_usize(args, "depth", 1) as u32;
            let include_tests = args
                .get("include_tests")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let root = std::path::Path::new(&workspace);
            // One graph per workspace: `memnoth kg index` writes
            // `.memnoth/graph.db`, and this is where the agent reads the same
            // rows the person sees.
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                let impact = graph.impact(&symbol, depth)?;
                let impacted = impact["references"].as_array().cloned().unwrap_or_default();
                let direct = impacted.len();
                return Ok(json!({
                    "target": symbol,
                    "precision": memnoth_codegraph::best_available().label(),
                    "counts": {"exact": 0, "likely": 0, "direct": direct, "transitive": 0},
                    "impacted": impacted,
                }));
            }
            match legacy_store(root) {
                Ok(store) => {
                    let imp = blast_radius(
                        &store,
                        &symbol,
                        depth as usize,
                        DEFAULT_BUDGET,
                        include_tests,
                    )?;
                    let impacted: Vec<Value> = imp
                        .direct
                        .iter()
                        .map(|caller| {
                            json!({
                                "name": caller.qname,
                                "file": caller.file,
                                "line": caller.line,
                                "kind": caller.kind,
                            })
                        })
                        .collect();
                    let direct = impacted.len();
                    let transitive = imp.transitive_symbols.len();
                    Ok(json!({
                        "target": symbol,
                        "precision": memnoth_codegraph::Precision::parse(&imp.precision)
                            .unwrap_or(memnoth_codegraph::Precision::Lexical)
                            .label(),
                        "counts": {"exact": 0, "likely": 0, "direct": direct, "transitive": transitive},
                        "impacted": impacted,
                    }))
                }
                // No graph, no blast radius — but this tool declares a typed
                // outputSchema, and an error envelope is not an answer a host
                // validating against that schema can read. The empty report is
                // the honest one: nothing is indexed, so nothing is impacted.
                Err(_) => Ok(json!({
                    "target": symbol,
                    "precision": "lexical",
                    "counts": {"exact": 0, "likely": 0, "direct": 0, "transitive": 0},
                    "impacted": [],
                })),
            }
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
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                return graph.trace_page(&symbol, page_size, cursor.as_deref());
            }
            let store = legacy_store(root)?;
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
        "kg_callees" => {
            let workspace = workspace_arg(args)?;
            let symbol = arg_str(args, "symbol")?;
            let limit = arg_usize(args, "limit", 50);
            let root = std::path::Path::new(&workspace);
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                // The same resolution `kg_impact` performs, so an unknown name is
                // reported unresolved instead of as "this function calls nothing".
                let subject = graph.impact(&symbol, 1)?;
                if subject["resolved"] != json!(true) {
                    return Ok(json!({
                        "graph": "memnoth kg",
                        "symbol": symbol,
                        "resolved": false,
                        "callees": [],
                        "truncated": false,
                        "note": "no declaration by that name; `kg_search` finds the spelling",
                        "indexed": subject["indexed"].clone(),
                    }));
                }
                return callees_from_cli_graph(root, &symbol, limit, subject["indexed"].clone());
            }
            // The bundled graph records mentions, not the declaration a mention
            // sits in, so it cannot answer this one at all.
            Ok(unsupported(
                "kg_callees",
                "the bundled graph does not record which declaration a mention sits in, so it cannot say what a symbol calls",
                Some(
                    "kg_impact on the symbol lists the places it is mentioned; kg_explore names its definition",
                ),
            ))
        }
        "kg_def" => {
            let workspace = workspace_arg(args)?;
            let symbol = arg_str(args, "symbol")?;
            let root = std::path::Path::new(&workspace);
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                let explored = graph.explore(&symbol)?;
                let definitions: Vec<Value> = explored["definitions"]
                    .as_array()
                    .and_then(|definitions| definitions.first().cloned().map(|first| vec![first]))
                    .unwrap_or_default();
                let resolved = !definitions.is_empty();
                return Ok(json!({
                    "symbol": symbol,
                    "resolved": resolved,
                    "precision": memnoth_codegraph::best_available().label(),
                    "definitions": definitions,
                }));
            }
            match legacy_store(root) {
                Ok(store) => {
                    let explored = explore(&store, root, &symbol)?;
                    let info = &explored.symbol;
                    let resolved = !info.file.is_empty();
                    let definitions = if resolved {
                        vec![json!({
                            "name": info.qname,
                            "kind": info.kind,
                            "file": info.file,
                            "line": info.line,
                            "signature": info.signature,
                            "doc_comment": info.doc_comment,
                        })]
                    } else {
                        Vec::new()
                    };
                    Ok(json!({
                        "symbol": symbol,
                        "resolved": resolved,
                        "precision": explored.precision.label(),
                        "definitions": definitions,
                    }))
                }
                // Unindexed workspace: the typed answer, empty. See `kg_impact`.
                Err(_) => Ok(json!({
                    "symbol": symbol,
                    "resolved": false,
                    "precision": "lexical",
                    "definitions": [],
                })),
            }
        }
        "kg_refs" => {
            let workspace = workspace_arg(args)?;
            let symbol = arg_str(args, "symbol")?;
            let limit = arg_usize(args, "limit", 100).max(1);
            let root = std::path::Path::new(&workspace);
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                let impact = graph.impact(&symbol, 1)?;
                let mut references = impact["references"].as_array().cloned().unwrap_or_default();
                let listed = references.len();
                references.truncate(limit);
                return Ok(json!({
                    "symbol": symbol,
                    "resolved": impact["resolved"].clone(),
                    "precision": memnoth_codegraph::best_available().label(),
                    "references": references,
                    "references_direct": listed,
                }));
            }
            match legacy_store(root) {
                Ok(store) => {
                    let mut stmt = store
                        .conn()
                        .prepare("SELECT 1 FROM symbols WHERE qname = ? LIMIT 1")?;
                    let mut cursor = stmt.query(rusqlite::params![symbol])?;
                    let resolved = cursor.next()?.is_some();
                    drop(cursor);
                    drop(stmt);
                    let imp = blast_radius(&store, &symbol, 1, DEFAULT_BUDGET, false)?;
                    let references: Vec<Value> = imp
                        .direct
                        .iter()
                        .map(|caller| {
                            json!({
                                "name": caller.qname,
                                "file": caller.file,
                                "line": caller.line,
                                "kind": caller.kind,
                            })
                        })
                        .collect();
                    let references_direct = references.len();
                    Ok(json!({
                        "symbol": symbol,
                        "resolved": resolved,
                        "precision": memnoth_codegraph::Precision::parse(&imp.precision)
                            .unwrap_or(memnoth_codegraph::Precision::Lexical)
                            .label(),
                        "references": references,
                        "references_direct": references_direct,
                    }))
                }
                // Unindexed workspace: see `kg_impact`.
                Err(_) => Ok(json!({
                    "symbol": symbol,
                    "resolved": false,
                    "precision": "lexical",
                    "references": [],
                    "references_direct": 0,
                })),
            }
        }
        "kg_status" => {
            let workspace = workspace_arg(args)?;
            let max_checks = arg_usize(args, "max_checks", 64);
            let root = std::path::Path::new(&workspace);
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                let status = graph.status();
                return Ok(json!({
                    "workspace": workspace,
                    "files_indexed": status["files"].clone(),
                    "symbols": status["symbols"].clone(),
                    "references": status["references"].clone(),
                    "stale_files": status["stale"]
                        .as_array()
                        .map(|rows| rows.len())
                        .unwrap_or(0),
                    "precision": memnoth_codegraph::best_available().label(),
                }));
            }
            match legacy_store(root) {
                Ok(store) => {
                    // The same accessor the `mem://index/status` resource uses,
                    // so the tool and the resource cannot disagree about stale
                    // files. The tool keeps its own `max_checks` because a
                    // caller asking for a full verdict is entitled to a wider
                    // sweep than a cache-hint question.
                    let freshness = freshness_of(&store, root, max_checks).ok_or_else(|| {
                        anyhow::anyhow!(
                            "no code graph at {} — run kg_index for this workspace first",
                            root.join(".memnoth/kg.db").display()
                        )
                    })?;
                    fn table_count(store: &memnoth_codegraph::Store, table: &str) -> i64 {
                        let sql = format!("SELECT COUNT(*) FROM {table}");
                        store
                            .conn()
                            .query_row(&sql, [], |row| row.get::<_, i64>(0))
                            .unwrap_or(0)
                    }
                    Ok(json!({
                        "workspace": workspace,
                        "files_indexed": table_count(&store, "files"),
                        "symbols": table_count(&store, "symbols"),
                        "references": table_count(&store, "relations"),
                        "stale_files": freshness.report.stale_files,
                        "precision": memnoth_codegraph::best_available().label(),
                    }))
                }
                // Unindexed workspace: see `kg_impact`. Zeros are the truth —
                // there is no graph, so zero files are indexed.
                Err(_) => Ok(json!({
                    "workspace": workspace,
                    "files_indexed": 0,
                    "symbols": 0,
                    "references": 0,
                    "stale_files": 0,
                    "precision": "lexical",
                })),
            }
        }
        "kg_explore" => {
            let workspace = workspace_arg(args)?;
            let query = arg_str(args, "query")?;
            let root = std::path::Path::new(&workspace);
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                return graph.explore(&query);
            }
            let store = legacy_store(root)?;
            let resp = explore(&store, root, &query)?;
            let mut value = serde_json::to_value(&resp)?;
            // One spelling of `symbol` across both graphs: the name that was
            // asked about. The legacy response put the whole record there, so it
            // moves beside it rather than being dropped.
            if let Some(record) = value.get("symbol").cloned()
                && record.is_object()
            {
                value["symbol"] = json!(query);
                value["symbol_info"] = record;
            }
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
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                let found = graph.search(&query, limit)?;
                return Ok(json!({
                    "query": query,
                    "hits": found["hits"].clone(),
                    "precision": memnoth_codegraph::best_available().label(),
                }));
            }
            let store = match legacy_store(root) {
                Ok(store) => store,
                // Unindexed workspace: see `kg_impact`.
                Err(_) => {
                    return Ok(json!({
                        "query": query,
                        "hits": [],
                        "precision": "lexical",
                    }));
                }
            };

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
            // `kg_search` declares a typed outputSchema; the ranked hits are
            // the answer, and the extra diagnostics above would make a strict
            // host reject it. Project to exactly {query, hits, precision}.
            let _ = (store, root);
            Ok(json!({
                "query": query,
                "hits": value["hits"].clone(),
                "precision": memnoth_codegraph::best_available().label(),
            }))
        }
        "kg_coverage" => {
            let workspace = workspace_arg(args)?;
            let max_checks = arg_usize(args, "max_checks", 64);
            let root = std::path::Path::new(&workspace);
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                return Ok(graph.coverage());
            }
            let store = legacy_store(root)?;
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
            // Communities are components of live CALLS edges. The CLI graph has
            // no edges — it records where a name appears, not who calls whom —
            // so a partition of it would be a partition of something else
            // wearing this report's name. `kg_index` builds the CLI graph
            // whenever it can, which makes this the common path, so the refusal
            // has to be useful rather than an error a caller has to decode.
            if crate::cli_graph::CliGraph::open(root).is_some() {
                return Ok(unsupported(
                    "kg_communities",
                    "communities are clusters of call edges; this graph records name mentions, so any partition it produced would be a different metric under this report's name",
                    Some(
                        "kg_hotspots ranks files by churn and declaration count, and kg_coverage reports what the graph actually holds",
                    ),
                ));
            }
            let store = legacy_store(root)?;
            let report = load_communities(&store, limit)?;
            stamped(report, &store, root)
        }
        "kg_dead" => {
            let workspace = workspace_arg(args)?;
            let min_depth = arg_usize(args, "min_loop_depth", 0) as i64;
            let root = std::path::Path::new(&workspace);
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                return graph.dead(arg_usize(args, "limit", 200));
            }
            let store = legacy_store(root)?;
            // Public API excluded, as the CLI's default is: a `pub` function may
            // be called from a crate this graph never read.
            let dead = memnoth_codegraph::dead_symbols(&store, min_depth as usize, true)?;
            stamped(json!({ "dead_symbols": dead }), &store, root)
        }
        "kg_unresolved" => {
            let workspace = workspace_arg(args)?;
            let status = args.get("status").and_then(|v| v.as_str());
            let root = std::path::Path::new(&workspace);
            if crate::cli_graph::CliGraph::open(root).is_some() {
                return Ok(unsupported(
                    "kg_unresolved",
                    "the graph records where a name appears, not what it binds to, so a name it never resolved is indistinguishable from one it never saw",
                    Some(
                        "kg_impact lists every mention with its source line, which is what a resolution answer was standing in for",
                    ),
                ));
            }
            let store = legacy_store(root)?;
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
            // The CLI answers this question as `kg hotspots`: churn times size.
            // Ranking the same checkout differently here would make the tool
            // and the command disagree about which file is riskiest.
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                return graph.hotspots(limit);
            }
            let store = legacy_store(root)?;
            let rows = memnoth_codegraph::complexity_report(&store, limit)?;
            stamped(json!({ "complexity": rows }), &store, root)
        }
        "kg_changes" => {
            let workspace = workspace_arg(args)?;
            let base = args.get("base").and_then(|v| v.as_str()).unwrap_or("HEAD");
            let depth = arg_usize(args, "depth", 1) as u32;
            let root = std::path::Path::new(&workspace);

            // changed files via git
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

            let paths: Vec<String> = files.iter().map(|s| s.to_string()).collect();
            // One graph: the declarations in the files git just named come from
            // the same rows `memnoth kg diff-impact` reads.
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                let symbols = graph.symbols_in(&paths)?;
                let mut impacts = Vec::new();
                for symbol in symbols.iter().take(30) {
                    let name = symbol["name"].as_str().unwrap_or_default();
                    if let Ok(impact) = graph.impact(name, depth) {
                        impacts.push(json!({
                            "symbol": name,
                            "file": symbol["path"],
                            "direct_callers": impact["references_direct"],
                            "references": impact["references"].as_array().map_or(0, Vec::len),
                            "files_touched": impact["files"],
                        }));
                    }
                }
                return Ok(json!({
                    "graph": "memnoth kg",
                    "changed_files": files.len(),
                    "affected_symbols": symbols.len(),
                    "impact": impacts,
                    "guidance": "each impact row is how many files mention that declaration — the reachable surface of your uncommitted work",
                    "indexed": graph.status()["indexed"],
                }));
            }

            let store = legacy_store(root)?;
            let symbols = memnoth_codegraph::symbols_in_files(&store, &paths)?;
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
            // No layer reads compose files or route tables, and the functions
            // behind this tool return constants rather than findings. Every
            // list here would be empty for a reason that has nothing to do with
            // the repository, so say that instead of returning four empty
            // arrays that read as "this repo has no services".
            Ok(unsupported(
                "kg_services",
                "this build does not map services: no layer reads compose files, route declarations or cross-service calls",
                Some("kg_search finds a service's entry points by name"),
            ))
        }
        "kg_risk" => Ok(unsupported(
            "kg_risk",
            "taint reach needs resolved call edges; this index holds textual mentions, which would over-count every same-named symbol",
            Some("kg_impact with depth=2 follows mentions two hops out"),
        )),
        "kg_why" => {
            let workspace = workspace_arg(args)?;
            let from = arg_str(args, "from")?;
            let to = arg_str(args, "to")?;
            let root = std::path::Path::new(&workspace);
            if crate::cli_graph::CliGraph::open(root).is_some() {
                return Ok(unsupported(
                    "kg_why",
                    "a path between two symbols needs directed call edges; this graph knows which files mention which names, not who calls whom",
                    Some(
                        "kg_impact on each end reports the files they share, which is the evidence a call path would be built from",
                    ),
                ));
            }
            let direction_arg = args
                .get("direction")
                .and_then(|v| v.as_str())
                .unwrap_or("down");
            let direction =
                memnoth_codegraph::Direction::parse(direction_arg).ok_or_else(|| {
                    anyhow::anyhow!("unknown direction {direction_arg:?} — use down, up or both")
                })?;
            let depth = arg_usize(args, "depth", memnoth_codegraph::DEFAULT_PATH_DEPTH) as u32;
            let store = legacy_store(root)?;
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
            // The health check follows the graph the other tools answer from.
            // Reporting on `.memnoth/kg.db` while `kg_search` reads
            // `.memnoth/graph.db` would diagnose a file the caller never
            // touched.
            if let Some(graph) = crate::cli_graph::CliGraph::open(root) {
                return Ok(graph.doctor());
            }
            let store = legacy_store(root)?;
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
    fn a_tool_with_no_data_behind_it_says_so_instead_of_returning_empties() {
        // "no services found" and "this build never looks for services" are
        // different answers, and only one of them is true.
        for tool in ["kg_services", "kg_risk"] {
            let answer = call(tool, &json!({})).expect("an unsupported tool still answers");
            assert_eq!(answer["supported"], false, "{tool}: {answer}");
            assert_eq!(answer["tool"], tool);
            assert!(
                answer["reason"].as_str().is_some_and(|r| !r.is_empty()),
                "{tool} must say why: {answer}"
            );
            assert!(
                answer["instead"].as_str().is_some(),
                "{tool} must name what does work instead: {answer}"
            );
        }
    }

    #[test]
    fn a_workspace_that_was_never_indexed_is_an_error_not_an_empty_graph() {
        let root =
            std::env::temp_dir().join(format!("memnoth-mcp-no-graph-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let error = call(
            "kg_communities",
            &json!({"workspace": root.display().to_string()}),
        )
        .expect_err("an unindexed workspace must not answer with an empty graph");
        assert!(
            error.to_string().contains("no code graph"),
            "the error says what is missing and what to run: {error}"
        );
    }

    /// A scratch workspace with a graph the CLI wrote and nothing else in it.
    fn workspace_with_a_cli_graph(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("memnoth-mcp-kg-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        crate::cli_graph::write_test_graph(&root);
        root
    }

    #[test]
    fn a_cli_graph_changes_which_questions_can_be_answered_not_just_where_from() {
        let root = workspace_with_a_cli_graph("communities");
        let args = json!({"workspace": root.display().to_string()});

        // There is a graph here. Saying "no code graph" would be false, and
        // computing a partition of mentions and calling it communities would be
        // an answer to a different question.
        let answer = call("kg_communities", &args).expect("a graph exists, so this must answer");
        assert_eq!(answer["supported"], false, "{answer}");
        assert_eq!(answer["tool"], "kg_communities");
        assert!(
            answer["reason"].as_str().unwrap().contains("call edges"),
            "the refusal names what the report would have needed: {answer}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_doctor_checks_the_graph_the_other_tools_read() {
        let root = workspace_with_a_cli_graph("doctor");
        let answer = call(
            "kg_doctor",
            &json!({"workspace": root.display().to_string()}),
        )
        .expect("a graph exists, so the health check must run");

        assert_eq!(answer["graph"], "memnoth kg", "{answer}");
        assert!(
            answer["findings"].as_array().is_some_and(|f| !f.is_empty()),
            "a health check always says something: {answer}"
        );
        assert!(
            answer["not_checked"]
                .as_array()
                .is_some_and(|f| !f.is_empty()),
            "the verdict states what it could not check: {answer}"
        );
        assert_eq!(
            answer["path"],
            root.join(".memnoth").join("graph.db").display().to_string(),
            "it reports on the file the kg_* tools answer from"
        );

        let _ = std::fs::remove_dir_all(&root);
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
