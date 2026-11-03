//! Memory Brain feed: decisions + lessons + graph counts (§17.6).
//!
//! One call — [`MemoryEngine::brain_feed`] — returns everything Lane C
//! needs to render the Brain view, with zero TUI code on this side:
//!
//! ```text
//! DECISIONS                                    LESSONS (injected every turn)
//! ❯ sandbox profiles, not toggles             ⚠ never parse PTY bytes as text
//!   rejected: per-tool flags (flag sprawl)       from: 2 failures, kept 2026-09
//! ──────────────────────────────────────────────────────────────────────────
//! GRAPH 1,204 symbols · 3,908 edges · gen 42 · stale 2 files [Map] [Heal]
//! ```
//!
//! - DECISIONS: current `decision` entities + `decision` observations
//!   (title :: rationale), newest first.
//! - LESSONS: current `lesson`/`gotcha` observations with provenance
//!   (confidence + date) so trust is earned — injected every turn.
//! - GRAPH counts come from the codegraph side
//!   (`memnoth_codegraph::feeds::graph_counts`); this feed carries the
//!   memory half plus vault stats. Bounded (`BRAIN_*` constants), read-only.

use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::store::MemoryEngine;

/// Max decisions in one feed.
pub const BRAIN_MAX_DECISIONS: usize = 32;
/// Max lessons in one feed.
pub const BRAIN_MAX_LESSONS: usize = 32;

/// One decision row (`title :: rationale`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainDecision {
    /// What was decided (the `title` half of `title :: rationale`).
    pub title: String,
    /// Why it was decided.
    pub rationale: String,
    /// Stored confidence, `0.0`–`1.0`.
    pub confidence: f64,
}

/// One lesson row (with provenance).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainLesson {
    /// The lesson itself.
    pub text: String,
    /// Lesson category (`lesson` / `gotcha` / …).
    pub kind: String,
    /// Stored confidence, `0.0`–`1.0`.
    pub confidence: f64,
    /// `valid_from` (ms) — "kept since" date for provenance.
    pub kept_since_ms: i64,
}

/// The Memory Brain feed (memory half; pair with codegraph graph counts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainFeed {
    /// Most recent decisions (capped at [`BRAIN_MAX_DECISIONS`]).
    pub decisions: Vec<BrainDecision>,
    /// Most recent lessons (capped at [`BRAIN_MAX_LESSONS`]).
    pub lessons: Vec<BrainLesson>,
    /// Live entity count in the vault.
    pub entities_current: i64,
    /// Live relation count in the vault.
    pub relations_current: i64,
    /// Total observations ever recorded.
    pub observations_total: i64,
}

impl MemoryEngine {
    /// Read-only Brain feed: decisions + lessons + vault stats.
    pub fn brain_feed(&self) -> Result<BrainFeed> {
        let now = crate::now_ms();
        let conn = self.connection();

        // Decisions: current decision entities (name :: summary)...
        let mut decisions: Vec<BrainDecision> = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT name, COALESCE(summary,''), confidence FROM entities
                 WHERE kind = 'decision' AND valid_from <= ?1
                 AND valid_to IS NULL ORDER BY valid_from DESC LIMIT ?2",
            )?;
            let rows: Vec<(String, String, f64)> = stmt
                .query_map(params![now, BRAIN_MAX_DECISIONS as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .filter_map(|x| x.ok())
                .collect();
            for (name, summary, confidence) in rows {
                decisions.push(BrainDecision {
                    title: name,
                    rationale: summary,
                    confidence,
                });
            }
        }
        // ...plus decision observations attached to any current entity.
        {
            let mut stmt = conn.prepare(
                "SELECT o.content, o.confidence FROM observations o
                 JOIN entities e ON e.id = o.entity_id
                 WHERE o.kind = 'decision' AND o.valid_from <= ?1 AND o.valid_to IS NULL
                 AND e.valid_to IS NULL ORDER BY o.valid_from DESC LIMIT ?2",
            )?;
            let rows: Vec<(String, f64)> = stmt
                .query_map(params![now, BRAIN_MAX_DECISIONS as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .filter_map(|x| x.ok())
                .collect();
            for (content, confidence) in rows {
                let (title, rationale) = match content.split_once("::") {
                    Some((t, r)) => (t.trim().to_string(), r.trim().to_string()),
                    None => (content.clone(), String::new()),
                };
                if decisions.len() < BRAIN_MAX_DECISIONS {
                    decisions.push(BrainDecision {
                        title,
                        rationale,
                        confidence,
                    });
                }
            }
        }

        // Lessons: current lesson/gotcha observations with provenance.
        let mut lessons: Vec<BrainLesson> = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT o.content, o.kind, o.confidence, o.valid_from FROM observations o
                 JOIN entities e ON e.id = o.entity_id
                 WHERE o.kind IN ('lesson','gotcha') AND o.valid_from <= ?1 AND o.valid_to IS NULL
                 AND e.valid_to IS NULL ORDER BY o.confidence DESC, o.valid_from DESC LIMIT ?2",
            )?;
            let rows: Vec<(String, String, f64, i64)> = stmt
                .query_map(params![now, BRAIN_MAX_LESSONS as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .filter_map(|x| x.ok())
                .collect();
            for (text, kind, confidence, kept_since_ms) in rows {
                lessons.push(BrainLesson {
                    text,
                    kind,
                    confidence,
                    kept_since_ms,
                });
            }
        }

        let stats = self.stats()?;
        Ok(BrainFeed {
            decisions,
            lessons,
            entities_current: stats.entities_current,
            relations_current: stats.relations_current,
            observations_total: stats.observations_total,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{EntityKind, open_in_memory};

    #[test]
    fn feed_surfaces_decisions_lessons_and_stats() {
        let eng = open_in_memory().unwrap();
        let now = super::super::now_ms();
        let d = eng
            .add_entity(
                EntityKind::Decision,
                "sandbox profiles, not toggles",
                0.9,
                now,
            )
            .unwrap();
        eng.add_observation(
            d,
            "rejected: per-tool flags :: flag sprawl risk",
            "decision",
            0.9,
            now,
        )
        .unwrap();
        let c = eng
            .add_entity(EntityKind::Concept, "pty", 0.8, now)
            .unwrap();
        eng.add_observation(c, "never parse PTY bytes as text", "gotcha", 0.95, now)
            .unwrap();

        let feed = eng.brain_feed().unwrap();
        assert!(
            feed.decisions
                .iter()
                .any(|d| d.title.contains("sandbox profiles"))
        );
        assert!(
            feed.decisions
                .iter()
                .any(|d| d.rationale.contains("flag sprawl"))
        );
        assert!(
            feed.lessons
                .iter()
                .any(|l| l.text.contains("PTY") && l.kind == "gotcha")
        );
        assert!(feed.entities_current >= 2);
        assert!(feed.observations_total >= 2);
    }

    #[test]
    fn empty_vault_feeds_empty_never_errors() {
        let eng = open_in_memory().unwrap();
        let feed = eng.brain_feed().unwrap();
        assert!(feed.decisions.is_empty() && feed.lessons.is_empty());
    }
}
