//! Integration test for the standalone LME-derived diagnostic module.
//!
//! The module is included by source path so this test does not depend on the
//! `mn bench lme-verify` wiring landing first (that wiring belongs to the
//! parent agent). The shim supplies the two `crate::output` types the module
//! uses, re-exported from the real library, so the graded code is the same code
//! the CLI will dispatch to once wired.

#[path = "../src/bench_lme.rs"]
mod bench_lme;

mod output {
    pub use memnoth::output::{ExitCode, Outcome};
}

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::bench_lme::{LmeVerifyArgs, verify};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/lme-cases.json")
}

#[test]
fn the_committed_derived_diagnostic_passes() {
    let report = verify(&fixture()).expect("verify committed fixture");
    assert_eq!(report["gate"], "pass", "fixture: {report}");
    assert_eq!(report["failed"], json!(0));
    assert_eq!(report["diagnostic"], "derived");
    assert_eq!(report["official"], false);
    assert_eq!(report["reader"], "memnoth_engine::current_facts_known");

    let cases = report["cases"].as_u64().unwrap();
    assert!(
        cases >= 5,
        "expected all five question classes, got {cases}"
    );

    let kinds: Vec<&str> = report["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["kind"].as_str().unwrap())
        .collect();
    for kind in ["current", "as-of", "knowledge", "why", "change"] {
        assert!(kinds.contains(&kind), "fixture must exercise {kind}");
    }
}

#[test]
fn a_regression_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("regression.json");
    std::fs::write(
        &path,
        r#"{"schema":"mn-lme-derived/1","cases":[{"id":"reg","kind":"current",
            "question":"q?","entity":"e","sessions":[
              {"id":"s1","date":1000,"user":"asha","facts":[
                {"content":"real","valid_from":1000,"valid_to":null,"recorded_at":1000}]},
              {"id":"s2","date":2000,"user":"bob","facts":[
                {"content":"real","valid_from":2000,"valid_to":null,"recorded_at":2000}]}],
            "at":3000,"expect":["invented"]}]}"#,
    )
    .unwrap();

    let outcome = crate::bench_lme::run(&LmeVerifyArgs { cases: Some(path) })
        .expect("run derived diagnostic");

    // This is the exact exit code the CLI dispatcher will turn into a process
    // exit: a failed case must be a non-zero gate, never a printed opinion.
    assert_eq!(outcome.exit_code, 1);
    assert_eq!(outcome.command, "bench.lme-verify");
    let data: &Value = &outcome.data;
    assert_eq!(data["gate"], "fail");
    assert_eq!(data["failed"], 1);
    assert_eq!(data["results"][0]["observed"][0], "real");
}

#[test]
fn the_report_never_claims_to_be_an_official_score() {
    let outcome = crate::bench_lme::run(&LmeVerifyArgs {
        cases: Some(fixture()),
    })
    .expect("run derived diagnostic against committed fixture");

    assert_eq!(outcome.exit_code, 0);
    assert!(
        outcome.text.contains("derived diagnostic"),
        "text must self-identify as a derived diagnostic: {}",
        outcome.text
    );
    assert!(
        !outcome.text.to_lowercase().contains("official score"),
        "text must not claim an official score: {}",
        outcome.text
    );
    assert_eq!(outcome.data["official"], false);
}
