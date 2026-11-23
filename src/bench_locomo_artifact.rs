//! Standalone validator for a publishable LoCoMo raw-run artifact (BENCH-07).
//!
//! Like [`crate::bench_proof`] this module is deliberately dependency-light and
//! has **no** `crate::` references and performs **no** network I/O. Its inputs
//! are JSON strings (plus local-path helpers), so it can be compiled on its own
//! — e.g. by an integration test using `#[path = "../src/bench_locomo_artifact.rs"]`
//! — before it is wired into the CLI command tree.
//!
//! It enforces the LoCoMo publication gate:
//!
//! * [`validate`] requires every field a published raw run must trace to:
//!   a dataset sha256 plus a **public** source, `full`/`subset` status,
//!   **pinned** judge and reader versions, **all** item rows (each with a
//!   prediction, a gold answer and a pass flag), the failures list, a cost
//!   basis, a reproduction command, an observed date, a held-out declaration
//!   and a `score_kind` of either `derived_diagnostic` or `official`.
//! * A `subset` run may never carry `score_kind: "official"`, and a
//!   `derived_diagnostic` score may never be published as official.
//! * A reported score must equal the pass rate recomputed from the item rows.
//!   A headline score with no raw rows behind it is rejected (`unbacked_score`).
//! * [`validate_scoreboard`] recomputes every published score row from its
//!   committed raw artifact, so a hand-typed number (the classic "95.65%"
//!   claim) with no backing artifact cannot be published.
//! * Unsourced competitor adjectives are rejected in both the artifact and the
//!   scoreboard document (BENCH-12).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Fields every committed raw-run artifact must carry before it may be published.
pub const REQUIRED_FIELDS: &[&str] = &[
    "dataset_sha256",
    "dataset_source",
    "status",
    "judge_version",
    "reader_version",
    "rows",
    "failures",
    "cost_basis",
    "reproduction",
    "observed_at",
    "held_out",
    "score_kind",
];

/// Keys every item row must carry. A row without all four is not an item row.
pub const ROW_FIELDS: &[&str] = &["item_id", "prediction", "gold", "pass"];

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

/// Moving labels that are not a pin. A pinned version also has to contain a digit.
pub const MOVING_VERSION_LABELS: &[&str] = &[
    "latest", "unpinned", "head", "main", "master", "auto", "current", "stable", "tip", "trunk",
    "dev",
];

/// Measurement scope of a run. Only `Full` may carry an official score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MeasurementStatus {
    #[default]
    Full,
    Subset,
}

impl MeasurementStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            MeasurementStatus::Full => "full",
            MeasurementStatus::Subset => "subset",
        }
    }
}

/// What a reported score means. A derived diagnostic is for internal use only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScoreKind {
    #[default]
    DerivedDiagnostic,
    Official,
}

impl ScoreKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ScoreKind::DerivedDiagnostic => "derived_diagnostic",
            ScoreKind::Official => "official",
        }
    }
}

/// One raw item row. All four parts must be present; `pass` is the verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRow {
    pub item_id: String,
    pub prediction: String,
    pub gold: String,
    pub pass: bool,
}

/// A validated raw-run artifact. Building one means every rule passed.
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    pub result_id: String,
    pub dataset_sha256: String,
    pub dataset_source: String,
    pub status: MeasurementStatus,
    pub judge_version: String,
    pub reader_version: String,
    pub rows: Vec<ItemRow>,
    pub failures: Vec<String>,
    pub cost_basis: String,
    pub reproduction: String,
    pub observed_at: String,
    pub held_out: bool,
    pub score_kind: ScoreKind,
    /// The score the author reported, if one was reported at all.
    pub reported_score: Option<f64>,
    /// The score recomputed from the item rows (passed / total).
    pub computed_score: f64,
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

/// Validate a committed raw-run artifact from its JSON text.
///
/// Returns the extracted [`Artifact`] on success, or every violation found on
/// failure. Parsing, field presence, types, value domains and the publication
/// rules are all checked; no field is inferred or defaulted.
pub fn validate(artifact_json: &str) -> Result<Artifact, Vec<Violation>> {
    let value: Value = serde_json::from_str(artifact_json).map_err(|e| {
        vec![Violation::new(
            "invalid_json",
            format!("artifact is not valid JSON: {e}"),
            None,
        )]
    })?;
    validate_value(&value)
}

/// Validate an already-parsed artifact value. See [`validate`].
pub fn validate_value(value: &Value) -> Result<Artifact, Vec<Violation>> {
    let mut v = Vec::new();
    let obj: &Map<String, Value> = match value.as_object() {
        Some(o) => o,
        None => {
            return Err(vec![Violation::new(
                "not_object",
                "artifact must be a JSON object",
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

    let dataset_source = req_string(obj, "dataset_source", &mut v);
    if let Some(s) = &dataset_source
        && !is_public_source(s)
    {
        v.push(Violation::new(
            "invalid_value",
            "dataset_source must be a public http(s) URL",
            Some("dataset_source".into()),
        ));
    }

    let status = req_status(obj, &mut v);
    let judge_version = req_pinned_version(obj, "judge_version", &mut v);
    let reader_version = req_pinned_version(obj, "reader_version", &mut v);
    let rows = req_rows(obj, &mut v);
    let failures = req_failures(obj, &mut v);
    let cost_basis = req_string(obj, "cost_basis", &mut v);
    let reproduction = req_string(obj, "reproduction", &mut v);
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
    let held_out = req_bool(obj, "held_out", &mut v);
    let score_kind = req_score_kind(obj, &mut v);
    let result_id = obj
        .get("result_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // "All item rows" means the row count must match the declared sample size.
    if let Some(arr) = obj.get("rows").and_then(Value::as_array)
        && let Some(expected) = obj.get("sample_size").and_then(Value::as_u64)
        && arr.len() != expected as usize
    {
        v.push(Violation::new(
            "row_count_mismatch",
            format!(
                "rows has {} item(s) but sample_size declares {expected}",
                arr.len()
            ),
            Some("sample_size".into()),
        ));
    }

    // 3. Scores must be backed by the raw rows and match the recomputed rate.
    let reported_score = match obj.get("score").or_else(|| obj.get("published_score")) {
        None | Some(Value::Null) => None,
        Some(Value::Number(n))
            if n.as_f64()
                .map(|x| x.is_finite() && (0.0..=1.0).contains(&x))
                .unwrap_or(false) =>
        {
            Some(n.as_f64().expect("finite"))
        }
        Some(_) => {
            v.push(Violation::new(
                "invalid_value",
                "score must be a finite number in 0..=1",
                Some("score".into()),
            ));
            None
        }
    };
    let computed_score = if rows.is_empty() {
        0.0
    } else {
        rows.iter().filter(|r| r.pass).count() as f64 / rows.len() as f64
    };
    if let Some(reported) = reported_score {
        if rows.is_empty() {
            v.push(Violation::new(
                "unbacked_score",
                format!("reported score {reported} has no raw item rows behind it"),
                Some("score".into()),
            ));
        } else if (reported - computed_score).abs() > 1e-9 {
            v.push(Violation::new(
                "score_mismatch",
                format!("reported score {reported} != pass rate {computed_score} from item rows"),
                Some("score".into()),
            ));
        }
    }

    // 4. Publication rules.
    if status != Some(MeasurementStatus::Full) && score_kind == Some(ScoreKind::Official) {
        v.push(Violation::new(
            "subset_as_official",
            "a subset run must never carry score_kind 'official'",
            Some("score_kind".into()),
        ));
    }
    if score_kind == Some(ScoreKind::DerivedDiagnostic) && is_labelled_official(value) {
        v.push(Violation::new(
            "subset_as_official",
            "a derived diagnostic must never be labelled official",
            Some("score_kind".into()),
        ));
    }
    check_claims(value, &mut v);

    if !v.is_empty() {
        return Err(v);
    }

    Ok(Artifact {
        result_id,
        dataset_sha256: dataset_sha256.expect("validated"),
        dataset_source: dataset_source.expect("validated"),
        status: status.expect("validated"),
        judge_version: judge_version.expect("validated"),
        reader_version: reader_version.expect("validated"),
        rows,
        failures,
        cost_basis: cost_basis.expect("validated"),
        reproduction: reproduction.expect("validated"),
        observed_at: observed_at.expect("validated"),
        held_out: held_out.expect("validated"),
        score_kind: score_kind.expect("validated"),
        reported_score,
        computed_score,
    })
}

/// A committed raw-run artifact: its committed path plus its JSON contents.
/// The caller owns I/O, keeping the validator pure and network-free.
#[derive(Debug, Clone)]
pub struct CommittedArtifact {
    /// Committed path/label, used as the `source` in the scoreboard.
    pub source: String,
    /// Raw JSON text of the artifact file.
    pub json: String,
}

impl CommittedArtifact {
    pub fn new(source: impl Into<String>, json: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            json: json.into(),
        }
    }

    /// Read a committed artifact from a local path (never network).
    pub fn from_file(path: &Path) -> std::io::Result<Self> {
        let json = std::fs::read_to_string(path)?;
        Ok(Self {
            source: path.display().to_string(),
            json,
        })
    }
}

/// Summary of a scoreboard that successfully reproduced its committed JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoreboardCheck {
    pub rows: usize,
    pub sources: Vec<String>,
}

/// Recompute a published LoCoMo scoreboard from committed raw artifacts.
///
/// The document is a JSON object with a `rows` array. Each row carries a
/// `source` (matching a committed artifact), a `score` (or `value`) and may
/// carry an official label. Every committed artifact must validate via
/// [`validate`] and every published row must equal its artifact's recomputed
/// pass rate. A hand-typed headline (e.g. 95.65%) with no raw artifact, a
/// tampered number, a wrong path or an unsourced adjective is a rejection.
pub fn validate_scoreboard(
    scoreboard_doc: &str,
    artifacts: &[CommittedArtifact],
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

    let published = match doc.get("rows").and_then(Value::as_array) {
        Some(arr) => arr,
        None => {
            v.push(Violation::new(
                "missing_field",
                "scoreboard must contain a 'rows' array",
                Some("rows".into()),
            ));
            return Err(v);
        }
    };

    let mut sources = Vec::with_capacity(published.len());
    for (index, item) in published.iter().enumerate() {
        let row = match parse_score_row(item, index, &mut v) {
            Some(row) => row,
            None => continue,
        };
        // A claim in a row is as publishable as one in the document headline.
        check_claims(item, &mut v);

        let committed = match artifacts.iter().find(|a| a.source == row.source) {
            Some(a) => a,
            None => {
                v.push(Violation::new(
                    "unbacked_score",
                    format!(
                        "published score {} has no committed raw artifact at {:?}",
                        row.score, row.source
                    ),
                    Some(row.source.clone()),
                ));
                continue;
            }
        };
        let proof = match validate(&committed.json) {
            Ok(p) => p,
            Err(errs) => {
                v.extend(errs.into_iter().map(|e| e.with_source(&committed.source)));
                continue;
            }
        };
        if (row.score - proof.computed_score).abs() > 1e-9 {
            v.push(Violation::new(
                "score_mismatch",
                format!(
                    "published score {} != pass rate {} from {:?}",
                    row.score, proof.computed_score, row.source
                ),
                Some(row.source.clone()),
            ));
        }
        if row.label_official
            && (proof.status == MeasurementStatus::Subset
                || proof.score_kind == ScoreKind::DerivedDiagnostic)
        {
            v.push(Violation::new(
                "subset_as_official",
                format!("row {:?} labels a subset/derived run official", row.source),
                Some(row.source.clone()),
            ));
        }
        sources.push(row.source);
    }

    if !v.is_empty() {
        return Err(v);
    }
    Ok(ScoreboardCheck {
        rows: published.len(),
        sources,
    })
}

/// File-backed convenience wrapper ([`validate_scoreboard`] with local reads).
pub fn validate_scoreboard_files(
    scoreboard_path: &Path,
    artifact_paths: &[PathBuf],
) -> Result<ScoreboardCheck, Vec<Violation>> {
    let doc = std::fs::read_to_string(scoreboard_path).map_err(|e| {
        vec![Violation::new(
            "io_error",
            format!("read {}: {e}", scoreboard_path.display()),
            None,
        )]
    })?;
    let mut artifacts = Vec::with_capacity(artifact_paths.len());
    for path in artifact_paths {
        match CommittedArtifact::from_file(path) {
            Ok(a) => artifacts.push(a),
            Err(e) => {
                return Err(vec![Violation::new(
                    "io_error",
                    format!("read {}: {e}", path.display()),
                    None,
                )]);
            }
        }
    }
    validate_scoreboard(&doc, &artifacts)
}

/// Validate a single committed raw artifact file ([`validate`] with a read).
pub fn validate_artifact_file(path: &Path) -> Result<Artifact, Vec<Violation>> {
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

struct ScoreRow {
    source: String,
    score: f64,
    label_official: bool,
}

fn parse_score_row(item: &Value, index: usize, v: &mut Vec<Violation>) -> Option<ScoreRow> {
    let obj = match item.as_object() {
        Some(o) => o,
        None => {
            v.push(Violation::new(
                "invalid_value",
                format!("scoreboard row {index} must be an object"),
                None,
            ));
            return None;
        }
    };
    let source = obj
        .get("source")
        .or_else(|| obj.get("artifact"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if source.is_empty() {
        v.push(Violation::new(
            "invalid_value",
            format!("scoreboard row {index} needs a non-empty source"),
            None,
        ));
        return None;
    }
    let score = match obj
        .get("score")
        .or_else(|| obj.get("value"))
        .and_then(Value::as_f64)
    {
        Some(x) if x.is_finite() && (0.0..=1.0).contains(&x) => x,
        _ => {
            v.push(Violation::new(
                "invalid_value",
                format!("scoreboard row {index} score must be a finite number in 0..=1"),
                Some(source),
            ));
            return None;
        }
    };
    let label_official = is_labelled_official(item)
        || obj
            .get("score_kind")
            .and_then(Value::as_str)
            .map(|s| s == "official")
            .unwrap_or(false);
    Some(ScoreRow {
        source,
        score,
        label_official,
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

fn req_bool(obj: &Map<String, Value>, field: &str, v: &mut Vec<Violation>) -> Option<bool> {
    match obj.get(field) {
        None | Some(Value::Null) => None, // presence already reported
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => {
            v.push(Violation::new(
                "invalid_type",
                format!("{field} must be a boolean declaration"),
                Some(field.to_string()),
            ));
            None
        }
    }
}

fn req_status(obj: &Map<String, Value>, v: &mut Vec<Violation>) -> Option<MeasurementStatus> {
    let s = req_string(obj, "status", v)?;
    match s.as_str() {
        "full" => Some(MeasurementStatus::Full),
        "subset" => Some(MeasurementStatus::Subset),
        other => {
            v.push(Violation::new(
                "invalid_value",
                format!("status must be full or subset; got {other:?}"),
                Some("status".into()),
            ));
            None
        }
    }
}

fn req_score_kind(obj: &Map<String, Value>, v: &mut Vec<Violation>) -> Option<ScoreKind> {
    let s = req_string(obj, "score_kind", v)?;
    match s.as_str() {
        "derived_diagnostic" => Some(ScoreKind::DerivedDiagnostic),
        "official" => Some(ScoreKind::Official),
        other => {
            v.push(Violation::new(
                "invalid_value",
                format!("score_kind must be derived_diagnostic or official; got {other:?}"),
                Some("score_kind".into()),
            ));
            None
        }
    }
}

fn req_pinned_version(
    obj: &Map<String, Value>,
    field: &str,
    v: &mut Vec<Violation>,
) -> Option<String> {
    let s = req_string(obj, field, v)?;
    let lower = s.trim().to_ascii_lowercase();
    if MOVING_VERSION_LABELS.contains(&lower.as_str()) {
        v.push(Violation::new(
            "unpinned_version",
            format!("{field} must be a pinned version, not {s:?}"),
            Some(field.to_string()),
        ));
        return None;
    }
    if !s.bytes().any(|b| b.is_ascii_digit()) {
        v.push(Violation::new(
            "unpinned_version",
            format!("{field} must be a pinned version containing a version number"),
            Some(field.to_string()),
        ));
        return None;
    }
    Some(s)
}

fn req_rows(obj: &Map<String, Value>, v: &mut Vec<Violation>) -> Vec<ItemRow> {
    let arr = match obj.get("rows") {
        None | Some(Value::Null) => return Vec::new(), // presence already reported
        Some(Value::Array(a)) => a,
        Some(_) => {
            v.push(Violation::new(
                "invalid_type",
                "rows must be an array of item rows",
                Some("rows".into()),
            ));
            return Vec::new();
        }
    };
    if arr.is_empty() {
        v.push(Violation::new(
            "invalid_value",
            "rows must contain at least one item row",
            Some("rows".into()),
        ));
    }
    let mut rows = Vec::with_capacity(arr.len());
    let mut seen = HashSet::new();
    for (index, item) in arr.iter().enumerate() {
        let row_obj = match item.as_object() {
            Some(o) => o,
            None => {
                v.push(Violation::new(
                    "invalid_row",
                    format!("row {index} must be an object"),
                    Some("rows".into()),
                ));
                continue;
            }
        };
        for &field in ROW_FIELDS {
            if !row_obj.contains_key(field) || row_obj[field].is_null() {
                v.push(Violation::new(
                    "invalid_row",
                    format!("row {index} is missing {field}"),
                    Some("rows".into()),
                ));
            }
        }
        let item_id = row_obj
            .get("item_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if item_id.is_empty() && row_obj.contains_key("item_id") {
            v.push(Violation::new(
                "invalid_row",
                format!("row {index} has an empty item_id"),
                Some("rows".into()),
            ));
        }
        if !item_id.is_empty() && !seen.insert(item_id.clone()) {
            v.push(Violation::new(
                "duplicate_item",
                format!("item_id {item_id:?} appears more than once"),
                Some("rows".into()),
            ));
        }
        let text = |field: &str| {
            row_obj
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let pass = match row_obj.get("pass") {
            Some(Value::Bool(b)) => *b,
            Some(_) => {
                v.push(Violation::new(
                    "invalid_row",
                    format!("row {index} pass must be a boolean"),
                    Some("rows".into()),
                ));
                false
            }
            None => false,
        };
        rows.push(ItemRow {
            item_id,
            prediction: text("prediction"),
            gold: text("gold"),
            pass,
        });
    }
    rows
}

fn req_failures(obj: &Map<String, Value>, v: &mut Vec<Violation>) -> Vec<String> {
    let arr = match obj.get("failures") {
        None | Some(Value::Null) => return Vec::new(), // presence already reported
        Some(Value::Array(a)) => a,
        Some(_) => {
            v.push(Violation::new(
                "invalid_type",
                "failures must be an array",
                Some("failures".into()),
            ));
            return Vec::new();
        }
    };
    arr.iter()
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
        .collect()
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_public_source(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with("https://") && s.len() > "https://".len())
        || (s.starts_with("http://") && s.len() > "http://".len())
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
