//! `mn vault invariants` as a gate, driven through the real binary.
//!
//! The engine-level suite (`vault_invariants.rs`) proves the checker never
//! lies about the rows it reads. This suite proves the *CLI* honours those
//! results: a clean vault exits zero, an unmodelled rule is printed as a word
//! rather than counted as compliant, and a violated rule exits non-zero.

use std::process::Command;

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

const ALL_IDS: [&str; 10] = [
    "MI-1", "MI-2", "MI-3", "MI-4", "MI-5", "MI-6", "MI-7", "MI-8", "MI-9", "MI-10",
];

#[test]
fn clean_vault_reports_ten_rows_and_never_fakes_a_pass() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    assert!(run(&["vault", "init"], &home).status.success());

    let out = run(&["vault", "invariants", "--json"], &home);
    assert!(
        out.status.success(),
        "a clean vault must pass the gate: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON envelope");
    assert_eq!(value["command"], "vault.invariants");
    let data = &value["data"];
    assert_eq!(data["schema"], "mn-invariants/1");
    assert_eq!(data["gate"], "pass");
    assert_eq!(data["has_violations"], false);

    let rows = data["rows"].as_array().expect("rows array");
    assert_eq!(rows.len(), 10);
    let ids: Vec<&str> = rows
        .iter()
        .map(|row| row["id"].as_str().expect("row id"))
        .collect();
    assert_eq!(ids, ALL_IDS);

    // MI-3, MI-6 and MI-10 are plan-only columns. Counting them as a pass
    // would claim a schema the vault does not have.
    for row in rows {
        let id = row["id"].as_str().unwrap();
        if matches!(id, "MI-3" | "MI-6" | "MI-10") {
            assert_eq!(
                row["status"], "not_modelled",
                "{id} must not be reported as a pass"
            );
        }
    }
    assert_eq!(data["not_modelled"], 3);
    assert_eq!(data["violations"], 0);
}

#[test]
fn a_violated_rule_exits_non_zero() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    assert!(run(&["vault", "init"], &home).status.success());
    let seeded = run(&["vault", "remember", "gate-probe", "seed fact"], &home);
    assert!(
        seeded.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&seeded.stderr)
    );

    // Write an inverted validity window directly. The checker is read-only, so
    // this is the only way to make MI-1 fail without changing product code.
    let conn = rusqlite::Connection::open(home.join("vault.db")).expect("open vault");
    conn.execute(
        "UPDATE entities SET valid_to = valid_from - 1 WHERE name = 'gate-probe'",
        [],
    )
    .expect("seed inverted window");
    drop(conn);

    let out = run(&["vault", "invariants", "--json"], &home);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a violated invariant must fail the gate: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON envelope");
    let data = &value["data"];
    assert_eq!(data["gate"], "fail");
    assert_eq!(data["has_violations"], true);
    // Closing a window by hand can also trip MI-7 (closed window without a
    // version row). What matters for the gate is that at least one rule failed
    // and MI-1 in particular saw the inverted row.
    assert!(data["violations"].as_u64().unwrap() >= 1);
    let mi1 = data["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "MI-1")
        .expect("MI-1 row");
    assert_eq!(mi1["status"], "violation");
    assert!(mi1["affected_count"].as_u64().unwrap() >= 1);
}
