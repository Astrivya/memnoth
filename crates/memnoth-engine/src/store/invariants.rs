//! Vault invariant checks for the ten memory rules in S-MEM-3 (MI-1 … MI-10).
//!
//! [`check_invariants`] runs **read-only** proofs against an already-open vault
//! and returns exactly one [`InvariantRow`] per rule. A row is a
//! [`InvariantStatus::Pass`], a [`InvariantStatus::Violation`] with an
//! affected-row count, or an [`InvariantStatus::NotModelled`] when the current
//! schema has no column with which to express the rule. Rules whose supporting
//! columns do not exist (`forgotten_at`, `trust`, `predicate`/`object`) are
//! reported as `NotModelled` — never as a pass — so the report cannot fake
//! compliance with a schema the vault does not have. As of schema v5
//! `predicate`/`object` exist and MI-3 is a counted check; `forgotten_at`
//! and `trust` exist but their *write paths* still do not record the facts
//! those rules require, so MI-6 and MI-10 remain `NotModelled`.

use rusqlite::Connection;
use serde_json::Value;

/// Outcome of checking one invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvariantStatus {
    /// The proof found no offending rows.
    Pass,
    /// The proof found at least one offending row.
    Violation,
    /// The current schema cannot express the rule; no pass/fail claim is made.
    NotModelled,
}

/// One invariant's result.
#[derive(Debug, Clone)]
pub struct InvariantRow {
    /// Stable id, `MI-1` … `MI-10`.
    pub id: &'static str,
    /// Short name of the rule.
    pub title: &'static str,
    /// Whether the rule passed, was violated, or is not modelled.
    pub status: InvariantStatus,
    /// The proof SQL, or the schema reason when the rule is not modelled.
    pub evidence: String,
    /// Number of offending rows, when the rule is checkable.
    pub affected_count: Option<usize>,
}

/// The result of checking all ten invariants.
#[derive(Debug, Clone)]
pub struct InvariantReport {
    /// One row per rule, ordered MI-1 … MI-10.
    pub rows: Vec<InvariantRow>,
}

impl InvariantReport {
    /// True when at least one checkable rule was violated.
    pub fn has_violations(&self) -> bool {
        self.rows
            .iter()
            .any(|row| row.status == InvariantStatus::Violation)
    }

    /// Only the rows that were violated.
    pub fn violations(&self) -> Vec<&InvariantRow> {
        self.rows
            .iter()
            .filter(|row| row.status == InvariantStatus::Violation)
            .collect()
    }

    /// Only the rows whose rule cannot be expressed by the current schema.
    pub fn not_modelled(&self) -> Vec<&InvariantRow> {
        self.rows
            .iter()
            .filter(|row| row.status == InvariantStatus::NotModelled)
            .collect()
    }
}

/// MI-2 skew: one day, in the millisecond unit used by the stored timestamps.
const MI2_SKEW_MS: i64 = 86_400_000;

const MI4_EVIDENCE: &str = "Rust scan of entities.metadata: every successor S with metadata.supersedes = P (excluding degraded_add rows, which are ADDs rather than true supersessions) must have S.valid_from = P.valid_to. Uses entities(valid_from, valid_to, metadata); a missing predecessor is reported by MI-5.";
const MI5_EVIDENCE: &str = "Rust scan of entities.metadata: metadata.supersedes and metadata.superseded_by must be reciprocal in both directions. Scope is metadata reciprocity only — the `supersedes` relation edge is written single-directionally (successor to predecessor), so edge reciprocity cannot be proven from a snapshot.";

/// Count offending rows with one read-only statement.
fn count_rows(conn: &Connection, sql: &str) -> Result<usize, String> {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0))
        .map(|count| count as usize)
        .map_err(|err| err.to_string())
}

/// A rule whose proof statement failed to run is a violation with no count: a
/// check that could not execute must never be reported as a pass.
fn failed_row(id: &'static str, title: &'static str, evidence: String) -> InvariantRow {
    InvariantRow {
        id,
        title,
        status: InvariantStatus::Violation,
        evidence,
        affected_count: None,
    }
}

fn counted_row(
    id: &'static str,
    title: &'static str,
    sql: &str,
    conn: &Connection,
) -> InvariantRow {
    match count_rows(conn, sql) {
        Ok(count) => InvariantRow {
            id,
            title,
            status: if count == 0 {
                InvariantStatus::Pass
            } else {
                InvariantStatus::Violation
            },
            evidence: sql.to_string(),
            affected_count: Some(count),
        },
        Err(err) => failed_row(id, title, format!("{sql}\n[query failed: {err}]")),
    }
}

fn not_modelled_row(id: &'static str, title: &'static str, reason: &str) -> InvariantRow {
    InvariantRow {
        id,
        title,
        status: InvariantStatus::NotModelled,
        evidence: reason.to_string(),
        affected_count: None,
    }
}

/// The columns MI-4 and MI-5 need from `entities`.
struct EntityLinkRow {
    id: i64,
    valid_from: i64,
    valid_to: Option<i64>,
    metadata: String,
}

fn load_entities(conn: &Connection) -> Result<Vec<EntityLinkRow>, String> {
    let mut stmt = conn
        .prepare("SELECT id, valid_from, valid_to, metadata FROM entities")
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(EntityLinkRow {
                id: row.get(0)?,
                valid_from: row.get(1)?,
                valid_to: row.get(2)?,
                metadata: row.get(3)?,
            })
        })
        .map_err(|err| err.to_string())?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())
}

/// Read one `i64` lineage stamp out of an entity's JSON metadata.
fn meta_link(metadata: &str, key: &str) -> Option<i64> {
    serde_json::from_str::<Value>(metadata)
        .ok()?
        .get(key)?
        .as_i64()
}

/// Read one boolean flag out of an entity's JSON metadata.
fn meta_flag(metadata: &str, key: &str) -> bool {
    serde_json::from_str::<Value>(metadata)
        .ok()
        .and_then(|value| value.get(key).and_then(Value::as_bool))
        .unwrap_or(false)
}

fn check_mi4(conn: &Connection) -> InvariantRow {
    const ID: &str = "MI-4";
    const TITLE: &str = "Successor.valid_from equals predecessor.valid_to";
    let rows = match load_entities(conn) {
        Ok(rows) => rows,
        Err(err) => return failed_row(ID, TITLE, format!("{MI4_EVIDENCE}\n[load failed: {err}]")),
    };
    let by_id: std::collections::HashMap<i64, &EntityLinkRow> =
        rows.iter().map(|row| (row.id, row)).collect();
    let mut broken = 0usize;
    for successor in &rows {
        if meta_flag(&successor.metadata, "degraded_add") {
            continue;
        }
        let Some(predecessor_id) = meta_link(&successor.metadata, "supersedes") else {
            continue;
        };
        // A dangling predecessor is a broken chain, reported by MI-5.
        if let Some(predecessor) = by_id.get(&predecessor_id)
            && successor.valid_from != predecessor.valid_to.unwrap_or(i64::MIN)
        {
            broken += 1;
        }
    }
    InvariantRow {
        id: ID,
        title: TITLE,
        status: if broken == 0 {
            InvariantStatus::Pass
        } else {
            InvariantStatus::Violation
        },
        evidence: MI4_EVIDENCE.to_string(),
        affected_count: Some(broken),
    }
}

fn check_mi5(conn: &Connection) -> InvariantRow {
    const ID: &str = "MI-5";
    const TITLE: &str = "supersedes / superseded_by metadata links are reciprocal";
    let rows = match load_entities(conn) {
        Ok(rows) => rows,
        Err(err) => return failed_row(ID, TITLE, format!("{MI5_EVIDENCE}\n[load failed: {err}]")),
    };
    let by_id: std::collections::HashMap<i64, &EntityLinkRow> =
        rows.iter().map(|row| (row.id, row)).collect();
    let mut broken = 0usize;
    for row in &rows {
        let mut one_broken = false;
        if let Some(predecessor_id) = meta_link(&row.metadata, "supersedes")
            && !matches!(
                by_id.get(&predecessor_id),
                Some(predecessor)
                    if meta_link(&predecessor.metadata, "superseded_by") == Some(row.id)
            )
        {
            one_broken = true;
        }
        if let Some(successor_id) = meta_link(&row.metadata, "superseded_by")
            && !matches!(
                by_id.get(&successor_id),
                Some(successor) if meta_link(&successor.metadata, "supersedes") == Some(row.id)
            )
        {
            one_broken = true;
        }
        if one_broken {
            broken += 1;
        }
    }
    InvariantRow {
        id: ID,
        title: TITLE,
        status: if broken == 0 {
            InvariantStatus::Pass
        } else {
            InvariantStatus::Violation
        },
        evidence: MI5_EVIDENCE.to_string(),
        affected_count: Some(broken),
    }
}

/// Run all ten invariant checks against `conn`. Every query is read-only: the
/// checker never inserts, updates or deletes, even when it finds a violation.
pub fn check_invariants(conn: &Connection) -> InvariantReport {
    let mi1_sql = "\
SELECT
  (SELECT COUNT(*) FROM entities  WHERE valid_to IS NOT NULL AND valid_to < valid_from) +
  (SELECT COUNT(*) FROM relations WHERE valid_to IS NOT NULL AND valid_to < valid_from) +
  (SELECT COUNT(*) FROM observations WHERE valid_to IS NOT NULL AND valid_to < valid_from) +
  (SELECT COUNT(*) FROM entity_versions WHERE valid_to IS NOT NULL AND valid_to < valid_from) +
  (SELECT COUNT(*) FROM relation_versions WHERE valid_to IS NOT NULL AND valid_to < valid_from)";
    let mi2_sql = format!(
        "-- A NULL recorded_at (pre-attribution row) cannot be evaluated and is excluded.\n\
         SELECT COUNT(*) FROM observations \
         WHERE recorded_at IS NOT NULL AND recorded_at < valid_from - {MI2_SKEW_MS}"
    );
    let mi7_sql = "\
SELECT COUNT(*) FROM entities e
WHERE e.valid_to IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM entity_versions v WHERE v.entity_id = e.id)";
    let mi8_sql = "\
SELECT COUNT(*) FROM observations o
JOIN entities e ON e.id = o.entity_id
WHERE o.recorded_at IS NOT NULL AND o.recorded_at < e.created_at";
    let mi9_sql = "\
-- observations has provenance (NOT NULL DEFAULT '{}') but no source_id column;
-- an empty default provenance is treated as missing.
SELECT COUNT(*) FROM observations
WHERE provenance IS NULL OR TRIM(provenance) IN ('', '{}', 'null')";

    let rows = vec![
        counted_row("MI-1", "Valid interval is not inverted", mi1_sql, conn),
        counted_row(
            "MI-2",
            "recorded_at >= valid_from minus one-day skew",
            &mi2_sql,
            conn,
        ),
        counted_row(
            "MI-3",
            "At most one open row per (entity, predicate)",
            "SELECT COUNT(*) FROM ( \
               SELECT entity_id, predicate FROM observations \
                WHERE valid_to IS NULL AND forgotten_at IS NULL AND predicate IS NOT NULL \
                GROUP BY entity_id, predicate HAVING COUNT(*) > 1 \
             )",
            conn,
        ),
        check_mi4(conn),
        check_mi5(conn),
        not_modelled_row(
            "MI-6",
            "Retraction sets forgotten_at, never DELETE",
            "Schema v5 adds forgotten_at, but the product's retraction path (decay::forget -> revoke_entity) still records only valid_to and a metadata reason, so no row can be shown to have set it. A past DELETE also cannot be distinguished from a never-written row in a single snapshot. The column existing is not proof the rule is followed.",
        ),
        counted_row(
            "MI-7",
            "Every closed entity window has an entity_versions row",
            mi7_sql,
            conn,
        ),
        counted_row(
            "MI-8",
            "observation.recorded_at >= entities.created_at",
            mi8_sql,
            conn,
        ),
        counted_row(
            "MI-9",
            "Every belief has a source_id or provenance",
            mi9_sql,
            conn,
        ),
        not_modelled_row(
            "MI-10",
            "Trust never increases without a human decision",
            "Schema v5 adds observations.trust with default 2, but there is no trust-change event ledger naming the human who authorised an increase. An initial value and an unapproved increase are indistinguishable from one snapshot, so counting either would fake compliance.",
        ),
    ];
    InvariantReport { rows }
}

#[cfg(test)]
mod tests {
    use super::{InvariantStatus, check_invariants};
    use crate::store::MemoryEngine;
    use crate::{EntityKind, now_ms};

    #[test]
    fn clean_vault_reports_no_violations() {
        let engine = MemoryEngine::open_in_memory().unwrap();
        let t0 = now_ms();
        let entity = engine
            .add_entity(EntityKind::Concept, "rust", 0.9, t0)
            .unwrap();
        let at = t0 + 1_000;
        let successor = engine
            .supersede_entity(entity, None, None, None, None, "rename", at)
            .unwrap();
        engine
            .ingest_observation_attributed(
                successor,
                "Rust is a systems language",
                "fact",
                0.9,
                at,
                r#"{"user":"tester"}"#,
                Some(at + 2_000),
            )
            .unwrap();

        let report = check_invariants(engine.connection());
        assert_eq!(report.rows.len(), 10);
        assert!(
            !report.has_violations(),
            "clean vault produced violations: {:?}",
            report.violations()
        );
    }

    #[test]
    fn inverted_interval_inserted_directly_is_reported() {
        let engine = MemoryEngine::open_in_memory().unwrap();
        engine
            .connection()
            .execute(
                "INSERT INTO entities (kind, name, valid_from, valid_to, created_at) \
                 VALUES ('concept', 'inverted', 2000, 1000, 1500)",
                [],
            )
            .unwrap();

        let report = check_invariants(engine.connection());
        let mi1 = report.rows.iter().find(|row| row.id == "MI-1").unwrap();
        assert_eq!(mi1.status, InvariantStatus::Violation);
        assert!(mi1.affected_count.unwrap_or(0) >= 1);
    }

    #[test]
    fn missing_provenance_is_reported() {
        let engine = MemoryEngine::open_in_memory().unwrap();
        let entity = engine
            .add_entity(EntityKind::Concept, "sourced", 1.0, now_ms())
            .unwrap();
        // Omitting provenance leaves the schema default '{}', i.e. unsourced.
        engine
            .connection()
            .execute(
                "INSERT INTO observations (entity_id, content, kind, valid_from, recorded_at) \
                 VALUES (?1, 'unsourced', 'fact', 100, 200)",
                rusqlite::params![entity],
            )
            .unwrap();

        let report = check_invariants(engine.connection());
        let mi9 = report.rows.iter().find(|row| row.id == "MI-9").unwrap();
        assert_eq!(mi9.status, InvariantStatus::Violation);
        assert!(mi9.affected_count.unwrap_or(0) >= 1);
    }

    #[test]
    fn plan_only_fields_are_not_modelled() {
        let engine = MemoryEngine::open_in_memory().unwrap();
        let report = check_invariants(engine.connection());

        let ids: Vec<&str> = report.rows.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![
                "MI-1", "MI-2", "MI-3", "MI-4", "MI-5", "MI-6", "MI-7", "MI-8", "MI-9", "MI-10",
            ]
        );
        // Schema v5 added predicate/object, so MI-3 is a counted check; an
        // empty vault must prove it with a real count rather than claiming a
        // pass from a column that does not exist.
        let mi3 = report.rows.iter().find(|row| row.id == "MI-3").unwrap();
        assert_eq!(mi3.status, InvariantStatus::Pass);
        assert_eq!(mi3.affected_count, Some(0));

        // The retraction and trust write paths still do not record what these
        // rules require, so they must not be reported as passes.
        for id in ["MI-6", "MI-10"] {
            let row = report.rows.iter().find(|row| row.id == id).unwrap();
            assert_eq!(
                row.status,
                InvariantStatus::NotModelled,
                "{id} faked a pass"
            );
            assert!(row.affected_count.is_none());
        }
    }

    #[test]
    fn checker_is_read_only() {
        let engine = MemoryEngine::open_in_memory().unwrap();
        let entity = engine
            .add_entity(EntityKind::Concept, "stable", 1.0, now_ms())
            .unwrap();
        let count_before: i64 = engine
            .connection()
            .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
            .unwrap();

        let _ = check_invariants(engine.connection());

        let count_after: i64 = engine
            .connection()
            .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count_before, count_after);
        let metadata: String = engine
            .connection()
            .query_row(
                "SELECT metadata FROM entities WHERE id = ?1",
                rusqlite::params![entity],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(metadata, "{}");
    }
}
