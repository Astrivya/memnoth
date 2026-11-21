//! Standalone proof validator for benchmark results and the scoreboard.
//!
//! This module is deliberately dependency-light and has **no** `crate::`
//! references and performs **no** network I/O. Its inputs are JSON strings
//! (and, in the file helpers, local paths), so it can be compiled on its own
//! — e.g. by an integration test using `#[path = "../src/bench_proof.rs"]`
//! — before it is wired into the CLI command tree.
//!
//! It enforces the publication gate described by BENCH-10/11/12:
//!
//! * [`validate`] requires every field a published number must trace to
//!   (commit, dataset hash, model/reader versions, measurement status,
//!   sample size, trials, raw score, uncertainty interval, failures list,
//!   reproducibility status, observation date). Missing or malformed fields
//!   are rejected.
//! * A subset or adapted result may never be labelled "official" and an
//!   unsourced competitor superlative is rejected.
//! * [`validate_scoreboard`] recomputes the scoreboard from the committed
//!   result JSON and compares it to the published document, so a scoreboard
//!   cannot be hand-typed or drift from its artifacts.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Fields every committed result must carry before it may be published.
pub const REQUIRED_FIELDS: &[&str] = &[
    "commit",
    "dataset_sha256",
    "model_version",
    "reader_version",
    "status",
    "sample_size",
    "trials",
    "raw_score",
    "uncertainty",
    "failures",
    "reproducibility",
    "observed_at",
];

/// Free-text fields scanned for unsourced competitor adjectives (BENCH-12).
pub const CLAIM_TEXT_FIELDS: &[&str] = &[
    "claim",
    "marketing_claim",
    "competitor_claim",
    "headline",
    "title",
    "summary",
    "description",
    "notes",
];

/// Keys whose non-empty string value counts as a citation for a claim.
pub const CITATION_FIELDS: &[&str] = &["source_url", "citation", "claim_source", "evidence_url"];

/// Superlatives / competitor adjectives that require a source when published.
pub const COMPETITOR_ADJECTIVES: &[&str] = &[
    "best",
    "fastest",
    "leading",
    "unmatched",
    "unrivaled",
    "unrivalled",
    "unbeatable",
    "superior",
    "outperforms",
    "beats",
    "winner",
    "top",
    "sota",
    "state-of-the-art",
    "no. 1",
    "#1",
    "number one",
];

/// Measurement scope of a result. Only `Full` may be published as official.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MeasurementStatus {
    #[default]
    Full,
    Subset,
    Adapted,
}

impl MeasurementStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            MeasurementStatus::Full => "full",
            MeasurementStatus::Subset => "subset",
            MeasurementStatus::Adapted => "adapted",
        }
    }
}

/// The uncertainty interval attached to a raw score.
#[derive(Debug, Clone, PartialEq)]
pub struct Uncertainty {
    pub ci_low: f64,
    pub ci_high: f64,
    pub level: f64,
}

/// A validated result. Building one means every required field passed.
#[derive(Debug, Clone, PartialEq)]
pub struct Proof {
    pub commit: String,
    pub dataset_sha256: String,
    pub model_version: String,
    pub reader_version: String,
    pub status: MeasurementStatus,
    pub sample_size: u64,
    pub trials: u64,
    pub raw_score: f64,
    pub uncertainty: Uncertainty,
    pub failures: Vec<String>,
    pub reproducibility: String,
    pub observed_at: String,
}

/// A single validation failure. `field` is the offending JSON field or source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub code: &'static str,
    pub message: String,
    pub field: Option<String>,
}

impl Violation {
    pub fn new(code: &'static str, message: impl Into<String>, field: Option<String>) -> Self {
        Self {
            code,
            message: message.into(),
            field,
        }
    }

    /// Attach a source path when the violation came from a committed file and
    /// it has no more specific field already.
    pub fn with_source(mut self, source: &str) -> Self {
        if self.field.is_none() {
            self.field = Some(source.to_string());
        }
        self
    }
}

/// Human-readable rendering of violations, for a CLI error line.
pub fn format_violations(violations: &[Violation]) -> String {
    violations
        .iter()
        .map(|v| match &v.field {
            Some(field) => format!("{}: {} ({field})", v.code, v.message),
            None => format!("{}: {}", v.code, v.message),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Validate a committed result from its JSON text.
///
/// Returns the extracted [`Proof`] on success, or every violation found on
/// failure. Parsing, field presence, types and the special publication rules
/// are all checked; no field is inferred or defaulted.
pub fn validate(result_json: &str) -> Result<Proof, Vec<Violation>> {
    let value: Value = serde_json::from_str(result_json).map_err(|e| {
        vec![Violation::new(
            "invalid_json",
            format!("result is not valid JSON: {e}"),
            None,
        )]
    })?;
    validate_value(&value)
}

/// Validate an already-parsed result value. See [`validate`].
pub fn validate_value(value: &Value) -> Result<Proof, Vec<Violation>> {
    let mut v = Vec::new();
    let obj: &Map<String, Value> = match value.as_object() {
        Some(o) => o,
        None => {
            return Err(vec![Violation::new(
                "not_object",
                "result must be a JSON object",
                None,
            )]);
        }
    };

    // 1. Presence. A field that is absent or null is a hard rejection.
    for &field in REQUIRED_FIELDS {
        match obj.get(field) {
            None => v.push(Violation::new(
                "missing_field",
                format!("missing required field: {field}"),
                Some(field.to_string()),
            )),
            Some(Value::Null) => v.push(Violation::new(
                "missing_field",
                format!("required field is null: {field}"),
                Some(field.to_string()),
            )),
            Some(_) => {}
        }
    }

    // 2. Types and value domains.
    let commit = req_string(obj, "commit", &mut v);
    if let Some(c) = &commit
        && c.trim().is_empty()
    {
        v.push(Violation::new(
            "invalid_value",
            "commit must be non-empty",
            Some("commit".into()),
        ));
    }

    let dataset_sha256 = req_string(obj, "dataset_sha256", &mut v);
    if let Some(h) = &dataset_sha256
        && !is_sha256_hex(h)
    {
        v.push(Violation::new(
            "invalid_value",
            "dataset_sha256 must be 64 hex characters",
            Some("dataset_sha256".into()),
        ));
    }

    let model_version = req_string(obj, "model_version", &mut v);
    let reader_version = req_string(obj, "reader_version", &mut v);
    let status = req_status(obj, &mut v);
    let sample_size = req_u64(obj, "sample_size", &mut v);
    let trials = req_u64(obj, "trials", &mut v);
    let raw_score = req_f64(obj, "raw_score", &mut v);
    let uncertainty = req_uncertainty(obj, &mut v);
    let reproducibility = req_string(obj, "reproducibility", &mut v);
    let observed_at = req_string(obj, "observed_at", &mut v);
    if let Some(d) = &observed_at
        && !is_iso_date_like(d)
    {
        v.push(Violation::new(
            "invalid_value",
            "observed_at must start with an ISO date (YYYY-MM-DD)",
            Some("observed_at".into()),
        ));
    }

    let failures: Vec<String> = match obj.get("failures") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item.as_str() {
                Some(s) => s.to_string(),
                None => {
                    v.push(Violation::new(
                        "invalid_type",
                        "each failure must be a string",
                        Some("failures".into()),
                    ));
                    "<non-string failure>".to_string()
                }
            })
            .collect(),
        Some(_) => {
            v.push(Violation::new(
                "invalid_type",
                "failures must be an array",
                Some("failures".into()),
            ));
            Vec::new()
        }
    };

    // 3. Publication rules (BENCH-12).
    let status_text = obj.get("status").and_then(Value::as_str).unwrap_or("");
    if status_text != "full" && is_labelled_official(value) {
        v.push(Violation::new(
            "subset_as_official",
            "a subset or adapted result must never be labelled official",
            Some("status".into()),
        ));
    }
    check_claims(value, &mut v);

    if !v.is_empty() {
        return Err(v);
    }

    Ok(Proof {
        commit: commit.expect("validated"),
        dataset_sha256: dataset_sha256.expect("validated"),
        model_version: model_version.expect("validated"),
        reader_version: reader_version.expect("validated"),
        status: status.expect("validated"),
        sample_size: sample_size.expect("validated"),
        trials: trials.expect("validated"),
        raw_score: raw_score.expect("validated"),
        uncertainty: uncertainty.expect("validated"),
        failures,
        reproducibility: reproducibility.expect("validated"),
        observed_at: observed_at.expect("validated"),
    })
}

/// A committed result file: its committed path plus its JSON contents. The
/// caller owns I/O, keeping the validator pure and network-free.
#[derive(Debug, Clone)]
pub struct CommittedResult {
    /// Committed path/label, used as the `source` in the scoreboard.
    pub source: String,
    /// Raw JSON text of the result file.
    pub json: String,
}

impl CommittedResult {
    pub fn new(source: impl Into<String>, json: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            json: json.into(),
        }
    }

    /// Read a committed result from a local path (never network).
    pub fn from_file(path: &Path) -> std::io::Result<Self> {
        let json = std::fs::read_to_string(path)?;
        Ok(Self {
            source: path.display().to_string(),
            json,
        })
    }
}

/// Summary of a scoreboard that successfully reproduced its committed JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreboardCheck {
    pub rows: usize,
    pub sources: Vec<String>,
}

/// Recompute a scoreboard from committed result JSON and compare it to the
/// published document (BENCH-11).
///
/// The document is expected to be a JSON object with a `rows` array. Each row
/// carries `result_id`, `metric` (default `raw_score`), `value` and `source`.
/// Every committed result must validate via [`validate`], supply a `result_id`,
/// and appear in the document with an identical metric, value (within 1e-9)
/// and `source`. A hand-typed row, a tampered number, a wrong path, an invalid
/// result or an omitted result is a rejection.
pub fn validate_scoreboard(
    scoreboard_doc: &str,
    results: &[CommittedResult],
) -> Result<ScoreboardCheck, Vec<Violation>> {
    let mut v = Vec::new();
    let doc: Value = serde_json::from_str(scoreboard_doc).map_err(|e| {
        vec![Violation::new(
            "invalid_json",
            format!("scoreboard is not valid JSON: {e}"),
            None,
        )]
    })?;

    // Unsourced adjectives in the published document also fail review.
    check_claims(&doc, &mut v);

    // Recompute the expected rows from the committed artifacts.
    let mut expected: Vec<Row> = Vec::new();
    for committed in results {
        let proof = match validate(&committed.json) {
            Ok(p) => p,
            Err(errs) => {
                v.extend(errs.into_iter().map(|e| e.with_source(&committed.source)));
                continue;
            }
        };
        let result_json: Value = serde_json::from_str(&committed.json).unwrap_or(Value::Null);
        let id = result_json
            .get("result_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let metric = result_json
            .get("metric")
            .and_then(Value::as_str)
            .unwrap_or("raw_score");
        if id.is_empty() {
            v.push(Violation::new(
                "invalid_result",
                "a committed result needs a non-empty result_id to appear on the scoreboard",
                Some(committed.source.clone()),
            ));
            continue;
        }
        expected.push(Row {
            id: id.to_string(),
            metric: metric.to_string(),
            value: proof.raw_score,
            source: committed.source.clone(),
        });
    }

    // Parse the published rows.
    let published: Vec<Row> = match doc.get("rows").and_then(Value::as_array) {
        Some(arr) => arr
            .iter()
            .filter_map(|item| parse_row(item, &mut v))
            .collect(),
        None => {
            v.push(Violation::new(
                "missing_field",
                "scoreboard must contain a 'rows' array",
                Some("rows".into()),
            ));
            Vec::new()
        }
    };

    // Compare recomputed rows against the published document.
    if published.len() != expected.len() {
        v.push(Violation::new(
            "scoreboard_mismatch",
            format!(
                "scoreboard has {} rows but {} committed results were supplied",
                published.len(),
                expected.len()
            ),
            None,
        ));
    }
    for exp in &expected {
        match published.iter().find(|row| row.id == exp.id) {
            None => v.push(Violation::new(
                "omitted_result",
                format!(
                    "committed result '{}' is missing from the scoreboard",
                    exp.id
                ),
                Some(exp.source.clone()),
            )),
            Some(got) => {
                if got.metric != exp.metric {
                    v.push(Violation::new(
                        "scoreboard_mismatch",
                        format!(
                            "row '{}' metric {:?} != committed {:?}",
                            exp.id, got.metric, exp.metric
                        ),
                        Some(exp.id.clone()),
                    ));
                }
                if (got.value - exp.value).abs() > 1e-9 {
                    v.push(Violation::new(
                        "scoreboard_mismatch",
                        format!(
                            "row '{}' value {} != committed {}",
                            exp.id, got.value, exp.value
                        ),
                        Some(exp.id.clone()),
                    ));
                }
                if got.source != exp.source {
                    v.push(Violation::new(
                        "scoreboard_mismatch",
                        format!(
                            "row '{}' source {:?} != committed {:?}",
                            exp.id, got.source, exp.source
                        ),
                        Some(exp.id.clone()),
                    ));
                }
            }
        }
    }
    for got in &published {
        if !expected.iter().any(|e| e.id == got.id) {
            v.push(Violation::new(
                "unbacked_row",
                format!(
                    "scoreboard row '{}' is not backed by any committed result",
                    got.id
                ),
                Some(got.id.clone()),
            ));
        }
    }

    if !v.is_empty() {
        return Err(v);
    }
    Ok(ScoreboardCheck {
        rows: expected.len(),
        sources: results.iter().map(|r| r.source.clone()).collect(),
    })
}

/// File-backed convenience wrapper ([`validate_scoreboard`] with local reads).
pub fn validate_scoreboard_files(
    scoreboard_path: &Path,
    result_paths: &[PathBuf],
) -> Result<ScoreboardCheck, Vec<Violation>> {
    let doc = std::fs::read_to_string(scoreboard_path).map_err(|e| {
        vec![Violation::new(
            "io_error",
            format!("read {}: {e}", scoreboard_path.display()),
            None,
        )]
    })?;
    let mut results = Vec::with_capacity(result_paths.len());
    for path in result_paths {
        match CommittedResult::from_file(path) {
            Ok(r) => results.push(r),
            Err(e) => {
                return Err(vec![Violation::new(
                    "io_error",
                    format!("read {}: {e}", path.display()),
                    None,
                )]);
            }
        }
    }
    validate_scoreboard(&doc, &results)
}

/// Validate a single committed result file ([`validate`] with a local read).
pub fn validate_result_file(path: &Path) -> Result<Proof, Vec<Violation>> {
    let json = std::fs::read_to_string(path).map_err(|e| {
        vec![Violation::new(
            "io_error",
            format!("read {}: {e}", path.display()),
            None,
        )]
    })?;
    validate(&json)
}

// ---- internal helpers -----------------------------------------------------

#[derive(Debug)]
struct Row {
    id: String,
    metric: String,
    value: f64,
    source: String,
}

fn parse_row(item: &Value, v: &mut Vec<Violation>) -> Option<Row> {
    let obj = match item.as_object() {
        Some(o) => o,
        None => {
            v.push(Violation::new(
                "invalid_value",
                "each scoreboard row must be an object",
                None,
            ));
            return None;
        }
    };
    let text = |key: &str| {
        obj.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let id = text("result_id");
    if id.is_empty() {
        v.push(Violation::new(
            "invalid_value",
            "each scoreboard row needs a non-empty result_id",
            None,
        ));
        return None;
    }
    let metric = {
        let m = text("metric");
        if m.is_empty() {
            "raw_score".to_string()
        } else {
            m
        }
    };
    let value = match obj.get("value").and_then(Value::as_f64) {
        Some(x) if x.is_finite() => x,
        _ => {
            v.push(Violation::new(
                "invalid_type",
                "scoreboard row value must be a finite number",
                Some(id),
            ));
            return None;
        }
    };
    Some(Row {
        id,
        metric,
        value,
        source: text("source"),
    })
}

fn req_string(obj: &Map<String, Value>, field: &str, v: &mut Vec<Violation>) -> Option<String> {
    match obj.get(field) {
        None | Some(Value::Null) => None, // presence already reported
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        Some(_) => {
            v.push(Violation::new(
                "invalid_type",
                format!("{field} must be a non-empty string"),
                Some(field.to_string()),
            ));
            None
        }
    }
}

fn req_u64(obj: &Map<String, Value>, field: &str, v: &mut Vec<Violation>) -> Option<u64> {
    match obj.get(field).and_then(Value::as_u64) {
        Some(n) if n >= 1 => Some(n),
        Some(_) => {
            v.push(Violation::new(
                "invalid_value",
                format!("{field} must be a positive integer"),
                Some(field.to_string()),
            ));
            None
        }
        None => {
            if obj.contains_key(field) {
                v.push(Violation::new(
                    "invalid_type",
                    format!("{field} must be a positive integer"),
                    Some(field.to_string()),
                ));
            }
            None
        }
    }
}

fn req_f64(obj: &Map<String, Value>, field: &str, v: &mut Vec<Violation>) -> Option<f64> {
    match obj.get(field).and_then(Value::as_f64) {
        Some(x) if x.is_finite() => Some(x),
        Some(_) => {
            v.push(Violation::new(
                "invalid_value",
                format!("{field} must be a finite number"),
                Some(field.to_string()),
            ));
            None
        }
        None => {
            if obj.contains_key(field) {
                v.push(Violation::new(
                    "invalid_type",
                    format!("{field} must be a finite number"),
                    Some(field.to_string()),
                ));
            }
            None
        }
    }
}

fn req_status(obj: &Map<String, Value>, v: &mut Vec<Violation>) -> Option<MeasurementStatus> {
    let s = req_string(obj, "status", v)?;
    match s.as_str() {
        "full" => Some(MeasurementStatus::Full),
        "subset" => Some(MeasurementStatus::Subset),
        "adapted" => Some(MeasurementStatus::Adapted),
        other => {
            v.push(Violation::new(
                "invalid_value",
                format!("status must be full, subset or adapted; got {other:?}"),
                Some("status".into()),
            ));
            None
        }
    }
}

fn req_uncertainty(obj: &Map<String, Value>, v: &mut Vec<Violation>) -> Option<Uncertainty> {
    let raw = obj.get("uncertainty")?; // presence already reported
    let u = match raw.as_object() {
        Some(o) => o,
        None => {
            v.push(Violation::new(
                "invalid_type",
                "uncertainty must be an object with ci_low, ci_high and level",
                Some("uncertainty".into()),
            ));
            return None;
        }
    };
    let ci_low = match u.get("ci_low").and_then(Value::as_f64) {
        Some(x) if x.is_finite() => x,
        _ => {
            v.push(Violation::new(
                "invalid_type",
                "uncertainty.ci_low must be a finite number",
                Some("uncertainty.ci_low".into()),
            ));
            return None;
        }
    };
    let ci_high = match u.get("ci_high").and_then(Value::as_f64) {
        Some(x) if x.is_finite() => x,
        _ => {
            v.push(Violation::new(
                "invalid_type",
                "uncertainty.ci_high must be a finite number",
                Some("uncertainty.ci_high".into()),
            ));
            return None;
        }
    };
    let level = match u.get("level").and_then(Value::as_f64) {
        Some(x) if x.is_finite() && (0.0..1.0).contains(&x) => x,
        _ => {
            v.push(Violation::new(
                "invalid_value",
                "uncertainty.level must be a number in (0, 1)",
                Some("uncertainty.level".into()),
            ));
            return None;
        }
    };
    if ci_low > ci_high {
        v.push(Violation::new(
            "invalid_value",
            "uncertainty.ci_low must not exceed ci_high",
            Some("uncertainty".into()),
        ));
    }
    Some(Uncertainty {
        ci_low,
        ci_high,
        level,
    })
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_iso_date_like(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 10
        && b[0..4].iter().all(|c| c.is_ascii_digit())
        && b[4] == b'-'
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[7] == b'-'
        && b[8..10].iter().all(|c| c.is_ascii_digit())
}

fn is_labelled_official(value: &Value) -> bool {
    let obj = match value.as_object() {
        Some(o) => o,
        None => return false,
    };
    if obj.get("official").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    for key in [
        "label",
        "tier",
        "class",
        "publication",
        "ranking",
        "claim",
        "title",
        "headline",
        "status",
    ] {
        if let Some(s) = obj.get(key).and_then(Value::as_str)
            && s.to_ascii_lowercase().contains("official")
        {
            return true;
        }
    }
    false
}

fn check_claims(value: &Value, v: &mut Vec<Violation>) {
    let obj = match value.as_object() {
        Some(o) => o,
        None => return,
    };
    let has_citation = CITATION_FIELDS.iter().any(|key| {
        obj.get(*key)
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    });
    for key in CLAIM_TEXT_FIELDS {
        if let Some(text) = obj.get(*key).and_then(Value::as_str)
            && let Some(adjective) = find_competitor_adjective(text)
            && !has_citation
        {
            v.push(Violation::new(
                "unsourced_claim",
                format!("competitor adjective {adjective:?} in '{key}' has no source_url/citation"),
                Some((*key).to_string()),
            ));
        }
    }
}

fn find_competitor_adjective(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '#')
        .filter(|w| !w.is_empty())
        .collect();
    for &adjective in COMPETITOR_ADJECTIVES {
        let is_phrase = adjective.contains(' ')
            || adjective.contains('#')
            || adjective.contains('.')
            || adjective.contains('-');
        if is_phrase {
            if lower.contains(adjective) {
                return Some(adjective);
            }
        } else if words.contains(&adjective) {
            return Some(adjective);
        }
    }
    None
}
