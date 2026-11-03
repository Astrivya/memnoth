//! Reflection: derive a higher-level belief from the observations a vault
//! already holds, with provenance and without an LLM.
//!
//! # Why this exists
//!
//! A vault that only appends facts grows without ever learning. The rivals that
//! do abstract (letta's sleep-time compute, cognee's `remember()`) do it inside
//! a model call: the input is a prompt, the output is a rewritten memory, and
//! the link between the two is not inspectable. That is the one thing this lane
//! refuses to copy.
//!
//! A reflection here is **arithmetic over rows that already exist**:
//!
//! - group an entity's current observations by `kind`,
//! - for every group at or above [`ReflectPolicy::min_sources`], synthesize one
//!   `reflection` observation whose content names the group and its sources,
//! - give it a confidence derived from its sources (never above the best of
//!   them), and
//! - skip the write when a reflection with that exact content already exists.
//!
//! # Provenance
//!
//! The `observations` table has no metadata column on purpose — a claim's
//! provenance is part of the claim. A reflection therefore carries its source
//! ids in its own text (`from observations #3, #4, #7`), which means provenance
//! survives search, export and copy-paste, and needs no migration to exist.
//!
//! # Idempotency
//!
//! Content is built deterministically from ids and text, so a second run over an
//! unchanged vault produces the same content and writes nothing. That property
//! is asserted by test, because a reflection pass that duplicates itself every
//! turn would be worse than no reflection pass at all.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::model::Observation;
use crate::store::MemoryEngine;

/// Observation kinds that carry enough signal to be worth reflecting on.
pub const REFLECTABLE_KINDS: [&str; 3] = ["fact", "decision", "gotcha"];

/// The kind stamped on a synthesized observation.
pub const REFLECTION_KIND: &str = "reflection";

/// Tunables for one reflection pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectPolicy {
    /// Minimum observations of one kind before a reflection is worth writing.
    /// Values below `2` are clamped: a reflection of a single row is a copy.
    pub min_sources: usize,
    /// Kinds to reflect on. Empty → [`REFLECTABLE_KINDS`].
    pub kinds: Vec<String>,
    /// How many source texts appear in the reflection body.
    pub max_quoted: usize,
    /// Character cap per quoted source.
    pub quote_chars: usize,
}

impl Default for ReflectPolicy {
    fn default() -> Self {
        Self {
            min_sources: 3,
            kinds: REFLECTABLE_KINDS.iter().map(|s| s.to_string()).collect(),
            max_quoted: 3,
            quote_chars: 72,
        }
    }
}

/// One synthesized belief, and whether this run was the one that wrote it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reflection {
    /// Entity the reflection belongs to.
    pub entity_id: i64,
    /// Entity name at pass time.
    pub entity_name: String,
    /// Kind of the sources it was built from (`fact`, `decision`, …).
    pub source_kind: String,
    /// The synthesized observation id (`None` on a dry run).
    pub observation_id: Option<i64>,
    /// The synthesized text, including its provenance clause.
    pub content: String,
    /// Ids of the observations it was built from.
    pub sources: Vec<i64>,
    /// Derived confidence (never above the best source).
    pub confidence: f64,
    /// `false` when an identical reflection already existed.
    pub created: bool,
}

/// Confidence of a reflection over `sources`.
///
/// The mean of the sources, discounted by corroboration breadth
/// (`n/(n+1)`) and capped below the best source: a claim about claims is never
/// stronger than the claims it summarises. The discount is deliberate — three
/// weak facts do not add up to one strong fact, they add up to a weak theme.
pub fn reflection_confidence(sources: &[Observation]) -> f64 {
    if sources.is_empty() {
        return 0.0;
    }
    let mean = sources
        .iter()
        .map(|o| o.confidence.clamp(0.0, 1.0))
        .sum::<f64>()
        / sources.len() as f64;
    let n = sources.len() as f64;
    let corroboration = n / (n + 1.0);
    let best = sources
        .iter()
        .map(|o| o.confidence.clamp(0.0, 1.0))
        .fold(0.0_f64, f64::max);
    (mean * corroboration).min(best).clamp(0.0, 1.0)
}

/// Build the reflection text for a group. Pure and total, so the same inputs
/// always produce the same bytes — the property idempotency rests on.
pub fn reflection_text(
    entity_name: &str,
    kind: &str,
    sources: &[Observation],
    policy: &ReflectPolicy,
) -> String {
    let plural = if sources.len() == 1 { "" } else { "s" };
    let mut quoted: Vec<String> = sources
        .iter()
        .take(policy.max_quoted.max(1))
        .map(|o| {
            let text: String = o.content.chars().take(policy.quote_chars.max(1)).collect();
            text.replace('\n', " ")
        })
        .collect();
    if sources.len() > quoted.len() {
        quoted.push(format!("+{} more", sources.len() - quoted.len()));
    }
    let ids = sources
        .iter()
        .map(|o| format!("#{}", o.id))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{} {}{} about {}: {} (from observations {})",
        sources.len(),
        kind,
        plural,
        entity_name,
        quoted.join(" | "),
        ids
    )
}

/// Reflect one entity at instant `now`.
///
/// Only observations valid at `now` are read, so a superseded or contradicted
/// fact stops contributing the moment it is invalidated — a reflection is a
/// belief about current beliefs. `apply == false` plans without writing.
pub fn reflect_entity(
    engine: &MemoryEngine,
    entity_id: i64,
    now: i64,
    policy: &ReflectPolicy,
    apply: bool,
) -> Result<Vec<Reflection>> {
    let Some(entity) = engine.entity_at(entity_id, now)? else {
        return Ok(Vec::new());
    };
    let observations = engine.observations_for(entity_id, now)?;
    let existing = existing_contents(engine, entity_id, now)?;
    let kinds: Vec<String> = if policy.kinds.is_empty() {
        REFLECTABLE_KINDS.iter().map(|s| s.to_string()).collect()
    } else {
        policy.kinds.clone()
    };

    let mut out = Vec::new();
    for kind in kinds {
        let mut sources: Vec<&Observation> = observations
            .iter()
            .filter(|o| o.kind == kind && o.kind != REFLECTION_KIND)
            .collect();
        if sources.len() < policy.min_sources.max(2) {
            continue;
        }
        // Stable order by id: the same vault must produce the same text.
        sources.sort_by_key(|o| o.id);
        let owned: Vec<Observation> = sources.into_iter().cloned().collect();
        let content = reflection_text(&entity.name, &kind, &owned, policy);
        let confidence = reflection_confidence(&owned);
        if existing.contains(&content) {
            out.push(Reflection {
                entity_id,
                entity_name: entity.name.clone(),
                source_kind: kind.clone(),
                observation_id: None,
                content,
                sources: owned.iter().map(|o| o.id).collect(),
                confidence,
                created: false,
            });
            continue;
        }
        let observation_id = if apply {
            Some(engine.add_observation(entity_id, &content, REFLECTION_KIND, confidence, now)?)
        } else {
            None
        };
        out.push(Reflection {
            entity_id,
            entity_name: entity.name.clone(),
            source_kind: kind,
            observation_id,
            content,
            sources: owned.iter().map(|o| o.id).collect(),
            confidence,
            created: apply,
        });
    }
    Ok(out)
}

/// Reflect every current entity.
///
/// Entities are visited in id order so a pass is reproducible; [`reflect_entity`]
/// decides per entity whether anything is due.
pub fn reflect_all(
    engine: &MemoryEngine,
    now: i64,
    policy: &ReflectPolicy,
    apply: bool,
) -> Result<Vec<Reflection>> {
    let mut out = Vec::new();
    for entity in engine.entities_current()? {
        out.extend(reflect_entity(engine, entity.id, now, policy, apply)?);
    }
    Ok(out)
}

/// Current reflection observations attached to an entity.
pub fn reflections_for(
    engine: &MemoryEngine,
    entity_id: i64,
    as_of: i64,
) -> Result<Vec<Observation>> {
    Ok(engine
        .observations_for(entity_id, as_of)?
        .into_iter()
        .filter(|o| o.kind == REFLECTION_KIND)
        .collect())
}

/// Contents of reflections already current for `entity_id` — the idempotency key.
fn existing_contents(
    engine: &MemoryEngine,
    entity_id: i64,
    now: i64,
) -> Result<std::collections::HashSet<String>> {
    Ok(reflections_for(engine, entity_id, now)?
        .into_iter()
        .map(|o| o.content)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EntityKind;

    fn engine() -> MemoryEngine {
        MemoryEngine::open_in_memory().expect("in-memory vault")
    }

    fn seed(engine: &MemoryEngine, name: &str, now: i64) -> i64 {
        let id = engine
            .add_entity(EntityKind::Concept, name, 0.8, now)
            .expect("add");
        for (i, text) in ["one", "two", "three"].iter().enumerate() {
            engine
                .add_observation(id, text, "fact", 0.6 + i as f64 * 0.1, now)
                .expect("observation");
        }
        id
    }

    #[test]
    fn three_facts_become_one_reflection_with_provenance() {
        let e = engine();
        let now = 1_000_000_000_000;
        let id = seed(&e, "ledger", now);
        let policy = ReflectPolicy::default();

        let made = reflect_entity(&e, id, now, &policy, true).expect("reflect");
        assert_eq!(made.len(), 1);
        assert!(made[0].created);
        assert_eq!(made[0].sources.len(), 3);
        assert!(made[0].content.contains("from observations #"));
        // Never stronger than the best source, never above 1.0.
        assert!(made[0].confidence <= 0.8);
        assert!(made[0].confidence > 0.0);

        let stored = reflections_for(&e, id, now).expect("stored");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].kind, REFLECTION_KIND);
    }

    #[test]
    fn a_second_pass_writes_nothing() {
        let e = engine();
        let now = 1_000_000_000_000;
        let id = seed(&e, "ledger", now);
        let policy = ReflectPolicy::default();
        reflect_entity(&e, id, now, &policy, true).expect("first");
        let again = reflect_entity(&e, id, now, &policy, true).expect("second");
        assert_eq!(again.len(), 1);
        assert!(!again[0].created);
        assert_eq!(again[0].observation_id, None);
        assert_eq!(reflections_for(&e, id, now).expect("stored").len(), 1);
    }

    #[test]
    fn below_the_source_floor_nothing_is_synthesized() {
        let e = engine();
        let now = 1_000_000_000_000;
        let id = e
            .add_entity(EntityKind::Concept, "thin", 0.8, now)
            .expect("add");
        for text in ["a", "b"] {
            e.add_observation(id, text, "fact", 0.8, now).expect("obs");
        }
        let made = reflect_entity(&e, id, now, &ReflectPolicy::default(), true).expect("reflect");
        assert!(made.is_empty());
        assert!(reflections_for(&e, id, now).expect("stored").is_empty());
    }

    #[test]
    fn dry_run_plans_without_writing() {
        let e = engine();
        let now = 1_000_000_000_000;
        let id = seed(&e, "ledger", now);
        let made = reflect_entity(&e, id, now, &ReflectPolicy::default(), false).expect("plan");
        assert_eq!(made.len(), 1);
        assert!(!made[0].created);
        assert!(made[0].observation_id.is_none());
        assert!(reflections_for(&e, id, now).expect("stored").is_empty());
    }

    #[test]
    fn an_invalidated_fact_stops_contributing() {
        let e = engine();
        let now = 1_000_000_000_000;
        let id = seed(&e, "ledger", now);
        // Revoke one fact by closing its validity, then reflect as of later.
        let later = now + 1_000;
        e.connection()
            .execute(
                "UPDATE observations SET valid_to = ?1 WHERE entity_id = ?2 AND content = 'two'",
                rusqlite::params![later, id],
            )
            .expect("close");
        let made = reflect_entity(&e, id, later, &ReflectPolicy::default(), true).expect("reflect");
        assert!(
            made.is_empty(),
            "two live facts is below the floor of three"
        );
    }

    #[test]
    fn reflect_all_covers_every_entity_and_is_searchable() {
        let e = engine();
        let now = 1_000_000_000_000;
        seed(&e, "ledger", now);
        seed(&e, "atlas", now);
        let made = reflect_all(&e, now, &ReflectPolicy::default(), true).expect("pass");
        assert_eq!(made.len(), 2);
        assert!(made.iter().all(|r| r.created));

        let hits = e.search_observations("ledger", 10, now).expect("search");
        assert!(
            hits.iter().any(|(o, _)| o.kind == REFLECTION_KIND),
            "reflections are searchable like any other observation"
        );
    }

    #[test]
    fn text_is_stable_for_the_same_inputs() {
        let e = engine();
        let now = 1_000_000_000_000;
        let id = seed(&e, "ledger", now);
        let policy = ReflectPolicy::default();
        let obs = e.observations_for(id, now).expect("obs");
        let once = reflection_text("ledger", "fact", &obs, &policy);
        let twice = reflection_text("ledger", "fact", &obs, &policy);
        assert_eq!(once, twice);
    }
}
