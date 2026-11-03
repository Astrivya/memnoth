// ============================================================================
use super::*;

// Scoring primitives
// ============================================================================

/// BEAM scores every rubric nugget on a three-point scale and **clamps** a judge's
/// continuous score onto it. The thresholds are part of the benchmark, not a
/// presentation choice: a judge that says 0.7 means "correct" (≥0.75), and one that
/// says 0.3 means "partial" (≥0.25). Implementing our own thresholds would measure a
/// different task while printing the same column name.
pub fn nugget_score(raw: f64) -> f64 {
    if !raw.is_finite() {
        return 0.0;
    }
    if raw >= 0.75 {
        1.0
    } else if raw >= 0.25 {
        0.5
    } else {
        0.0
    }
}

/// Mean of a finite series, `None` when empty.
///
/// `None` rather than `0.0` on purpose: an empty benchmark must never print as a
/// score. Every caller in this module is expected to surface the absence.
pub fn mean(values: &[f64]) -> Option<f64> {
    let usable: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if usable.is_empty() {
        return None;
    }
    Some(usable.iter().sum::<f64>() / usable.len() as f64)
}

/// A per-class tally: how many items, how many correct.
///
/// Classes exist because an aggregate hides the only actionable information. Our
/// LongMemEval aggregate of 70.0% concealed a 35.7% class and a 92.9% class, and
/// those two facts imply completely different fixes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClassTally {
    pub total: usize,
    pub correct: usize,
    pub score_sum: f64,
}

impl ClassTally {
    pub fn add(&mut self, correct: bool, score: f64) {
        self.total += 1;
        if correct {
            self.correct += 1;
        }
        if score.is_finite() {
            self.score_sum += score;
        }
    }

    pub fn accuracy(&self) -> Option<f64> {
        if self.total == 0 {
            None
        } else {
            Some(self.correct as f64 / self.total as f64 * 100.0)
        }
    }

    pub fn avg_score(&self) -> Option<f64> {
        if self.total == 0 {
            None
        } else {
            Some(self.score_sum / self.total as f64)
        }
    }
}

/// Render a class table worst-first. Worst-first because the whole point of a class
/// breakdown is to point at the loss, and a table sorted by class name buries it.
pub fn class_report(tallies: &BTreeMap<String, ClassTally>) -> String {
    let mut rows: Vec<(&String, &ClassTally)> = tallies.iter().collect();
    rows.sort_by(|a, b| {
        let ka = a.1.accuracy().unwrap_or(-1.0);
        let kb = b.1.accuracy().unwrap_or(-1.0);
        ka.total_cmp(&kb)
    });
    rows.iter()
        .map(|(k, v)| {
            format!(
                "{:<26} n={:<4} acc={:>6.1}% score={:.3}",
                k,
                v.total,
                v.accuracy().unwrap_or(0.0),
                v.avg_score().unwrap_or(0.0)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
