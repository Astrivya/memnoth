//! Empirical confidence calibration from labelled fixtures (ALG09).
//!
//! A confidence number is only meaningful if it can be checked against
//! outcomes. This module calibrates confidence *by stratum* — a `(language,
//! query_class)` pair — using nothing but a set of labelled fixtures and
//! arithmetic. There is no model, no network and no fitted parameter: the
//! calibrated probability of a stratum is the observed correctness rate of its
//! labelled outcomes, reported with a 95% Wilson interval.
//!
//! Two kinds of information are kept strictly apart:
//!
//! * [`Provenance`] is **who recorded the label, when, and from which fixture**.
//!   It describes the evidence, never its likelihood. It is carried on every
//!   [`LabelledObservation`] and echoed back on the report, but it is *never*
//!   read by the calibration arithmetic.
//! * [`Probability`] is **how likely an answer in a stratum is to be correct**.
//!   It is computed only from `Some(true)` / `Some(false)` outcomes. A missing
//!   outcome (`None`) is excluded and counted, never silently counted as wrong
//!   and never turned into a number out of thin air.
//!
//! The module refuses to claim calibration when the evidence cannot support
//! it. There is a per-stratum [`AbstentionThreshold`] (minimum label count and
//! maximum interval width) and a global refusal when no stratum clears it. When
//! only some strata clear it the report *degrades* honestly: calibrated strata
//! get probabilities, the rest carry an [`AbstentionReason`] and their real
//! sample size, and the whole report is labelled `degraded`. Nothing is
//! zero-filled, back-filled from self-reported confidence, or invented.
//!
//! Pure (`std` plus `serde` for report serialization) so it can be exercised
//! through a path-included integration test before it is wired into `lib.rs`.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

/// Default minimum number of labelled outcomes required per stratum.
pub const DEFAULT_MIN_LABELS: usize = 20;
/// Default maximum allowed width of the 95% Wilson interval.
pub const DEFAULT_MAX_INTERVAL_WIDTH: f64 = 0.5;
/// z for a two-sided 95% normal interval, used by the Wilson score interval.
const Z_95: f64 = 1.959_963_983_943_286;

/// The kind of query an answer came from. Mirrors the wire classes already
/// used by the KG (`exact`, `likely`, `lexical`) so calibration can be keyed by
/// the same vocabulary a caller sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum QueryClass {
    Exact,
    Likely,
    Lexical,
}

impl QueryClass {
    pub fn label(self) -> &'static str {
        match self {
            QueryClass::Exact => "exact",
            QueryClass::Likely => "likely",
            QueryClass::Lexical => "lexical",
        }
    }
}

impl fmt::Display for QueryClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A calibration stratum: a language crossed with a query class.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct StratumKey {
    pub language: String,
    pub query_class: QueryClass,
}

impl StratumKey {
    pub fn new(language: impl Into<String>, query_class: QueryClass) -> Self {
        Self {
            language: language.into(),
            query_class,
        }
    }
}

impl fmt::Display for StratumKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.language, self.query_class)
    }
}

/// Provenance: WHO recorded a label, WHEN, and from WHICH labelled fixture.
///
/// This is metadata about the evidence. It is deliberately a different type
/// from [`Probability`] and is never consulted by the calibration arithmetic;
/// a change to any of these fields cannot move a probability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Provenance {
    pub recorded_by: String,
    pub recorded_at: String,
    pub fixture: String,
}

impl Provenance {
    pub fn new(
        recorded_by: impl Into<String>,
        recorded_at: impl Into<String>,
        fixture: impl Into<String>,
    ) -> Self {
        Self {
            recorded_by: recorded_by.into(),
            recorded_at: recorded_at.into(),
            fixture: fixture.into(),
        }
    }
}

/// One labelled fixture row.
///
/// `predicted_confidence` is the confidence the system *claimed*; it is kept
/// as metadata so a calibration gap can be reported, but it is NOT the
/// calibrated probability. `outcome` is the ground truth: `Some(correct)` is a
/// usable label, `None` means the fixture has no answer for this row and the
/// row is excluded rather than fabricated into a data point.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LabelledObservation {
    pub id: String,
    pub stratum: StratumKey,
    pub predicted_confidence: f64,
    pub outcome: Option<bool>,
    pub provenance: Provenance,
}

/// An empirical probability that an answer in a stratum is correct.
///
/// Derived only from labelled outcomes. `lower`/`upper` are a deterministic
/// 95% Wilson interval; `correct`/`sample_size` are the real counts behind it,
/// carried alongside so a probability can never be quoted without its
/// denominator.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Probability {
    pub value: f64,
    pub lower: f64,
    pub upper: f64,
    pub correct: usize,
    pub sample_size: usize,
}

/// The evidence floor required before a stratum is allowed a probability.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AbstentionThreshold {
    /// Minimum labelled outcomes in a stratum.
    pub min_labels: usize,
    /// Maximum tolerated width of the 95% interval (`upper - lower`).
    pub max_interval_width: f64,
}

impl Default for AbstentionThreshold {
    fn default() -> Self {
        Self {
            min_labels: DEFAULT_MIN_LABELS,
            max_interval_width: DEFAULT_MAX_INTERVAL_WIDTH,
        }
    }
}

/// Why a stratum was not granted a probability.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum AbstentionReason {
    /// Fewer labelled outcomes than the policy requires.
    InsufficientLabels { have: usize, need: usize },
    /// The interval is too wide for the evidence to be precise enough.
    Imprecise { width: f64, max_width: f64 },
    /// The stratum appears in the fixtures but has no labelled outcomes.
    NoLabels,
}

impl fmt::Display for AbstentionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AbstentionReason::InsufficientLabels { have, need } => {
                write!(f, "insufficient labels: {have}/{need}")
            }
            AbstentionReason::Imprecise { width, max_width } => {
                write!(
                    f,
                    "imprecise interval: width {width:.3} exceeds {max_width:.3}"
                )
            }
            AbstentionReason::NoLabels => f.write_str("no labelled outcomes"),
        }
    }
}

/// What the evidence supports for one stratum.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum StratumOutcome {
    /// The stratum cleared the threshold and has an empirical probability.
    Calibrated {
        probability: Probability,
        /// Mean self-reported confidence, reported for the calibration gap
        /// only — never used to compute `probability`.
        mean_predicted_confidence: f64,
    },
    /// The stratum did not clear the threshold; no probability is claimed.
    Abstain {
        reason: AbstentionReason,
        /// The real number of labelled outcomes, never hidden or zero-filled.
        sample_size: usize,
    },
}

/// Whether every stratum was calibrated or only some were.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CalibrationStatus {
    Full,
    Degraded,
}

impl fmt::Display for CalibrationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CalibrationStatus::Full => "full",
            CalibrationStatus::Degraded => "degraded",
        })
    }
}

/// One stratum's result, kept as a row (not a map key) so the report
/// serializes cleanly and deterministically.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StratumCalibration {
    pub key: StratumKey,
    pub outcome: StratumOutcome,
}

/// An honest calibration report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CalibrationReport {
    pub status: CalibrationStatus,
    pub label: String,
    pub method: String,
    pub policy: AbstentionThreshold,
    pub strata: Vec<StratumCalibration>,
    /// WHO/WHEN recorded the labelled evidence. Reported separately from any
    /// probability; computed probabilities never read this field.
    pub provenance: Vec<Provenance>,
    pub labelled_count: usize,
    pub excluded_unlabelled: usize,
    pub calibrated_strata: usize,
    pub abstained_strata: usize,
    pub summary: String,
}

impl CalibrationReport {
    /// The calibrated probability for a stratum, or `None` when it abstained.
    pub fn probability_for(&self, key: &StratumKey) -> Option<Probability> {
        self.strata
            .iter()
            .find(|row| &row.key == key)
            .and_then(|row| {
                if let StratumOutcome::Calibrated { probability, .. } = &row.outcome {
                    Some(*probability)
                } else {
                    None
                }
            })
    }
}

/// Why calibration could not be computed honestly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Refusal {
    /// Nothing to calibrate, or every unlabelled row was excluded.
    InsufficientData { detail: String },
    /// The request or a fixture row was malformed.
    Invalid { detail: String },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::InsufficientData { detail } => write!(f, "insufficient data: {detail}"),
            Refusal::Invalid { detail } => write!(f, "invalid calibration input: {detail}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Calibrate with the default abstention threshold.
pub fn calibrate(labels: &[LabelledObservation]) -> Result<CalibrationReport, Refusal> {
    calibrate_with(labels, &AbstentionThreshold::default())
}

/// Calibrate with an explicit abstention threshold.
///
/// Returns [`Refusal`] instead of a report when there are no usable labels or
/// when no stratum clears the threshold — a claim of calibration is never made
/// on evidence that cannot support it. Under-supported strata inside an
/// otherwise usable set are labelled `Abstain` with their real denominators.
pub fn calibrate_with(
    labels: &[LabelledObservation],
    policy: &AbstentionThreshold,
) -> Result<CalibrationReport, Refusal> {
    validate_policy(policy)?;
    if labels.is_empty() {
        return Err(Refusal::InsufficientData {
            detail: "no labelled observations were supplied".to_string(),
        });
    }

    // Accumulate labelled outcomes per stratum. Unlabelled rows are counted
    // and excluded; they never reach a probability.
    #[derive(Default)]
    struct Accum {
        correct: usize,
        n: usize,
        predicted_sum: f64,
    }
    let mut groups: BTreeMap<StratumKey, Accum> = BTreeMap::new();
    let mut seen_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut excluded_unlabelled = 0usize;
    let mut provenance: Vec<Provenance> = Vec::new();

    for obs in labels {
        if obs.id.is_empty() {
            return Err(Refusal::Invalid {
                detail: "an observation has an empty id".to_string(),
            });
        }
        if !seen_ids.insert(obs.id.as_str()) {
            return Err(Refusal::Invalid {
                detail: format!("duplicate observation id '{}'", obs.id),
            });
        }
        if obs.stratum.language.is_empty() {
            return Err(Refusal::Invalid {
                detail: format!("observation '{}' has an empty language", obs.id),
            });
        }
        if !obs.predicted_confidence.is_finite() || !(0.0..=1.0).contains(&obs.predicted_confidence)
        {
            return Err(Refusal::Invalid {
                detail: format!(
                    "observation '{}' has non-finite or out-of-range predicted_confidence",
                    obs.id
                ),
            });
        }

        let acc = groups.entry(obs.stratum.clone()).or_default();
        match obs.outcome {
            Some(correct) => {
                acc.n += 1;
                acc.predicted_sum += obs.predicted_confidence;
                if correct {
                    acc.correct += 1;
                }
                provenance.push(obs.provenance.clone());
            }
            None => {
                excluded_unlabelled += 1;
            }
        }
    }

    let labelled_count: usize = groups.values().map(|a| a.n).sum();
    if labelled_count == 0 {
        return Err(Refusal::InsufficientData {
            detail: format!(
                "all {} observations are unlabelled; a probability cannot be fabricated from them",
                excluded_unlabelled
            ),
        });
    }

    let mut strata: Vec<StratumCalibration> = Vec::with_capacity(groups.len());
    let mut calibrated_strata = 0usize;
    let mut abstained_strata = 0usize;

    for (key, acc) in groups {
        let outcome = if acc.n == 0 {
            abstained_strata += 1;
            StratumOutcome::Abstain {
                reason: AbstentionReason::NoLabels,
                sample_size: 0,
            }
        } else if acc.n < policy.min_labels {
            abstained_strata += 1;
            StratumOutcome::Abstain {
                reason: AbstentionReason::InsufficientLabels {
                    have: acc.n,
                    need: policy.min_labels,
                },
                sample_size: acc.n,
            }
        } else {
            let (lower, upper) = wilson_interval(acc.correct, acc.n, Z_95);
            let width = upper - lower;
            if width > policy.max_interval_width {
                abstained_strata += 1;
                StratumOutcome::Abstain {
                    reason: AbstentionReason::Imprecise {
                        width,
                        max_width: policy.max_interval_width,
                    },
                    sample_size: acc.n,
                }
            } else {
                calibrated_strata += 1;
                StratumOutcome::Calibrated {
                    probability: Probability {
                        value: acc.correct as f64 / acc.n as f64,
                        lower,
                        upper,
                        correct: acc.correct,
                        sample_size: acc.n,
                    },
                    mean_predicted_confidence: acc.predicted_sum / acc.n as f64,
                }
            }
        };
        strata.push(StratumCalibration { key, outcome });
    }

    if calibrated_strata == 0 {
        return Err(Refusal::InsufficientData {
            detail: format!(
                "0 of {} strata cleared the calibration threshold ({labelled_count} labelled, \
                 {excluded_unlabelled} unlabelled excluded)",
                strata.len()
            ),
        });
    }

    let status = if abstained_strata == 0 {
        CalibrationStatus::Full
    } else {
        CalibrationStatus::Degraded
    };
    let label = if status == CalibrationStatus::Full {
        "full".to_string()
    } else {
        format!(
            "degraded ({calibrated_strata}/{} strata calibrated)",
            strata.len()
        )
    };

    // Deterministic provenance: sort and dedup the records that actually
    // contributed labelled outcomes.
    provenance.sort_by(|a, b| {
        (&a.recorded_at, &a.recorded_by, &a.fixture).cmp(&(
            &b.recorded_at,
            &b.recorded_by,
            &b.fixture,
        ))
    });
    provenance.dedup();

    let summary = format!(
        "empirical calibration ({status}): {calibrated_strata}/{} strata calibrated, \
         {labelled_count} labelled outcomes, {excluded_unlabelled} unlabelled excluded; \
         probabilities are observed correctness rates with 95% Wilson intervals, \
         provenance reported separately",
        strata.len()
    );

    Ok(CalibrationReport {
        status,
        label,
        method: "empirical, labelled, 95% Wilson".to_string(),
        policy: *policy,
        strata,
        provenance,
        labelled_count,
        excluded_unlabelled,
        calibrated_strata,
        abstained_strata,
        summary,
    })
}

fn validate_policy(policy: &AbstentionThreshold) -> Result<(), Refusal> {
    if policy.min_labels < 2 {
        return Err(Refusal::Invalid {
            detail: "min_labels must be at least 2 for an interval".to_string(),
        });
    }
    if !policy.max_interval_width.is_finite() || policy.max_interval_width <= 0.0 {
        return Err(Refusal::Invalid {
            detail: "max_interval_width must be a positive finite number".to_string(),
        });
    }
    Ok(())
}

/// Deterministic Wilson score interval for a binomial proportion.
fn wilson_interval(correct: usize, n: usize, z: f64) -> (f64, f64) {
    let n = n as f64;
    let p = correct as f64 / n;
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let centre = (p + z2 / (2.0 * n)) / denom;
    let margin = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denom;
    ((centre - margin).max(0.0), (centre + margin).min(1.0))
}
