//! Observation gate for the committed deterministic bench baseline.
//!
//! `ci/bench-baseline.json` is the dated baseline captured by
//! `scripts/bench-baseline.sh` from the model-free `mn bench` verifier lanes.
//! This test validates that artifact: its schema, its provenance fields, and
//! — critically — that **every recorded lane carries the classification
//! "derived diagnostic, not official score"**. A lane result that drifts to an
//! official-score claim fails here.
//!
//! This is a pure artifact check: no CLI is invoked, no model is loaded, no
//! network is used, and the commit/date values are checked for shape rather
//! than pinned, so a regenerated baseline remains valid.

use std::path::{Path, PathBuf};

use serde_json::Value;

const SCHEMA: &str = "mn-bench-baseline/1";
const CLASSIFICATION: &str = "derived diagnostic, not official score";
const MANDATORY_LANES: &[&str] = &["trs", "kg", "handoff", "repoqa", "lme", "proof"];
const OPTIONAL_LANES: &[&str] = &["mts"];

fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/bench-baseline.json")
}

fn load_baseline() -> Value {
    let path = baseline_path();
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn is_hex_of_len(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_iso_date_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 10
        && b[0..4].iter().all(|c| c.is_ascii_digit())
        && b[4] == b'-'
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[7] == b'-'
        && b[8..10].iter().all(|c| c.is_ascii_digit())
}

/// A passing lane has a zero exit, a truthful ok flag, and no failed cases.
fn assert_lane_passes(lane: &str, value: &Value) {
    assert_eq!(
        value["exit_code"].as_u64(),
        Some(0),
        "lane {lane} did not exit 0: {value}"
    );
    assert_eq!(value["ok"], true, "lane {lane} envelope ok is not true");
    assert_eq!(
        value["model_free"], true,
        "lane {lane} is not marked model-free: {value}"
    );
    let command = value["command"].as_str().unwrap_or_default();
    assert!(
        command.starts_with("mn bench "),
        "lane {lane} has no reproducible mn bench command: {command}"
    );

    let data = &value["data"];
    assert!(data.is_object(), "lane {lane} has no data object: {value}");
    if let Some(gate) = data["gate"].as_str() {
        assert_eq!(gate, "pass", "lane {lane} gate is not pass: {data}");
    }
    if let Some(failed) = data["failed"].as_u64() {
        assert_eq!(failed, 0, "lane {lane} reported failed cases: {data}");
    }
    if lane == "proof" {
        let validated = data["validated"]
            .as_array()
            .expect("proof lane validated rows");
        assert!(!validated.is_empty(), "proof lane validated no results");
    }
}

#[test]
fn baseline_has_the_expected_schema_and_provenance() {
    let baseline = load_baseline();
    assert!(baseline.is_object(), "baseline must be a JSON object");
    assert_eq!(
        baseline["schema"].as_str(),
        Some(SCHEMA),
        "unexpected baseline schema: {}",
        baseline["schema"]
    );

    let captured = baseline["captured_at"]
        .as_str()
        .expect("captured_at is a string");
    assert!(
        is_iso_date_prefix(captured) && captured.contains('T'),
        "captured_at must be an ISO timestamp: {captured}"
    );
    let date = baseline["date"].as_str().expect("date is a string");
    assert_eq!(
        &captured[..10],
        date,
        "date must match captured_at's ISO date"
    );

    let commit = baseline["commit"].as_str().expect("commit is a string");
    assert!(
        is_hex_of_len(commit, 40),
        "commit must be a full 40-character git sha: {commit}"
    );
    assert!(
        baseline["commit_dirty"].is_boolean(),
        "commit_dirty must record whether the tree was dirty"
    );
    let binary = &baseline["binary"];
    assert!(
        binary["path"].as_str().is_some_and(|s| !s.is_empty()),
        "binary path must be recorded"
    );
    assert!(
        binary["version"].as_str().is_some_and(|s| !s.is_empty()),
        "binary version must be recorded"
    );
}

#[test]
fn every_lane_result_carries_the_derived_diagnostic_classification() {
    let baseline = load_baseline();
    assert_eq!(
        baseline["classification"].as_str(),
        Some(CLASSIFICATION),
        "top-level classification drifted from the required phrase"
    );

    let lanes = baseline["lanes"].as_object().expect("lanes is an object");
    assert!(!lanes.is_empty(), "baseline recorded no lanes");

    for (name, lane) in lanes {
        assert_eq!(
            lane["classification"].as_str(),
            Some(CLASSIFICATION),
            "lane {name} does not carry '{CLASSIFICATION}': {lane}"
        );
        // The classification is not cosmetic: no lane may also claim to be an
        // official score, whatever its own upstream label happens to say.
        let data = &lane["data"];
        assert_ne!(
            data["official"].as_bool(),
            Some(true),
            "lane {name} marks itself official: {data}"
        );
        assert_ne!(
            data["score_kind"].as_str(),
            Some("official"),
            "lane {name} uses score_kind=official: {data}"
        );
        assert_ne!(
            data["status"].as_str(),
            Some("full"),
            "lane {name} claims a full (publishable as official) result: {data}"
        );
    }
}

#[test]
fn baseline_is_complete_model_free_and_hashes_are_pinned() {
    let baseline = load_baseline();
    for field in ["model_free", "no_network", "complete"] {
        assert_eq!(
            baseline[field].as_bool(),
            Some(true),
            "{field} must be true for a published baseline: {baseline}"
        );
    }

    let combined = baseline["dataset_sha256"]
        .as_str()
        .expect("dataset_sha256 is a string");
    assert!(
        is_hex_of_len(combined, 64),
        "dataset_sha256 must be 64 hex characters: {combined}"
    );

    let datasets = baseline["datasets"]
        .as_object()
        .expect("datasets is an object");
    assert!(!datasets.is_empty(), "no dataset hashes were pinned");
    for (path, hash) in datasets {
        assert!(
            path.starts_with("ci/") && path.ends_with(".json"),
            "dataset key must be a committed ci fixture: {path}"
        );
        let hash = hash.as_str().unwrap_or_default();
        assert!(
            is_hex_of_len(hash, 64),
            "dataset {path} has a bad sha256: {hash}"
        );
    }

    let notes = baseline["notes"].as_str().unwrap_or_default();
    assert!(
        notes.contains("derived diagnostic"),
        "notes must restate the derived-diagnostic limitation: {notes}"
    );
}

#[test]
fn all_mandatory_lanes_are_recorded_and_passed() {
    let baseline = load_baseline();
    let lanes = baseline["lanes"].as_object().expect("lanes is an object");

    for name in MANDATORY_LANES {
        let lane = lanes
            .get(*name)
            .unwrap_or_else(|| panic!("mandatory lane {name} is missing from the baseline"));
        assert_lane_passes(name, lane);
    }

    // An optional lane is only recorded when the CLI actually shipped it; if
    // it is present it must meet the same gate as the mandatory lanes.
    for name in OPTIONAL_LANES {
        if let Some(lane) = lanes.get(*name) {
            assert_lane_passes(name, lane);
        }
    }
}
