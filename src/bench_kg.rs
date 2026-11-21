//! Deterministic Code Graph Suite verification: no model, no network.
//!
//! `mn bench kg-verify` seeds small in-memory code-graph stores from a committed
//! fixture and checks the graph engine's own answers against ground truth that
//! is written by hand from the seeded rows. This is the lane that can actually
//! fail: a substring match on an LLM envelope cannot establish that `kg def`,
//! `kg refs`, `kg callers`, `kg callees` or `kg impact` answer the right thing.
//!
//! Ground truth comes from the fixture's `expect` object, never from the query
//! under test and never from running the indexer over a sample tree. Only the
//! fixture builder writes rows directly into the graph store; every graded
//! value is read back through the public query functions the CLI itself calls.
//!
//! The precision tier is reported, not asserted above what this build can
//! produce. This build has no LSP client (`capabilities().lsp == false`), so no
//! answer is allowed to claim `lsp`; see the `precision` cases in the fixture.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use memnoth_codegraph::impact;
use memnoth_codegraph::{
    Precision, Store, best_available, capabilities, explore, lsp_available, search_symbols,
    tree_sitter_available,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::output::{ExitCode, Outcome};

/// One committed fixture. `mn-kg/1`: a format change must be deliberate.
#[derive(Debug, Clone, Deserialize)]
struct Fixture {
    schema: String,
    cases: Vec<Case>,
}

/// One case. `symbols`/`edges` seed the store; `expect` is the hand-written
/// ground truth. Documentary labels (`kind`, `symbol`) are never trusted on
/// their own — the observed answer is compared field by field.
#[derive(Debug, Clone, Deserialize)]
struct Case {
    id: String,
    kind: String,
    #[serde(default)]
    symbol: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    symbols: Vec<SeedSymbol>,
    #[serde(default)]
    edges: Vec<SeedEdge>,
    #[serde(default = "default_depth")]
    depth: usize,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    churn: Option<u32>,
    #[serde(default)]
    coverage: Option<f64>,
    expect: Value,
}

fn default_depth() -> usize {
    3
}

fn default_limit() -> usize {
    10
}

#[derive(Debug, Clone, Deserialize)]
struct SeedSymbol {
    id: i64,
    qname: String,
    kind: String,
    file: String,
    line: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct SeedEdge {
    source: i64,
    target: i64,
    kind: String,
}

/// One case's observed and expected answer, kept whole so a failure is
/// diagnosable without re-running anything.
#[derive(Debug, Clone, serde::Serialize)]
struct CaseResult {
    id: String,
    kind: String,
    symbol: String,
    passed: bool,
    expected: Value,
    observed: Value,
    reason: String,
}

/// CLI arguments for `mn bench kg-verify`. Defined here so `bench.rs` only has
/// to add a variant and a dispatch arm.
#[derive(Debug, clap::Args)]
pub struct KgVerifyArgs {
    /// Fixture JSON (defaults to ci/kg-verify-cases.json).
    #[arg(long)]
    pub cases: Option<PathBuf>,
    /// Optional source tree for the separate 100k-file index budget probe.
    /// Without it the budget section reports "skipped" and the gate is fast.
    #[arg(long)]
    pub index_budget_root: Option<PathBuf>,
    /// Budget in milliseconds for that probe (default 60_000).
    #[arg(long, default_value_t = 60_000)]
    pub index_budget_ms: u64,
}

static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A private SQLite file per case. `Store` has no in-memory constructor, so a
/// unique temp path is the closest equivalent; the guard removes it on drop
/// after the connection is closed (which matters on Windows).
struct GraphDb {
    path: PathBuf,
    store: Option<Store>,
}

impl GraphDb {
    fn new() -> Result<Self> {
        let n = DB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("mn-kg-verify-{}-{n}.db", std::process::id()));
        // A stale file from a killed run must not seed old rows.
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).context("open graph store")?;
        Ok(Self {
            path,
            store: Some(store),
        })
    }

    fn store(&self) -> &Store {
        self.store.as_ref().expect("graph store is live")
    }
}

impl Drop for GraphDb {
    fn drop(&mut self) {
        self.store.take();
        let _ = std::fs::remove_file(&self.path);
    }
}

fn seed(case: &Case, store: &Store) -> Result<()> {
    let ids: std::collections::HashSet<i64> = case.symbols.iter().map(|s| s.id).collect();
    if ids.len() != case.symbols.len() {
        bail!("case {} repeats a symbol id", case.id);
    }
    for edge in &case.edges {
        if !ids.contains(&edge.source) || !ids.contains(&edge.target) {
            bail!("case {} has an edge with an unknown endpoint", case.id);
        }
    }
    for symbol in &case.symbols {
        store
            .conn()
            .execute(
                "INSERT INTO symbols (id, qname, kind, file_path, span_start, span_end) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    symbol.id,
                    symbol.qname,
                    symbol.kind,
                    symbol.file,
                    symbol.line as i64,
                    (symbol.line + 1) as i64,
                ],
            )
            .with_context(|| format!("seed symbol in case {}", case.id))?;
    }
    for edge in &case.edges {
        store
            .conn()
            .execute(
                "INSERT INTO relations (source_id, target_id, kind) VALUES (?1, ?2, ?3)",
                params![edge.source, edge.target, edge.kind],
            )
            .with_context(|| format!("seed edge in case {}", case.id))?;
    }
    Ok(())
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

/// Round to four decimals: confidence is a propagated product, and the same
/// decimal in the fixture and in the run must not disagree by one ULP.
fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Grade one case through a public query function. No indexer is run here.
fn observe(case: &Case, store: &Store) -> Result<Value> {
    let root = Path::new("");
    match case.kind.as_str() {
        "definition" => {
            let answer = explore(store, root, &case.symbol)?;
            // `ExploreResult` carries no `resolved` flag; an unresolved answer
            // is the one whose symbol has no file.
            let resolved = !answer.symbol.file.is_empty();
            Ok(json!({
                "resolved": resolved,
                "qname": answer.symbol.qname,
                "kind": answer.symbol.kind,
                "file": answer.symbol.rel_path,
                "line": answer.symbol.line,
            }))
        }
        "refs" | "callers" | "callees" => {
            let answer = explore(store, root, &case.symbol)?;
            let resolved = !answer.symbol.file.is_empty();
            let hits: Vec<String> = if case.kind == "callees" {
                answer
                    .callees
                    .iter()
                    .map(|c| format!("{}@{}:{}", c.qname, c.file, c.line))
                    .collect()
            } else {
                answer
                    .callers
                    .iter()
                    .map(|c| format!("{}@{}:{}", c.qname, c.rel_path, c.line))
                    .collect()
            };
            Ok(json!({ "resolved": resolved, "hits": sorted(hits) }))
        }
        "impact" => {
            let answer = impact::blast_radius(
                store,
                &case.symbol,
                case.depth.max(1),
                case.churn,
                case.coverage,
            )?;
            let mut evidence: Vec<Value> = answer
                .exact
                .iter()
                .chain(answer.likely.iter())
                .map(|path| {
                    json!({
                        "qname": path.qname,
                        "confidence": round4(path.confidence),
                        "class": path.class,
                    })
                })
                .collect();
            evidence.sort_by(|a, b| a["qname"].as_str().cmp(&b["qname"].as_str()));
            Ok(json!({
                "resolved": answer.resolved,
                "precision": answer.precision.label(),
                "counts": {
                    "exact": answer.counts.exact,
                    "likely": answer.counts.likely,
                    "direct": answer.counts.direct,
                    "transitive": answer.counts.transitive,
                },
                "exact": sorted(answer.exact.iter().map(|p| p.qname.clone()).collect()),
                "likely": sorted(answer.likely.iter().map(|p| p.qname.clone()).collect()),
                "evidence": evidence,
                "tests": sorted(
                    answer.affected_tests.iter().map(|t| t.file.clone()).collect()
                ),
                "truncated": answer.truncated,
                "risk": answer.risk.as_ref().map(|risk| json!({
                    "score": round4(risk.score),
                    "fan_in": risk.inputs.fan_in,
                    "churn_90d": risk.inputs.churn_90d,
                    "test_coverage": risk.inputs.test_coverage,
                })),
            }))
        }
        "search" => {
            let hits = search_symbols(store, &case.query, case.limit)?;
            Ok(json!({
                "hits": sorted(hits.iter().map(|hit| hit.qname.clone()).collect())
            }))
        }
        "precision" => {
            let caps = capabilities();
            let answer = impact::blast_radius(store, &case.symbol, case.depth.max(1), None, None)?;
            let tier = Precision::parse(answer.precision.label()).unwrap_or(Precision::Lexical);
            Ok(json!({
                "lexical": caps.lexical,
                "tree_sitter": caps.tree_sitter,
                "lsp": caps.lsp,
                "lsp_available": lsp_available(),
                "tree_sitter_available": tree_sitter_available(),
                "best": best_available().label(),
                "answer_precision": answer.precision.label(),
                // A build with no LSP client must never label an answer `lsp`.
                "answer_within_capability": tier <= best_available(),
            }))
        }
        other => bail!("unsupported KG case kind {other:?}"),
    }
}

/// Verify every case. Returns the structured report; the caller sets the exit
/// code so a failing case is a gate, not a printed opinion.
pub fn verify(fixture_path: &Path) -> Result<Value> {
    if !fixture_path.exists() {
        bail!("KG fixture not found: {}", fixture_path.display());
    }
    let raw = std::fs::read_to_string(fixture_path)
        .with_context(|| format!("read {}", fixture_path.display()))?;
    let fixture: Fixture = serde_json::from_str(&raw).context("parse KG fixture")?;
    if fixture.schema != "mn-kg/1" {
        bail!("unsupported KG fixture schema {:?}", fixture.schema);
    }
    if fixture.cases.is_empty() {
        bail!("KG fixture has no cases");
    }

    let mut results = Vec::with_capacity(fixture.cases.len());
    let mut failures = 0usize;
    for case in &fixture.cases {
        if !case.expect.is_object() {
            bail!("case {} expect must be a JSON object", case.id);
        }
        if case.kind != "search" && case.symbol.trim().is_empty() {
            bail!("case {} has no symbol", case.id);
        }
        if case.kind == "search" && case.query.trim().is_empty() {
            bail!("case {} has no query", case.id);
        }
        let db = GraphDb::new()?;
        seed(case, db.store())?;
        let observed = observe(case, db.store())?;
        let passed = case.expect == observed;
        if !passed {
            failures += 1;
        }
        let reason = if passed {
            String::new()
        } else {
            mismatch_reason(&case.expect, &observed)
        };
        results.push(CaseResult {
            id: case.id.clone(),
            kind: case.kind.clone(),
            symbol: if case.kind == "search" {
                case.query.clone()
            } else {
                case.symbol.clone()
            },
            passed,
            expected: case.expect.clone(),
            observed,
            reason,
        });
    }

    Ok(json!({
        "schema": "mn-kg-result/1",
        "fixture": fixture_path.to_string_lossy(),
        "cases": fixture.cases.len(),
        "passed": fixture.cases.len() - failures,
        "failed": failures,
        "gate": if failures == 0 { "pass" } else { "fail" },
        "results": results,
    }))
}

/// The top-level differing keys, so a failure text says what moved.
fn mismatch_reason(expected: &Value, observed: &Value) -> String {
    let (Value::Object(expected), Value::Object(observed)) = (expected, observed) else {
        return "value mismatch".to_string();
    };
    let mut keys: Vec<String> = expected
        .keys()
        .filter(|key| observed.get(*key) != expected.get(*key))
        .cloned()
        .collect();
    keys.extend(
        observed
            .keys()
            .filter(|key| !expected.contains_key(*key))
            .cloned(),
    );
    format!("mismatch in: {}", keys.join(", "))
}

/// Optional performance probe. Reported separately from correctness: timing is
/// a budget, not a fact about whether the graph answered the right rows.
fn index_budget(root: &Path, budget_ms: u64) -> Result<Value> {
    if !root.is_dir() {
        bail!("index budget root is not a directory: {}", root.display());
    }
    let n = DB_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("mn-kg-budget-{}-{n}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut store = Store::open(&path).context("open budget store")?;
    let started = Instant::now();
    let report = memnoth_codegraph::index_full(&mut store, root).context("full index")?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    drop(store);
    let _ = std::fs::remove_file(&path);
    let met = elapsed_ms <= budget_ms;
    Ok(json!({
        "case": "full_index",
        "root": root.to_string_lossy(),
        "files_indexed": report.files_indexed,
        "symbols_extracted": report.symbols_extracted,
        "truncated": report.truncated,
        "elapsed_ms": elapsed_ms,
        "budget_ms": budget_ms,
        "met_budget": met,
        "gate": if met { "pass" } else { "fail" },
    }))
}

/// `mn bench kg-verify` — non-zero when any deterministic case fails.
pub fn run(verify_args: &KgVerifyArgs) -> Result<Outcome> {
    let default = Path::new("ci").join("kg-verify-cases.json");
    let path = verify_args.cases.as_deref().unwrap_or(&default);
    let mut data = verify(path)?;
    data["index_budget"] = match &verify_args.index_budget_root {
        Some(root) => index_budget(root, verify_args.index_budget_ms)?,
        None => json!({ "gate": "skipped", "reason": "no --index-budget-root supplied" }),
    };

    let passed = data["passed"].as_u64().unwrap_or(0);
    let failed = data["failed"].as_u64().unwrap_or(0);
    let budget_gate = data["index_budget"]["gate"].as_str().unwrap_or("skipped");
    let budget_failed = budget_gate == "fail";
    let text = format!(
        "kg-verify — {passed} passed, {failed} failed, gate {}, index budget {budget_gate}",
        data["gate"].as_str().unwrap_or("unknown")
    );
    let mut outcome = Outcome::new("bench.kg-verify", text, data);
    // A case mismatch is always fatal. A budget failure is fatal only when the
    // caller opted into the probe and supplied its own root and threshold.
    if failed > 0 || budget_failed {
        outcome.exit_code = ExitCode::Unexpected as u8;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_fixture_passes_against_the_engine() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/kg-verify-cases.json");
        let report = verify(&path).expect("verify committed fixture");
        assert_eq!(report["gate"], "pass", "fixture: {report}");
        assert!(report["cases"].as_u64().unwrap() >= 10);
        let kinds: Vec<&str> = report["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["kind"].as_str().unwrap())
            .collect();
        for kind in [
            "definition",
            "refs",
            "callers",
            "callees",
            "impact",
            "search",
            "precision",
        ] {
            assert!(kinds.contains(&kind), "fixture must exercise {kind}");
        }
    }

    #[test]
    fn a_wrong_ground_truth_fails_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wrong.json");
        std::fs::write(
            &path,
            r#"{"schema":"mn-kg/1","cases":[{"id":"bad","kind":"definition",
                "symbol":"app::helper",
                "symbols":[{"id":1,"qname":"app::helper","kind":"function","file":"src/app.rs","line":10}],
                "edges":[],
                "expect":{"resolved":true,"qname":"app::helper","kind":"function","file":"src/app.rs","line":99}}]}"#,
        )
        .unwrap();
        let report = verify(&path).expect("verify wrong fixture");
        assert_eq!(report["gate"], "fail");
        assert_eq!(report["failed"], 1);
        assert_eq!(report["results"][0]["observed"]["line"], 10);
    }
}
