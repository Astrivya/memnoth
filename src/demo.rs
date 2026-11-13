//! `memnoth demo` — "why is this code like this?" in one offline command.
//!
//! Seeds (or re-opens) a tiny temporal scenario in the invocation's vault and
//! answers with all five elements: the decision, when it was believed, what
//! superseded it, the commit that introduced it, and both sides of a
//! contradiction with their dates.
//!
//! Two properties hold this together:
//!
//! * **One data structure, two renderings.** The text a person reads and the
//!   `--json` payload a program reads are both derived from [`Story`], so a
//!   date or a side cannot disagree between them.
//! * **No second surface.** The demo opens the vault directly. It does not go
//!   through the TUI, an MCP host, or the network, so it works in a bare
//!   terminal on a machine with no account.
//!
//! Re-running is idempotent: the seeded entities and relations are found before
//! they are created, and a belief that was already superseded is reported, not
//! superseded again.

use std::fmt::Write as _;

use anyhow::{Context, Result, bail};
use memnoth_engine::{EntityKind, MemoryEngine, now_ms};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::AppContext;
use crate::output::{ErrorOutcome, ExitCode, Outcome, rfc3339};

/// One day in milliseconds.
const DAY_MS: i64 = 86_400_000;

/// The story the demo seeds, bundled into the binary.
///
/// It lives with the test fixtures on purpose: the seeded regression test reads
/// the same bytes the product ships, so editing the story cannot change one and
/// leave the other behind.
pub const SCENARIO_JSON: &str = include_str!("../tests/fixtures/why-demo/scenario.json");

/// `memnoth demo` entry point.
pub fn run(ctx: &AppContext) -> Result<Outcome, ErrorOutcome> {
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open demo vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url(crate::context::docs_url("demo"))
    })?;

    let scenario = Scenario::bundled().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "the bundled demo story is unreadable")
            .with_cause(e.to_string())
            .with_fix("reinstall memnoth; this build's demo data is corrupt")
            .with_docs_url(crate::context::docs_url("demo"))
    })?;

    let mut story = seed(&engine, &scenario, now_ms()).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to seed the demo story")
            .with_cause(e.to_string())
            .with_fix("check the vault state with `memnoth vault status`")
            .with_docs_url(crate::context::docs_url("demo"))
    })?;
    story.vault = ctx.vault_path.display().to_string();

    let text = render(&story);
    let data = serde_json::to_value(&story).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to render the demo answer")
            .with_cause(e.to_string())
    })?;
    Ok(Outcome::new("demo", text, data))
}

// ---------------------------------------------------------------- the script

/// The demo's script: what to believe, when, and what contradicts it.
///
/// Day offsets are relative to the run, so the story is always recent without
/// the dates being fabricated on every re-run — the vault stores the instant
/// the story was first seeded and every rendering reads it back.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Scenario {
    /// The question the demo answers.
    pub question: String,
    /// The decision as it was first believed.
    pub decision: DecisionSpec,
    /// The correction that replaced it.
    pub superseded_by: SupersessionSpec,
    /// The commit that introduced the decision.
    pub source: SourceSpec,
    /// Two beliefs that disagree about the same thing.
    pub contradiction: ContradictionSpec,
}

/// The first belief.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DecisionSpec {
    /// Entity name in the vault.
    pub entity: String,
    /// What was believed.
    pub belief: String,
    /// The observation recorded against the entity.
    pub observation: String,
    /// Confidence, 0.0–1.0.
    pub confidence: f64,
    /// How many days before the run the belief was recorded.
    pub days_ago: u32,
}

/// The belief that replaced it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SupersessionSpec {
    /// Entity name; the successor keeps the name and gains a generation.
    pub entity: String,
    /// What is believed now.
    pub belief: String,
    /// The observation recorded against the successor.
    pub observation: String,
    /// Why it changed.
    pub reason: String,
    /// Confidence, 0.0–1.0.
    pub confidence: f64,
    /// How many days before the run the correction was recorded.
    pub days_ago: u32,
}

/// The commit that introduced the decision.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SourceSpec {
    /// Entity name for the commit.
    pub entity: String,
    /// The commit subject.
    pub message: String,
    /// Relation kind from the decision to the commit.
    pub relation: String,
    /// How many days before the run the commit was made.
    pub days_ago: u32,
}

/// The two sides that disagree.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContradictionSpec {
    /// Entity the sides are about.
    pub entity: String,
    /// Relation kind from the entity to each side.
    pub relation: String,
    /// The sides, in the order they are reported.
    pub sides: Vec<SideSpec>,
}

/// One side of a contradiction.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SideSpec {
    /// How this side is labelled in the answer (`side A`, `side B`).
    pub label: String,
    /// Entity name for the side's target.
    pub target: String,
    /// What this side claims.
    pub belief: String,
    /// How many days before the run the side was believed.
    pub days_ago: u32,
    /// Whether the side is still believed or was revoked.
    pub state: SideState,
}

/// Whether a side of a contradiction is still believed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SideState {
    /// Still believed.
    Current,
    /// Explicitly revoked; readable as history, never as the answer.
    Revoked,
}

impl Scenario {
    /// Read the story this build ships.
    pub fn bundled() -> Result<Self> {
        serde_json::from_str(SCENARIO_JSON).context("parse the bundled demo scenario")
    }
}

// -------------------------------------------------------------- the answer

/// Everything the demo reports, in the shape both renderings use.
#[derive(Debug, Clone, Serialize)]
pub struct Story {
    /// The question, so the answer can be read on its own.
    pub question: String,
    /// The vault the answer came from.
    pub vault: String,
    /// The decision and when it was believed.
    pub decision: Decision,
    /// What replaced it and when.
    pub superseded_by: Supersession,
    /// The commit that introduced it.
    pub source_commit: Source,
    /// Both sides of the contradiction, with dates.
    pub contradiction: Contradiction,
    /// What to run next; printed by the shared text renderer.
    pub next_commands: Vec<String>,
}

/// An instant, in the two forms it is read in.
///
/// `epoch_ms` is what the vault stores; `rfc3339` is what a person reads. Both
/// are built together by [`Moment::from_ms`], so they cannot drift.
#[derive(Debug, Clone, Serialize)]
pub struct Moment {
    /// Milliseconds since the Unix epoch.
    pub epoch_ms: i64,
    /// The same instant as RFC 3339.
    pub rfc3339: String,
}

impl Moment {
    /// The one constructor: both fields come from the same instant.
    fn from_ms(ms: i64) -> Self {
        Self {
            epoch_ms: ms,
            rfc3339: rfc3339(ms),
        }
    }
}

/// The decision, as first believed.
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    /// Entity name.
    pub entity: String,
    /// Vault row id.
    pub entity_id: i64,
    /// Entity kind.
    pub kind: &'static str,
    /// What was believed.
    pub belief: String,
    /// When it was believed.
    pub believed_at: Moment,
}

/// The belief that superseded it.
#[derive(Debug, Clone, Serialize)]
pub struct Supersession {
    /// Entity name (the successor keeps it).
    pub entity: String,
    /// The closed row.
    pub old_entity_id: i64,
    /// The row that replaced it.
    pub entity_id: i64,
    /// What is believed now.
    pub belief: String,
    /// Why it changed.
    pub reason: String,
    /// When the replacement was recorded.
    pub at: Moment,
}

/// The commit that introduced the decision.
#[derive(Debug, Clone, Serialize)]
pub struct Source {
    /// Entity name.
    pub entity: String,
    /// Vault row id.
    pub entity_id: i64,
    /// The commit subject.
    pub message: String,
    /// Relation kind from the decision to the commit.
    pub relation: String,
    /// When the link was recorded.
    pub at: Moment,
}

/// Both sides of a contradiction.
#[derive(Debug, Clone, Serialize)]
pub struct Contradiction {
    /// Entity the sides are about.
    pub entity: String,
    /// Vault row id, absent when the scenario records no contradiction.
    pub entity_id: Option<i64>,
    /// Relation kind from the entity to each side.
    pub relation: String,
    /// The sides; empty when nothing contradicts.
    pub sides: Vec<Side>,
}

/// One side of a contradiction.
#[derive(Debug, Clone, Serialize)]
pub struct Side {
    /// How this side is labelled in the answer.
    pub label: String,
    /// Entity name for the side's target.
    pub target: String,
    /// Vault row id.
    pub entity_id: i64,
    /// What this side claims.
    pub belief: String,
    /// Whether the side is still believed or was revoked.
    pub state: SideState,
    /// When it was believed.
    pub believed_at: Moment,
    /// When it was revoked; `None` for the current side.
    pub revoked_at: Option<Moment>,
}

/// The text half, from the same [`Story`] the JSON half uses.
pub fn render(story: &Story) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "demo — {}", story.question);
    let _ = writeln!(out, "  vault          {}", story.vault);
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  decision       {} (#{}): {}",
        story.decision.entity, story.decision.entity_id, story.decision.belief
    );
    let _ = writeln!(
        out,
        "                 believed at {}",
        story.decision.believed_at.rfc3339
    );
    let _ = writeln!(
        out,
        "  superseded by  {} (#{}) at {}",
        story.superseded_by.entity, story.superseded_by.entity_id, story.superseded_by.at.rfc3339
    );
    let _ = writeln!(
        out,
        "                 {} ({})",
        story.superseded_by.belief, story.superseded_by.reason
    );
    let _ = writeln!(
        out,
        "  source         {} (#{}): {}",
        story.source_commit.entity, story.source_commit.entity_id, story.source_commit.message
    );
    let _ = writeln!(
        out,
        "                 relation \"{}\" at {}",
        story.source_commit.relation, story.source_commit.at.rfc3339
    );
    let _ = writeln!(
        out,
        "  contradiction  {} relation \"{}\"",
        match story.contradiction.entity_id {
            Some(id) => format!("{} (#{id})", story.contradiction.entity),
            None => format!("{} (none recorded)", story.contradiction.entity),
        },
        story.contradiction.relation
    );
    if story.contradiction.sides.is_empty() {
        let _ = writeln!(
            out,
            "                 only one belief is on record; nothing contradicts it"
        );
    }
    for side in &story.contradiction.sides {
        match (side.state, &side.revoked_at) {
            (SideState::Revoked, Some(revoked)) => {
                let _ = writeln!(
                    out,
                    "                 {} (revoked {}) {}: {} — believed at {}",
                    side.label, revoked.rfc3339, side.target, side.belief, side.believed_at.rfc3339
                );
            }
            _ => {
                let _ = writeln!(
                    out,
                    "                 {} (current) {}: {} — believed at {}",
                    side.label, side.target, side.belief, side.believed_at.rfc3339
                );
            }
        }
    }
    out
}

// ------------------------------------------------------------------ seeding

/// Seed the scenario into `engine` and read the story back out.
///
/// Reading back is deliberate: the answer reports the instants the vault
/// actually stored, so a second run a day later repeats the original dates
/// instead of inventing new ones.
pub fn seed(engine: &MemoryEngine, scenario: &Scenario, now: i64) -> Result<Story> {
    let decision_at = now - i64::from(scenario.decision.days_ago) * DAY_MS;
    let superseded_at = now - i64::from(scenario.superseded_by.days_ago) * DAY_MS;
    if superseded_at <= decision_at {
        bail!(
            "demo timeline is inverted: the superseding belief is {} day(s) ago and the decision is {} day(s) ago",
            scenario.superseded_by.days_ago,
            scenario.decision.days_ago
        );
    }

    // 1. The decision.
    let decision_id = match root_entity(engine, EntityKind::Decision, &scenario.decision.entity)? {
        Some(id) => id,
        None => engine.add_entity(
            EntityKind::Decision,
            &scenario.decision.entity,
            scenario.decision.confidence,
            decision_at,
        )?,
    };
    observe(
        engine,
        decision_id,
        &scenario.decision.observation,
        "decision",
        scenario.decision.confidence,
        decision_at,
    )?;

    // 2. What superseded it. A run that already superseded this row reports the
    //    successor it created; superseding it again would grow the history every
    //    time the demo is opened.
    let successor_id = match superseded_by_of(engine, decision_id)? {
        Some(id) => id,
        None => engine
            .supersede_entity(
                decision_id,
                Some(EntityKind::Decision),
                Some(&scenario.superseded_by.entity),
                None,
                Some(scenario.superseded_by.confidence),
                &scenario.superseded_by.reason,
                superseded_at,
            )
            .with_context(|| format!("supersede demo decision {decision_id}"))?,
    };
    observe(
        engine,
        successor_id,
        &scenario.superseded_by.observation,
        "correction",
        scenario.superseded_by.confidence,
        superseded_at,
    )?;

    // 3. The commit that introduced it.
    let commit_at = now - i64::from(scenario.source.days_ago) * DAY_MS;
    let commit_id = ensure_entity(
        engine,
        EntityKind::Other,
        &scenario.source.entity,
        1.0,
        commit_at,
    )?;
    observe(
        engine,
        commit_id,
        &scenario.source.message,
        "fact",
        1.0,
        commit_at,
    )?;
    let source_relation = ensure_relation(
        engine,
        decision_id,
        commit_id,
        &scenario.source.relation,
        1.0,
        decision_at,
    )?;

    // 4. Both sides of the contradiction, with the revocation that makes them a
    //    contradiction rather than two unrelated claims.
    let side_times: Vec<i64> = scenario
        .contradiction
        .sides
        .iter()
        .map(|side| now - i64::from(side.days_ago) * DAY_MS)
        .collect();
    let mut sides = Vec::with_capacity(scenario.contradiction.sides.len());
    let format_id = if side_times.is_empty() {
        None
    } else {
        let current = scenario
            .contradiction
            .sides
            .iter()
            .find(|side| side.state == SideState::Current)
            .map(|side| side.target.clone())
            .with_context(|| {
                format!(
                    "demo contradiction {:?} has no current side",
                    scenario.contradiction.entity
                )
            })?;
        let revoke_at = side_times.iter().copied().max().unwrap_or(decision_at);
        let born_at = side_times.iter().copied().min().unwrap_or(decision_at);
        let format_id = ensure_entity(
            engine,
            EntityKind::Concept,
            &scenario.contradiction.entity,
            1.0,
            born_at,
        )?;
        for (side, at) in scenario.contradiction.sides.iter().zip(&side_times) {
            let target_id = ensure_entity(engine, EntityKind::Other, &side.target, 1.0, *at)?;
            observe(engine, target_id, &side.belief, "fact", 1.0, *at)?;
            let relation_id = ensure_relation(
                engine,
                format_id,
                target_id,
                &scenario.contradiction.relation,
                1.0,
                *at,
            )?;
            if side.state == SideState::Revoked {
                revoke_if_open(engine, relation_id, &revocation_reason(&current), revoke_at)?;
            }
            let (believed_at, revoked_at) = relation_times(engine, relation_id)?;
            sides.push(Side {
                label: side.label.clone(),
                target: side.target.clone(),
                entity_id: target_id,
                belief: side.belief.clone(),
                state: side.state,
                believed_at: Moment::from_ms(believed_at),
                revoked_at: revoked_at.map(Moment::from_ms),
            });
        }
        Some(format_id)
    };

    // 5. Read the story back: the dates below are the vault's, not this run's.
    let decision_believed_at = entity_born_at(engine, decision_id)?;
    let superseded_believed_at = entity_born_at(engine, successor_id)?;
    let source_at = relation_times(engine, source_relation)?.0;

    let mut next_commands = vec![
        format!("memnoth vault history {}", scenario.decision.entity),
        format!(
            "memnoth vault asof --name {} --at {}",
            scenario.decision.entity,
            rfc3339(decision_believed_at)
        ),
    ];
    if format_id.is_some() {
        next_commands.push(format!(
            "memnoth vault contradictions --name {}",
            scenario.contradiction.entity
        ));
    }

    Ok(Story {
        question: scenario.question.clone(),
        vault: String::new(),
        decision: Decision {
            entity: scenario.decision.entity.clone(),
            entity_id: decision_id,
            kind: EntityKind::Decision.as_str(),
            belief: scenario.decision.belief.clone(),
            believed_at: Moment::from_ms(decision_believed_at),
        },
        superseded_by: Supersession {
            entity: scenario.superseded_by.entity.clone(),
            old_entity_id: decision_id,
            entity_id: successor_id,
            belief: scenario.superseded_by.belief.clone(),
            reason: scenario.superseded_by.reason.clone(),
            at: Moment::from_ms(superseded_believed_at),
        },
        source_commit: Source {
            entity: scenario.source.entity.clone(),
            entity_id: commit_id,
            message: scenario.source.message.clone(),
            relation: scenario.source.relation.clone(),
            at: Moment::from_ms(source_at),
        },
        contradiction: Contradiction {
            entity: scenario.contradiction.entity.clone(),
            entity_id: format_id,
            relation: scenario.contradiction.relation.clone(),
            sides,
        },
        next_commands,
    })
}

/// The reason recorded on the closing side of a contradiction.
fn revocation_reason(current: &str) -> String {
    format!("superseded by {current}")
}

/// Record one observation, idempotently.
fn observe(
    engine: &MemoryEngine,
    entity_id: i64,
    content: &str,
    kind: &str,
    confidence: f64,
    valid_from: i64,
) -> Result<i64> {
    let provenance = r#"{"user":"demo","agent":"demo"}"#;
    engine
        .ingest_observation_attributed(
            entity_id,
            content,
            kind,
            confidence,
            valid_from,
            provenance,
            Some(now_ms()),
        )
        .with_context(|| format!("record demo observation {content:?} on {entity_id}"))
}

/// The lowest-id entity with this kind and name: the row the demo first seeded.
///
/// Not "the live row by that name", which is the successor after the first run
/// and would make every re-run supersede the previous successor.
fn root_entity(engine: &MemoryEngine, kind: EntityKind, name: &str) -> Result<Option<i64>> {
    Ok(engine
        .connection()
        .query_row(
            "SELECT id FROM entities WHERE kind = ?1 AND name = ?2 ORDER BY id ASC LIMIT 1",
            params![kind.as_str(), name],
            |row| row.get(0),
        )
        .optional()?)
}

/// Reuse an entity with this kind and name, or create it.
fn ensure_entity(
    engine: &MemoryEngine,
    kind: EntityKind,
    name: &str,
    confidence: f64,
    valid_from: i64,
) -> Result<i64> {
    match root_entity(engine, kind, name)? {
        Some(id) => Ok(id),
        None => engine
            .add_entity(kind, name, confidence, valid_from)
            .with_context(|| format!("add demo entity {name:?}")),
    }
}

/// Reuse a relation between these rows, or create it once.
fn ensure_relation(
    engine: &MemoryEngine,
    source_id: i64,
    target_id: i64,
    kind: &str,
    confidence: f64,
    valid_from: i64,
) -> Result<i64> {
    let existing: Option<i64> = engine
        .connection()
        .query_row(
            "SELECT id FROM relations WHERE source_id = ?1 AND target_id = ?2 AND kind = ?3
             ORDER BY id ASC LIMIT 1",
            params![source_id, target_id, kind],
            |row| row.get(0),
        )
        .optional()?;
    match existing {
        Some(id) => Ok(id),
        None => engine
            .add_relation(source_id, target_id, kind, confidence, valid_from)
            .with_context(|| format!("link {source_id} -> {target_id} as {kind}")),
    }
}

/// Revoke a relation only when it is still open, so re-runs do not move the
/// date a contradiction was recorded.
fn revoke_if_open(engine: &MemoryEngine, relation_id: i64, reason: &str, at: i64) -> Result<()> {
    let revoked: Option<i64> = engine
        .connection()
        .query_row(
            "SELECT revoked_at FROM relations WHERE id = ?1",
            params![relation_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if revoked.is_none() {
        engine
            .revoke_relation(relation_id, reason, at)
            .with_context(|| format!("revoke demo relation {relation_id}"))?;
    }
    Ok(())
}

/// When an entity row began, in milliseconds.
fn entity_born_at(engine: &MemoryEngine, entity_id: i64) -> Result<i64> {
    engine
        .connection()
        .query_row(
            "SELECT valid_from FROM entities WHERE id = ?1",
            params![entity_id],
            |row| row.get(0),
        )
        .with_context(|| format!("read entity {entity_id}"))
}

/// The successor recorded on an entity, if it was superseded.
fn superseded_by_of(engine: &MemoryEngine, entity_id: i64) -> Result<Option<i64>> {
    let metadata: Option<String> = engine
        .connection()
        .query_row(
            "SELECT metadata FROM entities WHERE id = ?1",
            params![entity_id],
            |row| row.get(0),
        )
        .optional()?;
    let value: Value = serde_json::from_str(metadata.as_deref().unwrap_or("{}"))
        .with_context(|| format!("parse metadata on entity {entity_id}"))?;
    Ok(value.get("superseded_by").and_then(Value::as_i64))
}

/// When a relation began and, if revoked, when it was contradicted.
fn relation_times(engine: &MemoryEngine, relation_id: i64) -> Result<(i64, Option<i64>)> {
    engine
        .connection()
        .query_row(
            "SELECT valid_from, revoked_at FROM relations WHERE id = ?1",
            params![relation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .with_context(|| format!("read relation {relation_id}"))
}
