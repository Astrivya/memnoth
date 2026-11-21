//! Integration coverage for the BENCH-05 RepoQA-derived diagnostic.
//!
//! The module is pulled in by path so this test compiles before the parent
//! applies the four-line `lib.rs` + `bench.rs` wiring diff. The `output` shim
//! re-exports the real `Outcome`/`ExitCode` so the module's `crate::output`
//! references resolve inside this test crate identically to the library build.

#[path = "../src/bench_repoqa.rs"]
mod bench_repoqa;

pub mod output {
    pub use memnoth::output::{ExitCode, Outcome};
}

fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/repoqa-cases.json")
}

#[test]
fn committed_fixture_passes_end_to_end() {
    let report = bench_repoqa::verify(&fixture_path()).expect("verify committed fixture");
    assert_eq!(report["schema"], "mn-repoqa-result/1");
    assert_eq!(report["gate"], "pass");
    assert_eq!(report["metrics"]["top_k"], 10);
    assert_eq!(report["metrics"]["recall_at_10"], 1.0);
    assert_eq!(report["metrics"]["mrr"], 1.0);
    // Honesty fields must be present: derived, not official, no extrapolation.
    let label = report["label"].as_str().unwrap();
    assert!(label.contains("NOT an official RepoQA score"));
    let disclaimer = report["disclaimer"].as_str().unwrap();
    assert!(disclaimer.contains("not extrapolated"));
    assert!(disclaimer.contains("official RepoQA score"));
    // Latency is reported, but explicitly kept out of the gate.
    assert_eq!(report["latency"]["included_in_gate"], false);
    assert!(report["latency"]["total_ms"].as_f64().unwrap() >= 0.0);
    assert!(report["latency"]["p95_ms"].as_f64().unwrap() >= 0.0);
}

#[test]
fn it_covers_at_least_two_languages() {
    let report = bench_repoqa::verify(&fixture_path()).expect("verify");
    let langs = report["languages"].as_object().unwrap();
    assert!(langs.len() >= 2, "languages: {langs:?}");
    assert!(langs.contains_key("rust"));
    assert!(langs.contains_key("python"));
    assert!(langs.contains_key("typescript"));
}

#[test]
fn a_regression_against_the_baseline_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("regressed.json");
    let raw = std::fs::read_to_string(fixture_path()).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    value["baseline"]["min_recall_at_10"] = serde_json::json!(1.2);
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

    let report = bench_repoqa::verify(&path).expect("verify regressed fixture");
    assert_eq!(report["gate"], "fail");
    assert!(
        report["regressions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str().unwrap().contains("recall@10"))
    );

    let outcome = bench_repoqa::run(&bench_repoqa::RepoQaVerifyArgs { cases: Some(path) })
        .expect("run regressed fixture");
    assert_ne!(outcome.exit_code, 0);
}

#[test]
fn a_healthy_baseline_exits_zero() {
    let outcome = bench_repoqa::run(&bench_repoqa::RepoQaVerifyArgs { cases: None })
        .expect("run default fixture");
    assert_eq!(outcome.exit_code, 0);
    assert!(outcome.text.contains("NOT an official RepoQA score"));
}
