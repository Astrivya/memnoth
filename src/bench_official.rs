//! BENCH-08/09 — manifest registry and result validator for *official* external
//! benchmark suites (SWE-bench Lite / Verified / Pro V2, Terminal-Bench,
//! LongMemEval-V2 small tier, RULER, tau-bench).
//!
//! This module does **not** run a benchmark, call a model, or touch the network.
//! It is the bookkeeping layer that decides whether a committed result is
//! allowed to be called an official number, and whether two results may be
//! compared at all. It records, per suite:
//!
//! - exact version identity (suite id + pinned version label),
//! - the dataset hash, reader and harness that produced the result,
//! - official-vs-adapted-vs-derived status,
//! - the sample denominator (full suite size) and the number actually run,
//! - every per-task result and failure.
//!
//! HONESTY RULES (the point of the module):
//!
//! 1. A subset may never be presented as an official result. If a result claims
//!    `Official` but ran fewer tasks than its own denominator, validation fails
//!    closed. Mark it `Adapted` instead.
//! 2. Results may be compared only when their version, dataset hash, reader,
//!    harness, protocol status, denominator and sample count are *identical*.
//!    Different versions or protocols are refused, with the differing values
//!    named in the error.
//! 3. Suites whose correct version is retired or in conflict across published
//!    sources (SWE-bench Verified, Terminal-Bench, tau-bench) are recorded as
//!    `UNKNOWN until pinned`. Their identity is not accepted from a result
//!    file, they carry an explicit caveat, and they are refused in comparison.
//!    No score from such a suite may be published as official.
//!
//! The registry holds only what is safe to state without a live artifact. Where
//! a canonical dataset hash has not been committed it is recorded as `UNKNOWN`:
//! a comparison may then use the result-declared digest, but the caveat is
//! always surfaced. Nothing here fabricates a hash or a version.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::output::{ExitCode, Outcome};

/// Schema tag for the manifest/result documents this module understands.
pub const SCHEMA: &str = "mn-official-manifest/1";
/// Sentinel for an identity field that is not pinned by a committed artifact.
pub const UNKNOWN: &str = "UNKNOWN";

/// Whether a result is the official suite, an adapted run, or a derived
/// diagnostic. Only `Official` may be quoted as an official benchmark number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolStatus {
    Official,
    Adapted,
    Derived,
}

/// Whether a suite's version identity is established by a committed pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityState {
    Pinned,
    Unknown,
}

/// One row of the built-in manifest registry. These are the identity fields a
/// result must agree on; they are not inferred from the result being graded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteManifest {
    /// Stable key used by results and lookups (`swe-bench-verified`, ...).
    pub suite_id: String,
    /// Human family name.
    pub family: String,
    /// Exact pinned version label, or `UNKNOWN` until pinned.
    pub version: String,
    /// Canonical dataset digest, or `UNKNOWN` while no artifact is committed.
    pub dataset_sha256: String,
    /// Reader identity used to consume the dataset.
    pub reader: String,
    /// Harness / grader identity that produced the score.
    pub harness: String,
    /// Full official suite size, when it is known without a live download.
    pub sample_denominator: Option<u64>,
    /// True when the suite is retired and must not be quoted as a current rank.
    pub retired: bool,
    /// True when published versions conflict, so no single version is knowable.
    pub version_conflicted: bool,
    /// Honesty note attached to every validation of this suite.
    pub caveat: String,
}

/// A single graded task and its optional failure reason. Kept whole so a
/// failure is diagnosable from the committed JSON alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: String,
    pub passed: bool,
    #[serde(default)]
    pub failure: Option<String>,
}

/// The identity carried by one committed result. Comparison requires all of
/// these to match exactly (subject to the registry's UNKNOWN rules).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultIdentity {
    pub suite_id: String,
    pub suite_version: String,
    pub dataset_sha256: String,
    pub reader: String,
    pub harness: String,
    /// Protocol tag, e.g. `official-v1` vs `adapted-mn-reader-v2`.
    pub protocol: String,
    pub status: ProtocolStatus,
    /// Full suite size this result is graded against (its denominator).
    pub sample_denominator: u64,
    /// Number of tasks actually run.
    pub sample_count: u64,
}

/// One committed result document, including per-task outcomes and failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteResult {
    pub identity: ResultIdentity,
    #[serde(default)]
    pub tasks: Vec<TaskResult>,
    #[serde(default)]
    pub failures: Vec<String>,
}

/// Outcome of validating a single result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    pub suite_id: String,
    pub status: ProtocolStatus,
    pub identity_state: IdentityState,
    /// True only for a full, pinned, official run that may be quoted publicly.
    pub publishable_official: bool,
    pub tasks_total: usize,
    pub tasks_passed: usize,
    pub failures: Vec<String>,
    pub caveats: Vec<String>,
}

/// Outcome of comparing two results. Produced only when comparison is allowed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonReport {
    pub suite_id: String,
    pub status: ProtocolStatus,
    pub comparable: bool,
    pub sample_count: u64,
    pub sample_denominator: u64,
    pub caveats: Vec<String>,
}

/// CLI arguments for `mn bench official-verify`. Defined here so the only wiring
/// in `bench.rs` is one enum variant and one dispatch arm.
#[derive(Debug, clap::Args)]
pub struct OfficialVerifyArgs {
    /// Committed official result JSON files to validate (repeatable).
    #[arg(long)]
    pub result: Vec<PathBuf>,
}

fn is_unknown(value: &str) -> bool {
    value.trim().is_empty() || value == UNKNOWN
}

/// A suite's identity is unknowable when it is retired, its versions conflict,
/// or the registry has no pinned version label for it.
fn identity_unknown(manifest: &SuiteManifest) -> bool {
    manifest.retired || manifest.version_conflicted || is_unknown(&manifest.version)
}

/// The built-in registry. No network, no downloaded artifact: fields that
/// cannot be stated honestly are `UNKNOWN` and carry a caveat. Written as
/// struct literals rather than a long positional helper so each pinned field
/// is readable at the call site and no clippy arity lint can fire.
pub fn registry() -> Vec<SuiteManifest> {
    vec![
        SuiteManifest {
            suite_id: "swe-bench-lite".into(),
            family: "SWE-bench Lite".into(),
            version: "lite".into(),
            dataset_sha256: UNKNOWN.into(),
            reader: "official SWE-bench reader".into(),
            harness: "SWE-bench official evaluation harness".into(),
            sample_denominator: Some(300),
            retired: false,
            version_conflicted: false,
            caveat: "Lite is a fixed subset of SWE-bench; never label a hand-picked ten-issue run as the Lite suite. Pin the dataset hash before publishing.".into(),
        },
        SuiteManifest {
            suite_id: "swe-bench-verified".into(),
            family: "SWE-bench Verified".into(),
            version: UNKNOWN.into(),
            dataset_sha256: UNKNOWN.into(),
            reader: "official SWE-bench reader".into(),
            harness: "SWE-bench official evaluation harness".into(),
            sample_denominator: Some(500),
            retired: true,
            version_conflicted: true,
            caveat: "RETIRED: SWE-bench Verified is retired and published sources conflict (e.g. 97% vs 79%) on any current ranking. Identity and all scores are UNKNOWN until a specific dataset snapshot and grader are pinned by hash; do not quote a current rank.".into(),
        },
        SuiteManifest {
            suite_id: "swe-bench-pro-v2".into(),
            family: "SWE-bench Pro V2".into(),
            version: "pro-v2".into(),
            dataset_sha256: UNKNOWN.into(),
            reader: "official SWE-bench reader".into(),
            harness: "SWE-bench official evaluation harness (network locks, clean-image regrading)".into(),
            sample_denominator: None,
            retired: false,
            version_conflicted: false,
            caveat: "Pro V2 (2026-11-21) added network locks and clean-image regrading after checksum forging was detected. Pin the exact artifact hash and grader image before publishing.".into(),
        },
        SuiteManifest {
            suite_id: "terminal-bench".into(),
            family: "Terminal-Bench".into(),
            version: UNKNOWN.into(),
            dataset_sha256: UNKNOWN.into(),
            reader: "Terminal-Bench official reader".into(),
            harness: "Terminal-Bench pinned-release harness".into(),
            sample_denominator: None,
            retired: false,
            version_conflicted: true,
            caveat: "Version labeling is ambiguous across published sections (2.1 vs 4.0 vs 4.1); the same model scored 89.4% on 2.1 and 19.1% on 4.0. Pin one exact release and its official results artifact before quoting a number.".into(),
        },
        SuiteManifest {
            suite_id: "longmemeval-v2-small".into(),
            family: "LongMemEval-V2 small tier".into(),
            version: "v2-small-tier".into(),
            dataset_sha256: UNKNOWN.into(),
            reader: "LongMemEval-V2 official reader".into(),
            harness: "LongMemEval-V2 small-tier evaluator".into(),
            sample_denominator: None,
            retired: false,
            version_conflicted: false,
            caveat: "Use the official small tier with the original reader/configuration and official submission packaging. The LongMemEval-S derived diagnostic is a separate, non-official suite and must not be substituted.".into(),
        },
        SuiteManifest {
            suite_id: "ruler".into(),
            family: "RULER".into(),
            version: "original-protocol".into(),
            dataset_sha256: UNKNOWN.into(),
            reader: "RULER original reader".into(),
            harness: "RULER official synthetic-task generator/evaluator".into(),
            sample_denominator: None,
            retired: false,
            version_conflicted: false,
            caveat: "Keep the original model-context protocol separate from any Memnoth retrieval/compaction adaptation. Label adaptations `Adapted` or `Derived`; report selected lengths/tasks, not a full-suite claim.".into(),
        },
        SuiteManifest {
            suite_id: "tau-bench".into(),
            family: "tau-bench (tau2-bench)".into(),
            version: UNKNOWN.into(),
            dataset_sha256: UNKNOWN.into(),
            reader: "tau2-bench official reader".into(),
            harness: "tau2-bench version-pinned harness".into(),
            sample_denominator: None,
            retired: false,
            version_conflicted: true,
            caveat: "Different tau-bench versions are not input-identical. Pin the exact version and data before any comparison; customer-service results do not prove coding-agent quality.".into(),
        },
    ]
}

/// Look up a suite's manifest by id.
pub fn manifest_for(suite_id: &str) -> Option<SuiteManifest> {
    registry().into_iter().find(|m| m.suite_id == suite_id)
}

/// Collect the explicit result failures and every per-task failure reason into
/// one deterministic, sorted list. Nothing is dropped or silently inferred.
fn combined_failures(result: &SuiteResult) -> Vec<String> {
    let mut failures: BTreeSet<String> = BTreeSet::new();
    for failure in &result.failures {
        failures.insert(failure.clone());
    }
    for task in &result.tasks {
        if let Some(reason) = &task.failure {
            failures.insert(format!("{}: {}", task.task_id, reason));
        }
    }
    failures.into_iter().collect()
}

/// Validate one committed result against the registry. Hard refusals (unknown
/// suite, subset-as-official, inconsistent counts, hash mismatch) are errors;
/// UNKNOWN / retired suites validate with caveats but are never publishable.
pub fn validate_result(result: &SuiteResult) -> Result<ValidationReport> {
    let id = &result.identity;
    let manifest = manifest_for(&id.suite_id).ok_or_else(|| {
        anyhow::anyhow!(
            "suite {:?} is not in the official manifest registry",
            id.suite_id
        )
    })?;

    if id.sample_denominator == 0 {
        bail!(
            "{}: sample_denominator must be greater than zero",
            id.suite_id
        );
    }
    if id.sample_count > id.sample_denominator {
        bail!(
            "{}: sample_count {} exceeds sample_denominator {}",
            id.suite_id,
            id.sample_count,
            id.sample_denominator
        );
    }
    if !is_unknown(&manifest.dataset_sha256) && manifest.dataset_sha256 != id.dataset_sha256 {
        bail!(
            "{}: dataset hash mismatch: result {} != registry {}",
            id.suite_id,
            id.dataset_sha256,
            manifest.dataset_sha256
        );
    }
    if let Some(expected) = manifest.sample_denominator
        && id.sample_denominator != expected
    {
        bail!(
            "{}: sample_denominator {} does not match registry {}",
            id.suite_id,
            id.sample_denominator,
            expected
        );
    }
    if id.status == ProtocolStatus::Official && id.sample_count < id.sample_denominator {
        bail!(
            "subset-as-official refused for {}: ran {} of {} tasks; publish the full official suite or mark status=adapted",
            id.suite_id,
            id.sample_count,
            id.sample_denominator
        );
    }

    let identity_state = if identity_unknown(&manifest) {
        IdentityState::Unknown
    } else {
        IdentityState::Pinned
    };

    let mut caveats = Vec::new();
    if manifest.retired {
        caveats.push(format!(
            "{} is retired; never quote it as a current ranking",
            manifest.family
        ));
    }
    if manifest.version_conflicted {
        caveats.push(format!(
            "published versions of {} conflict; no single version is knowable",
            manifest.family
        ));
    }
    if identity_state == IdentityState::Unknown {
        caveats.push(format!(
            "version identity for {} is UNKNOWN until pinned; do not quote any score as official",
            id.suite_id
        ));
    }
    if is_unknown(&manifest.dataset_sha256) {
        caveats.push(
            "canonical dataset hash is not committed; comparison relies on the result-declared digest"
                .into(),
        );
    }
    if !manifest.caveat.is_empty() && !caveats.contains(&manifest.caveat) {
        caveats.push(manifest.caveat.clone());
    }
    if id.status != ProtocolStatus::Official {
        caveats.push(format!(
            "status is {:?}; this is not an official {} score",
            id.status, manifest.family
        ));
    }

    let publishable_official =
        id.status == ProtocolStatus::Official && identity_state == IdentityState::Pinned;

    Ok(ValidationReport {
        suite_id: id.suite_id.clone(),
        status: id.status,
        identity_state,
        publishable_official,
        tasks_total: result.tasks.len(),
        tasks_passed: result.tasks.iter().filter(|task| task.passed).count(),
        failures: combined_failures(result),
        caveats,
    })
}

/// Refuse when two results disagree on any identity field, naming the exact
/// differing values in the error.
fn require_same(field: &str, left: &str, right: &str) -> Result<()> {
    if left == right {
        Ok(())
    } else {
        bail!("cannot compare results with different {field}: {left:?} vs {right:?}")
    }
}

/// Compare two results for a matched claim. Both are validated first; a subset,
/// an unknown/retired identity, or any version/protocol mismatch is refused.
pub fn compare(left: &SuiteResult, right: &SuiteResult) -> Result<ComparisonReport> {
    let left_report = validate_result(left)?;
    let right_report = validate_result(right)?;

    if left.identity.suite_id != right.identity.suite_id {
        bail!(
            "cannot compare different suites: {:?} vs {:?}",
            left.identity.suite_id,
            right.identity.suite_id
        );
    }
    if left_report.identity_state == IdentityState::Unknown
        || right_report.identity_state == IdentityState::Unknown
    {
        bail!(
            "cannot compare {}: version identity is UNKNOWN until pinned ({})",
            left.identity.suite_id,
            left_report.caveats.join("; ")
        );
    }
    if left.identity.status != right.identity.status {
        bail!(
            "cannot compare results with different protocols: {:?} vs {:?}",
            left.identity.status,
            right.identity.status
        );
    }

    require_same(
        "suite_version",
        &left.identity.suite_version,
        &right.identity.suite_version,
    )?;
    require_same(
        "dataset_sha256",
        &left.identity.dataset_sha256,
        &right.identity.dataset_sha256,
    )?;
    require_same("reader", &left.identity.reader, &right.identity.reader)?;
    require_same("harness", &left.identity.harness, &right.identity.harness)?;
    require_same(
        "protocol",
        &left.identity.protocol,
        &right.identity.protocol,
    )?;

    if left.identity.sample_denominator != right.identity.sample_denominator {
        bail!(
            "cannot compare results with different denominators: {} vs {}",
            left.identity.sample_denominator,
            right.identity.sample_denominator
        );
    }
    if left.identity.sample_count != right.identity.sample_count {
        bail!(
            "cannot compare results with different sample counts: {} vs {} (subset vs full is not a matched comparison)",
            left.identity.sample_count,
            right.identity.sample_count
        );
    }

    let mut caveats = left_report.caveats;
    for caveat in right_report.caveats {
        if !caveats.contains(&caveat) {
            caveats.push(caveat);
        }
    }

    Ok(ComparisonReport {
        suite_id: left.identity.suite_id.clone(),
        status: left.identity.status,
        comparable: true,
        sample_count: left.identity.sample_count,
        sample_denominator: left.identity.sample_denominator,
        caveats,
    })
}

/// `mn bench official-verify` — emit the registry and validate/compare any
/// committed result files. Non-zero when a validated result is not a
/// publishable official run. No benchmark is executed here.
pub fn run(args: &OfficialVerifyArgs) -> Result<Outcome> {
    let registry_values: Vec<Value> = registry()
        .into_iter()
        .map(|manifest| serde_json::to_value(manifest).map_err(anyhow::Error::from))
        .collect::<Result<_>>()?;

    let mut validated = Vec::new();
    let mut comparisons = Vec::new();
    let mut first: Option<SuiteResult> = None;
    for path in &args.result {
        let raw = std::fs::read_to_string(path)?;
        let result: SuiteResult = serde_json::from_str(&raw)?;
        let report = validate_result(&result)?;
        if let Some(base) = &first {
            comparisons.push(compare(base, &result)?);
        } else {
            first = Some(result);
        }
        validated.push(report);
    }

    let all_publishable = validated.iter().all(|report| report.publishable_official);
    let data = json!({
        "schema": SCHEMA,
        "note": "Manifest registry and validation only; no benchmark run, model or network involved.",
        "registry": registry_values,
        "validated": validated,
        "comparisons": comparisons,
    });
    let text = format!(
        "official manifest registry: {} suites; validated {} result(s); {} comparison(s){}{}",
        registry().len(),
        validated.len(),
        comparisons.len(),
        if all_publishable {
            ""
        } else {
            "; one or more results are not publishable official runs"
        },
        if comparisons.is_empty() {
            ""
        } else {
            "; all comparisons matched version/protocol"
        },
    );
    let mut outcome = Outcome::new("bench.official-verify", text, data);
    if !all_publishable {
        outcome.exit_code = ExitCode::Unexpected as u8;
    }
    Ok(outcome)
}
