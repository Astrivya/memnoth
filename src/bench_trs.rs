//! Deterministic Temporal Recall Suite verification: no model, no network.
//!
//! `mn bench trs-verify` seeds small in-memory vaults from a committed fixture
//! and checks the engine's own temporal answers against ground truth derived
//! from the seeding operations. This is the lane that can actually fail: a
//! `score` read from an LLM envelope or a substring match on prose cannot
//! establish that the vault remembers the right thing at the right time.
//!
//! The fixture's expected facts are written by hand from the operation list,
//! never produced by the query under test. A case that the memory layer cannot
//! pass against itself is a seed bug or an engine bug — either way it fails
//! here before any agent is measured.

use std::path::Path;

use anyhow::{Context, Result};
use memnoth_engine::{EntityKind, MemoryEngine, current_facts_known};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::output::{ExitCode, Outcome};

/// The committed fixture. `mn-invariants/1` style versioning: a format change
/// must be deliberate, not a silent reinterpretation of old cases.
#[derive(Debug, Clone, Deserialize)]
struct Fixture {
    schema: String,
    cases: Vec<Case>,
}

#[derive(Debug, Clone, Deserialize)]
struct Case {
    id: String,
    /// `current`, `as-of` or `knowledge`. Documentary only: what is graded is
    /// the two-clock answer, not a label the fixture could lie about.
    kind: String,
    entity: String,
    ops: Vec<Op>,
    at: i64,
    /// When present, only facts recorded by this instant may be returned.
    known: Option<i64>,
    expect: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Op {
    op: String,
    name: Option<String>,
    content: Option<String>,
    valid_from: i64,
    valid_to: Option<i64>,
    /// None = the vault never captured when this was learned. The verifier
    /// must exclude such a fact rather than guess a time for it.
    recorded_at: Option<i64>,
    author: Option<String>,
}

/// One case's observed result, including the facts it actually returned so a
/// failure is diagnosable without re-running a model.
#[derive(Debug, Clone, serde::Serialize)]
struct CaseResult {
    id: String,
    entity: String,
    kind: String,
    passed: bool,
    expected: Vec<String>,
    observed: Vec<String>,
    reason: String,
}

/// Seed one case's vault from its operation list. Only the fixture builder
/// writes rows directly; the product write path stays attributed and
/// model-free through the engine.
fn seed(case: &Case) -> Result<(MemoryEngine, i64)> {
    let engine = MemoryEngine::open_in_memory().context("open in-memory vault")?;
    let mut entity_id = 0i64;
    for op in &case.ops {
        match op.op.as_str() {
            "add" => {
                let name = op.name.as_deref().context("add op needs a name")?;
                entity_id = engine
                    .add_entity(EntityKind::Concept, name, 1.0, op.valid_from)
                    .context("seed entity")?;
            }
            "fact" => {
                if entity_id == 0 {
                    anyhow::bail!("case {} has a fact before its entity", case.id);
                }
                let content = op.content.as_deref().context("fact op needs content")?;
                let provenance = json!({
                    "user": op.author.as_deref().unwrap_or("unknown"),
                    "agent": "fixture",
                })
                .to_string();
                engine
                    .connection()
                    .execute(
                        "INSERT INTO observations \
                            (entity_id, content, kind, confidence, valid_from, valid_to, \
                             recorded_at, provenance) \
                         VALUES (?1, ?2, 'fact', 1.0, ?3, ?4, ?5, ?6)",
                        params![
                            entity_id,
                            content,
                            op.valid_from,
                            op.valid_to,
                            op.recorded_at,
                            provenance
                        ],
                    )
                    .context("seed observation")?;
            }
            other => anyhow::bail!("case {} uses unknown op {other:?}", case.id),
        }
    }
    if entity_id == 0 {
        anyhow::bail!("case {} never creates an entity", case.id);
    }
    Ok((engine, entity_id))
}

/// Answer one case through the engine's two-clock query. `known` defaults to
/// the asked instant, so an as-of question means "believed and learned by
/// then" — never a fact recorded in the future.
fn answer(case: &Case, engine: &MemoryEngine, entity_id: i64) -> Result<Vec<String>> {
    let known = case.known.unwrap_or(case.at);
    let facts = current_facts_known(engine, entity_id, case.at, known)
        .with_context(|| format!("answer case {}", case.id))?;
    let mut observed: Vec<String> = facts
        .iter()
        .filter_map(|fact| fact["fact"].as_str().map(str::to_owned))
        .collect();
    observed.sort();
    Ok(observed)
}

/// Verify every case. Returns the structured report; the caller sets the exit
/// code so a failing case is a gate, not a printed opinion.
pub fn verify(fixture_path: &Path) -> Result<Value> {
    if !fixture_path.exists() {
        anyhow::bail!("TRS fixture not found: {}", fixture_path.display());
    }
    let raw = std::fs::read_to_string(fixture_path)
        .with_context(|| format!("read {}", fixture_path.display()))?;
    let fixture: Fixture = serde_json::from_str(&raw).context("parse TRS fixture")?;
    if fixture.schema != "mn-trs/1" {
        anyhow::bail!("unsupported TRS fixture schema {:?}", fixture.schema);
    }
    if fixture.cases.is_empty() {
        anyhow::bail!("TRS fixture has no cases");
    }

    let mut results = Vec::with_capacity(fixture.cases.len());
    let mut failures = 0usize;
    for case in &fixture.cases {
        // The label and the seeded entity must agree, or a case could pass by
        // answering about a different entity than the one it claims to test.
        if !case
            .ops
            .iter()
            .any(|op| op.op == "add" && op.name.as_deref() == Some(case.entity.as_str()))
        {
            anyhow::bail!(
                "case {} claims entity {:?} but never seeds it",
                case.id,
                case.entity
            );
        }
        let (engine, entity_id) = seed(case)?;
        let observed = answer(case, &engine, entity_id)?;
        let mut expected = case.expect.clone();
        expected.sort();
        let passed = observed == expected;
        if !passed {
            failures += 1;
        }
        results.push(CaseResult {
            id: case.id.clone(),
            entity: case.entity.clone(),
            kind: case.kind.clone(),
            passed,
            expected,
            observed,
            reason: if passed {
                String::new()
            } else {
                "returned facts differ from the fixture's ground truth".to_string()
            },
        });
    }

    Ok(json!({
        "schema": "mn-trs-result/1",
        "fixture": fixture_path.to_string_lossy(),
        "cases": fixture.cases.len(),
        "passed": fixture.cases.len() - failures,
        "failed": failures,
        "gate": if failures == 0 { "pass" } else { "fail" },
        "results": results,
    }))
}

/// `mn bench trs-verify` — non-zero when any deterministic case fails.
pub fn run(verify_args: &crate::bench::TrsVerifyArgs) -> Result<Outcome> {
    let default = Path::new("ci").join("trs-cases.json");
    let path = verify_args.cases.as_deref().unwrap_or(&default);
    let data = verify(path)?;
    let passed = data["passed"].as_u64().unwrap_or(0);
    let failed = data["failed"].as_u64().unwrap_or(0);
    let text = format!(
        "trs-verify — {passed} passed, {failed} failed, gate {}",
        data["gate"].as_str().unwrap_or("unknown")
    );
    let mut outcome = Outcome::new("bench.trs-verify", text, data);
    if failed > 0 {
        outcome.exit_code = ExitCode::Unexpected as u8;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_fixture_passes_against_the_engine() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/trs-cases.json");
        let report = verify(&path).expect("verify committed fixture");
        assert_eq!(report["gate"], "pass", "fixture: {report}");
        assert!(report["cases"].as_u64().unwrap() >= 5);
    }

    #[test]
    fn a_wrong_expected_answer_fails_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wrong.json");
        std::fs::write(
            &path,
            r#"{"schema":"mn-trs/1","cases":[{"id":"x","kind":"current","entity":"e",
                "ops":[{"op":"add","name":"e","valid_from":1000,"recorded_at":1000},
                       {"op":"fact","content":"real","valid_from":1000,"recorded_at":1000}],
                "at":2000,"expect":["invented"]}]}"#,
        )
        .unwrap();
        let report = verify(&path).expect("verify wrong fixture");
        assert_eq!(report["gate"], "fail");
        assert_eq!(report["failed"], 1);
        assert_eq!(report["results"][0]["observed"][0], "real");
    }
}
