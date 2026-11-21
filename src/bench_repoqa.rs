//! BENCH-05 — model-free, RepoQA-*derived* code-retrieval diagnostic.
//!
//! `mn bench repoqa-verify` seeds a tiny in-memory code graph from a committed
//! fixture, then grades the engine's own deterministic FTS rank against ground
//! truth that is written by hand from the seed. It measures Recall@10 and MRR
//! and reports query latency separately. No model, no network, no subprocess:
//! the same fixture always produces the same metrics.
//!
//! HONESTY. This is a *derived* diagnostic on a fixed synthetic seed; it is NOT
//! an official RepoQA score and must never be extrapolated to a full RepoQA
//! suite or to real repositories. The numbers here describe only this 21-symbol
//! seed and these hand-written queries. Latency is informational and excluded
//! from the quality gate. A regression against the committed baseline returns a
//! non-zero exit code so CI can fail on it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use memnoth_engine::{EntityKind, MemoryEngine};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::output::{ExitCode, Outcome};

/// Only this fixture schema is understood; a format change must be deliberate.
const SCHEMA: &str = "mn-repoqa-derived/1";
/// Fixed validity instant for every seeded symbol (well before `as_of`).
const VALID_FROM: i64 = 1_000_000_000_000;
/// Retrieval depth of the graded metric.
const TOP_K: usize = 10;

/// CLI arguments for `mn bench repoqa-verify`. Defined here so the only wiring
/// needed in `bench.rs` is one enum variant and one dispatch arm.
#[derive(Debug, clap::Args)]
pub struct RepoQaVerifyArgs {
    /// Fixture JSON (defaults to ci/repoqa-cases.json).
    #[arg(long)]
    pub cases: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
struct Fixture {
    schema: String,
    as_of: i64,
    symbols: Vec<SeedSymbol>,
    queries: Vec<RetrievalQuery>,
    baseline: Baseline,
}

#[derive(Debug, Clone, Deserialize)]
struct SeedSymbol {
    name: String,
    lang: String,
    summary: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RetrievalQuery {
    id: String,
    query: String,
    /// Hand-written relevant symbol names, never derived from the ranker.
    relevant: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Baseline {
    min_recall_at_10: f64,
    min_mrr: f64,
}

/// One query's observed rank details, so a failure is diagnosable without a
/// model or a re-run.
#[derive(Debug, Clone, serde::Serialize)]
struct RankHit {
    name: String,
    rank: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
struct QueryResult {
    id: String,
    query: String,
    expected: Vec<String>,
    top10: Vec<String>,
    found: Vec<RankHit>,
    recall_at_10: f64,
    reciprocal_rank: f64,
    latency_ms: f64,
}

/// Stable, content-derived fingerprint of the seed so a silent fixture edit is
/// visible in the report. FNV-1a over sorted `lang|name|summary` lines.
fn seed_hash(fixture: &Fixture) -> String {
    let mut lines: Vec<String> = fixture
        .symbols
        .iter()
        .map(|s| format!("{}|{}|{}", s.lang, s.name, s.summary))
        .collect();
    lines.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in lines.join("\n").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Validate the fixture before touching the engine: a malformed seed must fail
/// loudly rather than produce a plausible-looking score.
fn validate(fixture: &Fixture) -> Result<()> {
    if fixture.schema != SCHEMA {
        bail!("unsupported RepoQA fixture schema {:?}", fixture.schema);
    }
    if fixture.as_of <= VALID_FROM {
        bail!("fixture as_of must be after the seed validity instant");
    }
    if fixture.symbols.len() < 2 {
        bail!("RepoQA fixture needs at least two symbols");
    }
    let mut langs = std::collections::BTreeSet::new();
    let mut names = std::collections::BTreeSet::new();
    for symbol in &fixture.symbols {
        if symbol.name.is_empty() || symbol.lang.is_empty() || symbol.summary.is_empty() {
            bail!("symbol name, lang and summary must be non-empty");
        }
        if !names.insert(symbol.name.as_str()) {
            bail!("duplicate seeded symbol {:?}", symbol.name);
        }
        langs.insert(symbol.lang.as_str());
    }
    if langs.len() < 2 {
        bail!("RepoQA fixture needs at least two languages");
    }
    if fixture.queries.is_empty() {
        bail!("RepoQA fixture has no queries");
    }
    let mut ids = std::collections::BTreeSet::new();
    for query in &fixture.queries {
        if !ids.insert(query.id.as_str()) {
            bail!("duplicate query id {:?}", query.id);
        }
        if query.query.trim().is_empty() {
            bail!("query {} has empty text", query.id);
        }
        if query.relevant.is_empty() {
            bail!("query {} has no hand-written ground truth", query.id);
        }
        for name in &query.relevant {
            if !names.contains(name.as_str()) {
                bail!("query {} names unseeded symbol {:?}", query.id, name);
            }
        }
    }
    Ok(())
}

/// Seed the in-memory code graph. The engine is a temporal property graph; a
/// code symbol is an `EntityKind::Symbol` with its docstring as the summary
/// (indexed by FTS at insert time) and its language in JSON metadata.
fn seed(fixture: &Fixture) -> Result<MemoryEngine> {
    let engine = MemoryEngine::open_in_memory().context("open in-memory code graph")?;
    for symbol in &fixture.symbols {
        let id = engine
            .ingest_entity_idempotent(
                EntityKind::Symbol,
                &symbol.name,
                Some(&symbol.summary),
                1.0,
                VALID_FROM,
            )
            .with_context(|| format!("seed symbol {}", symbol.name))?;
        // metadata is not part of the FTS index, so setting it after insert
        // cannot desynchronise the index (which is insert-trigger only).
        engine
            .connection()
            .execute(
                "UPDATE entities SET metadata = ?1 WHERE id = ?2",
                rusqlite::params![json!({ "lang": symbol.lang }).to_string(), id],
            )
            .context("seed symbol language metadata")?;
    }
    Ok(engine)
}

/// Mean of a non-empty slice; empty is defined as zero (callers always pass
/// at least one query after validation).
fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// Nearest-rank percentile on an already sorted slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let last = sorted.len() - 1;
    let index = ((p * last as f64).round() as usize).min(last);
    sorted[index]
}

fn round(value: f64, decimals: u32) -> f64 {
    let scale = 10_f64.powi(decimals as i32);
    (value * scale).round() / scale
}

/// Run the whole diagnostic and return the structured report. Public so the
/// integration test can exercise the full pipeline without wiring the CLI.
pub fn verify(fixture_path: &Path) -> Result<Value> {
    if !fixture_path.exists() {
        bail!("RepoQA fixture not found: {}", fixture_path.display());
    }
    let raw = std::fs::read_to_string(fixture_path)
        .with_context(|| format!("read {}", fixture_path.display()))?;
    let fixture: Fixture = serde_json::from_str(&raw).context("parse RepoQA fixture")?;
    validate(&fixture)?;

    let engine = seed(&fixture)?;
    let hash = seed_hash(&fixture);
    let mut results = Vec::with_capacity(fixture.queries.len());
    let mut latencies = Vec::with_capacity(fixture.queries.len());
    let mut recalls = Vec::with_capacity(fixture.queries.len());
    let mut reciprocal_ranks = Vec::with_capacity(fixture.queries.len());

    for query in &fixture.queries {
        let started = Instant::now();
        let hits = engine
            .search_entities(&query.query, TOP_K, fixture.as_of)
            .with_context(|| format!("retrieve query {}", query.id))?;
        let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
        latencies.push(latency_ms);

        let top10: Vec<String> = hits.iter().map(|(entity, _)| entity.name.clone()).collect();
        let found: Vec<RankHit> = query
            .relevant
            .iter()
            .filter_map(|expected| {
                top10
                    .iter()
                    .position(|name| name == expected)
                    .map(|index| RankHit {
                        name: expected.clone(),
                        rank: index + 1,
                    })
            })
            .collect();
        let recall = found.len() as f64 / query.relevant.len() as f64;
        let reciprocal_rank = found
            .iter()
            .map(|hit| 1.0 / hit.rank as f64)
            .fold(0.0_f64, f64::max);
        recalls.push(recall);
        reciprocal_ranks.push(reciprocal_rank);
        results.push(QueryResult {
            id: query.id.clone(),
            query: query.query.clone(),
            expected: query.relevant.clone(),
            top10,
            found,
            recall_at_10: round(recall, 6),
            reciprocal_rank: round(reciprocal_rank, 6),
            latency_ms: round(latency_ms, 3),
        });
    }

    let recall_at_10 = mean(&recalls);
    let mrr = mean(&reciprocal_ranks);
    let mut regressions = Vec::new();
    if recall_at_10 + f64::EPSILON < fixture.baseline.min_recall_at_10 {
        regressions.push(format!(
            "recall@10 {recall_at_10:.4} below baseline {:.4}",
            fixture.baseline.min_recall_at_10
        ));
    }
    if mrr + f64::EPSILON < fixture.baseline.min_mrr {
        regressions.push(format!(
            "mrr {mrr:.4} below baseline {:.4}",
            fixture.baseline.min_mrr
        ));
    }
    let passed = regressions.is_empty();

    let mut sorted_latency = latencies.clone();
    sorted_latency.sort_by(|a, b| a.total_cmp(b));
    let mut language_counts = BTreeMap::new();
    for symbol in &fixture.symbols {
        *language_counts.entry(symbol.lang.clone()).or_insert(0usize) += 1;
    }

    Ok(json!({
        "schema": "mn-repoqa-result/1",
        "label": "RepoQA-derived diagnostic (NOT an official RepoQA score)",
        "disclaimer": "Derived, synthetic diagnostic on a fixed in-memory seed with hand-written ground truth. Not an official RepoQA score and not extrapolated to any full RepoQA suite or real repository.",
        "scope": "21 seeded symbols across 3 languages; 21 hand-written queries; local deterministic FTS rank only",
        "fixture": fixture_path.to_string_lossy(),
        "seed_hash": hash,
        "symbols": fixture.symbols.len(),
        "languages": language_counts,
        "queries": fixture.queries.len(),
        "metrics": {
            "recall_at_10": round(recall_at_10, 6),
            "mrr": round(mrr, 6),
            "top_k": TOP_K,
        },
        "baseline": {
            "min_recall_at_10": fixture.baseline.min_recall_at_10,
            "min_mrr": fixture.baseline.min_mrr,
        },
        "gate": if passed { "pass" } else { "fail" },
        "regressions": regressions,
        "latency": {
            "unit": "ms",
            "included_in_gate": false,
            "note": "Informational only: wall-clock FTS query time, excluded from the quality gate.",
            "total_ms": round(latencies.iter().sum::<f64>(), 3),
            "mean_ms": round(mean(&latencies), 3),
            "p50_ms": round(percentile(&sorted_latency, 0.50), 3),
            "p95_ms": round(percentile(&sorted_latency, 0.95), 3),
            "min_ms": round(sorted_latency.first().copied().unwrap_or(0.0), 3),
            "max_ms": round(sorted_latency.last().copied().unwrap_or(0.0), 3),
        },
        "results": results,
    }))
}

/// `mn bench repoqa-verify` — non-zero when a metric regresses against the
/// committed baseline.
pub fn run(args: &RepoQaVerifyArgs) -> Result<Outcome> {
    let default = Path::new("ci").join("repoqa-cases.json");
    let path = args.cases.as_deref().unwrap_or(&default);
    let data = verify(path)?;
    let passed = data["gate"] == "pass";
    let recall = data["metrics"]["recall_at_10"].as_f64().unwrap_or(0.0);
    let mrr = data["metrics"]["mrr"].as_f64().unwrap_or(0.0);
    let symbols = data["symbols"].as_u64().unwrap_or(0);
    let languages = data["languages"]
        .as_object()
        .map(|map| map.len())
        .unwrap_or(0);
    let text = format!(
        "repoqa-derived diagnostic — recall@10 {recall:.3}, mrr {mrr:.3}, gate {} \
         (NOT an official RepoQA score; {symbols} synthetic symbols across {languages} languages)",
        data["gate"].as_str().unwrap_or("unknown")
    );
    let mut outcome = Outcome::new("bench.repoqa-verify", text, data);
    if !passed {
        outcome.exit_code = ExitCode::Unexpected as u8;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/repoqa-cases.json")
    }

    #[test]
    fn committed_fixture_passes_the_gate() {
        let report = verify(&fixture_path()).expect("verify committed fixture");
        assert_eq!(report["gate"], "pass", "report: {report}");
        assert_eq!(report["metrics"]["recall_at_10"], 1.0);
        assert_eq!(report["metrics"]["mrr"], 1.0);
        assert_eq!(report["symbols"], 21);
        assert_eq!(report["queries"], 21);
        assert!(report["languages"].as_object().unwrap().len() >= 2);
        assert_eq!(report["results"].as_array().unwrap().len(), 21);
        for row in report["results"].as_array().unwrap() {
            assert_eq!(row["found"].as_array().unwrap().len(), 1, "row: {row}");
            assert_eq!(row["found"][0]["rank"], 1, "row: {row}");
        }
    }

    #[test]
    fn metrics_are_deterministic_and_latency_is_separate() {
        let first = verify(&fixture_path()).expect("first verify");
        let second = verify(&fixture_path()).expect("second verify");
        assert_eq!(first["metrics"], second["metrics"]);
        assert_eq!(first["seed_hash"], second["seed_hash"]);
        assert_eq!(first["gate"], second["gate"]);
        assert_eq!(first["latency"]["included_in_gate"], false);
        assert!(first["latency"]["mean_ms"].as_f64().unwrap() >= 0.0);
    }

    #[test]
    fn a_tightened_baseline_fails_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("regressed.json");
        let raw = std::fs::read_to_string(fixture_path()).unwrap();
        let mut value: Value = serde_json::from_str(&raw).unwrap();
        value["baseline"]["min_mrr"] = json!(1.1);
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        let report = verify(&path).expect("verify regressed fixture");
        assert_eq!(report["gate"], "fail");
        assert_eq!(report["metrics"]["mrr"], 1.0);
        assert!(
            report["regressions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r.as_str().unwrap().contains("mrr"))
        );

        let outcome = run(&RepoQaVerifyArgs {
            cases: Some(path.clone()),
        })
        .unwrap();
        assert_eq!(outcome.exit_code, ExitCode::Unexpected as u8);
    }

    #[test]
    fn an_unseeded_ground_truth_symbol_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        let raw = std::fs::read_to_string(fixture_path()).unwrap();
        let mut value: Value = serde_json::from_str(&raw).unwrap();
        value["queries"][0]["relevant"][0] = json!("never_seeded_symbol");
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        assert!(verify(&path).is_err());
    }

    #[test]
    fn percentile_and_mean_handle_empty_and_single_values() {
        assert_eq!(mean(&[]), 0.0);
        assert_eq!(mean(&[1.0, 2.0, 3.0]), 2.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
        assert_eq!(percentile(&[7.0], 0.95), 7.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.5), 3.0);
    }
}
