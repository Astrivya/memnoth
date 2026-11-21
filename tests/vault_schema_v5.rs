//! Schema v5 is additive and non-aborting: an older vault opens unchanged,
//! the new predicate vocabulary exists, and MI-3 becomes a counted check
//! instead of an honest `NotModelled`.

use memnoth_engine::store::invariants::{InvariantStatus, check_invariants};
use memnoth_engine::{EntityKind, MemoryEngine, now_ms};

#[test]
fn a_fresh_vault_is_v5_and_has_the_predicate_vocabulary() {
    let engine = MemoryEngine::open_in_memory().expect("in-memory vault");
    let version: i64 = engine
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5);

    let id = engine
        .add_entity(EntityKind::Concept, "schema-probe", 1.0, now_ms())
        .unwrap();
    engine
        .connection()
        .execute(
            "INSERT INTO observations \
                (entity_id, content, kind, valid_from, recorded_at, predicate, object, trust) \
             VALUES (?1, 'cap is 5', 'decision', ?2, ?2, 'retry-cap', '5', 3)",
            rusqlite::params![id, now_ms()],
        )
        .expect("the v5 columns accept a predicate-level belief");

    let trust: i64 = engine
        .connection()
        .query_row(
            "SELECT trust FROM observations WHERE predicate = 'retry-cap'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(trust, 3);
}

#[test]
fn mi3_reports_a_duplicate_open_predicate_and_the_index_refuses_it() {
    let engine = MemoryEngine::open_in_memory().expect("in-memory vault");
    let id = engine
        .add_entity(EntityKind::Concept, "predicate-probe", 1.0, now_ms())
        .unwrap();
    let insert = |content: &str| {
        engine.connection().execute(
            "INSERT INTO observations \
                    (entity_id, content, kind, valid_from, recorded_at, predicate) \
                 VALUES (?1, ?2, 'fact', ?3, ?3, 'cap')",
            rusqlite::params![id, content, now_ms()],
        )
    };
    insert("first").expect("first open belief");
    assert!(
        insert("second").is_err(),
        "the unique open-belief index must refuse a second open belief for one predicate"
    );

    // Drop the index to prove the checker itself detects the violation, not
    // just the constraint that normally prevents it.
    engine
        .connection()
        .execute_batch("DROP INDEX obs_one_open_per_predicate;")
        .unwrap();
    insert("second").expect("fixture duplicate after dropping the index");

    let report = check_invariants(engine.connection());
    let mi3 = report.rows.iter().find(|row| row.id == "MI-3").unwrap();
    assert_eq!(mi3.status, InvariantStatus::Violation);
    assert_eq!(mi3.affected_count, Some(1));
}

#[test]
fn a_pre_v5_vault_opens_and_gains_the_new_columns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old-v4.db");
    // Build the v4 shape by hand: only the columns a 0.1.1 vault had.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE entities (
                id INTEGER PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
                canonical_name TEXT, summary TEXT, confidence REAL NOT NULL DEFAULT 1.0,
                valid_from INTEGER NOT NULL, valid_to INTEGER, metadata TEXT NOT NULL DEFAULT '{}',
                created_at INTEGER NOT NULL, content_hash TEXT, invalid_at INTEGER,
                proof_count INTEGER NOT NULL DEFAULT 0, consolidated_at INTEGER,
                generation INTEGER NOT NULL DEFAULT 0, base_checksum TEXT
             );
             CREATE TABLE observations (
                id INTEGER PRIMARY KEY, entity_id INTEGER NOT NULL, content TEXT NOT NULL,
                kind TEXT NOT NULL DEFAULT 'fact', confidence REAL NOT NULL DEFAULT 1.0,
                valid_from INTEGER NOT NULL, valid_to INTEGER, content_hash TEXT,
                invalid_at INTEGER, recorded_at INTEGER, provenance TEXT NOT NULL DEFAULT '{}'
             );
             PRAGMA user_version = 4;",
        )
        .unwrap();
    }

    let engine = MemoryEngine::open(&path).expect("an old vault must still open");
    let version: i64 = engine
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5);
    let columns: Vec<String> = {
        let mut stmt = engine
            .connection()
            .prepare("PRAGMA table_info(observations)")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    for name in [
        "predicate",
        "object",
        "forgotten_at",
        "trust",
        "episode_id",
        "idem_key",
    ] {
        assert!(
            columns.iter().any(|c| c == name),
            "missing migrated column {name}"
        );
    }
}
