//! DERIVED DIAGNOSTIC — LongMemEval-S-inspired, model-free.
//!
//! This is **not** an official LongMemEval score and is **not** comparable to
//! any vendor-reported number. It borrows only the question taxonomy (current,
//! as-of, knowledge, why, change) and answers every question with the vault's
//! own temporal recall — no model, no network, no API key. The reader is
//! `memnoth_engine::current_facts_known`, the same two-clock query the
//! `vault asof` verb calls.
//!
//! Each case seeds several sessions of attributed facts into an in-memory
//! vault. Ground truth is hand-written in the fixture from those seeding
//! operations; it is never read from the question under test and never produced
//! by the query being graded. Only the fixture builder writes rows directly;
//! every graded value is read back through the public query function. A case
//! the memory layer cannot pass against itself is a seed bug or an engine bug —
//! either way the gate fails, which is what makes this a regression detector
//! rather than a printed opinion.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use memnoth_engine::{EntityKind, MemoryEngine, current_facts_known};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::output::{ExitCode, Outcome};

/// The committed fixture. `mn-lme-derived/1`: a format change must be
/// deliberate, not a silent reinterpretation of old cases.
#[derive(Debug, Clone, Deserialize)]
struct Fixture {
    schema: String,
    cases: Vec<Case>,
}

/// One case. `question` is documentary and is never used for grading — the
/// expected answer is written by hand from `sessions` in the fixture.
#[derive(Debug, Clone, Deserialize)]
struct Case {
    id: String,
    /// One of the five canonical classes: current, as-of, knowledge, why,
    /// change. An unknown class is rejected rather than silently skipped.
    kind: String,
    question: String,
    entity: String,
    /// At least two sessions: the diagnostic exists to test recall that spans
    /// conversations, so a single-session case is not a valid LME-derived case.
    sessions: Vec<Session>,
    /// Valid-time instant the question is asked at.
    at: i64,
    /// Earlier instant for a `change` question.
    from: Option<i64>,
    /// Recorded-time cutoff for a `knowledge` question. Defaults to `at`.
    known: Option<i64>,
    expect: Vec<String>,
    /// `why` question: the attributing humans the answer must name. Absence is
    /// reported as null, never filled in with an agent or OS user.
    expect_authors: Option<Vec<String>>,
    /// `change` question: facts that must appear between `from` and `at`.
    expect_added: Option<Vec<String>>,
    expect_removed: Option<Vec<String>>,
}

/// One conversation in the seed. `date` is when the session happened; a fact's
/// `recorded_at` is when the vault captured that fact as learned. They differ
/// on purpose for a fact the vault never attributed a learning time to.
#[derive(Debug, Clone, Deserialize)]
struct Session {
    id: String,
    /// The attributing human. `unknown` is a placeholder and is reported as
    /// absent by the engine's provenance reader rather than treated as a name.
    user: String,
    date: i64,
    facts: Vec<SeedFact>,
}

#[derive(Debug, Clone, Deserialize)]
struct SeedFact {
    content: String,
    valid_from: i64,
    valid_to: Option<i64>,
    /// `null` = the vault never captured when this was learned; the reader must
    /// exclude such a fact rather than guess a time for it.
    #[serde(default)]
    recorded_at: Option<i64>,
}

/// One case's observed and expected answer, kept whole so a failure is
/// diagnosable without re-running anything.
#[derive(Debug, Clone, serde::Serialize)]
struct CaseResult {
    id: String,
    kind: String,
    question: String,
    entity: String,
    passed: bool,
    expected: Vec<String>,
    observed: Vec<String>,
    expected_authors: Option<Vec<String>>,
    observed_authors: Option<Vec<String>>,
    expected_added: Option<Vec<String>>,
    observed_added: Option<Vec<String>>,
    expected_removed: Option<Vec<String>>,
    observed_removed: Option<Vec<String>>,
    reason: String,
}

/// CLI arguments for `mn bench lme-verify`. Defined here so `bench.rs` only has
/// to add a variant and a dispatch arm.
#[derive(Debug, clap::Args)]
pub struct LmeVerifyArgs {
    /// Fixture JSON (defaults to ci/lme-cases.json).
    #[arg(long)]
    pub cases: Option<PathBuf>,
}

/// Seed one case's vault from its session list. Only the fixture builder writes
/// rows directly; the product write path stays attributed and model-free.
fn seed(case: &Case) -> Result<(MemoryEngine, i64)> {
    let engine = MemoryEngine::open_in_memory().context("open in-memory vault")?;
    let earliest = case
        .sessions
        .iter()
        .flat_map(|session| session.facts.iter())
        .map(|fact| fact.valid_from)
        .min()
        .unwrap_or(0);
    let entity_id = engine
        .add_entity(EntityKind::Concept, &case.entity, 1.0, earliest)
        .context("seed entity")?;
    for session in &case.sessions {
        for fact in &session.facts {
            let provenance = json!({
                "user": session.user,
                "agent": "lme-derived-fixture",
                "session": session.id,
                "session_date": session.date,
                "source": format!("session:{}", session.id),
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
                        fact.content,
                        fact.valid_from,
                        fact.valid_to,
                        fact.recorded_at,
                        provenance,
                    ],
                )
                .with_context(|| format!("seed observation in case {}", case.id))?;
        }
    }
    Ok((engine, entity_id))
}

/// Answer one case through the engine's two-clock query. `known` defaults to
/// the asked instant, so an as-of question means "believed and learned by
/// then" — never a fact recorded in the future.
fn answer(
    case: &Case,
    engine: &MemoryEngine,
    entity_id: i64,
    at: i64,
    known: i64,
) -> Result<(Vec<String>, Vec<String>)> {
    let facts = current_facts_known(engine, entity_id, at, known)
        .with_context(|| format!("answer case {}", case.id))?;
    let mut contents: Vec<String> = facts
        .iter()
        .filter_map(|fact| fact["fact"].as_str().map(str::to_owned))
        .collect();
    contents.sort();
    let mut authors: Vec<String> = facts
        .iter()
        .filter_map(|fact| fact["author"].as_str().map(str::to_owned))
        .collect();
    authors.sort();
    authors.dedup();
    Ok((contents, authors))
}

/// Sorted, deduplicated comparison form for an expected fact list.
fn sorted(values: &[String]) -> Vec<String> {
    let mut values = values.to_vec();
    values.sort();
    values.dedup();
    values
}

/// Set difference over sorted fact lists: the `change` question type.
fn difference(left: &[String], right: &[String]) -> Vec<String> {
    left.iter()
        .filter(|item| !right.contains(item))
        .cloned()
        .collect()
}

/// Reject a fixture that could pass without ever being graded, or that drifts
/// from the five canonical classes this diagnostic owns.
fn validate(case: &Case) -> Result<()> {
    if !matches!(
        case.kind.as_str(),
        "current" | "as-of" | "knowledge" | "why" | "change"
    ) {
        bail!(
            "case {} uses unknown question class {:?}",
            case.id,
            case.kind
        );
    }
    if case.sessions.len() < 2 {
        bail!(
            "case {} must seed at least two sessions; got {}",
            case.id,
            case.sessions.len()
        );
    }
    if case.entity.trim().is_empty() {
        bail!("case {} has no entity", case.id);
    }
    if case.question.trim().is_empty() {
        bail!("case {} has no question", case.id);
    }
    if case.expect.is_empty() {
        bail!("case {} has no hand-written expected facts", case.id);
    }
    if case.kind == "why" && case.expect_authors.is_none() {
        bail!("why case {} has no expect_authors", case.id);
    }
    if case.kind == "change" {
        if case.from.is_none() {
            bail!("change case {} has no from instant", case.id);
        }
        if case.expect_added.is_none() || case.expect_removed.is_none() {
            bail!(
                "change case {} needs expect_added and expect_removed",
                case.id
            );
        }
    }
    for session in &case.sessions {
        if session.id.trim().is_empty() {
            bail!("case {} has a session with no id", case.id);
        }
        if session.user.trim().is_empty() {
            bail!("case {} has a session with no attributed user", case.id);
        }
        if session.facts.is_empty() {
            bail!("case {} has an empty session {}", case.id, session.id);
        }
    }
    Ok(())
}

/// Verify every case. Returns the structured report; the caller sets the exit
/// code so a failing case is a gate, not a printed opinion.
pub fn verify(fixture_path: &Path) -> Result<Value> {
    if !fixture_path.exists() {
        bail!(
            "LME derived diagnostic fixture not found: {}",
            fixture_path.display()
        );
    }
    let raw = std::fs::read_to_string(fixture_path)
        .with_context(|| format!("read {}", fixture_path.display()))?;
    let fixture: Fixture = serde_json::from_str(&raw).context("parse LME fixture")?;
    if fixture.schema != "mn-lme-derived/1" {
        bail!("unsupported LME fixture schema {:?}", fixture.schema);
    }
    if fixture.cases.is_empty() {
        bail!("LME fixture has no cases");
    }

    let mut results = Vec::with_capacity(fixture.cases.len());
    let mut failures = 0usize;
    for case in &fixture.cases {
        validate(case)?;
        let (engine, entity_id) = seed(case)?;
        let known = case.known.unwrap_or(case.at);
        let (observed, authors) = answer(case, &engine, entity_id, case.at, known)?;
        let (observed_added, observed_removed) = match case.from {
            Some(from) => {
                let (before, _) = answer(case, &engine, entity_id, from, from)?;
                (
                    Some(difference(&observed, &before)),
                    Some(difference(&before, &observed)),
                )
            }
            None => (None, None),
        };
        let observed_authors = case.expect_authors.as_ref().map(|_| authors);

        let expected = sorted(&case.expect);
        let expected_authors = case.expect_authors.as_ref().map(|v| sorted(v));
        let expected_added = case.expect_added.as_ref().map(|v| sorted(v));
        let expected_removed = case.expect_removed.as_ref().map(|v| sorted(v));

        let mut reasons = Vec::new();
        if expected != observed {
            reasons.push("facts");
        }
        if expected_authors != observed_authors {
            reasons.push("authors");
        }
        if expected_added != observed_added {
            reasons.push("added");
        }
        if expected_removed != observed_removed {
            reasons.push("removed");
        }
        let passed = reasons.is_empty();
        if !passed {
            failures += 1;
        }
        results.push(CaseResult {
            id: case.id.clone(),
            kind: case.kind.clone(),
            question: case.question.clone(),
            entity: case.entity.clone(),
            passed,
            expected,
            observed,
            expected_authors,
            observed_authors,
            expected_added,
            observed_added,
            expected_removed,
            observed_removed,
            reason: if passed {
                String::new()
            } else {
                format!("mismatch in: {}", reasons.join(", "))
            },
        });
    }

    Ok(json!({
        "schema": "mn-lme-result/1",
        "diagnostic": "derived",
        "official": false,
        "reader": "memnoth_engine::current_facts_known",
        "note": "Derived diagnostic of the vault's temporal recall, not an official LongMemEval score and not comparable to vendor-reported results.",
        "fixture": fixture_path.to_string_lossy(),
        "cases": fixture.cases.len(),
        "passed": fixture.cases.len() - failures,
        "failed": failures,
        "gate": if failures == 0 { "pass" } else { "fail" },
        "results": results,
    }))
}

/// `mn bench lme-verify` — non-zero when any case regresses.
pub fn run(verify_args: &LmeVerifyArgs) -> Result<Outcome> {
    let default = Path::new("ci").join("lme-cases.json");
    let path = verify_args.cases.as_deref().unwrap_or(&default);
    let data = verify(path)?;
    let passed = data["passed"].as_u64().unwrap_or(0);
    let failed = data["failed"].as_u64().unwrap_or(0);
    let text = format!(
        "lme-derived — {passed} passed, {failed} failed, gate {} (derived diagnostic; not an official LongMemEval score)",
        data["gate"].as_str().unwrap_or("unknown")
    );
    let mut outcome = Outcome::new("bench.lme-verify", text, data);
    if failed > 0 {
        outcome.exit_code = ExitCode::Unexpected as u8;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/lme-cases.json")
    }

    #[test]
    fn the_committed_derived_fixture_passes_against_the_engine() {
        let report = verify(&fixture()).expect("verify committed fixture");
        assert_eq!(report["gate"], "pass", "fixture: {report}");
        assert_eq!(report["diagnostic"], "derived");
        assert_eq!(report["official"], false);
        assert!(report["cases"].as_u64().unwrap() >= 7);
        assert_eq!(report["failed"], 0);
        let kinds: Vec<&str> = report["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["kind"].as_str().unwrap())
            .collect();
        for kind in ["current", "as-of", "knowledge", "why", "change"] {
            assert!(kinds.contains(&kind), "fixture must exercise {kind}");
        }
    }

    #[test]
    fn every_case_seeds_at_least_two_sessions() {
        let report = verify(&fixture()).expect("verify committed fixture");
        // A single-session case would not test cross-session recall at all.
        for row in report["results"].as_array().unwrap() {
            assert_eq!(row["passed"], true, "case {} failed", row["id"]);
        }
    }

    #[test]
    fn a_wrong_ground_truth_fails_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wrong.json");
        std::fs::write(
            &path,
            r#"{"schema":"mn-lme-derived/1","cases":[{"id":"bad","kind":"current",
                "question":"q?","entity":"e","sessions":[
                  {"id":"s1","date":1000,"user":"asha","facts":[
                    {"content":"truth","valid_from":1000,"valid_to":null,"recorded_at":1000}]},
                  {"id":"s2","date":2000,"user":"bob","facts":[
                    {"content":"truth","valid_from":2000,"valid_to":null,"recorded_at":2000}]}],
                "at":3000,"expect":["fabrication"]}]}"#,
        )
        .unwrap();
        let report = verify(&path).expect("verify wrong fixture");
        assert_eq!(report["gate"], "fail");
        assert_eq!(report["failed"], 1);
        assert_eq!(report["results"][0]["observed"][0], "truth");
    }

    #[test]
    fn unrecorded_learning_is_excluded_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unrecorded.json");
        std::fs::write(
            &path,
            r#"{"schema":"mn-lme-derived/1","cases":[{"id":"u","kind":"knowledge",
                "question":"q?","entity":"e","sessions":[
                  {"id":"s1","date":1000,"user":"unknown","facts":[
                    {"content":"when-unknown","valid_from":1000,"valid_to":null,"recorded_at":null}]},
                  {"id":"s2","date":1000,"user":"asha","facts":[
                    {"content":"recorded","valid_from":1000,"valid_to":null,"recorded_at":1000}]}],
                "at":2000,"known":2000,"expect":["recorded"]}]}"#,
        )
        .unwrap();
        let report = verify(&path).expect("verify unrecorded fixture");
        assert_eq!(report["gate"], "pass");
        assert_eq!(report["results"][0]["observed"][0], "recorded");
        assert_eq!(
            report["results"][0]["observed"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn an_unknown_question_class_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bogus.json");
        std::fs::write(
            &path,
            r#"{"schema":"mn-lme-derived/1","cases":[{"id":"b","kind":"sentiment",
                "question":"q?","entity":"e","sessions":[
                  {"id":"s1","date":1000,"user":"a","facts":[
                    {"content":"x","valid_from":1000,"valid_to":null,"recorded_at":1000}]},
                  {"id":"s2","date":2000,"user":"b","facts":[
                    {"content":"y","valid_from":2000,"valid_to":null,"recorded_at":2000}]}],
                "at":3000,"expect":["x"]}]}"#,
        )
        .unwrap();
        assert!(verify(&path).is_err(), "unknown class must be rejected");
    }

    #[test]
    fn a_single_session_case_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("single.json");
        std::fs::write(
            &path,
            r#"{"schema":"mn-lme-derived/1","cases":[{"id":"s","kind":"current",
                "question":"q?","entity":"e","sessions":[
                  {"id":"s1","date":1000,"user":"a","facts":[
                    {"content":"x","valid_from":1000,"valid_to":null,"recorded_at":1000}]}],
                "at":2000,"expect":["x"]}]}"#,
        )
        .unwrap();
        let err = verify(&path).unwrap_err();
        assert!(err.to_string().contains("at least two sessions"));
    }

    #[test]
    fn sorted_and_difference_are_mechanical() {
        assert_eq!(sorted(&["b".into(), "a".into(), "b".into()]), ["a", "b"]);
        let before = vec!["off".to_string(), "stay".to_string()];
        let after = vec!["on".to_string(), "stay".to_string()];
        assert_eq!(difference(&after, &before), ["on"]);
        assert_eq!(difference(&before, &after), ["off"]);
    }
}
