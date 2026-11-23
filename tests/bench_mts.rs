//! The MTS-60 derived diagnostic is a gate: bad patches, forbidden edits and
//! wrong deterministic checks all exit non-zero, with no model or network.
//!
//! The module is pulled in by path so this test compiles before the parent
//! applies the `lib.rs` + `bench.rs` wiring diff. The `output` shim re-exports
//! the real `Outcome`/`ExitCode` so `crate::output` resolves here exactly as it
//! does in the library build.

#[path = "../src/bench_mts.rs"]
mod bench_mts;

pub mod output {
    pub use memnoth::output::{ExitCode, Outcome};
}

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/mts-cases.json")
}

fn load_fixture() -> Value {
    let raw = std::fs::read_to_string(fixture_path()).expect("read committed fixture");
    serde_json::from_str(&raw).expect("parse committed fixture")
}

fn write_fixture(dir: &Path, value: &Value) -> PathBuf {
    let path = dir.join("mutated.json");
    std::fs::write(&path, value.to_string()).expect("write mutated fixture");
    path
}

fn find_case(fixture: &Value, category: &str) -> usize {
    fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .position(|row| row["category"] == category)
        .expect("fixture must contain the category")
}

fn lines(text: &str) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n");
    let body = normalized.strip_suffix('\n').unwrap_or(&normalized);
    if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').map(str::to_string).collect()
    }
}

/// A minimal full-file replacement hunk, used to forge a forbidden edit.
fn full_file_hunk(path: &str, old: &str, new: &str) -> String {
    let old_lines = lines(old);
    let new_lines = lines(new);
    let mut out = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -1,{} +1,{} @@\n",
        old_lines.len(),
        new_lines.len()
    );
    for line in old_lines {
        out.push('-');
        out.push_str(&line);
        out.push('\n');
    }
    for line in new_lines {
        out.push('+');
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[test]
fn the_committed_deterministic_suite_passes_without_a_model() {
    let report = bench_mts::verify(&fixture_path()).expect("verify committed fixture");
    assert_eq!(report["schema"], "mn-mts-result/1");
    assert_eq!(report["gate"], "pass", "report: {report}");
    assert_eq!(report["cases"], 60);
    assert_eq!(report["passed"], 60);
    assert_eq!(report["failed"], 0);
    assert_eq!(report["official"], false);
    assert_eq!(report["basis"], "derived");
    assert!(
        report["label"]
            .as_str()
            .unwrap()
            .contains("NOT an official Terminal-Bench score")
    );
    assert!(
        report["disclaimer"]
            .as_str()
            .unwrap()
            .contains("No model, no network")
    );

    for category in [
        "exact-file",
        "line-count",
        "token-present",
        "json-field",
        "absent-marker",
        "multi-file",
    ] {
        assert_eq!(report["categories"][category]["cases"], 10, "{category}");
        assert_eq!(report["categories"][category]["failed"], 0, "{category}");
    }
    let rows = report["results"].as_array().unwrap();
    assert_eq!(rows.len(), 60);
    for row in rows {
        assert_eq!(row["patch_applies"], true, "row: {row}");
        assert_eq!(row["forbidden_touched"], json!([]), "row: {row}");
        assert_eq!(row["check"]["passed"], true, "row: {row}");
        assert_eq!(row["passed"], true, "row: {row}");
    }

    let outcome =
        bench_mts::run(&bench_mts::MtsVerifyArgs { cases: None }).expect("run default fixture");
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.command, "bench.mts-verify");
    assert!(
        outcome
            .text
            .contains("NOT an official Terminal-Bench score")
    );
}

#[test]
fn a_wrong_deterministic_expectation_fails_the_case_and_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = load_fixture();
    let index = find_case(&fixture, "exact-file");
    fixture["cases"][index]["expect"]["content"] = json!("an invented answer");
    let path = write_fixture(dir.path(), &fixture);

    let report = bench_mts::verify(&path).expect("verify mutated fixture");
    assert_eq!(report["gate"], "fail");
    assert_eq!(report["failed"], 1);
    assert_eq!(report["results"][index]["patch_applies"], true);
    assert_eq!(report["results"][index]["check"]["passed"], false);
    assert!(
        report["results"][index]["reason"]
            .as_str()
            .unwrap()
            .contains("deterministic_check")
    );

    let outcome = bench_mts::run(&bench_mts::MtsVerifyArgs { cases: Some(path) })
        .expect("run mutated fixture");
    assert_ne!(outcome.exit_code, 0, "a wrong check must exit non-zero");
}

#[test]
fn an_edit_to_a_forbidden_file_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = load_fixture();
    let index = find_case(&fixture, "exact-file");
    let target = fixture["cases"][index]["forbidden"][0]
        .as_str()
        .unwrap()
        .to_string();

    let old = fixture["cases"][index]["seed"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == target)
        .expect("forbidden file must be seeded")["content"]
        .as_str()
        .unwrap()
        .to_string();
    let forged = full_file_hunk(&target, &old, "TAMPERED\n");
    let patch = fixture["cases"][index]["patch"]
        .as_str()
        .unwrap()
        .to_string();
    fixture["cases"][index]["patch"] = json!(format!("{patch}{forged}"));
    let path = write_fixture(dir.path(), &fixture);

    let report = bench_mts::verify(&path).expect("verify tampered fixture");
    assert_eq!(report["gate"], "fail");
    assert_eq!(report["failed"], 1);
    let row = &report["results"][index];
    // The intended edit still applies and its check still passes; only the
    // anti-gaming gate fires, so this test cannot pass for the wrong reason.
    assert_eq!(row["patch_applies"], true, "row: {row}");
    assert_eq!(row["check"]["passed"], true, "row: {row}");
    assert!(
        row["forbidden_touched"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some(target.as_str())),
        "row: {row}"
    );
    assert!(
        row["reason"]
            .as_str()
            .unwrap()
            .contains("forbidden_file_edit")
    );
}

#[test]
fn a_patch_that_does_not_apply_to_the_clean_checkout_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = load_fixture();
    let index = find_case(&fixture, "exact-file");
    let patch = fixture["cases"][index]["patch"]
        .as_str()
        .unwrap()
        .to_string();
    // Corrupt the first removed line's context without changing hunk counts.
    let mut replaced = false;
    let corrupted = patch
        .lines()
        .map(|line| {
            if !replaced && line.starts_with('-') && !line.starts_with("--- ") {
                replaced = true;
                format!("{line} # altered context")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert!(replaced, "fixture patch must contain a removed line");
    fixture["cases"][index]["patch"] = json!(corrupted);
    let path = write_fixture(dir.path(), &fixture);

    let report = bench_mts::verify(&path).expect("verify corrupt patch");
    assert_eq!(report["gate"], "fail");
    assert_eq!(report["failed"], 1);
    let row = &report["results"][index];
    assert_eq!(row["patch_applies"], false, "row: {row}");
    assert_eq!(row["check"]["observed"], json!(null), "row: {row}");
    assert!(
        row["reason"]
            .as_str()
            .unwrap()
            .contains("patch_does_not_apply")
    );
}

#[test]
fn the_fixture_rejects_a_case_count_that_is_not_ten_per_category() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = load_fixture();
    fixture["cases"].as_array_mut().unwrap().pop();
    let path = write_fixture(dir.path(), &fixture);
    assert!(
        bench_mts::verify(&path).is_err(),
        "a 59-case fixture must be rejected before grading"
    );
}
