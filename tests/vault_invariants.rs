//! CLI-side integration proof for the vault invariant checker (S-MEM-3).
//!
//! The checker itself lives in `memnoth-engine`; this suite drives it through
//! the *public* API from outside that crate, against a real in-memory vault.
//! That matters because a checker which only passes from inside its own module
//! (with access to private helpers) proves nothing to the CLI that has to call
//! it. Every assertion here is about honest status: a rule the current schema
//! cannot express must say `NotModelled`, a broken vault must say `Violation`,
//! and neither may be disguised as a `Pass`.

use std::collections::BTreeMap;

use memnoth_engine::store::invariants::{
    InvariantReport, InvariantRow, InvariantStatus, check_invariants,
};
use memnoth_engine::{EntityKind, MemoryEngine, now_ms};

/// Rules whose *write path* still does not record what the rule requires.
/// Schema v5 added predicate/object, so MI-3 is now a counted check; the
/// retraction and trust paths still do not set forgotten_at or record a trust
/// decision, so marking MI-6 or MI-10 `Pass` would claim behaviour that does
/// not exist.
const NOT_MODELLED_IDS: [&str; 2] = ["MI-6", "MI-10"];

const ALL_IDS: [&str; 10] = [
    "MI-1", "MI-2", "MI-3", "MI-4", "MI-5", "MI-6", "MI-7", "MI-8", "MI-9", "MI-10",
];

fn row<'a>(report: &'a InvariantReport, id: &str) -> &'a InvariantRow {
    report
        .rows
        .iter()
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("invariant report is missing {id}"))
}

/// Row counts for every table in the vault, keyed by name. Reading
/// `sqlite_master` rather than a hard-coded list means a future table cannot
/// quietly escape the read-only proof.
fn table_counts(engine: &MemoryEngine) -> BTreeMap<String, i64> {
    let conn = engine.connection();
    let names: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .expect("prepare table list");
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query table list");
        rows.collect::<Result<Vec<_>, _>>()
            .expect("collect table list")
    };
    let mut counts = BTreeMap::new();
    for name in names {
        // `name` is from sqlite_master; double-quoting keeps identifiers safe.
        let sql = format!("SELECT COUNT(*) FROM \"{name}\"");
        let count: i64 = conn
            .query_row(&sql, [], |r| r.get(0))
            .unwrap_or_else(|err| panic!("count rows in {name}: {err}"));
        counts.insert(name, count);
    }
    counts
}

#[test]
fn fresh_vault_has_no_violations_but_never_all_pass() {
    let engine = MemoryEngine::open_in_memory().expect("in-memory vault");
    let report = check_invariants(engine.connection());

    // All ten rules are always reported, MI-1 … MI-10 in order (S-MEM-3).
    let ids: Vec<&str> = report.rows.iter().map(|r| r.id).collect();
    assert_eq!(ids, ALL_IDS);

    assert!(
        !report.has_violations(),
        "an empty vault has no bad rows: {:?}",
        report.violations()
    );

    // The plan-only rules must be honest about the missing schema, and a clean
    // report must never be a wall of fabricated passes.
    for id in NOT_MODELLED_IDS {
        let row = row(&report, id);
        assert_eq!(
            row.status,
            InvariantStatus::NotModelled,
            "{id} faked a pass for a column the schema does not have"
        );
        assert!(row.affected_count.is_none());
    }
    assert!(
        !report.not_modelled().is_empty(),
        "a report claiming every rule passes would be a lie"
    );
    assert!(
        report
            .rows
            .iter()
            .any(|r| r.status == InvariantStatus::Pass),
        "checkable rules on an empty vault should still pass"
    );
}

#[test]
fn inverted_validity_interval_is_reported_as_mi1() {
    let engine = MemoryEngine::open_in_memory().expect("in-memory vault");
    // Bypass the engine's writers: only raw SQL can produce a window where
    // valid_to precedes valid_from, which is exactly the corruption MI-1 names.
    engine
        .connection()
        .execute(
            "INSERT INTO entities (kind, name, valid_from, valid_to, created_at) \
             VALUES ('concept', 'inverted', 2000, 1000, 1500)",
            [],
        )
        .expect("insert an inverted interval directly");

    let report = check_invariants(engine.connection());
    let mi1 = row(&report, "MI-1");
    assert_eq!(mi1.status, InvariantStatus::Violation);
    assert!(
        mi1.affected_count.unwrap_or(0) >= 1,
        "MI-1 must count the corrupted row, got {:?}",
        mi1.affected_count
    );
}

#[test]
fn observation_missing_provenance_is_never_a_fabricated_pass() {
    let engine = MemoryEngine::open_in_memory().expect("in-memory vault");
    let entity = engine
        .add_entity(EntityKind::Concept, "sourced", 1.0, now_ms())
        .expect("add entity");
    // Omitting provenance leaves the schema default '{}': an unsourced belief.
    // MI-9 is a counted rule here, but a checker that cannot express it is
    // allowed to say NotModelled — what is forbidden is reporting a Pass.
    engine
        .connection()
        .execute(
            "INSERT INTO observations (entity_id, content, kind, valid_from, recorded_at) \
             VALUES (?1, 'unsourced', 'fact', 100, 200)",
            rusqlite::params![entity],
        )
        .expect("insert observation without provenance");

    let report = check_invariants(engine.connection());
    let status = row(&report, "MI-9").status;
    assert!(
        matches!(
            status,
            InvariantStatus::Violation | InvariantStatus::NotModelled
        ),
        "an unsourced observation must be a violation or explicitly unmodelled, not {status:?}"
    );
    assert_ne!(
        status,
        InvariantStatus::Pass,
        "MI-9 must not fabricate a pass"
    );
}

#[test]
fn checker_is_read_only() {
    let engine = MemoryEngine::open_in_memory().expect("in-memory vault");
    let t0 = now_ms();
    let entity = engine
        .add_entity(EntityKind::Concept, "stable", 0.9, t0)
        .expect("add entity");
    engine
        .ingest_observation_attributed(
            entity,
            "Rust is a systems language",
            "fact",
            0.9,
            t0,
            r#"{"user":"tester"}"#,
            None,
        )
        .expect("add attributed observation");

    let before = table_counts(&engine);
    let report = check_invariants(engine.connection());
    let after = table_counts(&engine);

    assert_eq!(before, after, "check_invariants changed table row counts");
    assert_eq!(report.rows.len(), ALL_IDS.len());
}
