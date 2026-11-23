//! MTS-60 *derived* diagnostic: mechanical verifiers, no model, no network.
//!
//! `mn bench mts-verify` seeds a tiny checkout (a stand-in for a clean git
//! checkout) in a temp dir for every case, then grades the case's committed
//! patch with three independent gates:
//!
//! 1. the patch applies to that pristine checkout;
//! 2. it never edits a forbidden file (the anti-gaming rule);
//! 3. the resulting tree satisfies a deterministic check written by hand from
//!    the seed — an exact file, a line count, a code token, a parsed JSON
//!    field, a removed marker, or a multi-file agreement.
//!
//! There is no model, no socket, no API key and no `expected_contains` match
//! on prose. The old `ci/mts.jsonl` lane asked a model for an answer and
//! searched the reply for a word; that can pass while the repo is wrong. Here
//! the "answer" is a patch and the verifier inspects the files it produces.
//! A case that fails reports which gate failed, with expected and observed
//! facts, and `run` exits non-zero so CI can gate on it.
//!
//! HONESTY. This is a derived diagnostic over 60 synthetic cases, NOT an
//! official Terminal-Bench score. The number here describes only this fixed
//! seed and these hand-written checks; it must never be extrapolated to a full
//! MTS-60 run or to any real agent, model or repository.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::output::{ExitCode, Outcome};

/// Only this fixture schema is understood; a format change must be deliberate.
const SCHEMA: &str = "mn-mts/1";
const RESULT_SCHEMA: &str = "mn-mts-result/1";
const CATEGORIES: [&str; 6] = [
    "exact-file",
    "line-count",
    "token-present",
    "json-field",
    "absent-marker",
    "multi-file",
];
const PER_CATEGORY: usize = 10;

/// CLI arguments for `mn bench mts-verify`. Defined here so the only wiring in
/// `bench.rs` is one enum variant and one dispatch arm.
#[derive(Debug, clap::Args)]
pub struct MtsVerifyArgs {
    /// Fixture JSON (defaults to ci/mts-cases.json).
    #[arg(long)]
    pub cases: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
struct Fixture {
    schema: String,
    note: String,
    cases: Vec<Case>,
}

#[derive(Debug, Clone, Deserialize)]
struct Case {
    id: String,
    category: String,
    /// The pristine checkout: path -> exact bytes.
    seed: Seed,
    /// Files the answer must never touch.
    forbidden: Vec<String>,
    /// Unified diff representing the answer under test.
    patch: String,
    /// Hand-written deterministic outcome. Never derived from running the patch.
    expect: Check,
}

#[derive(Debug, Clone, Deserialize)]
struct Seed {
    files: Vec<SeedFile>,
}

#[derive(Debug, Clone, Deserialize)]
struct SeedFile {
    path: String,
    content: String,
}

/// Six categories, six mechanically different checks. The `kind` tag must equal
/// the case's `category`; a label that lies about what it grades is rejected.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Check {
    ExactFile {
        path: String,
        content: String,
    },
    LineCount {
        path: String,
        lines: usize,
    },
    TokenPresent {
        path: String,
        all: Vec<String>,
        none: Vec<String>,
    },
    JsonField {
        path: String,
        field: String,
        value: Value,
    },
    AbsentMarker {
        path: String,
        marker: String,
    },
    MultiFile {
        files: Vec<FileExpect>,
    },
}

impl Check {
    fn kind(&self) -> &'static str {
        match self {
            Check::ExactFile { .. } => "exact-file",
            Check::LineCount { .. } => "line-count",
            Check::TokenPresent { .. } => "token-present",
            Check::JsonField { .. } => "json-field",
            Check::AbsentMarker { .. } => "absent-marker",
            Check::MultiFile { .. } => "multi-file",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct FileExpect {
    path: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct CheckOutcome {
    passed: bool,
    expected: Value,
    observed: Value,
}

/// One case's full verdict. Every gate is reported, so a red case says *why*
/// without re-running anything.
#[derive(Debug, Clone, Serialize)]
struct CaseResult {
    id: String,
    category: String,
    passed: bool,
    patch_applies: bool,
    forbidden_touched: Vec<String>,
    check: CheckOutcome,
    reason: String,
}

// A private temp checkout per case. `tempfile` is only a dev-dependency of the
// crate, so the library code cleans up itself; a file handle still open on
// Windows can leave a directory behind, which is harmless because the next run
// uses a different numbered path.
static SCRATCH_SEQ: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Result<Self> {
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("mn-mts-verify-{}-{tag}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .with_context(|| format!("create temp checkout {}", path.display()))?;
        Ok(Self(path))
    }

    fn write(&self, rel: &str, content: &str) -> Result<()> {
        let rel = safe_rel(rel)?;
        let full = self.0.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&full, content).with_context(|| format!("write {}", full.display()))?;
        Ok(())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Reject traversal, absolute paths and drive prefixes. A patch can only name
/// files inside its own checkout.
fn safe_rel(path: &str) -> Result<&str> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed != path {
        bail!("unsafe patch path {path:?}");
    }
    let p = Path::new(path);
    if p.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) | Component::CurDir
        )
    }) {
        bail!("unsafe patch path {path:?}");
    }
    Ok(path)
}

/// Normalize `a/foo` / `b/foo` to `foo`; `/dev/null` is kept as a sentinel.
fn patch_path(raw: &str) -> String {
    let raw = raw.trim();
    if raw == "/dev/null" {
        return "/dev/null".to_string();
    }
    raw.strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw)
        .to_string()
}

#[derive(Debug, Clone)]
enum LineOp {
    Context(String),
    Remove(String),
    Add(String),
}

#[derive(Debug, Clone)]
struct Hunk {
    old_start: usize,
    old_count: usize,
    #[allow(dead_code)] // reported for completeness; apply keys on old_start
    new_start: usize,
    new_count: usize,
    ops: Vec<LineOp>,
}

#[derive(Debug, Clone)]
struct FilePatch {
    path: String,
    hunks: Vec<Hunk>,
}

/// Parse the small, well-defined subset of unified diff that a case answer uses:
/// `---`/`+++` file headers and one or more `@@ -a,b +c,d @@` hunks with
/// context, remove and add lines. `\ No newline...` and metadata lines are
/// ignored. Malformed input is an error, which the caller reports as
/// "patch does not apply" rather than crashing the suite.
fn parse_patch(patch: &str) -> Result<Vec<FilePatch>> {
    let lines: Vec<String> = patch
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect();
    let mut files = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let Some(rest) = lines[i].strip_prefix("--- ") else {
            i += 1;
            continue;
        };
        let old_path = patch_path(rest);
        i += 1;
        let plus = lines
            .get(i)
            .and_then(|line| line.strip_prefix("+++ "))
            .with_context(|| format!("header --- {rest} is not followed by +++"))?;
        let new_path = patch_path(plus);
        i += 1;
        let path = if new_path == "/dev/null" {
            old_path
        } else {
            new_path
        };
        if path != "/dev/null" {
            safe_rel(&path)?;
        }

        let mut hunks = Vec::new();
        while i < lines.len() {
            let line = &lines[i];
            if line.starts_with("--- ") {
                break;
            }
            let Some(header) = line.strip_prefix("@@ ") else {
                // diff/index/new-file-mode metadata: not graded.
                i += 1;
                continue;
            };
            let mut hunk = parse_hunk_header(header)?;
            i += 1;
            let mut ops = Vec::new();
            while i < lines.len() {
                let op_line = &lines[i];
                if op_line.starts_with("@@ ") || op_line.starts_with("--- ") {
                    break;
                }
                if op_line.starts_with('\\') {
                    i += 1;
                    continue;
                }
                if let Some(content) = op_line.strip_prefix(' ') {
                    ops.push(LineOp::Context(content.to_string()));
                } else if let Some(content) = op_line.strip_prefix('-') {
                    ops.push(LineOp::Remove(content.to_string()));
                } else if let Some(content) = op_line.strip_prefix('+') {
                    ops.push(LineOp::Add(content.to_string()));
                } else if op_line.is_empty() {
                    // A blank line inside a hunk is an empty context line.
                    ops.push(LineOp::Context(String::new()));
                } else {
                    break;
                }
                i += 1;
            }
            hunk.ops = ops;
            hunks.push(hunk);
        }
        if hunks.is_empty() {
            bail!("patch for {path} has no hunks");
        }
        files.push(FilePatch { path, hunks });
    }
    if files.is_empty() {
        bail!("patch has no file sections");
    }
    Ok(files)
}

fn parse_hunk_header(header: &str) -> Result<Hunk> {
    let main = header.split(" @@").next().unwrap_or(header).trim();
    let mut parts = main.split_whitespace();
    let old = parts.next().context("hunk has no old range")?;
    let new = parts.next().context("hunk has no new range")?;
    fn range(text: &str) -> Result<(usize, usize)> {
        let digits = text
            .strip_prefix('-')
            .or_else(|| text.strip_prefix('+'))
            .context("hunk range needs - or +")?;
        let mut fields = digits.split(',');
        let start = fields
            .next()
            .context("missing range start")?
            .parse::<usize>()
            .context("bad range start")?;
        let count = match fields.next() {
            Some(value) => value.parse::<usize>().context("bad range count")?,
            None => 1,
        };
        Ok((start, count))
    }
    let (old_start, old_count) = range(old)?;
    let (new_start, new_count) = range(new)?;
    Ok(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        ops: Vec::new(),
    })
}

/// Content lines with a single trailing newline, normalized to `\n`.
fn split_lines(text: &str) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n");
    let body = normalized.strip_suffix('\n').unwrap_or(&normalized);
    if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').map(str::to_string).collect()
    }
}

fn join_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

/// Apply every file patch to a pristine checkout. All hunks are validated
/// before anything is written, so a bad patch never leaves a half-edited tree.
fn apply_patch(
    checkout: &BTreeMap<String, String>,
    files: &[FilePatch],
) -> Result<BTreeMap<String, String>> {
    let mut result = checkout.clone();
    for patch in files {
        let original = result.get(&patch.path).cloned().unwrap_or_default();
        let mut lines = split_lines(&original);

        // Latest hunk first: replacing it cannot shift an earlier hunk's offset.
        let mut hunks = patch.hunks.clone();
        hunks.sort_by_key(|h| std::cmp::Reverse(h.old_start));
        for hunk in &hunks {
            let old_side: Vec<&String> = hunk
                .ops
                .iter()
                .filter_map(|op| match op {
                    LineOp::Context(text) | LineOp::Remove(text) => Some(text),
                    LineOp::Add(_) => None,
                })
                .collect();
            let new_count = hunk
                .ops
                .iter()
                .filter(|op| matches!(op, LineOp::Context(_) | LineOp::Add(_)))
                .count();
            if old_side.len() != hunk.old_count {
                bail!("hunk old count mismatch in {}", patch.path);
            }
            if new_count != hunk.new_count {
                bail!("hunk new count mismatch in {}", patch.path);
            }
            let start = if hunk.old_count == 0 {
                hunk.old_start
            } else {
                hunk.old_start.saturating_sub(1)
            };
            if start > lines.len() || start + hunk.old_count > lines.len() {
                bail!("hunk starts past end of {}", patch.path);
            }
            let actual = &lines[start..start + hunk.old_count];
            let expected: Vec<String> = old_side.into_iter().cloned().collect();
            if actual != expected.as_slice() {
                bail!("hunk context mismatch in {}", patch.path);
            }
            let mut rebuilt = Vec::with_capacity(lines.len());
            rebuilt.extend_from_slice(&lines[..start]);
            for op in &hunk.ops {
                match op {
                    LineOp::Context(text) | LineOp::Add(text) => rebuilt.push(text.clone()),
                    LineOp::Remove(_) => {}
                }
            }
            rebuilt.extend_from_slice(&lines[start + hunk.old_count..]);
            lines = rebuilt;
        }
        result.insert(patch.path.clone(), join_lines(&lines));
    }
    Ok(result)
}

/// Grade a deterministic check against the tree the patch produced.
fn evaluate(check: &Check, files: &BTreeMap<String, String>) -> CheckOutcome {
    match check {
        Check::ExactFile { path, content } => {
            let observed = files
                .get(path)
                .cloned()
                .unwrap_or_else(|| "<missing>".into());
            CheckOutcome {
                passed: observed == *content,
                expected: json!({ "path": path, "content": content }),
                observed: json!({ "path": path, "content": observed }),
            }
        }
        Check::LineCount { path, lines } => {
            let observed = files
                .get(path)
                .map(|text| split_lines(text).len())
                .unwrap_or(0);
            CheckOutcome {
                passed: observed == *lines,
                expected: json!({ "path": path, "lines": lines }),
                observed: json!({ "path": path, "lines": observed }),
            }
        }
        Check::TokenPresent { path, all, none } => {
            let text = files.get(path).map(String::as_str).unwrap_or("");
            let missing: Vec<&str> = all
                .iter()
                .map(String::as_str)
                .filter(|token| !text.contains(token))
                .collect();
            let unexpected: Vec<&str> = none
                .iter()
                .map(String::as_str)
                .filter(|token| text.contains(token))
                .collect();
            CheckOutcome {
                passed: missing.is_empty() && unexpected.is_empty(),
                expected: json!({ "path": path, "all": all, "none": none }),
                observed: json!({ "missing": missing, "unexpected": unexpected }),
            }
        }
        Check::JsonField { path, field, value } => {
            let observed = files
                .get(path)
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .and_then(|doc| doc.get(field).cloned());
            CheckOutcome {
                passed: observed.as_ref() == Some(value),
                expected: json!({ "path": path, "field": field, "value": value }),
                observed: json!({ "path": path, "field": field, "value": observed }),
            }
        }
        Check::AbsentMarker { path, marker } => {
            let exists = files.contains_key(path);
            let present = files.get(path).is_some_and(|text| text.contains(marker));
            CheckOutcome {
                passed: exists && !present,
                expected: json!({ "path": path, "marker": marker, "absent": true }),
                observed: json!({ "file_exists": exists, "marker_present": present }),
            }
        }
        Check::MultiFile { files: expects } => {
            let mismatched: Vec<&str> = expects
                .iter()
                .filter(|item| {
                    files.get(&item.path).map(String::as_str) != Some(item.content.as_str())
                })
                .map(|item| item.path.as_str())
                .collect();
            CheckOutcome {
                passed: mismatched.is_empty(),
                expected: json!({ "files": expects }),
                observed: json!({ "mismatched": mismatched }),
            }
        }
    }
}

fn verify_case(case: &Case) -> Result<CaseResult> {
    // The checkout is pristine for every case: the patch is graded against
    // exactly the bytes the fixture seeded, never against a prior case.
    let scratch = Scratch::new(&case.id).with_context(|| format!("case {}", case.id))?;
    let mut checkout = BTreeMap::new();
    for file in &case.seed.files {
        safe_rel(&file.path)?;
        if checkout
            .insert(file.path.clone(), file.content.clone())
            .is_some()
        {
            bail!("case {} seeds {} twice", case.id, file.path);
        }
        scratch.write(&file.path, &file.content)?;
    }
    for path in &case.forbidden {
        safe_rel(path)?;
        if !checkout.contains_key(path) {
            bail!("case {} forbids unseeded file {path}", case.id);
        }
    }

    let parsed = parse_patch(&case.patch);
    let patch_paths: Vec<String> = match &parsed {
        Ok(files) => files.iter().map(|f| f.path.clone()).collect(),
        Err(_) => Vec::new(),
    };
    let forbidden: BTreeSet<&str> = case.forbidden.iter().map(String::as_str).collect();
    let mut touched: BTreeSet<String> = patch_paths
        .into_iter()
        .filter(|path| forbidden.contains(path.as_str()))
        .collect();

    let (patch_applies, final_files) = match parsed {
        Err(_) => (false, checkout.clone()),
        Ok(files) => match apply_patch(&checkout, &files) {
            Ok(applied) => {
                // Defense in depth: a forbidden file whose bytes moved is a
                // forbidden edit even if the patch header tried to hide it.
                for path in &case.forbidden {
                    if applied.get(path) != checkout.get(path) {
                        touched.insert(path.clone());
                    }
                }
                for (path, content) in &applied {
                    scratch.write(path, content)?;
                }
                (true, applied)
            }
            Err(_) => (false, checkout.clone()),
        },
    };

    let check = if patch_applies {
        evaluate(&case.expect, &final_files)
    } else {
        CheckOutcome {
            passed: false,
            expected: serde_json::to_value(&case.expect)?,
            observed: json!(null),
        }
    };
    let forbidden_touched: Vec<String> = touched.into_iter().collect();
    let mut reasons = Vec::new();
    if !patch_applies {
        reasons.push("patch_does_not_apply");
    }
    if !forbidden_touched.is_empty() {
        reasons.push("forbidden_file_edit");
    }
    if !check.passed {
        reasons.push("deterministic_check");
    }
    let passed = reasons.is_empty();
    Ok(CaseResult {
        id: case.id.clone(),
        category: case.category.clone(),
        passed,
        patch_applies,
        forbidden_touched,
        check,
        reason: if passed {
            String::new()
        } else {
            reasons.join(", ")
        },
    })
}

fn validate(fixture: &Fixture) -> Result<()> {
    if fixture.schema != SCHEMA {
        bail!("unsupported MTS fixture schema {:?}", fixture.schema);
    }
    if !fixture
        .note
        .contains("NOT an official Terminal-Bench score")
    {
        bail!("MTS fixture must carry the derived-diagnostic note");
    }
    if fixture.cases.is_empty() {
        bail!("MTS fixture has no cases");
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for case in &fixture.cases {
        if case.id.is_empty() || !ids.insert(case.id.as_str()) {
            bail!("MTS case has empty or duplicate id {:?}", case.id);
        }
        if !CATEGORIES.contains(&case.category.as_str()) {
            bail!("case {} has unknown category {:?}", case.id, case.category);
        }
        if case.expect.kind() != case.category {
            bail!(
                "case {} is labelled {:?} but checks {:?}",
                case.id,
                case.category,
                case.expect.kind()
            );
        }
        if case.seed.files.is_empty() || case.forbidden.is_empty() {
            bail!("case {} needs a seed and a forbidden list", case.id);
        }
        *counts.entry(case.category.as_str()).or_insert(0) += 1;
    }
    for category in CATEGORIES {
        if counts.get(category).copied().unwrap_or(0) != PER_CATEGORY {
            bail!("category {category} needs exactly {PER_CATEGORY} cases");
        }
    }
    Ok(())
}

/// Verify every case and return the structured report. The caller sets the
/// exit code, so a failing gate is a CI failure rather than a printed opinion.
pub fn verify(fixture_path: &Path) -> Result<Value> {
    if !fixture_path.exists() {
        bail!("MTS fixture not found: {}", fixture_path.display());
    }
    let raw = std::fs::read_to_string(fixture_path)
        .with_context(|| format!("read {}", fixture_path.display()))?;
    let fixture: Fixture = serde_json::from_str(&raw).context("parse MTS fixture")?;
    validate(&fixture)?;

    let mut results = Vec::with_capacity(fixture.cases.len());
    let mut failures = 0usize;
    for case in &fixture.cases {
        let result = verify_case(case).with_context(|| format!("case {}", case.id))?;
        if !result.passed {
            failures += 1;
        }
        results.push(result);
    }

    let mut categories = serde_json::Map::new();
    for category in CATEGORIES {
        let rows: Vec<&CaseResult> = results
            .iter()
            .filter(|row| row.category == category)
            .collect();
        let passed = rows.iter().filter(|row| row.passed).count();
        categories.insert(
            category.to_string(),
            json!({ "cases": rows.len(), "passed": passed, "failed": rows.len() - passed }),
        );
    }

    Ok(json!({
        "schema": RESULT_SCHEMA,
        "label": "MTS-60 derived diagnostic (NOT an official Terminal-Bench score)",
        "disclaimer": "Derived diagnostic over 60 synthetic cases with hand-written checks and seeded temp checkouts. No model, no network, no official Terminal-Bench run; not extrapolated to any real agent or repository.",
        "official": false,
        "basis": "derived",
        "note": fixture.note,
        "scope": "6 categories x 10 cases; pristine temp checkout, patch application, forbidden-file guard, deterministic check",
        "fixture": fixture_path.to_string_lossy(),
        "cases": fixture.cases.len(),
        "passed": fixture.cases.len() - failures,
        "failed": failures,
        "gate": if failures == 0 { "pass" } else { "fail" },
        "categories": categories,
        "results": results,
    }))
}

/// `mn bench mts-verify` — non-zero when any deterministic case fails.
pub fn run(args: &MtsVerifyArgs) -> Result<Outcome> {
    let default = Path::new("ci").join("mts-cases.json");
    let path = args.cases.as_deref().unwrap_or(&default);
    let data = verify(path)?;
    let passed = data["passed"].as_u64().unwrap_or(0);
    let failed = data["failed"].as_u64().unwrap_or(0);
    let text = format!(
        "mts-verify (derived diagnostic, NOT an official Terminal-Bench score) \
         — {passed} passed, {failed} failed, gate {}",
        data["gate"].as_str().unwrap_or("unknown")
    );
    let mut outcome = Outcome::new("bench.mts-verify", text, data);
    if failed > 0 {
        outcome.exit_code = ExitCode::Unexpected as u8;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/mts-cases.json")
    }

    #[test]
    fn committed_fixture_passes_all_sixty_cases() {
        let report = verify(&fixture_path()).expect("verify committed fixture");
        assert_eq!(report["schema"], RESULT_SCHEMA);
        assert_eq!(report["gate"], "pass", "report: {report}");
        assert_eq!(report["cases"], 60);
        assert_eq!(report["failed"], 0);
        for category in CATEGORIES {
            assert_eq!(report["categories"][category]["cases"], 10);
            assert_eq!(report["categories"][category]["failed"], 0);
        }
    }

    #[test]
    fn a_wrong_deterministic_expectation_fails_its_case() {
        let dir = std::env::temp_dir().join(format!("mn-mts-wrong-{}-unit", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("wrong.json");
        let raw = std::fs::read_to_string(fixture_path()).unwrap();
        let mut value: Value = serde_json::from_str(&raw).unwrap();
        value["cases"][0]["expect"]["content"] = json!("an invented answer");
        std::fs::write(&path, value.to_string()).unwrap();
        let report = verify(&path).expect("verify mutated fixture");
        assert_eq!(report["gate"], "fail");
        assert_eq!(report["failed"], 1);
        assert_eq!(report["results"][0]["category"], "exact-file");
        assert!(
            report["results"][0]["reason"]
                .as_str()
                .unwrap()
                .contains("deterministic_check")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
