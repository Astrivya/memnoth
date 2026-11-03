//! Retention and forgetting: which memories earn their place, and what happens
//! when one stops earning it.
//!
//! Every rival on this board either never forgets or forgets opaquely. Graphiti
//! invalidates facts by *contradiction* only — a fact nobody contradicts lives
//! forever, however stale. letta's sleep-time compute rewrites memory in an LLM
//! call with no per-item account of why. Mem0 decays toward recency inside a
//! scoring function but never records the decision.
//!
//! This module adds the missing half of a temporal store: **retention as an
//! explained, reversible decision**.
//!
//! # The policy
//!
//! A retention score in `0.0..=1.0` is the product of four measured factors:
//!
//! | Factor | Reads | Effect |
//! |---|---|---|
//! | `recency` | `entities.valid_from` against [`RetentionPolicy::half_life_ms`] | halves every half-life |
//! | `reinforcement` | `access_stats.access_count` + `last_access` | reading a memory resists decay |
//! | `confidence` | `entities.confidence` | low-confidence claims go first |
//! | `contradiction` | revoked relations touching the entity | a memory that keeps losing arguments goes first |
//!
//! The factors are returned alongside the score, so "why is this being
//! forgotten?" is answered by arithmetic rather than by a prompt.
//!
//! # Forgetting is invalidation, not deletion
//!
//! [`forget`] calls the ordinary `revoke_entity` path: the row is closed with a
//! reason that names the score and the dominant factor, so the decision appears
//! in `vault_contradictions`, in `entity_history`, and in every `as_of` query.
//! Nothing is deleted — which is what makes [`reinstate`] possible: a forgotten
//! entity can be re-opened from its own closed row, carrying a `supersedes`
//! back-link so the round trip is visible in history.
//!
//! # Determinism
//!
//! Scoring takes `now` explicitly and reads no wall clock, so the same vault and
//! the same policy produce the same decision list on every run. That is what
//! lets [`forget`] be re-run safely: it plans, applies, and reports the batch.

use anyhow::{Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::model::Entity;
use crate::store::MemoryEngine;

/// Memory category for per-category retention tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Category {
    /// Corrections to previous beliefs — longest lived (365 days)
    Correction,
    /// User preferences — medium-long lived (90 days)
    Preference,
    /// Factual claims — standard half-life (30 days)
    Fact,
    /// Procedures and how-to knowledge — medium lived (60 days)
    Procedure,
    /// Inferred/derived knowledge — shortest lived (7 days)
    Inferred,
}

impl Category {
    /// Default half-life in milliseconds for this category.
    pub fn default_half_life_ms(self) -> i64 {
        const DAY: i64 = 24 * 60 * 60 * 1000;
        match self {
            Category::Correction => 365 * DAY,
            Category::Preference => 90 * DAY,
            Category::Fact => 30 * DAY,
            Category::Procedure => 60 * DAY,
            Category::Inferred => 7 * DAY,
        }
    }

    /// All categories in a fixed order.
    pub fn all() -> &'static [Category] {
        &[
            Category::Correction,
            Category::Preference,
            Category::Fact,
            Category::Procedure,
            Category::Inferred,
        ]
    }
}

/// Default half-life: 30 days. A memory that is never read again is worth half
/// as much every 30 days.
pub const DEFAULT_HALF_LIFE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Tunables for the retention score. `Default` is the shipped policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Milliseconds for the recency factor to halve (default, used when category not specified).
    pub half_life_ms: i64,
    /// Per-category half-lives in milliseconds. Overrides `half_life_ms` when set.
    #[serde(default)]
    pub half_life_by_category: HashMap<Category, i64>,
    /// How much reinforcement can rescue a memory. `0.0` = access counts for
    /// nothing, `1.0` = a single read keeps a memory fully alive.
    pub reinforcement_weight: f64,
    /// Multiplier applied per revoked relation touching the entity.
    pub contradiction_penalty: f64,
    /// Score below which an entity is a forget candidate.
    pub floor: f64,
    /// Never even consider an entity younger than this (protects fresh writes
    /// from a policy run in the same second).
    pub min_age_ms: i64,
    /// Never consider an entity with confidence at or above this (protects
    /// high-confidence beliefs regardless of age).
    pub confidence_exempt: f64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        let mut half_life_by_category = HashMap::new();
        for &cat in Category::all() {
            half_life_by_category.insert(cat, cat.default_half_life_ms());
        }
        Self {
            half_life_ms: DEFAULT_HALF_LIFE_MS,
            half_life_by_category,
            reinforcement_weight: 0.6,
            contradiction_penalty: 0.5,
            floor: 0.15,
            min_age_ms: 24 * 60 * 60 * 1000,
            confidence_exempt: 0.9,
        }
    }
}

impl RetentionPolicy {
    /// Get the half-life for a specific category, falling back to the default.
    pub fn half_life_for(&self, category: Category) -> i64 {
        self.half_life_by_category
            .get(&category)
            .copied()
            .unwrap_or(self.half_life_ms)
    }
}

/// One measured factor behind a retention score. Kept separate from the score
/// so a decision can be justified without re-deriving it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RetentionFactors {
    /// `0.5^(age / half_life)` — 1.0 for a brand-new memory.
    pub recency: f64,
    /// `1.0` for never-read, up to `1.0` for well-used.
    pub reinforcement: f64,
    /// The stored confidence, clamped to `0.0..=1.0`.
    pub confidence: f64,
    /// `contradiction_penalty^revocations`, clamped to `0.0..=1.0`.
    pub contradiction: f64,
}

/// An entity's retention judgement: the score, its factors, and the raw inputs
/// the factors were computed from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Retention {
    /// Entity id.
    pub entity_id: i64,
    /// Entity name at scoring time (for reports; the id is the identity).
    pub name: String,
    /// Entity kind as stored.
    pub kind: String,
    /// Final score in `0.0..=1.0`.
    pub score: f64,
    /// The four factors that produced it.
    pub factors: RetentionFactors,
    /// Age in milliseconds at `now`.
    pub age_ms: i64,
    /// Lifetime access count.
    pub access_count: i64,
    /// Revoked relations currently touching the entity.
    pub revocations: i64,
    /// `true` when the policy exempts this entity regardless of score.
    pub exempt: bool,
    /// Why it is exempt, when it is.
    pub exempt_reason: Option<String>,
}

impl Retention {
    /// Whether this entity is a forget candidate under `policy`: below the
    /// floor, and not exempt.
    pub fn is_candidate(&self, policy: &RetentionPolicy) -> bool {
        !self.exempt && self.score < policy.floor
    }

    /// The dominant factor — the one that did most to push the score down.
    /// Returned by name so a forget reason can cite it.
    pub fn dominant_factor(&self) -> &'static str {
        // Score each factor by how far below 1.0 it sits; the deepest wins.
        let mut worst = ("recency", 1.0 - self.factors.recency);
        for (name, value) in [
            ("reinforcement", 1.0 - self.factors.reinforcement),
            ("confidence", 1.0 - self.factors.confidence),
            ("contradiction", 1.0 - self.factors.contradiction),
        ] {
            if value > worst.1 {
                worst = (name, value);
            }
        }
        worst.0
    }

    /// Human-readable justification, e.g.
    /// `score 0.04 (recency 0.02 · reinforcement 0.40 · confidence 0.50 · contradictions 2)`.
    pub fn explanation(&self) -> String {
        format!(
            "score {:.3} (recency {:.2} · reinforcement {:.2} · confidence {:.2} · contradictions {})",
            self.score,
            self.factors.recency,
            self.factors.reinforcement,
            self.factors.confidence,
            self.revocations
        )
    }
}

/// Score one entity from its measured inputs. Pure: no clock, no IO.
pub fn score(
    confidence: f64,
    age_ms: i64,
    access_count: i64,
    revocations: i64,
    policy: &RetentionPolicy,
    category: Option<Category>,
) -> RetentionFactors {
    let half_life = category
        .map(|c| policy.half_life_for(c))
        .unwrap_or(policy.half_life_ms)
        .max(1) as f64;
    let age = age_ms.max(0) as f64;
    let recency = 0.5_f64.powf(age / half_life);

    // Reinforcement is a saturating curve: the first few reads matter most.
    // `1 - w + w * n/(n+1)` keeps a never-read memory at `1 - w` rather than 0,
    // so a single factor cannot exile a memory on its own.
    let w = policy.reinforcement_weight.clamp(0.0, 1.0);
    let n = access_count.max(0) as f64;
    let reinforcement = (1.0 - w) + w * (n / (n + 1.0));

    let confidence = confidence.clamp(0.0, 1.0);

    let penalty = policy.contradiction_penalty.clamp(0.0, 1.0);
    let contradiction = penalty
        .powi(revocations.clamp(0, 1_000) as i32)
        .clamp(0.0, 1.0);

    RetentionFactors {
        recency,
        reinforcement,
        confidence,
        contradiction,
    }
}

/// Combine factors into the final score (the product — every factor is a
/// necessary condition, so no factor can be traded away for another).
pub fn total(factors: &RetentionFactors) -> f64 {
    (factors.recency * factors.reinforcement * factors.confidence * factors.contradiction)
        .clamp(0.0, 1.0)
}

/// Raw per-entity inputs, gathered in one pass over the vault.
/// W1: expiry-staged rows excluded (same guard as as-of queries).
fn gather(engine: &MemoryEngine) -> Result<Vec<(Entity, i64, i64, i64)>> {
    // One statement, three LEFT JOINs: entity + access history + currently
    // revoked relations touching it. Doing this per entity would be N+1.
    let now = crate::now_ms();
    let mut stmt = engine.connection().prepare(
        "SELECT e.id, e.kind, e.name, e.canonical_name, e.summary, e.confidence,
                e.valid_from, e.valid_to, e.metadata,
                COALESCE(a.access_count, 0),
                COALESCE(a.last_access, 0),
                (SELECT COUNT(*) FROM relations r
                  WHERE r.revoked_at IS NOT NULL
                    AND (r.source_id = e.id OR r.target_id = e.id))
         FROM entities e
         LEFT JOIN access_stats a ON a.entity_id = e.id
         WHERE e.valid_to IS NULL AND (e.invalid_at IS NULL OR e.invalid_at > ?1)
         ORDER BY e.id",
    )?;
    let rows = stmt
        .query_map(params![now], |row| {
            Ok((
                MemoryEngine::row_to_entity(row)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Score every current entity under `policy`, worst first.
///
/// Exemption is checked before the score is trusted: a high-confidence belief
/// is never a candidate, and neither is an entity younger than
/// [`RetentionPolicy::min_age_ms`], so a policy run in the same second as a
/// write cannot undo it.
pub fn retention_report(
    engine: &MemoryEngine,
    policy: &RetentionPolicy,
    now: i64,
) -> Result<Vec<Retention>> {
    let rows = gather(engine)?;
    let mut out: Vec<Retention> = Vec::with_capacity(rows.len());
    for (e, access_count, last_access, revocations) in rows {
        let age_ms = (now - e.valid_from).max(0);
        // No category: `EntityKind` (Concept/Person/File) is a different taxonomy from
        // `Category` (Correction/Preference/Fact/...), and inventing a mapping here would
        // silently change half-lives. Per-category retention is chosen by the caller that
        // knows the category; this pass falls back to the policy's default half-life.
        let factors = score(
            e.confidence,
            age_ms,
            access_count,
            revocations,
            policy,
            None,
        );

        let mut exempt = false;
        let mut exempt_reason = None;
        if e.confidence >= policy.confidence_exempt {
            exempt = true;
            exempt_reason = Some(format!("confidence {:.2} >= exempt", e.confidence));
        } else if age_ms < policy.min_age_ms {
            exempt = true;
            exempt_reason = Some(format!("age {}ms < min_age", age_ms));
        }
        // A recently-read memory is protected even if its raw score is low:
        // `last_access` is the strongest single signal that a memory is in use.
        else if last_access > 0 && now - last_access < policy.half_life_ms {
            exempt = true;
            exempt_reason = Some("read within one half-life".to_string());
        }

        out.push(Retention {
            entity_id: e.id,
            name: e.name,
            kind: e.kind.as_str().to_string(),
            score: total(&factors),
            factors,
            age_ms,
            access_count,
            revocations,
            exempt,
            exempt_reason,
        });
    }
    out.sort_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.entity_id.cmp(&b.entity_id))
    });
    Ok(out)
}

/// What one [`forget`] run did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgetBatch {
    /// Entities considered (all current entities, exempt or not).
    pub considered: usize,
    /// Entities that were candidates (below the floor, not exempt).
    pub candidates: usize,
    /// Entities actually closed in this batch.
    pub forgotten: Vec<ForgottenEntity>,
    /// When the batch ran, in ms since epoch.
    pub at: i64,
    /// The policy thresholds the batch applied.
    pub floor: f64,
}

/// One remembered forgetting decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgottenEntity {
    /// Entity id (now closed; still queryable `as_of` any earlier instant).
    pub entity_id: i64,
    /// Name at forget time.
    pub name: String,
    /// Score at forget time.
    pub score: f64,
    /// Factor that did most to cause it.
    pub dominant_factor: String,
    /// Full justification string.
    pub explanation: String,
}

/// Plan (and optionally apply) forgetting.
///
/// `max` bounds the batch so a policy mistake cannot revoke a whole vault at
/// once. With `apply == false` this is a dry run: the report is produced and
/// nothing is written, which is the shape an operator should run first.
///
/// Applied forgetting goes through `revoke_entity`, so each decision lands in
/// the journal with its reason and is visible to `vault_contradictions`,
/// `entity_history` and every `as_of` query.
pub fn forget(
    engine: &MemoryEngine,
    policy: &RetentionPolicy,
    now: i64,
    max: usize,
    apply: bool,
) -> Result<ForgetBatch> {
    let report = retention_report(engine, policy, now)?;
    let candidates: Vec<&Retention> = report
        .iter()
        .filter(|r| !r.exempt && r.score < policy.floor)
        .collect();
    let mut forgotten = Vec::new();
    for r in candidates.iter().take(max) {
        if apply {
            let reason = format!(
                "retention below floor {:.2}: {} (dominant: {})",
                policy.floor,
                r.explanation(),
                r.dominant_factor()
            );
            engine
                .revoke_entity(r.entity_id, &reason, now)
                .with_context(|| format!("forget entity {}", r.entity_id))?;
        }
        forgotten.push(ForgottenEntity {
            entity_id: r.entity_id,
            name: r.name.clone(),
            score: r.score,
            dominant_factor: r.dominant_factor().to_string(),
            explanation: r.explanation(),
        });
    }
    Ok(ForgetBatch {
        considered: report.len(),
        candidates: candidates.len(),
        forgotten,
        at: now,
        floor: policy.floor,
    })
}

/// Re-open a forgotten (or superseded) entity from its own closed row.
///
/// The new row is a fresh entity carrying the closed row's content, with
/// `supersedes: <old_id>` in its metadata; the old row gains
/// `superseded_by: <new_id>`. Both directions of the history therefore resolve,
/// exactly as if the entity had been superseded rather than revoked.
///
/// Fails when `entity_id` is already current — reinstating the current belief
/// would create a second live copy of it.
pub fn reinstate(engine: &MemoryEngine, entity_id: i64, at: i64) -> Result<i64> {
    engine.reopen_entity(entity_id, at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EntityKind;

    const DAY: i64 = 24 * 60 * 60 * 1000;

    fn engine() -> MemoryEngine {
        MemoryEngine::open_in_memory().expect("in-memory vault")
    }

    #[test]
    fn recency_halves_every_half_life() {
        let p = RetentionPolicy::default();
        let fresh = score(0.5, 0, 0, 0, &p, None);
        let one = score(0.5, p.half_life_ms, 0, 0, &p, None);
        let two = score(0.5, p.half_life_ms * 2, 0, 0, &p, None);
        assert!((fresh.recency - 1.0).abs() < 1e-9);
        assert!((one.recency - 0.5).abs() < 1e-9);
        assert!((two.recency - 0.25).abs() < 1e-9);
        // A never-read memory is not zero: one factor cannot exile a memory.
        assert!(fresh.reinforcement > 0.0);
        assert!(fresh.reinforcement < 1.0);
    }

    #[test]
    fn access_resists_decay_and_contradictions_accelerate_it() {
        let p = RetentionPolicy::default();
        let cold = score(1.0, p.half_life_ms * 2, 0, 0, &p, None);
        let warm = score(1.0, p.half_life_ms * 2, 9, 0, &p, None);
        assert!(warm.reinforcement > cold.reinforcement);
        assert!(total(&warm) > total(&cold));

        let contested = score(1.0, 0, 0, 2, &p, None);
        assert!((contested.contradiction - 0.25).abs() < 1e-9);
        assert!(total(&contested) < total(&score(1.0, 0, 0, 0, &p, None)));
    }

    #[test]
    fn score_is_never_above_one_and_never_negative() {
        let p = RetentionPolicy::default();
        for (c, age, n, rev) in [
            (1.0, 0, 0, 0),
            (0.0, 0, 0, 0),
            (1.0, p.half_life_ms * 40, 100, 0),
            (1.0, 0, 0, 50),
        ] {
            let t = total(&score(c, age, n, rev, &p, None));
            assert!((0.0..=1.0).contains(&t), "score {t} out of range");
        }
    }

    #[test]
    fn fresh_and_high_confidence_entities_are_exempt() {
        let p = RetentionPolicy::default();
        let e = engine();
        let now = 1_000_000_000_000;
        let fresh = e
            .add_entity(EntityKind::Concept, "fresh", 0.2, now)
            .expect("add");
        let strong = e
            .add_entity(EntityKind::Concept, "strong", 0.95, now - 90 * DAY)
            .expect("add");
        let report = retention_report(&e, &p, now).expect("report");
        for r in &report {
            if r.entity_id == fresh || r.entity_id == strong {
                assert!(r.exempt, "{} should be exempt", r.name);
                assert!(!r.is_candidate(&p));
            }
        }
    }

    #[test]
    fn a_memory_in_active_use_is_exempt_and_a_heavily_used_old_one_outranks_an_unread_twin() {
        let p = RetentionPolicy::default();
        let e = engine();
        let now = 1_000_000_000_000;
        let stale = e
            .add_entity(EntityKind::Concept, "stale", 0.3, now - 90 * DAY)
            .expect("add");
        let warm = e
            .add_entity(EntityKind::Concept, "warm", 0.3, now - 90 * DAY)
            .expect("add");
        let frequent = e
            .add_entity(EntityKind::Concept, "frequent", 0.3, now - 90 * DAY)
            .expect("add");
        e.record_access(warm, now - DAY).expect("access");
        for _ in 0..8 {
            e.record_access(frequent, now - 60 * DAY).expect("access");
        }

        let report = retention_report(&e, &p, now).expect("report");
        let by_id = |id: i64| report.iter().find(|r| r.entity_id == id).expect("row");

        // Read inside one half-life: protected outright, whatever the score says.
        assert!(by_id(warm).exempt);
        assert_eq!(
            by_id(warm).exempt_reason.as_deref(),
            Some("read within one half-life")
        );
        // Read a long time ago but many times: not exempt, yet still ranked above
        // the entity nobody ever read — reinforcement does real work.
        assert!(!by_id(frequent).exempt);
        assert!(by_id(frequent).score > by_id(stale).score);
        assert!(by_id(stale).score < p.floor);
    }

    #[test]
    fn dry_run_reports_without_writing_then_apply_revokes_with_a_reason() {
        let p = RetentionPolicy::default();
        let e = engine();
        let now = 1_000_000_000_000;
        let id = e
            .add_entity(EntityKind::Concept, "forgotten-lore", 0.2, now - 365 * DAY)
            .expect("add");

        let dry = forget(&e, &p, now, 100, false).expect("dry run");
        assert_eq!(dry.forgotten.len(), 1);
        assert_eq!(e.stats().expect("stats").entities_current, 1);
        assert!(e.entity_at(id, now).expect("at").is_some());

        let applied = forget(&e, &p, now, 100, true).expect("apply");
        assert_eq!(applied.forgotten.len(), 1);
        assert_eq!(applied.forgotten[0].entity_id, id);
        assert!(!applied.forgotten[0].explanation.is_empty());
        // Closed, not deleted: the row still exists and is readable as-of the past.
        assert_eq!(e.stats().expect("stats").entities_current, 0);
        assert!(e.entity_at(id, now - 1).expect("past").is_some());
        // The reason names the score and the dominant factor, so the decision is
        // explained in the journal rather than implied.
        let history = crate::vault_history::vault_history(&e, "forgotten-lore").expect("history");
        let text = history.to_string();
        assert!(text.contains("retention below floor"), "{text}");
        assert!(text.contains("dominant:"), "{text}");
    }

    #[test]
    fn batch_cap_bounds_a_policy_mistake() {
        let p = RetentionPolicy::default();
        let e = engine();
        let now = 1_000_000_000_000;
        for i in 0..5 {
            e.add_entity(
                EntityKind::Concept,
                &format!("old-{i}"),
                0.2,
                now - 365 * DAY,
            )
            .expect("add");
        }
        let batch = forget(&e, &p, now, 2, true).expect("apply");
        assert_eq!(batch.candidates, 5);
        assert_eq!(batch.forgotten.len(), 2);
        assert_eq!(e.stats().expect("stats").entities_current, 3);
    }

    #[test]
    fn reinstatement_reopens_from_the_closed_row_in_both_directions() {
        let p = RetentionPolicy::default();
        let e = engine();
        let now = 1_000_000_000_000;
        let id = e
            .add_entity(EntityKind::Concept, "buried", 0.2, now - 365 * DAY)
            .expect("add");
        e.add_observation(id, "the buried fact", "fact", 0.9, now - 365 * DAY)
            .expect("observation");
        forget(&e, &p, now, 10, true).expect("forget");

        let revived = reinstate(&e, id, now + DAY).expect("reinstate");
        assert_ne!(revived, id);
        let row = e
            .entity_at(revived, now + DAY)
            .expect("at")
            .expect("current");
        assert_eq!(row.name, "buried");
        assert!(row.metadata.contains(&format!("\"supersedes\":{id}")));
        let old = e.entity_at(id, now - 1).expect("old").expect("as-of");
        assert!(old.metadata.contains("superseded_by"));
        // Both directions resolve through the ordinary history resolver.
        let chain = crate::vault_history::vault_history(&e, "buried").expect("chain");
        let chain = chain.to_string();
        assert!(chain.contains("buried"), "{chain}");
        // And re-instating a live entity is refused rather than duplicated.
        assert!(reinstate(&e, revived, now + 2 * DAY).is_err());
    }
}
