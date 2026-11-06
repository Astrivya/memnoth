//! Temporal history and contradiction queries over the vault journal.
//!
//! Lifted from the MCP tool layer (`memnoth-mcp/src/tools_agent.rs`,
//! `entity_chain` / `vault_history` / `vault_contradictions`) so the engine
//! owns its own history semantics and every surface — the MCP tools, the CLI
//! `VaultCommand::{History, Contradictions}` delegations (CLI lane K3), and
//! any future TUI verb — calls one implementation instead of re-deriving the
//! SQL. JSON shapes are identical to the MCP originals so the tool layer can
//! delegate without remapping.
//!
//! Chain semantics: discovery matches every row that carries `name` (as `name`
//! or `canonical_name`) — including *superseded* rows, which keep the name they
//! had — then follows the lineage links `supersede_entity` stamps into
//! `metadata` (`supersedes` / `superseded_by`), so a rename no longer splits
//! the history: querying **either** the pre-rename or post-rename name resolves
//! the whole lineage in both directions, journals included. Rows reached only
//! through a lineage link (not by name) carry `"via": "lineage"` so callers can
//! tell how each row was found. The walk is a visited-set closure, so link
//! cycles cannot loop it.

use crate::store::MemoryEngine;
use anyhow::{Context, Result};
use rusqlite::params;
use serde_json::{Value, json};

/// The id closure for `name`: every live row carrying the name (directly or as
/// canonical name), plus every row linked to it through the
/// `supersedes`/`superseded_by` metadata stamps, in both directions.
fn lineage_ids(engine: &MemoryEngine, name: &str) -> Result<Vec<i64>> {
    let conn = engine.connection();
    let mut ids: Vec<i64> = Vec::new();

    // Seeds: live-table name matches (a closed generation keeps its name, so
    // this finds every generation that ever carried the name).
    {
        let mut stmt = conn
            .prepare("SELECT id FROM entities WHERE name = ?1 OR canonical_name = ?1")
            .context("prepare lineage seed query")?;
        let rows = stmt.query_map(params![name], |r| r.get::<_, i64>(0))?;
        for id in rows {
            let id = id?;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }

    // Closure: follow supersedes (backward) and superseded_by (forward) from
    // every discovered row. Visited-set walk = the ids vector itself.
    let mut cursor = 0;
    while cursor < ids.len() {
        let id = ids[cursor];
        cursor += 1;
        let meta: Option<String> = conn
            .query_row(
                "SELECT metadata FROM entities WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .ok();
        let Some(meta) = meta else { continue };
        let Ok(parsed) = serde_json::from_str::<Value>(&meta) else {
            continue;
        };
        let Some(obj) = parsed.as_object() else {
            continue;
        };
        for key in ["supersedes", "superseded_by"] {
            if let Some(linked) = obj.get(key).and_then(Value::as_i64)
                && !ids.contains(&linked)
            {
                ids.push(linked);
            }
        }
    }
    Ok(ids)
}

/// Live rows + every journaled snapshot across the lineage, merged and ordered
/// by `valid_from`. Rows found by name carry `"via": "name"`; rows reached only
/// through a lineage link carry `"via": "lineage"`.
fn entity_chain(engine: &MemoryEngine, name: &str) -> Result<Vec<Value>> {
    let conn = engine.connection();
    let ids = lineage_ids(engine, name)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let name_ids: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT id FROM entities WHERE name = ?1 OR canonical_name = ?1")
            .context("prepare live entity query")?;
        let rows = stmt.query_map(params![name], |r| r.get::<_, i64>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let mut chain = Vec::new();
    for id in ids {
        let via = if name_ids.contains(&id) {
            "name"
        } else {
            "lineage"
        };
        let mut stmt = conn
            .prepare(
                "SELECT id, name, canonical_name, valid_from, valid_to \
                 FROM entities WHERE id = ?1",
            )
            .context("prepare live row query")?;
        if let Ok(row) = stmt.query_row(params![id], |r| {
            Ok(json!({
                "entity_id": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "canonical_name": r.get::<_, Option<String>>(2)?,
                "valid_from": r.get::<_, i64>(3)?,
                "valid_to": r.get::<_, Option<i64>>(4)?,
                "via": via,
            }))
        }) {
            chain.push(row);
        }

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
                "via": via,
            }))
        })?;
        for row in rows {
            chain.push(row?);
        }
    }
    chain.sort_by_key(|v| v["valid_from"].as_i64().unwrap_or(0));

    // A live row and the journaled snapshot of the same belief are one version,
    // not two. The snapshot is written when the row is created, so the chain
    // listed every version twice and a two-step history read as four — which
    // undermines the one verb whose whole job is to be the record.
    //
    // They are not duplicates of each other's content, though: the row carries
    // the name, the snapshot carries `change_reason`. So this folds rather than
    // drops, and a field the kept copy lacks is taken from the copy being merged
    // away.
    let mut merged: Vec<Value> = Vec::new();
    for row in chain {
        let entity_id = row["entity_id"].as_i64();
        let valid_from = row["valid_from"].as_i64();
        let valid_to = row["valid_to"].as_i64();
        let existing = merged.iter_mut().find(|kept| {
            kept["entity_id"].as_i64() == entity_id
                && kept["valid_from"].as_i64() == valid_from
                && kept["valid_to"].as_i64() == valid_to
        });
        match existing {
            Some(kept) => {
                for field in ["change_reason", "name", "canonical_name", "snapshot"] {
                    let missing = match kept.get(field) {
                        None | Some(Value::Null) => true,
                        Some(_) => false,
                    };
                    if missing && row.get(field).is_some_and(|value| !value.is_null()) {
                        kept[field] = row[field].clone();
                    }
                }
            }
            None => merged.push(row),
        }
    }
    Ok(merged)
}

/// Every belief the vault holds, superseded ones included, newest first.
///
/// `vault_history` answers "what happened to this named thing"; this answers "what has this vault
/// believed, and what did it stop believing". A timeline needs the second question, because the
/// interesting rows are the ones that are no longer current — they carry the reason the vault
/// changed its mind, which `supersede_entity` journals and nothing else exposes in bulk.
///
/// Supersession is read from the `superseded_by` stamp the supersede atom writes into metadata,
/// and the reason from the newest `entity_versions` row that closed the interval. Both are
/// additions to the row, never rewrites of it: the old belief keeps its own text.
pub fn vault_timeline(engine: &MemoryEngine, limit: usize) -> Result<Value> {
    let conn = engine.connection();
    let limit = limit.clamp(1, 500);
    let mut stmt = conn.prepare(
        "SELECT e.id, e.kind, e.name, e.summary, e.confidence, e.valid_from, e.valid_to,
                e.metadata,
                (SELECT v.change_reason FROM entity_versions v
                  WHERE v.entity_id = e.id ORDER BY v.id DESC LIMIT 1)
           FROM entities e
          ORDER BY e.valid_from DESC, e.id DESC
          LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
        let metadata: Option<String> = row.get(7)?;
        let parsed: Option<serde_json::Value> = metadata
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok());
        let stamp = |key: &str| {
            parsed
                .as_ref()
                .and_then(|value| value.get(key))
                .and_then(serde_json::Value::as_i64)
        };
        Ok(json!({
            "id": row.get::<_, i64>(0)?,
            "kind": row.get::<_, String>(1)?,
            "name": row.get::<_, String>(2)?,
            "summary": row.get::<_, Option<String>>(3)?,
            "confidence": row.get::<_, f64>(4)?,
            "valid_from": row.get::<_, i64>(5)?,
            "valid_to": row.get::<_, Option<i64>>(6)?,
            "supersedes": stamp("supersedes"),
            "superseded_by": stamp("superseded_by"),
            "reason": row.get::<_, Option<String>>(8)?,
        }))
    })?;
    let mut facts = Vec::new();
    for row in rows {
        facts.push(row?);
    }
    let active = facts
        .iter()
        .filter(|fact| fact["valid_to"].is_null())
        .count();
    Ok(json!({
        "facts": facts,
        "count": facts.len(),
        "active": active,
        "superseded": facts.len() - active,
    }))
}

/// Full temporal history of an entity: the live row plus every journaled
/// version, each with its validity window and (where superseded/revoked)
/// the recorded change reason.
pub fn vault_history(engine: &MemoryEngine, name: &str) -> Result<Value> {
    let chain = entity_chain(engine, name)?;
    Ok(json!({
        "entity": name,
        "versions": chain,
        "count": chain.len(),
    }))
}

/// Contradicted beliefs about an entity: every journaled version that closed
/// with a change reason (supersede / revoke), and every revoked outgoing
/// relation. Use before trusting current belief — if the vault changed its
/// mind, the reason is recorded here.
pub fn vault_contradictions(engine: &MemoryEngine, name: &str) -> Result<Value> {
    let chain = entity_chain(engine, name)?;
    // The chain intentionally carries a generation's live row *and* its journal
    // rows, so the same entity_id appears more than once. De-duplicate before
    // querying: otherwise every contradiction is reported once per occurrence
    // (the MCP original double-counts for exactly this reason).
    let mut ids: Vec<i64> = Vec::new();
    for id in chain.iter().filter_map(|v| v["entity_id"].as_i64()) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let conn = engine.connection();
    let mut contradicted = Vec::new();
    for id in ids {
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

/// Vault-wide contradiction audit: every explicitly revoked relation with the
/// reason its revocation recorded — **not** scoped to one entity.
///
/// This is the query the CLI's `vault contradictions` verb needs (K3): the
/// name-scoped [`vault_contradictions`] answers "what contradicts *this*
/// belief", while this answers "what anywhere in the vault has been explicitly
/// contradicted". Same journal, two zoom levels — one SQL site in the engine
/// instead of the CLI re-deriving it.
///
/// The engine's model doc is the contract: "Revoke marks explicit
/// contradiction; revoked rows are excluded from 'now' but visible as-of."
pub fn vault_contradictions_all(engine: &MemoryEngine) -> Result<Value> {
    let conn = engine.connection();
    let mut stmt = conn
        .prepare(
            "SELECT r.id, r.kind, r.revoked_at, r.valid_from, r.valid_to, \
             s.name, t.name, \
             (SELECT change_reason FROM relation_versions v \
               WHERE v.relation_id = r.id ORDER BY v.id DESC LIMIT 1) \
             FROM relations r \
             JOIN entities s ON s.id = r.source_id \
             JOIN entities t ON t.id = r.target_id \
             WHERE r.revoked_at IS NOT NULL \
             ORDER BY r.revoked_at DESC",
        )
        .context("prepare vault-wide contradiction query")?;
    let rows = stmt.query_map([], |r| {
        Ok(json!({
            "relation_id": r.get::<_, i64>(0)?,
            "kind": r.get::<_, String>(1)?,
            "revoked_at": r.get::<_, Option<i64>>(2)?,
            "valid_from": r.get::<_, Option<i64>>(3)?,
            "valid_to": r.get::<_, Option<i64>>(4)?,
            "source": r.get::<_, String>(5)?,
            "target": r.get::<_, String>(6)?,
            "reason": r.get::<_, Option<String>>(7)?,
        }))
    })?;
    let mut contradicted = Vec::new();
    for row in rows {
        contradicted.push(row?);
    }
    Ok(json!({
        "contradictions": contradicted,
        "count": contradicted.len(),
        "guidance": "every revoked relation with the reason its revocation recorded; treat related beliefs with caution",
    }))
}

/// What the vault currently believes about each of `names`, together with every
/// recorded reason it changed its mind — returned in **one** call.
///
/// The point is that a caller cannot act on a superseded belief without seeing
/// why it was superseded. Reading a belief and reading its contradiction are
/// two round-trips every other memory system makes the agent choose to spend;
/// here the correction travels with the belief it corrects.
///
/// Returns `{ beliefs: [...], contradictions: [...], warnings: [...], count }`
/// where each warning is a ready-to-use line for an agent or a transcript.
pub fn recall_with_contradictions(engine: &MemoryEngine, names: &[&str]) -> Result<Value> {
    let conn = engine.connection();
    let mut beliefs = Vec::new();
    let mut contradictions = Vec::new();
    let mut warnings = Vec::new();

    for name in names {
        // Current belief: the open row anywhere in the lineage — after a
        // rename the successor carries the new name, so the lookup crosses the
        // lineage link instead of matching the name literally.
        // W1: expiry-staged rows are not current (invalid_at guard).
        let ids = lineage_ids(engine, name)?;
        let mut belief: Option<Value> = None;
        for id in ids {
            let mut stmt = conn.prepare(
                "SELECT id, kind, name, confidence, valid_from FROM entities \
                 WHERE id = ?1 AND valid_to IS NULL
                   AND (invalid_at IS NULL OR invalid_at > strftime('%s','now')*1000)",
            )?;
            let row = stmt
                .query_row(params![id], |r| {
                    Ok(json!({
                        "entity_id": r.get::<_, i64>(0)?,
                        "kind": r.get::<_, String>(1)?,
                        "name": r.get::<_, String>(2)?,
                        "confidence": r.get::<_, f64>(3)?,
                        "valid_from": r.get::<_, i64>(4)?,
                    }))
                })
                .ok();
            drop(stmt);
            if let Some(row) = row {
                let newer = belief.as_ref().is_none_or(|b| {
                    row["valid_from"].as_i64().unwrap_or(0) > b["valid_from"].as_i64().unwrap_or(0)
                });
                if newer {
                    belief = Some(row);
                }
            }
        }
        if let Some(b) = &belief {
            beliefs.push(b.clone());
        }

        // Everything the vault changed its mind about, for this name.
        let found = vault_contradictions(engine, name)?;
        if let Some(items) = found["contradictions"].as_array() {
            for item in items {
                if let Some(reason) = item["reason"].as_str() {
                    warnings.push(format!("belief about `{name}` was superseded: {reason}"));
                } else if let Some(kind) = item["kind"].as_str() {
                    warnings.push(format!("relation `{kind}` from `{name}` was revoked"));
                }
                contradictions.push(item.clone());
            }
        }
    }

    Ok(json!({
        "beliefs": beliefs,
        "contradictions": contradictions,
        "warnings": warnings,
        "count": beliefs.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EntityKind;

    const T0: i64 = 1_700_000_000_000;
    const DAY: i64 = 86_400_000;

    #[test]
    fn the_timeline_keeps_superseded_beliefs_with_the_reason_they_closed() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        let old = eng
            .add_entity(EntityKind::Concept, "target-dir", 0.9, T0)
            .unwrap();
        let new = eng
            .supersede_entity(
                old,
                None,
                Some("target-dir-v2"),
                None,
                None,
                "moved out of tree",
                T0 + DAY,
            )
            .unwrap();

        let report = vault_timeline(&eng, 50).unwrap();
        assert_eq!(
            report["count"], 2,
            "both beliefs are still in the vault: {report}"
        );
        assert_eq!(report["active"], 1);
        assert_eq!(report["superseded"], 1);

        let facts = report["facts"].as_array().unwrap();
        let closed = facts
            .iter()
            .find(|fact| fact["id"] == serde_json::json!(old))
            .expect("the replaced belief is in the timeline");
        assert_eq!(closed["valid_to"], serde_json::json!(T0 + DAY));
        assert_eq!(closed["superseded_by"], serde_json::json!(new));
        assert_eq!(closed["reason"], serde_json::json!("moved out of tree"));

        let current = facts
            .iter()
            .find(|fact| fact["id"] == serde_json::json!(new))
            .expect("the successor is in the timeline");
        assert!(current["valid_to"].is_null());
        assert_eq!(current["supersedes"], serde_json::json!(old));
    }

    #[test]
    fn an_empty_vault_has_an_empty_timeline() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        let report = vault_timeline(&eng, 10).unwrap();
        assert_eq!(report["count"], 0);
        assert_eq!(report["active"], 0);
        assert!(report["facts"].as_array().unwrap().is_empty());
    }

    #[test]
    fn history_returns_versions_in_order_with_reasons() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        let e1 = eng
            .add_entity(EntityKind::Concept, "alice", 1.0, T0)
            .unwrap();
        let e2 = eng
            .supersede_entity(
                e1,
                None,
                Some("alice-smith"),
                None,
                None,
                "rename",
                T0 + DAY,
            )
            .unwrap();

        // pre-rename name: the whole lineage resolves — the closed generation
        // by name, the successor through the `superseded_by` link.
        let old = vault_history(&eng, "alice").unwrap();
        let old_rows = old["versions"].as_array().unwrap();
        let old_ids: Vec<i64> = old_rows
            .iter()
            .filter_map(|v| v["entity_id"].as_i64())
            .collect();
        assert!(
            old_ids.contains(&e1) && old_ids.contains(&e2),
            "lineage must be whole from the pre-rename name: {old_ids:?}"
        );
        assert!(
            old_rows
                .iter()
                .any(|v| v["entity_id"] == e2 && v["via"] == "lineage"),
            "successor must be tagged as lineage-reached: {old_rows:?}"
        );
        let reasons: Vec<&str> = old_rows
            .iter()
            .filter_map(|v| v["change_reason"].as_str())
            .collect();
        assert_eq!(reasons, vec!["rename"]);

        // new name: same lineage in the other direction.
        let new = vault_history(&eng, "alice-smith").unwrap();
        let new_rows = new["versions"].as_array().unwrap();
        assert!(
            new_rows
                .iter()
                .any(|v| v["entity_id"] == e1 && v["via"] == "lineage"),
            "predecessor must be reachable from the new name: {new_rows:?}"
        );
        assert!(
            new_rows
                .iter()
                .any(|v| v["entity_id"] == e2 && v["via"] == "name")
        );
    }

    #[test]
    fn a_belief_journaled_once_is_one_version_not_two() {
        // The regression this guards: every snapshot written at creation is a
        // second copy of the version the live row already is. A single `correct`
        // therefore listed the old belief twice and the new one twice, and a
        // reader counting versions over-counted a two-step history as four.
        let eng = MemoryEngine::open_in_memory().unwrap();
        let e1 = eng
            .add_entity(EntityKind::Concept, "api-port", 0.9, T0)
            .unwrap();
        let e2 = eng
            .supersede_entity(
                e1,
                None,
                None,
                Some("the API listens on 9090"),
                Some(0.95),
                "confirmed in prod config",
                T0 + DAY,
            )
            .unwrap();

        let h = vault_history(&eng, "api-port").unwrap();
        let rows = h["versions"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "one correction is two versions: {rows:?}");
        assert_eq!(h["count"], 2);
        assert_eq!(rows[0]["entity_id"], serde_json::json!(e1));
        assert_eq!(rows[1]["entity_id"], serde_json::json!(e2));
        assert_eq!(
            rows[0]["change_reason"],
            serde_json::json!("confirmed in prod config")
        );
        // The folded version still carries the snapshot the journal kept — the
        // fold merges the copies, it does not throw one away.
        assert_eq!(rows[0]["snapshot"]["name"], serde_json::json!("api-port"));
        assert!(rows[1]["change_reason"].is_null());
    }

    #[test]
    fn lineage_survives_chained_supersedes() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        let e1 = eng.add_entity(EntityKind::Concept, "acq", 1.0, T0).unwrap();
        let e2 = eng
            .supersede_entity(
                e1,
                None,
                Some("acq-inc"),
                None,
                None,
                "incorporated",
                T0 + DAY,
            )
            .unwrap();
        let e3 = eng
            .supersede_entity(
                e2,
                None,
                Some("acq-global"),
                None,
                None,
                "rebrand",
                T0 + 2 * DAY,
            )
            .unwrap();

        // Original name sees all three generations, one version each: the live
        // row and the journal entry for a closed generation describe the same
        // belief, so they fold into one version (the journal contributes the
        // reason). Three generations must read as three versions, not six.
        let h = vault_history(&eng, "acq").unwrap();
        let rows = h["versions"].as_array().unwrap();
        let mut ids: Vec<i64> = rows
            .iter()
            .filter_map(|v| v["entity_id"].as_i64())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids, vec![e1, e2, e3], "lineage must be whole: {ids:?}");
        assert_eq!(rows.len(), 3, "one version per generation: {rows:?}");
        assert_eq!(h["count"], 3, "the reported count matches: {h}");

        // Oldest first, each closed generation carrying the reason it closed
        // and the open one carrying none.
        let order: Vec<i64> = rows
            .iter()
            .filter_map(|v| v["entity_id"].as_i64())
            .collect();
        assert_eq!(order, vec![e1, e2, e3], "ordered by valid_from: {rows:?}");
        let reasons: Vec<Option<&str>> = rows.iter().map(|v| v["change_reason"].as_str()).collect();
        assert_eq!(reasons, vec![Some("incorporated"), Some("rebrand"), None]);
        // Every version keeps the name it was believed under, whether that came
        // from the live row or the folded-in journal snapshot.
        let names: Vec<&str> = rows.iter().filter_map(|v| v["name"].as_str()).collect();
        assert_eq!(names, vec!["acq", "acq-inc", "acq-global"]);

        // Recall by the original name returns the CURRENT (third-generation)
        // belief — a pre-rename name must not strand the caller on a closed
        // generation or miss the belief entirely.
        let bundle = recall_with_contradictions(&eng, &["acq"]).unwrap();
        let beliefs = bundle["beliefs"].as_array().unwrap();
        assert_eq!(beliefs.len(), 1);
        assert_eq!(beliefs[0]["entity_id"], e3);
        assert_eq!(beliefs[0]["name"], "acq-global");
    }

    #[test]
    fn contradictions_report_reasoned_changes_and_revoked_edges() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        let a = eng
            .add_entity(EntityKind::Concept, "acme", 1.0, T0)
            .unwrap();
        let b = eng
            .add_entity(EntityKind::Concept, "beta", 1.0, T0)
            .unwrap();
        let r = eng.add_relation(a, b, "partners-with", 1.0, T0).unwrap();
        eng.revoke_relation(r, "deal fell through", T0 + DAY)
            .unwrap();
        eng.supersede_entity(a, None, Some("acme-corp"), None, None, "rebrand", T0 + DAY)
            .unwrap();

        // queried by the name the journaled generation carried, which is
        // where its change reasons live (see the module-level note).
        let c = vault_contradictions(&eng, "acme").unwrap();
        assert_eq!(c["count"], 2);
        let reasons: Vec<&str> = c["contradictions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["reason"].as_str())
            .collect();
        assert!(reasons.contains(&"rebrand"));
        let kinds: Vec<&str> = c["contradictions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["kind"].as_str())
            .collect();
        assert!(kinds.contains(&"partners-with"));
    }

    #[test]
    fn recall_surfaces_the_reason_a_belief_changed() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        let a = eng
            .add_entity(EntityKind::Concept, "acme", 1.0, T0)
            .unwrap();
        let b = eng
            .add_entity(EntityKind::Concept, "beta", 1.0, T0)
            .unwrap();
        let r = eng.add_relation(a, b, "partners-with", 1.0, T0).unwrap();
        eng.revoke_relation(r, "deal fell through", T0 + DAY)
            .unwrap();
        eng.supersede_entity(a, None, Some("acme-corp"), None, None, "rebrand", T0 + DAY)
            .unwrap();

        let bundle = recall_with_contradictions(&eng, &["acme-corp", "acme"]).unwrap();
        let warnings: Vec<&str> = bundle["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w.as_str())
            .collect();
        // The correction travels with the belief, in the same response.
        assert!(
            warnings.iter().any(|w| w.contains("rebrand")),
            "the rename reason must reach the caller: {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("partners-with")),
            "the revoked relation must reach the caller: {warnings:?}"
        );
        assert!(
            bundle["count"].as_u64().unwrap() >= 1,
            "a current belief was found"
        );
    }

    #[test]
    fn recall_of_a_clean_entity_raises_no_warnings() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        eng.add_entity(EntityKind::Concept, "calm", 1.0, T0)
            .unwrap();
        let bundle = recall_with_contradictions(&eng, &["calm"]).unwrap();
        assert_eq!(bundle["count"], 1);
        assert_eq!(bundle["warnings"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn unknown_name_yields_empty_results() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        let h = vault_history(&eng, "nobody").unwrap();
        assert_eq!(h["count"], 0);
        let c = vault_contradictions(&eng, "nobody").unwrap();
        assert_eq!(c["count"], 0);
    }

    #[test]
    fn vault_wide_audit_finds_revocations_across_entities_with_reasons() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        let a = eng
            .add_entity(EntityKind::Concept, "acme-corp", 1.0, T0)
            .unwrap();
        let b = eng
            .add_entity(EntityKind::Concept, "globex", 1.0, T0)
            .unwrap();
        let c = eng
            .add_entity(EntityKind::Concept, "initech", 1.0, T0)
            .unwrap();
        let r1 = eng.add_relation(a, b, "partners-with", 1.0, T0).unwrap();
        let r2 = eng.add_relation(b, c, "supplies-to", 1.0, T0).unwrap();
        eng.revoke_relation(r1, "deal fell through", T0 + DAY)
            .unwrap();
        eng.revoke_relation(r2, "contract not renewed", T0 + 2 * DAY)
            .unwrap();

        let audit = vault_contradictions_all(&eng).unwrap();
        let rows = audit["contradictions"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            2,
            "both revoked relations must appear: {rows:?}"
        );
        assert_eq!(audit["count"], 2);
        // newest revocation first
        assert_eq!(
            rows[0]["reason"].as_str().unwrap(),
            "revoked: contract not renewed"
        );
        assert_eq!(
            rows[1]["reason"].as_str().unwrap(),
            "revoked: deal fell through"
        );
        // names resolved through the join, not raw ids
        assert_eq!(rows[1]["source"].as_str().unwrap(), "acme-corp");
        assert_eq!(rows[1]["target"].as_str().unwrap(), "globex");
        assert_eq!(rows[1]["valid_from"].as_i64().unwrap(), T0);
        assert_eq!(rows[1]["valid_to"], serde_json::Value::Null);
    }
}
