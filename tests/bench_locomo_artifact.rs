//! The LoCoMo raw-artifact validator is the publication gate for that suite:
//! a run missing provenance, a subset passed off as official, a headline score
//! with no raw rows behind it, or an unsourced superlative must all be rejected.
//! These tests compile the module directly from source so it stays standalone
//! until it is wired into the CLI command tree.

#[path = "../src/bench_locomo_artifact.rs"]
mod bench_locomo_artifact;

use serde_json::{Value, json};

use bench_locomo_artifact::{
    CommittedArtifact, MeasurementStatus, REQUIRED_FIELDS, ScoreKind, format_violations, validate,
    validate_artifact_file, validate_scoreboard, validate_scoreboard_files, validate_value,
};

const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PUB: &str = "https://example.org/datasets/locomo-v1";

/// Build `n` item rows, the first `passes` of which pass.
fn rows(n: usize, passes: usize) -> Value {
    (0..n)
        .map(|i| {
            json!({
                "item_id": format!("item-{i}"),
                "prediction": format!("answer-{i}"),
                "gold": format!("answer-{i}"),
                "pass": i < passes,
            })
        })
        .collect::<Vec<Value>>()
        .into()
}

/// A complete, publishable raw-run artifact. Every required field is present
/// and well formed; the (19/20 = 0.95) reported score matches the rows and the
/// competitor superlative in the claim is properly cited.
fn valid_artifact() -> Value {
    json!({
        "schema": "mn-locomo-artifact/1",
        "result_id": "locomo-full-20261120",
        "dataset_sha256": SHA,
        "dataset_source": PUB,
        "status": "full",
        "judge_version": "gpt-4o@2026-11-10",
        "reader_version": "mn-reader-2.3.1",
        "rows": rows(20, 19),
        "sample_size": 20,
        "failures": ["item-7 timed out under seed 20261120"],
        "cost_basis": "provider usage at the pinned price table, 3 trials",
        "reproduction": "mn bench locomo verify --seed 20261120 --json",
        "observed_at": "2026-11-20",
        "held_out": true,
        "score_kind": "official",
        "score": 0.95,
        "claim": "our reader is the best on long-context memory",
        "source_url": "https://example.org/results/locomo-full.json",
    })
}

fn assert_rejected(value: &Value, code: &str) {
    let errs = validate_value(value).expect_err("artifact must be rejected");
    assert!(
        errs.iter().any(|e| e.code == code),
        "expected a '{code}' violation, got: {}",
        format_violations(&errs)
    );
}

#[test]
fn a_valid_artifact_passes_and_extracts_its_fields() {
    let artifact = validate(&valid_artifact().to_string()).expect("valid artifact must pass");
    assert_eq!(artifact.result_id, "locomo-full-20261120");
    assert_eq!(artifact.dataset_sha256, SHA);
    assert_eq!(artifact.dataset_source, PUB);
    assert_eq!(artifact.status, MeasurementStatus::Full);
    assert_eq!(artifact.judge_version, "gpt-4o@2026-11-10");
    assert_eq!(artifact.reader_version, "mn-reader-2.3.1");
    assert_eq!(artifact.rows.len(), 20);
    assert_eq!(artifact.failures.len(), 1);
    assert_eq!(
        artifact.cost_basis,
        "provider usage at the pinned price table, 3 trials"
    );
    assert_eq!(artifact.observed_at, "2026-11-20");
    assert!(artifact.held_out);
    assert_eq!(artifact.score_kind, ScoreKind::Official);
    assert_eq!(artifact.reported_score, Some(0.95));
    assert!((artifact.computed_score - 0.95).abs() < 1e-9);
}

#[test]
fn every_required_field_can_be_removed_to_reject_the_artifact() {
    for field in REQUIRED_FIELDS {
        let mut value = valid_artifact();
        value.as_object_mut().unwrap().remove(*field);
        assert_rejected(&value, "missing_field");
    }
}

#[test]
fn a_null_required_field_is_rejected() {
    let mut value = valid_artifact();
    value["dataset_sha256"] = Value::Null;
    assert_rejected(&value, "missing_field");
}

#[test]
fn malformed_types_and_domains_are_rejected() {
    let mut v = valid_artifact();
    v["dataset_sha256"] = json!("not-a-hash");
    assert_rejected(&v, "invalid_value");

    let mut v = valid_artifact();
    v["dataset_source"] = json!("./local/private/locomo.jsonl");
    assert_rejected(&v, "invalid_value");

    let mut v = valid_artifact();
    v["status"] = json!("preliminary");
    assert_rejected(&v, "invalid_value");

    let mut v = valid_artifact();
    v["score_kind"] = json!("best-effort");
    assert_rejected(&v, "invalid_value");

    let mut v = valid_artifact();
    v["observed_at"] = json!("yesterday");
    assert_rejected(&v, "invalid_value");

    let mut v = valid_artifact();
    v["held_out"] = json!("yes");
    assert_rejected(&v, "invalid_type");

    let mut v = valid_artifact();
    v["judge_version"] = json!(42);
    assert_rejected(&v, "invalid_type");
}

#[test]
fn moving_version_labels_are_rejected_for_judge_and_reader() {
    for field in ["judge_version", "reader_version"] {
        for label in ["latest", "HEAD", "unpinned", "main"] {
            let mut v = valid_artifact();
            v[field] = json!(label);
            assert_rejected(&v, "unpinned_version");
        }
    }
}

#[test]
fn a_subset_labelled_official_is_rejected() {
    let mut value = valid_artifact();
    value["status"] = json!("subset");
    assert_rejected(&value, "subset_as_official");
}

#[test]
fn a_derived_diagnostic_labelled_official_is_rejected() {
    let mut value = valid_artifact();
    value["score_kind"] = json!("derived_diagnostic");
    value["official"] = json!(true);
    assert_rejected(&value, "subset_as_official");
}

#[test]
fn a_full_result_may_be_official_and_a_subset_may_be_a_diagnostic() {
    validate(&valid_artifact().to_string()).expect("a full official result must pass");

    let mut subset = valid_artifact();
    subset["status"] = json!("subset");
    subset["score_kind"] = json!("derived_diagnostic");
    validate(&subset.to_string()).expect("a subset diagnostic must pass");
}

#[test]
fn an_unsourced_competitor_adjective_is_rejected() {
    let mut value = valid_artifact();
    value.as_object_mut().unwrap().remove("source_url");
    assert_rejected(&value, "unsourced_claim");
}

#[test]
fn a_sourced_competitor_adjective_is_allowed() {
    // The default valid artifact has a superlative plus a source_url.
    validate(&valid_artifact().to_string()).expect("a cited claim must pass");
}

#[test]
fn every_item_row_needs_prediction_gold_and_pass() {
    for field in bench_locomo_artifact::ROW_FIELDS {
        let mut value = valid_artifact();
        value["rows"][0].as_object_mut().unwrap().remove(*field);
        assert_rejected(&value, "invalid_row");
    }
}

#[test]
fn duplicate_item_rows_and_an_empty_row_set_are_rejected() {
    let mut value = valid_artifact();
    value["rows"][1]["item_id"] = json!("item-0");
    assert_rejected(&value, "duplicate_item");

    let mut value = valid_artifact();
    value["rows"] = json!([]);
    value["sample_size"] = json!(0);
    assert_rejected(&value, "invalid_value");
}

#[test]
fn the_declared_sample_size_must_match_the_number_of_rows() {
    let mut value = valid_artifact();
    value["sample_size"] = json!(19);
    assert_rejected(&value, "row_count_mismatch");
}

#[test]
fn a_reported_score_must_match_the_recomputed_pass_rate() {
    let mut value = valid_artifact();
    value["score"] = json!(0.99);
    assert_rejected(&value, "score_mismatch");

    let mut value = valid_artifact();
    value["score"] = json!(1.56);
    assert_rejected(&value, "invalid_value");
}

#[test]
fn a_reported_score_with_no_raw_rows_is_rejected() {
    // This is the artifact-level form of the "95.65% claim": a headline
    // number with no raw item rows behind it.
    let mut value = valid_artifact();
    value["rows"] = json!([]);
    value["sample_size"] = json!(0);
    value["score"] = json!(0.9565);
    assert_rejected(&value, "unbacked_score");
}

#[test]
fn non_json_and_non_object_inputs_are_rejected() {
    assert_eq!(validate("not json").unwrap_err()[0].code, "invalid_json");
    assert_eq!(validate("[]").unwrap_err()[0].code, "not_object");
}

// ---- scoreboard recomputation ---------------------------------------------

fn committed() -> CommittedArtifact {
    CommittedArtifact::new("results/locomo-full.json", valid_artifact().to_string())
}

fn scoreboard_doc(score: f64) -> String {
    json!({
        "schema": "mn-locomo-scoreboard/1",
        "rows": [
            { "result_id": "locomo-full", "score": score,
              "source": "results/locomo-full.json" },
        ]
    })
    .to_string()
}

#[test]
fn a_scoreboard_matching_the_committed_raw_artifact_passes() {
    let check = validate_scoreboard(&scoreboard_doc(0.95), &[committed()])
        .expect("matching scoreboard must pass");
    assert_eq!(check.rows, 1);
    assert_eq!(check.sources, vec!["results/locomo-full.json"]);
}

#[test]
fn a_95_65_percent_row_with_no_raw_artifact_is_rejected() {
    // The classic hand-typed headline: 95.65% with nothing behind it.
    let errs = validate_scoreboard(&scoreboard_doc(0.9565), &[]).unwrap_err();
    assert!(
        errs.iter().any(|e| e.code == "unbacked_score"),
        "{}",
        format_violations(&errs)
    );
}

#[test]
fn a_score_that_drifts_from_the_raw_artifact_is_rejected() {
    let errs = validate_scoreboard(&scoreboard_doc(0.9565), &[committed()]).unwrap_err();
    assert!(
        errs.iter().any(|e| e.code == "score_mismatch"),
        "{}",
        format_violations(&errs)
    );
}

#[test]
fn a_row_pointing_at_the_wrong_artifact_is_rejected() {
    let doc = json!({
        "rows": [
            { "score": 0.95, "source": "results/typed-by-hand.json" },
        ]
    })
    .to_string();
    let errs = validate_scoreboard(&doc, &[committed()]).unwrap_err();
    assert!(errs.iter().any(|e| e.code == "unbacked_score"));
}

#[test]
fn a_row_may_not_publish_a_derived_or_subset_run_as_official() {
    let mut artifact_value = valid_artifact();
    artifact_value["score_kind"] = json!("derived_diagnostic");
    let doc = json!({
        "rows": [
            { "score": 0.95, "score_kind": "official",
              "source": "results/locomo-full.json" },
        ]
    })
    .to_string();
    let artifacts = [CommittedArtifact::new(
        "results/locomo-full.json",
        artifact_value.to_string(),
    )];
    let errs = validate_scoreboard(&doc, &artifacts).unwrap_err();
    assert!(
        errs.iter().any(|e| e.code == "subset_as_official"),
        "{}",
        format_violations(&errs)
    );
}

#[test]
fn an_invalid_committed_artifact_cannot_back_a_row() {
    let mut value = valid_artifact();
    value.as_object_mut().unwrap().remove("dataset_sha256");
    let artifacts = [CommittedArtifact::new(
        "results/locomo-full.json",
        value.to_string(),
    )];
    let errs = validate_scoreboard(&scoreboard_doc(0.95), &artifacts).unwrap_err();
    assert!(errs.iter().any(|e| e.code == "missing_field"));
}

#[test]
fn an_unsourced_adjective_in_the_scoreboard_document_is_rejected() {
    let doc = json!({
        "headline": "the fastest long-context reader available",
        "rows": [
            { "score": 0.95, "source": "results/locomo-full.json" },
        ]
    })
    .to_string();
    let errs = validate_scoreboard(&doc, &[committed()]).unwrap_err();
    assert!(errs.iter().any(|e| e.code == "unsourced_claim"));
}

#[test]
fn a_non_object_scoreboard_document_is_rejected() {
    assert_eq!(
        validate_scoreboard("not json", &[]).unwrap_err()[0].code,
        "invalid_json"
    );
    let errs = validate_scoreboard("[]", &[]).unwrap_err();
    assert!(errs.iter().any(|e| e.code == "missing_field"));
}

#[test]
fn the_file_helpers_validate_committed_files_without_a_network() {
    use std::path::PathBuf;

    assert_eq!(MeasurementStatus::Full.as_str(), "full");
    assert_eq!(MeasurementStatus::Subset.as_str(), "subset");
    assert_eq!(ScoreKind::DerivedDiagnostic.as_str(), "derived_diagnostic");
    assert_eq!(ScoreKind::Official.as_str(), "official");

    let dir = std::env::temp_dir().join(format!("mn-bench-locomo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let artifact = dir.join("locomo-full.json");
    std::fs::write(&artifact, valid_artifact().to_string()).unwrap();
    let validated = validate_artifact_file(&artifact).unwrap();
    assert_eq!(validated.result_id, "locomo-full-20261120");
    assert_eq!(
        CommittedArtifact::from_file(&artifact).unwrap().source,
        artifact.display().to_string()
    );

    let doc_path = dir.join("scoreboard.json");
    let file_doc = json!({
        "rows": [
            { "score": 0.95, "source": artifact.display().to_string() },
        ]
    })
    .to_string();
    std::fs::write(&doc_path, file_doc).unwrap();
    let paths: Vec<PathBuf> = vec![artifact];
    let check = validate_scoreboard_files(&doc_path, &paths).unwrap();
    assert_eq!(check.rows, 1);

    std::fs::remove_dir_all(&dir).unwrap();
}
