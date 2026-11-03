// ============================================================================
use super::*;

// Reconciliation — the spine that serves FAMA, BEAM and selective forgetting
// ============================================================================

/// Why a fact is or is not part of the current belief.
///
/// This exists as data rather than as an `Option` because **the reason is the
/// evidence**. FAMA penalises answers that rest on invalidated memory, so the
/// question a scorer asks is not "is this fact current" but "why did we stop
/// believing it, and was that decision recorded". A system that silently overwrites
/// cannot answer either question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconcileReason {
    /// Never contradicted.
    Current,
    /// Replaced by a later fact; the surviving id is named.
    Superseded { by: String },
    /// Explicitly revoked, with the operator's stated reason.
    Revoked { reason: String },
    /// Valid only after `at` — asked about a time before it existed.
    NotYetValid { at: i64 },
}

/// One fact as the vault stores it: a text, when it started being true, and — if we
/// stopped believing it — when and why.
#[derive(Clone, Debug)]
pub struct FactRecord {
    pub id: String,
    pub text: String,
    pub valid_from: i64,
    pub revoked_at: Option<i64>,
    pub revoked_reason: Option<String>,
    pub superseded_by: Option<String>,
}

impl FactRecord {
    pub fn new(id: &str, text: &str, valid_from: i64) -> Self {
        Self {
            id: id.to_string(),
            text: text.to_string(),
            valid_from,
            revoked_at: None,
            revoked_reason: None,
            superseded_by: None,
        }
    }

    pub fn superseded_by(mut self, by: &str) -> Self {
        self.superseded_by = Some(by.to_string());
        self
    }

    pub fn revoked(mut self, at: i64, reason: &str) -> Self {
        self.revoked_at = Some(at);
        self.revoked_reason = Some(reason.to_string());
        self
    }

    /// Was this fact still believed at `t`?
    ///
    /// Note the asymmetry with a plain validity window: a fact that has been
    /// superseded is *not* believed at any time from the moment it was replaced, and
    /// we do not know that moment except through `revoked_at`. Callers that need
    /// "believed at T" must go through [`belief_at`], which carries the reason.
    pub fn believed_at(&self, t: i64) -> bool {
        if self.valid_from > t {
            return false;
        }
        match self.revoked_at {
            Some(at) => t < at,
            None => true,
        }
    }
}

/// The outcome of asking "what did we believe at `t`", with its justification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Belief {
    pub fact_id: String,
    pub text: String,
    pub reason: ReconcileReason,
}

/// Choose the belief about one subject at time `t`, and **state why**.
///
/// The rule is: among facts that were valid at `t` and not revoked before `t`, take
/// the newest. If none survives, report the most recent *disbelief* with its reason
/// rather than returning nothing — an answer of "we no longer believe X because it
/// was revoked due to Y" is a correct answer to a FAMA-style question, and returning
/// `None` there would make our own scorer unable to express it.
pub fn belief_at(facts: &[FactRecord], t: i64) -> Option<Belief> {
    let mut believed: Vec<&FactRecord> = facts.iter().filter(|f| f.believed_at(t)).collect();
    if !believed.is_empty() {
        believed.sort_by_key(|f| f.valid_from);
        let top = believed[believed.len() - 1];
        let reason = if let Some(by) = &top.superseded_by {
            // Superseded but still inside its validity window: report the succession.
            ReconcileReason::Superseded { by: by.clone() }
        } else {
            ReconcileReason::Current
        };
        return Some(Belief {
            fact_id: top.id.clone(),
            text: top.text.clone(),
            reason,
        });
    }

    // Nothing is believed. Report the freshest disbelief so the reason survives.
    let mut all: Vec<&FactRecord> = facts.iter().collect();
    all.sort_by_key(|f| f.valid_from);
    for f in all.iter().rev() {
        if f.valid_from > t {
            return Some(Belief {
                fact_id: f.id.clone(),
                text: f.text.clone(),
                reason: ReconcileReason::NotYetValid { at: f.valid_from },
            });
        }
        if let Some(at) = f.revoked_at
            && t >= at
        {
            return Some(Belief {
                fact_id: f.id.clone(),
                text: f.text.clone(),
                reason: ReconcileReason::Revoked {
                    reason: f
                        .revoked_reason
                        .clone()
                        .unwrap_or_else(|| "unspecified".into()),
                },
            });
        }
    }
    None
}

/// FAMA — Forgetting-Aware Memory Accuracy — for one answered question.
///
/// The metric is *not* plain accuracy with a penalty bolted on. Its entire point is
/// that an answer resting on **invalidated** memory scores zero even when it matches
/// the gold, because the gold in these datasets is the current truth and the system
/// reached it by trusting something it should have discarded. Getting the right
/// answer the wrong way must not be rewarded, or the benchmark cannot distinguish a
/// memory system from a lucky one.
#[derive(Clone, Copy, Debug)]
pub struct FamaOutcome {
    pub correct: bool,
    pub relied_on_invalid: bool,
}

impl FamaOutcome {
    pub fn score(&self) -> f64 {
        if self.relied_on_invalid {
            0.0
        } else if self.correct {
            1.0
        } else {
            0.0
        }
    }

    /// True when the answer was right but reached through a fact we no longer hold.
    /// Reported separately because it is the single most diagnostic cell in the
    /// table: it is the failure mode Memora found the whole field exhibiting.
    pub fn lucky_but_invalid(&self) -> bool {
        self.correct && self.relied_on_invalid
    }
}

/// Whether the facts an answer cited were still believed when the question was asked.
pub fn relies_on_invalid(facts: &[FactRecord], cited: &[String], t: i64) -> bool {
    let by_id: HashMap<&str, &FactRecord> = facts.iter().map(|f| (f.id.as_str(), f)).collect();
    cited.iter().any(|c| match by_id.get(c.as_str()) {
        Some(f) => !f.believed_at(t),
        // A cited fact we do not hold is not evidence of a stale belief; it is a
        // citation error, and counting it here would inflate the forgetting rate.
        None => false,
    })
}

/// Aggregate FAMA over a run: the headline number plus the two cells that explain it.
#[derive(Clone, Debug, Default)]
pub struct FamaTally {
    pub total: usize,
    pub correct: usize,
    pub invalid_reliance: usize,
    pub lucky_but_invalid: usize,
    score_sum: f64,
}

impl FamaTally {
    pub fn add(&mut self, o: FamaOutcome) {
        self.total += 1;
        if o.correct {
            self.correct += 1;
        }
        if o.relied_on_invalid {
            self.invalid_reliance += 1;
        }
        if o.lucky_but_invalid() {
            self.lucky_but_invalid += 1;
        }
        self.score_sum += o.score();
    }

    pub fn accuracy(&self) -> Option<f64> {
        if self.total == 0 {
            None
        } else {
            Some(self.correct as f64 / self.total as f64 * 100.0)
        }
    }

    /// The metric FAMA reports.
    pub fn fama(&self) -> Option<f64> {
        if self.total == 0 {
            None
        } else {
            Some(self.score_sum / self.total as f64 * 100.0)
        }
    }

    /// The gap between plain accuracy and FAMA is exactly the cost of trusting
    /// invalidated memory. Publishing accuracy without this delta is how a memory
    /// system looks better than it is.
    pub fn forgetting_cost(&self) -> Option<f64> {
        Some(self.accuracy()? - self.fama()?)
    }
}
