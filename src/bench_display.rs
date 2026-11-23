//! Honest rendering of benchmark numbers.
//!
//! A figure is publishable only when the reader can see what it rests on: the
//! denominator, an interval with its method, the dataset (version, variant,
//! size), the scorer/judge version, the provenance (self-measured vs
//! independently reproduced, seed, date) and the metric definition. This
//! module is deliberately `std`-only and free of I/O: it turns an
//! [`Observation`] into a multi-line label or refuses to say anything at all.
//!
//! The rules enforced here, from `docs/research/cli-theme-and-design-2026-11-23.md`:
//!
//! * `n` is shown as `solved/total`, never as a bare percentage.
//! * A point estimate alone is not publishable — an interval and its method are
//!   required.
//! * Statistical ties are one tier; no `#1`/`#2` ordinal rank is ever emitted.
//! * A missing (or incomplete) baseline produces no comparison line at all,
//!   never a fabricated `Δ +0%`.
//! * Zero errors in `n` trials support at most `3/n` error rate at 95%
//!   confidence ([`reliability_ceiling`]); a clean run is not "100% reliable".

/// One benchmark figure together with everything needed to read it honestly.
///
/// `dataset`, `scorer` and `metric` are free-form because the exact version,
/// variant and cut are dataset-specific: put all of those parts in the string
/// (e.g. `LongMemEval-S v1.2 (500q)`). `provenance` must state whether the
/// figure was self-measured or independently reproduced; [`Observation::render`]
/// calls that out explicitly when it says "self-measured". `seed` is an
/// `Option` so a partially-built observation can exist, but rendering refuses
/// without it — a published seed is part of the provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// Number of successfully solved trials.
    pub solved: usize,
    /// Number of trials attempted; must be greater than zero.
    pub total: usize,
    /// Interval (lower, upper) for the estimate; required to publish.
    pub ci: Option<(f64, f64)>,
    /// Named interval method, e.g. "bootstrap percentile, 95%".
    pub ci_method: &'static str,
    /// Dataset name, version, variant and size.
    pub dataset: String,
    /// Scorer or judge name and version.
    pub scorer: String,
    /// "self-measured ..." or "independently reproduced ...".
    pub provenance: String,
    /// Random seed used for the run; required to publish.
    pub seed: Option<u64>,
    /// ISO-8601 date the figure was observed.
    pub observed_at: String,
    /// Metric definition (P75 vs mean, window, strict vs lenient).
    pub metric: String,
}

impl Observation {
    /// Every field that must be present before a figure may be published.
    ///
    /// Returned as a list so a refusal can name what was missing instead of
    /// silently dropping the figure.
    fn validation_errors(&self) -> Vec<&'static str> {
        let mut errors = Vec::new();

        if self.total == 0 {
            errors.push("total (must be > 0)");
        }
        if self.solved > self.total {
            errors.push("solved (must not exceed total)");
        }
        match self.ci {
            Some((lo, hi)) if lo.is_finite() && hi.is_finite() && lo <= hi => {}
            Some(_) => errors.push("finite CI with lower bound <= upper bound"),
            None => errors.push("CI (a bare point estimate is never publishable)"),
        }
        if self.ci_method.trim().is_empty() {
            errors.push("CI method");
        }
        if self.dataset.trim().is_empty() {
            errors.push("dataset version/variant/size");
        }
        if self.scorer.trim().is_empty() {
            errors.push("scorer/judge version");
        }
        if self.provenance.trim().is_empty() {
            errors.push("provenance");
        }
        if self.seed.is_none() {
            errors.push("seed");
        }
        if self.observed_at.trim().is_empty() {
            errors.push("observed date");
        }
        if self.metric.trim().is_empty() {
            errors.push("metric definition");
        }

        errors
    }

    fn provenance_class(&self) -> &'static str {
        let text = self.provenance.to_ascii_lowercase();
        if text.contains("self-measured") || text.contains("self measured") {
            // Deliberately loud: a self-measured number must never be read as
            // an independent reproduction of it.
            "SELF-MEASURED (not independently reproduced)"
        } else if text.contains("independent") {
            "INDEPENDENTLY REPRODUCED"
        } else {
            "PROVENANCE UNCLASSIFIED — verify before publishing"
        }
    }

    /// Render the honest multi-line label, or a refusal naming the missing data.
    ///
    /// A refusal is returned as text (rather than panicking or emitting an
    /// empty label) so a caller can show it directly: no half-labelled figure
    /// ever reaches a reader.
    pub fn render(&self) -> String {
        let errors = self.validation_errors();
        if !errors.is_empty() {
            return format!(
                "REFUSED — benchmark figure withheld; missing or invalid: {}.",
                errors.join(", ")
            );
        }

        let (lo, hi) = self
            .ci
            .expect("validation_errors guarantees a valid interval");
        let seed = self.seed.expect("validation_errors guarantees a seed");
        let solved = self.solved;
        let total = self.total;
        let ceiling = reliability_ceiling(total);

        let mut lines = Vec::with_capacity(8);
        lines.push("mn benchmark observation".to_owned());
        lines.push(format!("result: {solved}/{total} solved"));
        lines.push(format!(
            "interval: [{}, {}] ({})",
            format_number(lo),
            format_number(hi),
            self.ci_method
        ));
        lines.push(format!(
            "dataset (version, variant, size): {}",
            self.dataset
        ));
        lines.push(format!("scorer/judge (version): {}", self.scorer));
        lines.push(format!("metric (definition): {}", self.metric));
        lines.push(format!(
            "provenance: {} | {} | seed {seed} | observed {}",
            self.provenance_class(),
            self.provenance,
            self.observed_at
        ));
        if solved == total {
            lines.push(format!(
                "caveat: 0 errors in {total} trials supports at most {} error rate \
                 (rule of three, 3/{total}, 95% confidence)",
                format_number(ceiling)
            ));
        }
        // No ordinal rank ("#1", "best-in-class") is emitted: only a paired
        // test with a named anchor can support one, and this module has neither.
        lines.join("\n")
    }

    /// Short `solved/total` token used by the comparison line.
    fn count_label(&self) -> String {
        let solved = self.solved;
        let total = self.total;
        format!("{solved}/{total}")
    }
}

/// Upper bound on the error rate supported by zero errors in `trials` trials.
///
/// The rule of three gives roughly `3/n` at 95% confidence: 10 clean trials
/// still permit a 0.30 (30%) error rate. The result is capped at 1.0 because a
/// rate cannot exceed certainty, and zero trials support no bound better than
/// that.
pub fn reliability_ceiling(trials: usize) -> f64 {
    if trials == 0 {
        return 1.0;
    }
    (3.0 / trials as f64).min(1.0)
}

/// Whether two observations' intervals overlap and therefore form one tier.
///
/// Touching endpoints count as overlap. An absent or malformed interval makes
/// the observations incomparable, which is reported as "not tiered" rather
/// than as a confident ranking.
pub fn is_tiered(a: &Observation, b: &Observation) -> bool {
    let (Some((a_lo, a_hi)), Some((b_lo, b_hi))) = (a.ci, b.ci) else {
        return false;
    };
    let valid = |lo: f64, hi: f64| lo.is_finite() && hi.is_finite() && lo <= hi;
    if !valid(a_lo, a_hi) || !valid(b_lo, b_hi) {
        return false;
    }
    a_lo <= b_hi && b_lo <= a_hi
}

/// An honest comparison line, or `None` when there is no usable baseline.
///
/// `None` covers both "no baseline supplied" and "baseline is incomplete": in
/// either case the line is omitted entirely rather than shown as a fabricated
/// `Δ +0%`. A statistical tie is reported as one tier with no ordinal rank.
pub fn comparison(a: Option<&Observation>, b: &Observation) -> Option<String> {
    let baseline = a?;
    if !baseline.validation_errors().is_empty() || !b.validation_errors().is_empty() {
        return None;
    }

    let tied = is_tiered(baseline, b);
    let candidate = b.count_label();
    let baseline = baseline.count_label();
    if tied {
        return Some(format!(
            "comparison: {candidate} vs baseline {baseline} — CIs overlap; one tier (no ordinal rank)"
        ));
    }

    Some(format!(
        "comparison: {candidate} vs baseline {baseline} — CIs do not overlap; not a statistical tie"
    ))
}

/// Format a float without trailing zeroes so a CI reads like a measurement.
fn format_number(value: f64) -> String {
    let text = format!("{value:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}
