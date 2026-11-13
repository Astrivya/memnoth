//! `memnoth lineage` — trace an entity's provenance: source → belief → decision → code.
//!
//! Every edge carries the date it became valid and, where the hash-chained
//! audit trail records it, the person who attested it. Reads the vault graph,
//! the audit log and the change receipts.

use anyhow::Result;
use memnoth_engine::{AuditEvent, MemoryEngine, Relation, now_ms};
use rusqlite::params;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet, VecDeque};

use crate::audit_cmd::read_events;
use crate::context::AppContext;
use crate::output::{ErrorOutcome, ExitCode, Outcome, rfc3339};

/// Default depth when `--depth` is omitted.
const DEFAULT_DEPTH: usize = 3;

/// Node cap before traversal is truncated.
const NODE_CAP: usize = 500;

/// Run `memnoth lineage <entity> [--depth N]`.
pub fn run(
    ctx: &AppContext,
    entity: &str,
    depth: usize,
    at: Option<i64>,
    _json: bool,
) -> Result<Outcome, ErrorOutcome> {
    // `--depth 0` means "use the documented default", not "no ancestors".
    let depth = if depth == 0 { DEFAULT_DEPTH } else { depth };
    let at = at.unwrap_or_else(now_ms);
    let engine = open_vault(ctx, "lineage")?;
    let start = find_entity(&engine, entity, at, "lineage")?;

    let audit_path = ctx.audit_path();
    let audit_events = read_events(&audit_path).unwrap_or_default();

    let mut visited: HashSet<i64> = HashSet::new();
    let mut nodes: BTreeMap<i64, String> = BTreeMap::new();
    let mut edges: Vec<Value> = Vec::new();
    let mut queue: VecDeque<(i64, usize)> = VecDeque::new();
    let mut truncated = false;

    nodes.insert(start.id, start.name.clone());
    visited.insert(start.id);
    queue.push_back((start.id, 0));

    while let Some((entity_id, level)) = queue.pop_front() {
        if level >= depth {
            if !queue.is_empty() {
                truncated = true;
            }
            continue;
        }

        let relations = all_relations(&engine, entity_id, at)?;

        for rel in relations {
            let source_id = rel.source_id;
            let target_id = rel.target_id;
            let kind = rel.kind.clone();
            let confidence = rel.confidence;
            let valid_from = rel.valid_from;

            let source_name =
                entity_name(&engine, source_id).unwrap_or_else(|| format!("#{source_id}"));
            let target_name =
                entity_name(&engine, target_id).unwrap_or_else(|| format!("#{target_id}"));

            nodes
                .entry(source_id)
                .or_insert_with(|| source_name.clone());
            nodes
                .entry(target_id)
                .or_insert_with(|| target_name.clone());

            let edge_date = rfc3339(valid_from);
            let edge_by = attribution_for(&audit_events, &source_name, &target_name, &kind);

            edges.push(json!({
                "source": source_name,
                "source_id": source_id,
                "target": target_name,
                "target_id": target_id,
                "kind": kind,
                "confidence": confidence,
                "at": edge_date,
                "by": edge_by,
            }));

            if visited.insert(target_id) {
                queue.push_back((target_id, level + 1));
            }
        }

        if visited.len() > NODE_CAP {
            truncated = true;
            break;
        }
    }

    let mut text = format!("lineage from {} (depth {depth})\n", start.name);
    if edges.is_empty() {
        text.push_str("  no relations in this window\n");
    } else {
        for edge in &edges {
            text.push_str(&format!(
                "  {} --{}--> {}   @{} by {}\n",
                edge["source"].as_str().unwrap_or(""),
                edge["kind"].as_str().unwrap_or(""),
                edge["target"].as_str().unwrap_or(""),
                edge["at"].as_str().unwrap_or(""),
                edge["by"].as_str().unwrap_or("unknown"),
            ));
        }
        if truncated {
            text.push_str(&format!(
                "  ... truncated at {} nodes; raise --depth or narrow the entity\n",
                visited.len()
            ));
        }
    }

    let data = json!({
        "root": start.name,
        "root_id": start.id,
        "depth": depth,
        "at": at,
        "at_iso": rfc3339(at),
        "nodes": nodes.iter().map(|(id, name)| json!({ "id": id, "name": name })).collect::<Vec<_>>(),
        "edges": edges,
        "node_count": nodes.len(),
        "edge_count": edges.len(),
        "truncated": truncated,
    });

    Ok(Outcome::new("lineage", text.trim_end(), data))
}

/// Every relation touching `entity_id` believed at `as_of`, in both directions.
fn all_relations(
    engine: &MemoryEngine,
    entity_id: i64,
    as_of: i64,
) -> Result<Vec<Relation>, ErrorOutcome> {
    let mut out = engine
        .outgoing_relations(entity_id, as_of)
        .map_err(|e| vault_failure("lineage", "failed to read relations", e))?;
    let mut incoming = incoming_relations(engine, entity_id, as_of)?;
    out.append(&mut incoming);
    Ok(out)
}

/// Relations pointing at `entity_id`, read straight from SQLite so we do not
/// need a second public engine method.
fn incoming_relations(
    engine: &MemoryEngine,
    entity_id: i64,
    as_of: i64,
) -> Result<Vec<Relation>, ErrorOutcome> {
    let conn = engine.connection();
    let mut stmt = conn
        .prepare(
            "SELECT id, source_id, target_id, kind, confidence, valid_from, valid_to, revoked_at, metadata
             FROM relations
             WHERE target_id = ?1 AND revoked_at IS NULL AND valid_from <= ?2
               AND (valid_to IS NULL OR valid_to > ?2)
             ORDER BY kind, source_id",
        )
        .map_err(|e| vault_failure("lineage", "failed to prepare relation query", e))?;
    let rows = stmt
        .query_map(params![entity_id, as_of], |row| {
            Ok(Relation {
                id: row.get(0)?,
                source_id: row.get(1)?,
                target_id: row.get(2)?,
                kind: row.get(3)?,
                confidence: row.get(4)?,
                valid_from: row.get(5)?,
                valid_to: row.get(6)?,
                revoked_at: row.get(7)?,
                metadata: row.get(8)?,
            })
        })
        .map_err(|e| vault_failure("lineage", "failed to read relations", e))?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row.map_err(|e| vault_failure("lineage", "failed to read relations", e))?);
    }
    Ok(edges)
}

/// Find the current entity named `name`, as of `as_of`.
fn find_entity(
    engine: &MemoryEngine,
    name: &str,
    as_of: i64,
    action: &'static str,
) -> Result<memnoth_engine::Entity, ErrorOutcome> {
    let found = engine
        .find_entities_by_name(name, as_of)
        .map_err(|e| vault_failure(action, "failed to search for entity", e))?;
    found.into_iter().next().ok_or_else(|| {
        ErrorOutcome::not_found(format!("no current entity named {name:?}"))
            .with_fix(format!(
                "use `memnoth vault recall {name}` to find the right name"
            ))
            .with_docs_url(crate::context::docs_url(&format!("vault/{action}")))
    })
}

/// The name of an entity, read from SQLite.
fn entity_name(engine: &MemoryEngine, entity_id: i64) -> Option<String> {
    engine
        .connection()
        .query_row(
            "SELECT name FROM entities WHERE id = ?1",
            params![entity_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
}

/// Best guess at who attested an edge, from the audit trail.
///
/// Looks for the most recent audit event whose `target` names either endpoint
/// of the edge, and reports its `detail` field as the attribution.
fn attribution_for(
    events: &[AuditEvent],
    source: &str,
    target: &str,
    _kind: &str,
) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|event| {
            let target_text = event.target.as_str();
            let detail = event.detail.as_str().unwrap_or_default();
            target_text.contains(source)
                || target_text.contains(target)
                || detail.contains(source)
                || detail.contains(target)
        })
        .and_then(|event| event.detail.as_str().map(str::to_string))
}

/// Open the vault, or return a usable error.
fn open_vault(ctx: &AppContext, action: &'static str) -> Result<MemoryEngine, ErrorOutcome> {
    ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url(crate::context::docs_url(&format!("vault/{action}")))
    })
}

/// Whether this build renders JSON. Kept for signature parity with other TUI
/// commands; the text path is always produced.
#[allow(dead_code)]
fn _json_flag(_json: bool) {}

/// One vault read failed: the same shape every command's error path uses.
fn vault_failure(action: &'static str, what: &str, error: impl std::fmt::Display) -> ErrorOutcome {
    ErrorOutcome::new(ExitCode::Unexpected, what.to_string())
        .with_cause(error.to_string())
        .with_docs_url(crate::context::docs_url(&format!("vault/{action}")))
}
