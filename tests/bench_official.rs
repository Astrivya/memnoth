//! Integration coverage for BENCH-08/09: the official-suite manifest registry
//! and validator. The module is pulled in by path so it compiles against the
//! real `Outcome`/`ExitCode` shim without wiring the CLI first (same pattern as
//! `tests/bench_repoqa.rs`). No model, no network, no benchmark is executed.

#[path = "../src/bench_official.rs"]
mod bench_official;

pub mod output {
    pub use memnoth::output::{ExitCode, Outcome};
}

use bench_official::*;
use output::ExitCode;

/// Declared sample size for a suite when a test does not override it. The
/// registry pins Lite to 300 and Verified to 500; the other suites are recorded
/// here with a small self-consistent denominator for the fixtures.
fn default_denominator(suite_id: &str) -> u64 {
    match suite_id {
        "swe-bench-verified" => 500,
        _ => 300,
    }
}

fn identity_with(
    suite_id: &str,
    version: &str,
    status: ProtocolStatus,
    denominator: u64,
) -> ResultIdentity {
    ResultIdentity {
        suite_id: suite_id.into(),
        suite_version: version.into(),
        dataset_sha256: "a1b2c3d4".into(),
        reader: "reader-v1".into(),
        harness: "harness-v1".into(),
        protocol: format!("{status:?}").to_lowercase(),
        status,
        sample_denominator: denominator,
        sample_count: denominator,
    }
}

fn identity(suite_id: &str, version: &str, status: ProtocolStatus) -> ResultIdentity {
    identity_with(suite_id, version, status, default_denominator(suite_id))
}

fn passing_tasks(n: u64) -> Vec<TaskResult> {
    (0..n)
        .map(|i| TaskResult {
            task_id: format!("t{i}"),
            passed: true,
            failure: None,
        })
        .collect()
}

/// A self-consistent result: one row per declared sample.
fn suite_result(suite_id: &str, version: &str, status: ProtocolStatus) -> SuiteResult {
    let denominator = default_denominator(suite_id);
    SuiteResult {
        identity: identity(suite_id, version, status),
        tasks: passing_tasks(denominator),
        failures: Vec::new(),
    }
}

/// A full Lite result with one recorded task failure and one harness failure.
fn lite_result(status: ProtocolStatus) -> SuiteResult {
    let mut result = suite_result("swe-bench-lite", "lite", status);
    result.tasks[1] = TaskResult {
        task_id: "t1".into(),
        passed: false,
        failure: Some("timeout".into()),
    };
    result
        .failures
        .push("harness: t1 timed out after 60s".into());
    result
}

#[test]
fn registry_covers_every_named_suite() {
    let rows = registry();
    for id in [
        "swe-bench-lite",
        "swe-bench-verified",
        "swe-bench-pro-v2",
        "terminal-bench",
        "longmemeval-v2-small",
        "ruler",
        "tau-bench",
    ] {
        let manifest = manifest_for(id).unwrap_or_else(|| panic!("registry missing {id}"));
        assert_eq!(manifest.suite_id, id);
    }
    assert!(manifest_for("not-a-suite").is_none());
    assert_eq!(rows.len(), 7);
}

#[test]
fn compatible_results_are_accepted() {
    let a = lite_result(ProtocolStatus::Official);
    let b = lite_result(ProtocolStatus::Official);

    let va = validate_result(&a).expect("validate a");
    assert_eq!(va.identity_state, IdentityState::Pinned);
    assert!(
        va.publishable_official,
        "a full pinned official run is publishable"
    );
    assert_eq!(va.status, ProtocolStatus::Official);

    let comparison = compare(&a, &b).expect("identical results must compare");
    assert!(comparison.comparable);
    assert_eq!(comparison.suite_id, "swe-bench-lite");
    assert_eq!(comparison.sample_count, 300);
    assert_eq!(comparison.sample_denominator, 300);
}

#[test]
fn different_versions_are_refused() {
    let a = lite_result(ProtocolStatus::Official);
    let mut b = lite_result(ProtocolStatus::Official);
    b.identity.suite_version = "lite-older-snapshot".into();

    let err = compare(&a, &b).expect_err("different versions must be refused");
    assert!(
        err.to_string().contains("different suite_version"),
        "{err:#}"
    );
    assert!(err.to_string().contains("lite-older-snapshot"), "{err:#}");
}

#[test]
fn different_datasets_readers_or_harnesses_are_refused() {
    let a = lite_result(ProtocolStatus::Official);

    let mut b = lite_result(ProtocolStatus::Official);
    b.identity.dataset_sha256 = "deadbeef".into();
    let err = compare(&a, &b).expect_err("different dataset hashes must be refused");
    assert!(
        err.to_string().contains("different dataset_sha256"),
        "{err:#}"
    );

    let mut c = lite_result(ProtocolStatus::Official);
    c.identity.reader = "reader-v2".into();
    let err = compare(&a, &c).expect_err("different readers must be refused");
    assert!(err.to_string().contains("different reader"), "{err:#}");

    let mut d = lite_result(ProtocolStatus::Official);
    d.identity.harness = "harness-v2".into();
    let err = compare(&a, &d).expect_err("different harnesses must be refused");
    assert!(err.to_string().contains("different harness"), "{err:#}");
}

#[test]
fn different_protocols_or_statuses_are_refused() {
    let a = lite_result(ProtocolStatus::Official);

    let mut b = lite_result(ProtocolStatus::Official);
    b.identity.protocol = "adapted-mn-reader-v2".into();
    let err = compare(&a, &b).expect_err("different protocols must be refused");
    assert!(err.to_string().contains("different protocol"), "{err:#}");

    let c = lite_result(ProtocolStatus::Adapted);
    let err = compare(&a, &c).expect_err("official vs adapted must be refused");
    assert!(err.to_string().contains("different protocols"), "{err:#}");
}

#[test]
fn subset_presented_as_official_is_refused() {
    let mut result = lite_result(ProtocolStatus::Official);
    result.identity.sample_count = 10;
    result.tasks.truncate(10);

    let err = validate_result(&result).expect_err("subset-as-official must be refused");
    let message = err.to_string();
    assert!(message.contains("subset-as-official"), "{message}");
    assert!(message.contains("ran 10 of 300 tasks"), "{message}");

    // The refusal also propagates through comparison.
    let full = lite_result(ProtocolStatus::Official);
    assert!(compare(&result, &full).is_err());

    // Marking the same small run as adapted is permitted (with a caveat).
    result.identity.status = ProtocolStatus::Adapted;
    let report = validate_result(&result).expect("adapted subset is allowed");
    assert!(!report.publishable_official);
    assert!(report.caveats.iter().any(|c| c.contains("not an official")));
}

#[test]
fn unequal_sample_counts_are_not_a_matched_comparison() {
    let a = lite_result(ProtocolStatus::Adapted);
    let mut b = lite_result(ProtocolStatus::Adapted);
    b.identity.sample_count = 200;
    b.tasks.truncate(200);
    let err = compare(&a, &b).expect_err("different sample counts must be refused");
    assert!(
        err.to_string().contains("different sample counts"),
        "{err:#}"
    );
}

#[test]
fn retired_verified_suite_carries_unknown_caveat() {
    let manifest = manifest_for("swe-bench-verified").expect("verified manifest");
    assert!(manifest.retired);
    assert!(manifest.version_conflicted);
    assert_eq!(manifest.version, UNKNOWN);
    let registry_caveat = manifest.caveat.clone();
    assert!(registry_caveat.to_lowercase().contains("retired"));
    assert!(registry_caveat.to_lowercase().contains("conflict"));
    assert!(registry_caveat.contains(UNKNOWN));

    let result = suite_result("swe-bench-verified", UNKNOWN, ProtocolStatus::Official);
    let report =
        validate_result(&result).expect("validation surfaces caveats rather than a fake pin");
    assert_eq!(report.identity_state, IdentityState::Unknown);
    assert!(!report.publishable_official);
    let joined = report.caveats.join(" | ");
    assert!(joined.contains("retired"), "{joined}");
    assert!(joined.contains("conflict"), "{joined}");
    assert!(joined.contains(UNKNOWN), "{joined}");

    // Even two results agreeing on the UNKNOWN identity cannot be compared.
    let other = result.clone();
    let err = compare(&result, &other).expect_err("UNKNOWN identity must be refused");
    assert!(err.to_string().contains(UNKNOWN), "{err:#}");
}

#[test]
fn terminal_bench_and_tau_bench_version_conflicts_are_unknown() {
    for id in ["terminal-bench", "tau-bench"] {
        let manifest = manifest_for(id).unwrap();
        assert!(manifest.version_conflicted, "{id} has conflicting versions");
        assert_eq!(manifest.version, UNKNOWN);
        let result = suite_result(id, UNKNOWN, ProtocolStatus::Official);
        let report = validate_result(&result).unwrap();
        assert_eq!(report.identity_state, IdentityState::Unknown, "{id}");
        assert!(!report.publishable_official, "{id}");
        assert!(
            report.caveats.iter().any(|c| c.contains("conflict")),
            "{id}"
        );
    }
}

#[test]
fn pro_v2_and_v2_small_are_pinned_and_comparable() {
    for (id, version) in [
        ("swe-bench-pro-v2", "pro-v2"),
        ("longmemeval-v2-small", "v2-small-tier"),
    ] {
        let a = suite_result(id, version, ProtocolStatus::Official);
        let b = a.clone();
        let report = validate_result(&a).unwrap();
        assert_eq!(report.identity_state, IdentityState::Pinned, "{id}");
        assert!(report.publishable_official, "{id}");
        assert!(
            compare(&a, &b).is_ok(),
            "{id} identical results should compare"
        );
    }
}

#[test]
fn per_task_results_and_failures_are_recorded() {
    let report = validate_result(&lite_result(ProtocolStatus::Official)).unwrap();
    assert_eq!(report.tasks_total, 300);
    assert_eq!(report.tasks_passed, 299);
    assert_eq!(report.failures.len(), 2);
    let failures = report.failures.join(" | ");
    assert!(failures.contains("t1: timeout"), "{failures}");
    assert!(failures.contains("harness: t1 timed out"), "{failures}");
}

#[test]
fn unknown_suite_is_rejected() {
    let result = suite_result("mystery-suite", "1", ProtocolStatus::Official);
    let err = validate_result(&result).expect_err("unregistered suite must be refused");
    assert!(
        err.to_string()
            .contains("not in the official manifest registry"),
        "{err:#}"
    );
}

#[test]
fn run_emits_registry_and_validates_files() {
    let empty = run(&OfficialVerifyArgs { result: Vec::new() }).expect("run with no files");
    assert_eq!(empty.exit_code, 0);
    assert_eq!(empty.command, "bench.official-verify");
    assert_eq!(empty.data["schema"], SCHEMA);
    assert_eq!(empty.data["registry"].as_array().unwrap().len(), 7);
    assert!(empty.data["note"].is_string());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.json");
    std::fs::write(
        &path,
        serde_json::to_string(&lite_result(ProtocolStatus::Official)).unwrap(),
    )
    .unwrap();

    let outcome = run(&OfficialVerifyArgs { result: vec![path] }).expect("run with a clean result");
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.data["validated"].as_array().unwrap().len(), 1);
    assert_eq!(outcome.data["comparisons"].as_array().unwrap().len(), 0);

    // A non-publishable result (retired Verified) returns a non-zero code.
    let retired = suite_result("swe-bench-verified", UNKNOWN, ProtocolStatus::Official);
    let retired_path = dir.path().join("retired.json");
    std::fs::write(&retired_path, serde_json::to_string(&retired).unwrap()).unwrap();
    let outcome = run(&OfficialVerifyArgs {
        result: vec![retired_path],
    })
    .expect("run retired");
    assert_eq!(outcome.exit_code, ExitCode::Unexpected as u8);
}
