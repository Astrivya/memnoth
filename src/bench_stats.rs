//! Honest comparative statistics for a "#1" benchmark claim.
//!
//! A bare "our model scored higher" is not a claim worth publishing. This
//! module only produces a [`Verdict::Win`] when three conditions all hold:
//!
//! 1. **Paired.** Candidate and baseline were measured on the *same* tasks in
//!    the *same* runs. A mismatch in the task ids on the two sides is refused,
//!    never averaged away.
//! 2. **Held-out.** The verdict is computed on a task split reserved for the
//!    comparison, not on the tasks used to pick a configuration. The reported
//!    mean and interval are the paired per-task differences
//!    (`candidate - baseline`) on that held-out split.
//! 3. **Supported by the interval.** The 95% percentile-bootstrap confidence
//!    interval on the *held-out* mean difference has its lower bound strictly
//!    above zero. A point estimate alone is not enough.
//!
//! Missing values are refused, not zero-filled: an absent or non-finite score
//! would bias a difference downward and manufacture a win. A run that covers
//! fewer tasks than expected is not refused — it is *labelled* `incomplete-run`
//! and reported with its real denominator, but it can never yield a win.
//!
//! The module is pure (`std` plus `serde` for report serialization) so it can
//! be exercised through a path-included integration test before it is wired
//! into the CLI.

use std::collections::HashMap;

use serde::Serialize;

/// Default number of paired repetitions (N).
pub const DEFAULT_N_RUNS: usize = 3;
/// Default bootstrap resample count.
pub const DEFAULT_BOOTSTRAP: usize = 10_000;
/// Default confidence level.
pub const DEFAULT_CI_LEVEL: f64 = 0.95;

/// A percentile-bootstrap confidence interval on a mean.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ConfidenceInterval {
    pub level: f64,
    pub lower: f64,
    pub upper: f64,
}

/// The verdict of a matched, held-out comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Verdict {
    /// Candidate is significantly ahead on the held-out split.
    Win,
    /// The comparison ran, but the evidence does not support a win.
    NoWin,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Verdict::Win => "win",
            Verdict::NoWin => "no-win",
        })
    }
}

/// Why a comparison could not be computed honestly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Refusal {
    /// Candidate and baseline were not measured on the same task ids.
    Unpaired { detail: String },
    /// A required score was absent or non-finite; refusing to treat it as zero.
    MissingData { detail: String },
    /// The request itself was ill-formed (bad counts, unknown held-out id, ...).
    Invalid { detail: String },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unpaired { detail } => write!(f, "unpaired comparison: {detail}"),
            Refusal::MissingData { detail } => write!(f, "missing data: {detail}"),
            Refusal::Invalid { detail } => write!(f, "invalid comparison: {detail}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// One task's score on one side of one run. `None` is missing data, never zero.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskScore {
    pub task_id: String,
    pub score: Option<f64>,
}

/// One repetition's scores for a single configuration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunScores {
    pub run: usize,
    pub scores: Vec<TaskScore>,
}

/// Input to a paired, held-out comparison.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PairedBench {
    pub candidate_name: String,
    pub baseline_name: String,
    /// Expected number of repetitions (N = 3 by default).
    pub n_runs: usize,
    /// The full task set: defines the pairing key and the per-run denominator.
    pub task_ids: Vec<String>,
    /// Candidate scores, one entry per run.
    pub candidate_runs: Vec<RunScores>,
    /// Baseline scores, one entry per run, on the same task ids.
    pub baseline_runs: Vec<RunScores>,
    /// Task ids reserved for the verdict (subset of `task_ids`).
    pub held_out: Vec<String>,
    /// Seed for the deterministic bootstrap resampler.
    pub seed: u64,
}

/// A computed, honest comparison report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub candidate: String,
    pub baseline: String,
    /// States the design: "paired, held-out".
    pub method: String,
    pub n_runs: usize,
    /// False when at least one run covered fewer tasks than expected.
    pub runs_complete: bool,
    /// Human label, e.g. "paired, held-out, incomplete-run (2/3 tasks per run)".
    pub label: String,
    /// Held-out tasks actually paired across runs (the CI sample size).
    pub denominator: usize,
    /// Tasks present in the first run (actual count).
    pub tasks_per_run: usize,
    /// Tasks expected per run.
    pub expected_tasks_per_run: usize,
    /// Mean paired difference (candidate - baseline) on the held-out split.
    pub mean_difference: f64,
    pub ci: ConfidenceInterval,
    /// True only when the baseline matched on every task and every run.
    pub baseline_matched: bool,
    pub verdict: Verdict,
    /// One-line summary that names the comparison as paired and held-out.
    pub summary: String,
}

/// Percentile-bootstrap CI on the mean of `values`.
///
/// Deterministic for a given `seed`: resamples are drawn with a private
/// xorshift64 generator, so a published interval can be reproduced exactly.
/// Fewer than two values cannot support an interval and yields `NaN` bounds;
/// callers keep such cases out of a verdict (see [`compare`]).
pub fn bootstrap_ci(
    values: &[f64],
    level: f64,
    n_resamples: usize,
    seed: u64,
) -> ConfidenceInterval {
    let n = values.len();
    if n < 2 || n_resamples == 0 {
        return ConfidenceInterval {
            level,
            lower: f64::NAN,
            upper: f64::NAN,
        };
    }

    let mut rng = XorShift64::new(seed);
    let mut means = Vec::with_capacity(n_resamples);
    for _ in 0..n_resamples {
        let mut sum = 0.0;
        for _ in 0..n {
            sum += values[(rng.next() % n as u64) as usize];
        }
        means.push(sum / n as f64);
    }
    means.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let alpha = (1.0 - level) / 2.0;
    let lo = ((alpha * n_resamples as f64).floor() as usize).min(n_resamples - 1);
    let hi = (((1.0 - alpha) * n_resamples as f64).floor() as usize)
        .min(n_resamples - 1)
        .max(lo);
    ConfidenceInterval {
        level,
        lower: means[lo],
        upper: means[hi],
    }
}

/// Run a paired, held-out comparison.
///
/// Returns [`Refusal`] instead of a report when the data cannot support an
/// honest comparison: unpaired task sets, missing scores, or an ill-formed
/// request. Incomplete runs are reported with a label and a real denominator,
/// but never produce a [`Verdict::Win`].
pub fn compare(bench: &PairedBench) -> Result<Report, Refusal> {
    if bench.n_runs == 0 {
        return Err(Refusal::Invalid {
            detail: "n_runs must be greater than zero".to_string(),
        });
    }
    if bench.task_ids.is_empty() {
        return Err(Refusal::Invalid {
            detail: "task_ids must not be empty".to_string(),
        });
    }
    if bench.held_out.is_empty() {
        return Err(Refusal::Invalid {
            detail: "held_out must name at least two tasks for a CI".to_string(),
        });
    }

    validate_unique(&bench.task_ids, "task_ids")?;

    let mut expected: HashMap<&str, ()> = HashMap::with_capacity(bench.task_ids.len());
    for id in &bench.task_ids {
        expected.insert(id.as_str(), ());
    }

    let mut held_out_seen: HashMap<&str, ()> = HashMap::new();
    let mut held_out_tasks: Vec<&str> = Vec::new();
    for id in &bench.held_out {
        if !expected.contains_key(id.as_str()) {
            return Err(Refusal::Invalid {
                detail: format!("held-out task '{id}' is not in task_ids"),
            });
        }
        if held_out_seen.insert(id.as_str(), ()).is_none() {
            held_out_tasks.push(id.as_str());
        }
    }

    if bench.candidate_runs.len() != bench.n_runs || bench.baseline_runs.len() != bench.n_runs {
        return Err(Refusal::Invalid {
            detail: format!(
                "expected {} runs per side, got candidate={} baseline={}",
                bench.n_runs,
                bench.candidate_runs.len(),
                bench.baseline_runs.len()
            ),
        });
    }

    // Build and validate each run's paired maps. Refusal happens here, before
    // any statistic is computed, so no missing value can reach a mean as zero.
    let mut run_maps: Vec<(HashMap<String, f64>, HashMap<String, f64>)> =
        Vec::with_capacity(bench.n_runs);
    let mut all_complete = true;
    let mut first_run_count = 0usize;

    for (index, (cand, base)) in bench
        .candidate_runs
        .iter()
        .zip(bench.baseline_runs.iter())
        .enumerate()
    {
        if cand.run != index || base.run != index {
            return Err(Refusal::Invalid {
                detail: format!("run indices must be 0..{} in order", bench.n_runs),
            });
        }
        let candidate_map = to_map(&cand.scores, "candidate", index)?;
        let baseline_map = to_map(&base.scores, "baseline", index)?;

        let candidate_keys: std::collections::HashSet<&str> =
            candidate_map.keys().map(String::as_str).collect();
        let baseline_keys: std::collections::HashSet<&str> =
            baseline_map.keys().map(String::as_str).collect();
        if candidate_keys != baseline_keys {
            return Err(Refusal::Unpaired {
                detail: format!(
                    "run {index}: candidate and baseline were measured on different task ids"
                ),
            });
        }
        if candidate_keys.iter().any(|id| !expected.contains_key(*id)) {
            return Err(Refusal::Invalid {
                detail: format!("run {index}: a measured task id is not in task_ids"),
            });
        }

        if index == 0 {
            first_run_count = candidate_map.len();
        }
        if candidate_map.len() != bench.task_ids.len() {
            all_complete = false;
        }
        run_maps.push((candidate_map, baseline_map));
    }

    // Per-task mean paired difference on the held-out split. A task absent in
    // every run is excluded from the denominator rather than counted as zero.
    let mut held_out_differences: Vec<f64> = Vec::new();
    for task in held_out_tasks {
        let mut diffs = Vec::with_capacity(bench.n_runs);
        for (candidate_map, baseline_map) in &run_maps {
            if let (Some(c), Some(b)) = (candidate_map.get(task), baseline_map.get(task)) {
                diffs.push(c - b);
            }
        }
        if diffs.is_empty() {
            continue;
        }
        held_out_differences.push(diffs.iter().sum::<f64>() / diffs.len() as f64);
    }
    if held_out_differences.len() < 2 {
        return Err(Refusal::Invalid {
            detail: "fewer than two held-out tasks have a paired score in every run".to_string(),
        });
    }

    let mean_difference =
        held_out_differences.iter().sum::<f64>() / held_out_differences.len() as f64;
    let ci = bootstrap_ci(
        &held_out_differences,
        DEFAULT_CI_LEVEL,
        DEFAULT_BOOTSTRAP,
        bench.seed,
    );

    // The baseline is "matched" only when it covered the full task set on
    // every run (pairing itself was already enforced above).
    let baseline_matched = all_complete;
    let verdict = if baseline_matched && ci.lower > 0.0 {
        Verdict::Win
    } else {
        Verdict::NoWin
    };

    let label = if all_complete {
        "paired, held-out, complete".to_string()
    } else {
        format!(
            "paired, held-out, incomplete-run ({}/{} tasks per run)",
            first_run_count,
            bench.task_ids.len()
        )
    };
    let summary = format!(
        "{} vs {}: paired, held-out over {} tasks, N={} runs; mean paired difference {:+.4}, \
         95% CI [{:.4}, {:.4}] ({}); baseline_matched={}; verdict={}",
        bench.candidate_name,
        bench.baseline_name,
        held_out_differences.len(),
        bench.n_runs,
        mean_difference,
        ci.lower,
        ci.upper,
        label,
        baseline_matched,
        verdict,
    );

    Ok(Report {
        candidate: bench.candidate_name.clone(),
        baseline: bench.baseline_name.clone(),
        method: "paired, held-out".to_string(),
        n_runs: bench.n_runs,
        runs_complete: all_complete,
        label,
        denominator: held_out_differences.len(),
        tasks_per_run: first_run_count,
        expected_tasks_per_run: bench.task_ids.len(),
        mean_difference,
        ci,
        baseline_matched,
        verdict,
        summary,
    })
}

/// Convert one side's scores to a task-id -> score map, refusing missing data.
fn to_map(scores: &[TaskScore], side: &str, run: usize) -> Result<HashMap<String, f64>, Refusal> {
    let mut map = HashMap::with_capacity(scores.len());
    for entry in scores {
        if map.contains_key(&entry.task_id) {
            return Err(Refusal::Invalid {
                detail: format!("duplicate task '{}' in {side} run {run}", entry.task_id),
            });
        }
        let value = match entry.score {
            Some(v) if v.is_finite() => v,
            _ => {
                return Err(Refusal::MissingData {
                    detail: format!(
                        "{side} run {run} task '{}' has no finite score (refusing to treat it as zero)",
                        entry.task_id
                    ),
                });
            }
        };
        map.insert(entry.task_id.clone(), value);
    }
    Ok(map)
}

fn validate_unique(ids: &[String], what: &str) -> Result<(), Refusal> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for id in ids {
        if !seen.insert(id.as_str()) {
            return Err(Refusal::Invalid {
                detail: format!("duplicate id '{id}' in {what}"),
            });
        }
    }
    Ok(())
}

/// Tiny xorshift64 PRNG so results are reproducible without a new dependency.
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}
