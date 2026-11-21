//! The proof validator is the publication gate: a result missing provenance,
//! a subset passed off as official, an unsourced superlative, or a scoreboard
//! that drifted from its committed JSON must all be rejected. These tests
//! compile the module directly from source so it stays standalone until it is
//! wired into the CLI command tree.

#[path = "../src/bench_proof.rs"]
mod bench_proof;

use serde_json::{Value, json};

use bench_proof::{
    CommittedResult, MeasurementStatus, validate, validate_scoreboard, validate_value,
};

const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A complete, publishable manifest. Every required field is present and well
/// formed, and a competitor superlative in the claim is properly cited.
fn valid_manifest() -> Value {
    json!({
        "schema": "mn-proof/1",
        "result_id": "mts-full",
        "metric": "raw_score",
        "commit": "a1b2c3d4e5f6",
        "dataset_sha256": SHA,
        "model_version": "claude-sonnet-4-5@2026-11-10",
        "reader_version": "mn-reader-2.3.1",
        "status": "full",
        "sample_size": 60,
        "trials": 3,
        "raw_score": 0.87,
        "uncertainty": { "ci_low": 0.81, "ci_high": 0.92, "level": 0.95 },
        "failures": ["task-17 timed out under seed 20261111"],
        "reproducibility": "reproducible: paired seeds 20261110..12",
        "observed_at": "2026-11-20",
        "claim": "our reader is the best on this suite",
        "source_url": "https://example.org/results/mts-full.json"
    })
}

fn assert_rejected(value: &Value, code: &str) {
    let errs = validate_value(value).expect_err("result must be rejected");
    assert!(
        errs.iter().any(|e| e.code == code),
        "expected a '{code}' violation, got: {}",
        bench_proof::format_violations(&errs)
    );
}

#[test]
fn a_valid_manifest_passes_and_extracts_its_fields() {
    let proof = validate(&valid_manifest().to_string()).expect("valid manifest must pass");
    assert_eq!(proof.commit, "a1b2c3d4e5f6");
    assert_eq!(proof.dataset_sha256, SHA);
    assert_eq!(proof.status, MeasurementStatus::Full);
    assert_eq!(proof.sample_size, 60);
    assert_eq!(proof.trials, 3);
    assert!((proof.raw_score - 0.87).abs() < 1e-9);
    assert!((proof.uncertainty.ci_low - 0.81).abs() < 1e-9);
    assert!((proof.uncertainty.ci_high - 0.92).abs() < 1e-9);
    assert_eq!(proof.failures.len(), 1);
    assert_eq!(proof.observed_at, "2026-11-20");
}

#[test]
fn every_required_field_can_be_removed_to_reject_the_result() {
    for field in bench_proof::REQUIRED_FIELDS {
        let mut value = valid_manifest();
        value.as_object_mut().unwrap().remove(*field);
        assert_rejected(&value, "missing_field");
    }
}

#[test]
fn a_null_required_field_is_rejected() {
    let mut value = valid_manifest();
    value["commit"] = Value::Null;
    assert_rejected(&value, "missing_field");
}

#[test]
fn malformed_field_types_and_domains_are_rejected() {
    let bad_commit = {
        let mut v = valid_manifest();
        v["commit"] = json!(42);
        v
    };
    assert_rejected(&bad_commit, "invalid_type");

    let bad_hash = {
        let mut v = valid_manifest();
        v["dataset_sha256"] = json!("not-a-hash");
        v
    };
    assert_rejected(&bad_hash, "invalid_value");

    let bad_status = {
        let mut v = valid_manifest();
        v["status"] = json!("preliminary");
        v
    };
    assert_rejected(&bad_status, "invalid_value");

    let zero_trials = {
        let mut v = valid_manifest();
        v["trials"] = json!(0);
        v
    };
    assert_rejected(&zero_trials, "invalid_value");

    let bad_interval = {
        let mut v = valid_manifest();
        v["uncertainty"]["ci_low"] = json!(0.99);
        v
    };
    assert_rejected(&bad_interval, "invalid_value");

    let bad_date = {
        let mut v = valid_manifest();
        v["observed_at"] = json!("yesterday");
        v
    };
    assert_rejected(&bad_date, "invalid_value");
}

#[test]
fn a_subset_labelled_official_is_rejected() {
    let mut value = valid_manifest();
    value["status"] = json!("subset");
    value["official"] = json!(true);
    assert_rejected(&value, "subset_as_official");
}

#[test]
fn an_adapted_result_with_an_official_label_is_rejected() {
    let mut value = valid_manifest();
    value["status"] = json!("adapted");
    value["tier"] = json!("official");
    assert_rejected(&value, "subset_as_official");
}

#[test]
fn a_full_result_may_be_labelled_official() {
    let mut value = valid_manifest();
    value["official"] = json!(true);
    validate_value(&value).expect("a genuine full result may be official");
}

#[test]
fn an_unsourced_competitor_adjective_is_rejected() {
    let mut value = valid_manifest();
    value.as_object_mut().unwrap().remove("source_url");
    assert_rejected(&value, "unsourced_claim");
}

#[test]
fn a_sourced_competitor_adjective_is_allowed() {
    // The default valid manifest has a superlative plus a source_url.
    validate(&valid_manifest().to_string()).expect("a cited claim must pass");
}

#[test]
fn non_json_and_non_object_inputs_are_rejected() {
    assert_eq!(validate("not json").unwrap_err()[0].code, "invalid_json");
    assert_eq!(validate("[]").unwrap_err()[0].code, "not_object");
}

// ---- scoreboard recomputation --------------------------------------------

fn result_for(id: &str, score: f64, source: &str) -> CommittedResult {
    let mut value = valid_manifest();
    value["result_id"] = json!(id);
    value["raw_score"] = json!(score);
    CommittedResult::new(source, value.to_string())
}

fn scoreboard_doc() -> String {
    json!({
        "schema": "mn-scoreboard/1",
        "rows": [
            { "result_id": "mts-full", "metric": "raw_score", "value": 0.87,
              "source": "results/mts-full.json" },
            { "result_id": "trs-full", "metric": "raw_score", "value": 0.62,
              "source": "results/trs-full.json" }
        ]
    })
    .to_string()
}

fn committed_results() -> Vec<CommittedResult> {
    vec![
        result_for("mts-full", 0.87, "results/mts-full.json"),
        result_for("trs-full", 0.62, "results/trs-full.json"),
    ]
}

#[test]
fn a_scoreboard_matching_committed_json_passes() {
    let check = validate_scoreboard(&scoreboard_doc(), &committed_results())
        .expect("matching scoreboard must pass");
    assert_eq!(check.rows, 2);
    assert_eq!(
        check.sources,
        vec!["results/mts-full.json", "results/trs-full.json"]
    );
}

#[test]
fn a_tampered_scoreboard_value_is_rejected() {
    let doc = json!({
        "schema": "mn-scoreboard/1",
        "rows": [
            { "result_id": "mts-full", "metric": "raw_score", "value": 0.99,
              "source": "results/mts-full.json" },
            { "result_id": "trs-full", "metric": "raw_score", "value": 0.62,
              "source": "results/trs-full.json" }
        ]
    })
    .to_string();
    let errs = validate_scoreboard(&doc, &committed_results()).unwrap_err();
    assert!(
        errs.iter().any(|e| e.code == "scoreboard_mismatch"),
        "{}",
        bench_proof::format_violations(&errs)
    );
}

#[test]
fn a_hand_typed_row_with_no_committed_result_is_rejected() {
    let doc = json!({
        "schema": "mn-scoreboard/1",
        "rows": [
            { "result_id": "mts-full", "metric": "raw_score", "value": 0.87,
              "source": "results/mts-full.json" },
            { "result_id": "trs-full", "metric": "raw_score", "value": 0.62,
              "source": "results/trs-full.json" },
            { "result_id": "kg-full", "metric": "raw_score", "value": 0.95,
              "source": "results/kg-full.json" }
        ]
    })
    .to_string();
    let errs = validate_scoreboard(&doc, &committed_results()).unwrap_err();
    assert!(errs.iter().any(|e| e.code == "unbacked_row"));
}

#[test]
fn a_row_pointing_at_the_wrong_committed_file_is_rejected() {
    let doc = json!({
        "schema": "mn-scoreboard/1",
        "rows": [
            { "result_id": "mts-full", "metric": "raw_score", "value": 0.87,
              "source": "results/typed-by-hand.json" },
            { "result_id": "trs-full", "metric": "raw_score", "value": 0.62,
              "source": "results/trs-full.json" }
        ]
    })
    .to_string();
    let errs = validate_scoreboard(&doc, &committed_results()).unwrap_err();
    assert!(errs.iter().any(|e| e.code == "scoreboard_mismatch"));
}

#[test]
fn an_invalid_or_omitted_committed_result_is_rejected() {
    // A committed result missing its commit fails validation, so the
    // scoreboard cannot be reproduced from it.
    let mut bad = result_for("trs-full", 0.62, "results/trs-full.json");
    let mut bad_value: Value = serde_json::from_str(&bad.json).unwrap();
    bad_value.as_object_mut().unwrap().remove("commit");
    bad.json = bad_value.to_string();

    let results = vec![result_for("mts-full", 0.87, "results/mts-full.json"), bad];
    let errs = validate_scoreboard(&scoreboard_doc(), &results).unwrap_err();
    assert!(errs.iter().any(|e| e.code == "missing_field"));

    // A valid committed result that is simply not published is also caught.
    let single_doc = json!({
        "schema": "mn-scoreboard/1",
        "rows": [
            { "result_id": "mts-full", "metric": "raw_score", "value": 0.87,
              "source": "results/mts-full.json" }
        ]
    })
    .to_string();
    let results = vec![
        result_for("mts-full", 0.87, "results/mts-full.json"),
        result_for("kg-full", 0.71, "results/kg-full.json"),
    ];
    let errs = validate_scoreboard(&single_doc, &results).unwrap_err();
    assert!(
        errs.iter().any(|e| e.code == "omitted_result"),
        "{}",
        bench_proof::format_violations(&errs)
    );
}

#[test]
fn the_file_helpers_validate_committed_files_without_a_network() {
    use std::path::PathBuf;

    assert_eq!(MeasurementStatus::Full.as_str(), "full");
    assert_eq!(MeasurementStatus::Subset.as_str(), "subset");
    assert_eq!(MeasurementStatus::Adapted.as_str(), "adapted");

    let dir = std::env::temp_dir().join(format!("mn-bench-proof-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let mts = dir.join("mts-full.json");
    let trs = dir.join("trs-full.json");
    std::fs::write(
        &mts,
        result_for("mts-full", 0.87, "results/mts-full.json").json,
    )
    .unwrap();
    std::fs::write(
        &trs,
        result_for("trs-full", 0.62, "results/trs-full.json").json,
    )
    .unwrap();

    let proof = bench_proof::validate_result_file(&mts).unwrap();
    assert_eq!(proof.commit, "a1b2c3d4e5f6");
    assert_eq!(
        CommittedResult::from_file(&mts).unwrap().source,
        mts.display().to_string()
    );

    let doc_path = dir.join("scoreboard.json");
    let file_doc = json!({
        "schema": "mn-scoreboard/1",
        "rows": [
            { "result_id": "mts-full", "metric": "raw_score", "value": 0.87,
              "source": mts.display().to_string() },
            { "result_id": "trs-full", "metric": "raw_score", "value": 0.62,
              "source": trs.display().to_string() }
        ]
    })
    .to_string();
    std::fs::write(&doc_path, file_doc).unwrap();
    let paths: Vec<PathBuf> = vec![mts, trs];
    let check = bench_proof::validate_scoreboard_files(&doc_path, &paths).unwrap();
    assert_eq!(check.rows, 2);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_non_object_uncertainty_is_rejected() {
    let mut value = valid_manifest();
    value["uncertainty"] = json!("0.81..0.92");
    assert_rejected(&value, "invalid_type");
}

#[test]
fn an_unsourced_adjective_in_the_scoreboard_document_is_rejected() {
    let doc = json!({
        "schema": "mn-scoreboard/1",
        "headline": "the fastest memory layer available",
        "rows": [
            { "result_id": "mts-full", "metric": "raw_score", "value": 0.87,
              "source": "results/mts-full.json" },
            { "result_id": "trs-full", "metric": "raw_score", "value": 0.62,
              "source": "results/trs-full.json" }
        ]
    })
    .to_string();
    let errs = validate_scoreboard(&doc, &committed_results()).unwrap_err();
    assert!(errs.iter().any(|e| e.code == "unsourced_claim"));
}
