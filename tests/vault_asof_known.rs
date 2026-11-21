//! `--knew` is the second clock: valid time asks what was true then, recorded
//! time asks what the vault had learned by then. The stored `recorded_at`
//! existed for months but nothing filtered on it, so "what did we know by K?"
//! was not answerable.

use std::process::Command;

use serde_json::{Value, json};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

fn run(args: &[&str], home: &std::path::Path) -> std::process::Output {
    Command::new(binary())
        .args(args)
        .env("MEMNOTH_HOME", home)
        .env_remove("MEMNOTH_VAULT")
        .output()
        .expect("run memnoth")
}

fn answer(args: &[&str], home: &std::path::Path) -> Value {
    let out = run(args, home);
    assert!(
        out.status.success(),
        "command failed: {}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("JSON envelope")
}

#[test]
fn knew_filters_by_recorded_time_without_touching_valid_time() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    assert!(run(&["vault", "init"], &home).status.success());
    assert!(
        run(&["vault", "remember", "clock", "early fact"], &home)
            .status
            .success()
    );

    const EARLY: i64 = 1_700_000_000_000;
    const DAY: i64 = 86_400_000;
    let late = EARLY + 10 * DAY;
    let at = late + DAY;

    let conn = rusqlite::Connection::open(home.join("vault.db")).unwrap();
    // The entity was created at today's clock; move it into the fixture's
    // validity window so the as-of query can see it at all.
    conn.execute(
        "UPDATE entities SET valid_from = ?1 WHERE name = 'clock'",
        [EARLY],
    )
    .unwrap();
    conn.execute(
        "UPDATE observations SET recorded_at = ?1, valid_from = ?1 WHERE content = 'early fact'",
        [EARLY],
    )
    .unwrap();
    let entity_id: i64 = conn
        .query_row("SELECT id FROM entities WHERE name = 'clock'", [], |r| {
            r.get(0)
        })
        .unwrap();
    conn.execute(
        "INSERT INTO observations \
            (entity_id, content, kind, confidence, valid_from, recorded_at, provenance) \
         VALUES (?1, 'late fact', 'fact', 1.0, ?2, ?3, '{\"user\":\"a\"}')",
        rusqlite::params![entity_id, EARLY, late],
    )
    .unwrap();
    drop(conn);

    let at_text = at.to_string();
    let all = answer(
        &[
            "--json", "vault", "asof", "--name", "clock", "--at", &at_text,
        ],
        &home,
    );
    assert_eq!(all["data"]["facts"].as_array().unwrap().len(), 2);
    assert_eq!(all["data"]["knew"], Value::Null);

    let knew_text = (EARLY + 5 * DAY).to_string();
    let known = answer(
        &[
            "--json", "vault", "asof", "--name", "clock", "--at", &at_text, "--knew", &knew_text,
        ],
        &home,
    );
    assert_eq!(known["data"]["knew"], json!(EARLY + 5 * DAY));
    assert!(known["data"]["knew_iso"].is_string());
    let facts = known["data"]["facts"].as_array().unwrap();
    assert_eq!(facts.len(), 1, "a fact recorded later was not known then");
    assert_eq!(facts[0]["content"], "early fact");
    assert_eq!(facts[0]["recorded_at"], json!(EARLY));
}
