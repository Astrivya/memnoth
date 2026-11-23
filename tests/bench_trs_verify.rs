//! The deterministic TRS lane is a gate: a wrong answer exits non-zero, and
//! no model, network or API key is involved.

use std::process::Command;

use serde_json::{Value, json};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

fn run(cases: &str) -> std::process::Output {
    Command::new(binary())
        .args(["bench", "trs-verify", "--json", "--cases", cases])
        .output()
        .expect("run memnoth bench trs-verify")
}

#[test]
fn the_committed_deterministic_suite_passes_without_a_model() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/trs-cases.json");
    let out = run(&path.to_string_lossy());
    assert!(
        out.status.success(),
        "TRS must pass against the engine itself: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let value: Value = serde_json::from_slice(&out.stdout).expect("JSON envelope");
    assert_eq!(value["command"], "bench.trs-verify");
    let data = &value["data"];
    assert_eq!(data["gate"], "pass");
    assert_eq!(data["failed"], 0);
    assert!(data["cases"].as_u64().unwrap() >= 7);
}

#[test]
fn a_wrong_ground_truth_fails_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wrong.json");
    std::fs::write(
        &path,
        r#"{"schema":"mn-trs/1","cases":[{"id":"bad","kind":"current","entity":"e",
            "ops":[{"op":"add","name":"e","valid_from":1000,"recorded_at":1000},
                   {"op":"fact","content":"truth","valid_from":1000,"recorded_at":1000}],
            "at":2000,"expect":["fabrication"]}]}"#,
    )
    .unwrap();

    let out = run(&path.to_string_lossy());
    assert_eq!(
        out.status.code(),
        Some(1),
        "a failed deterministic case must fail the gate"
    );
    let value: Value = serde_json::from_slice(&out.stdout).expect("JSON envelope");
    assert_eq!(value["data"]["gate"], "fail");
    assert_eq!(value["data"]["failed"], json!(1));
}

#[test]
fn the_envelope_is_stamped_as_a_derived_non_official_diagnostic() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/trs-cases.json");
    let out = run(&path.to_string_lossy());
    assert!(
        out.status.success(),
        "fixture must pass: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let value: Value = serde_json::from_slice(&out.stdout).expect("JSON envelope");
    let data = &value["data"];
    assert_eq!(data["diagnostic"], "derived");
    assert_eq!(data["official"], false);
    assert_eq!(data["gate"], "pass");
    assert_eq!(data["schema"], "mn-trs-result/1");
    let note = data["note"].as_str().expect("note");
    assert!(note.starts_with("Derived diagnostic of"), "note: {note}");
    assert!(note.contains("not an official"), "note: {note}");
    assert!(
        note.contains("not comparable to vendor-reported results"),
        "note: {note}"
    );
    // No wall-clock timestamp exists in this lane; never fabricate one.
    assert!(data.get("observed_at").is_none());
}

#[test]
fn the_output_never_claims_to_be_an_official_score() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/trs-cases.json");
    let out = run(&path.to_string_lossy());
    let raw = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
    for phrase in [
        "official score",
        "official benchmark score",
        "official result",
        "vendor-reported score",
        "is an official",
    ] {
        assert!(
            !raw.contains(phrase),
            "output must not present this lane as an official score; found {phrase:?}"
        );
    }
}
