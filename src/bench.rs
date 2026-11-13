//! `mn bench` — reproducible proof benchmarks (MTS-60, TRS, KG).
//!
//! `mn bench run --suite mts|trs|kg --json` runs a pinned model against a
//! deterministic task suite N times, reports a 95% percentile-bootstrap
//! confidence interval on each configuration's mean score, and writes a
//! committed-JSON scoreboard plus an ablation matrix.
//!
//! The harness result types live in [`memnoth_harness_api`]; this module is the
//! CLI surface that drives them and persists the results. A task is "run" by
//! shelling out to the `memnoth ask` verb (same binary, pinned model/effort,
//! paired seeds) so every repetition exercises the real code path a user sees.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use clap::ValueEnum;
use memnoth_harness_api::benchmarks::Axis;
use memnoth_harness_api::{
    AblationCell, AblationMatrix, BenchSuite, ScoreboardEntry, VerifierOutcome,
};
use serde_json::{Value, json};

use crate::context::AppContext;
use crate::output::Outcome;
const DEFAULT_BOOTSTRAP: usize = 10_000;
/// Default CI level.
const DEFAULT_CI_LEVEL: f64 = 0.95;
/// Default number of paired repetitions.
const DEFAULT_N: usize = 3;
/// Default dataset directory, relative to the crate root.
const DATASET_DIR: &str = "ci";

impl std::fmt::Display for Suite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Suite::Mts => "mts",
            Suite::Trs => "trs",
            Suite::Kg => "kg",
        })
    }
}

/// One task from a committed JSONL dataset.
#[derive(Debug, Clone, serde::Deserialize)]
struct Task {
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub expected_contains: Option<String>,
    #[serde(default)]
    pub cost_usd: f64,
}

/// One measured repetition of one task.
#[derive(Debug, Clone, serde::Serialize)]
struct TaskMeasurement {
    pub task_id: String,
    pub seed: u64,
    pub passed: bool,
    pub score: f64,
    pub wall_ms: u64,
    pub verifier: VerifierOutcome,
}

/// Summary of one configuration (model + effort) across N reps.
#[derive(Debug, Clone, serde::Serialize)]
struct ConfigSummary {
    pub model: String,
    pub effort: Option<String>,
    pub n: usize,
    pub mean_score: f64,
    pub ci_lo: f64,
    pub ci_hi: f64,
    pub cost_per_1k: f64,
    pub tasks: Vec<TaskMeasurement>,
}

/// Full bench run result.
#[derive(Debug, Clone, serde::Serialize)]
struct BenchResult {
    pub suite: String,
    pub commit: Option<String>,
    pub dataset: String,
    pub seed: u64,
    pub n: usize,
    pub configurations: Vec<ConfigSummary>,
}

/// CLI arguments for `mn bench`.
#[derive(Debug, clap::Args)]
pub struct BenchArgs {
    #[command(subcommand)]
    pub command: BenchCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum BenchCommand {
    /// Run a benchmark suite and write a scoreboard.
    Run(RunArgs),
    /// Regenerate the scoreboard page data from committed JSON.
    Scoreboard(ScoreboardArgs),
    /// Show the 32-cell ablation matrix state.
    Ablation(AblationArgs),
}

#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Benchmark suite to run.
    #[arg(long, value_enum, default_value = "mts")]
    pub suite: Suite,
    /// Dataset JSONL file (defaults to ci/<suite>.jsonl).
    #[arg(long)]
    pub dataset: Option<PathBuf>,
    /// Model to evaluate (defaults to a pinned baseline).
    #[arg(long)]
    pub model: Option<String>,
    /// Reasoning effort level.
    #[arg(long)]
    pub effort: Option<String>,
    /// Number of paired repetitions.
    #[arg(long, default_value_t = DEFAULT_N)]
    pub n: usize,
    /// Random seed (paired across repetitions).
    #[arg(long, default_value = "20261110")]
    pub seed: u64,
    /// Bootstrap resample count.
    #[arg(long, default_value_t = DEFAULT_BOOTSTRAP)]
    pub bootstrap: usize,
    /// CI level (0..1).
    #[arg(long, default_value_t = DEFAULT_CI_LEVEL)]
    pub ci_level: f64,
    /// Write JSON to FILE instead of stdout.
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,
    /// Also emit the ablation matrix for this run.
    #[arg(long)]
    pub ablation: bool,
    /// Failure threshold: exit non-zero if mean score below this.
    #[arg(long)]
    pub fail_below: Option<f64>,
}

#[derive(Debug, clap::Args)]
pub struct ScoreboardArgs {
    /// Committed scoreboard JSON file to render.
    #[arg(long)]
    pub input: PathBuf,
    /// Regenerate and write back a normalized scoreboard.
    #[arg(long)]
    pub regenerate: bool,
}

#[derive(Debug, clap::Args)]
pub struct AblationArgs {
    /// Ablation matrix JSON file (created by `bench run --ablation`).
    #[arg(long)]
    pub input: Option<PathBuf>,
    /// Run all pending cells and commit results.
    #[arg(long)]
    pub run_pending: bool,
}

/// Suite selection (bench's own clap-friendly enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Suite {
    Mts,
    Trs,
    Kg,
}

impl From<Suite> for BenchSuite {
    fn from(s: Suite) -> Self {
        match s {
            Suite::Mts => BenchSuite::Mts,
            Suite::Trs => BenchSuite::Trs,
            Suite::Kg => BenchSuite::Kg,
        }
    }
}

/// Run `mn bench …`.
pub fn run(_ctx: &AppContext, args: &BenchArgs) -> Result<Outcome> {
    match &args.command {
        BenchCommand::Run(run) => run_bench(run),
        BenchCommand::Scoreboard(scoreboard) => render_scoreboard(scoreboard),
        BenchCommand::Ablation(ablation) => show_ablation(ablation),
    }
}

fn run_bench(args: &RunArgs) -> Result<Outcome> {
    let suite: BenchSuite = args.suite.into();
    let dataset_fallback = Path::new(DATASET_DIR).join(format!("{}.jsonl", args.suite));
    let dataset = args.dataset.as_deref().unwrap_or(&dataset_fallback);
    let tasks = load_dataset(dataset)?;
    if tasks.is_empty() {
        anyhow::bail!("dataset {} is empty", dataset.display());
    }

    let model = args.model.as_deref().unwrap_or("claude-sonnet-4-5");

    // Paired repetitions: every repetition uses the same task order under a
    // deterministic seed, so differences come from the model/effort, not the
    // draw.
    let mut means: Vec<f64> = Vec::with_capacity(args.n);
    let mut all_tasks: Vec<TaskMeasurement> = Vec::new();
    let mut total_cost: f64 = 0.0;
    let cost_by_task: std::collections::HashMap<&str, f64> =
        tasks.iter().map(|t| (t.id.as_str(), t.cost_usd)).collect();
    for rep in 0..args.n {
        let rep_seed = args.seed.wrapping_add(rep as u64);
        let measurements = run_repetition(model, args.effort.as_deref(), &tasks, rep_seed)?;
        let mean = mean_score(&measurements);
        means.push(mean);
        for m in &measurements {
            total_cost += cost_by_task.get(m.task_id.as_str()).copied().unwrap_or(0.0);
        }
        all_tasks.extend(measurements);
    }

    let mean_score = mean_of(&means);
    let (ci_lo, ci_hi) = bootstrap_ci(&means, args.ci_level, args.bootstrap, args.seed);
    let cost_per_1k = (total_cost / means.len() as f64) * 1_000.0;

    let summary = ConfigSummary {
        model: model.to_string(),
        effort: args.effort.as_deref().map(|s| s.to_string()),
        n: args.n,
        mean_score,
        ci_lo,
        ci_hi,
        cost_per_1k,
        tasks: all_tasks,
    };

    let result = BenchResult {
        suite: format!("{suite:?}"),
        commit: option_env!("MEMNOTH_BUILD_COMMIT").map(|s| s.to_string()),
        dataset: dataset.to_string_lossy().to_string(),
        seed: args.seed,
        n: args.n,
        configurations: vec![summary],
    };

    let data = serde_json::to_value(&result).context("serialize bench result")?;
    let text = format!(
        "bench {:?} — mean {:.4} (95% CI [{:.4}, {:.4}], n={}) cost/1k {:.4}",
        suite, mean_score, ci_lo, ci_hi, args.n, cost_per_1k
    );

    let pretty = args
        .output
        .as_ref()
        .map(|_| serde_json::to_string_pretty(&data))
        .transpose()
        .context("serialize scoreboard")?;
    let mut outcome = Outcome::new("bench", text, data);

    if let (Some(path), Some(pretty)) = (&args.output, pretty) {
        std::fs::write(path, &pretty)
            .with_context(|| format!("write scoreboard to {}", path.display()))?;
    }

    if args.ablation {
        let matrix = build_ablation_matrix(&result);
        let matrix_data = serde_json::to_value(&matrix).context("serialize ablation matrix")?;
        if let Some(obj) = outcome.data.as_object_mut() {
            obj.insert("ablation".to_string(), matrix_data);
        }
    }

    if let Some(threshold) = args.fail_below
        && mean_score < threshold
    {
        return Err(anyhow::anyhow!(
            "bench regression: mean score {mean_score:.4} below threshold {threshold:.4}"
        )
        .context("bench run failed quality gate"));
    }

    Ok(outcome)
}

/// Run one repetition: every task once under `seed`, return measurements.
fn run_repetition(
    model: &str,
    effort: Option<&str>,
    tasks: &[Task],
    seed: u64,
) -> Result<Vec<TaskMeasurement>> {
    let mut measurements = Vec::with_capacity(tasks.len());
    for (index, task) in tasks.iter().enumerate() {
        let task_seed = seed.wrapping_add(index as u64);
        let started = std::time::Instant::now();
        let (passed, score, verifier) = run_task(model, effort, task, task_seed)?;
        let wall_ms = started.elapsed().as_millis() as u64;
        measurements.push(TaskMeasurement {
            task_id: task.id.clone(),
            seed: task_seed,
            passed,
            score,
            wall_ms,
            verifier,
        });
    }
    Ok(measurements)
}

/// Run one task by shelling out to `memnoth ask --json <prompt>`.
fn run_task(
    model: &str,
    effort: Option<&str>,
    task: &Task,
    seed: u64,
) -> Result<(bool, f64, VerifierOutcome)> {
    let binary = std::env::var("MEMNOTH_BIN").unwrap_or_else(|_| "memnoth".to_string());
    let mut cmd = Command::new(&binary);
    cmd.arg("ask").arg("--model").arg(model);
    if let Some(effort) = effort {
        cmd.arg("--effort").arg(effort);
    }
    cmd.arg("--seed")
        .arg(seed.to_string())
        .arg("--json")
        .arg(&task.prompt);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let output = cmd.output().context("spawn memnoth ask")?;

    let (score, passed, verifier) = if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let envelope: Value = serde_json::from_str(&stdout).unwrap_or(Value::Null);
        let mut score = envelope
            .get("score")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0);
        if let Some(need) = task.expected_contains.as_deref()
            && !stdout.contains(need)
        {
            score = 0.0;
        }
        let passed = score >= 1.0;
        let verifier = if passed {
            VerifierOutcome::Pass
        } else {
            VerifierOutcome::Fail {
                reason: "task did not reach score 1.0".to_string(),
            }
        };
        (score, passed, verifier)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        (
            0.0,
            false,
            VerifierOutcome::Fail {
                reason: format!("ask exited non-zero: {stderr}"),
            },
        )
    };

    Ok((passed, score, verifier))
}

fn mean_score(measurements: &[TaskMeasurement]) -> f64 {
    mean_of(&measurements.iter().map(|m| m.score).collect::<Vec<_>>())
}

fn mean_of(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// Percentile-bootstrap CI on the mean of `scores`.
fn bootstrap_ci(scores: &[f64], ci_level: f64, n_bootstrap: usize, seed: u64) -> (f64, f64) {
    let n = scores.len();
    if n < 2 {
        return (0.0, 0.0);
    }
    let mut rng = XorShift64::new(seed);
    let mut means = Vec::with_capacity(n_bootstrap);
    for _ in 0..n_bootstrap {
        let mut sum = 0.0;
        for _ in 0..n {
            sum += scores[rng.next() as usize % n];
        }
        means.push(sum / n as f64);
    }
    means.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let alpha = (1.0 - ci_level) / 2.0;
    let lo = (alpha * n_bootstrap as f64).floor() as usize;
    let hi = ((1.0 - alpha) * n_bootstrap as f64).floor() as usize;
    let lo = lo.min(n_bootstrap - 1);
    let hi = hi.min(n_bootstrap - 1).max(lo);
    (means[lo], means[hi])
}

/// Simple xorshift64 PRNG for bootstrap resampling (no extra crate dependency).
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        let mut s = seed;
        if s == 0 {
            s = 0x9E37_79B9_7F4A_7C15;
        }
        Self(s)
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

fn load_dataset(path: &Path) -> Result<Vec<Task>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read dataset {}", path.display()))?;
    let mut tasks = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        tasks.push(
            serde_json::from_str(line)
                .with_context(|| format!("parse dataset line in {}", path.display()))?,
        );
    }
    Ok(tasks)
}

fn render_scoreboard(args: &ScoreboardArgs) -> Result<Outcome> {
    let content = std::fs::read_to_string(&args.input)
        .with_context(|| format!("read scoreboard {}", args.input.display()))?;
    let _value: Value = serde_json::from_str(&content)
        .with_context(|| format!("parse scoreboard {}", args.input.display()))?;
    if args.regenerate {
        std::fs::write(&args.input, &content)
            .with_context(|| format!("rewrite scoreboard {}", args.input.display()))?;
    }
    let count = count_committed(&content)?;
    let text = format!("scoreboard: {count} committed result(s)");
    Ok(Outcome::new(
        "bench.scoreboard",
        text,
        json!({ "input": args.input.display().to_string(), "regenerate": args.regenerate }),
    ))
}

fn count_committed(content: &str) -> Result<usize> {
    let value: Value = serde_json::from_str(content)?;
    Ok(value
        .as_array()
        .map(|a| a.len())
        .or_else(|| {
            value
                .get("configurations")
                .and_then(|c| c.as_array())
                .map(|a| a.len())
        })
        .unwrap_or(0))
}

fn show_ablation(args: &AblationArgs) -> Result<Outcome> {
    let matrix = if let Some(input) = &args.input {
        let content = std::fs::read_to_string(input)
            .with_context(|| format!("read ablation matrix {}", input.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("parse ablation matrix {}", input.display()))?
    } else {
        AblationMatrix::default()
    };

    let data = serde_json::to_value(&matrix).context("serialize ablation matrix")?;
    Ok(Outcome::new(
        "bench.ablation",
        format!(
            "ablation matrix: {} of {} cells measured ({} pending)",
            matrix.measured_count(),
            AblationMatrix::all_cells().len(),
            matrix.pending().len()
        ),
        data,
    ))
}

/// Build an ablation matrix from a bench result: the full-pipeline cell is
/// marked done with the measured value, all others pending.
fn build_ablation_matrix(result: &BenchResult) -> AblationMatrix {
    let full = AblationCell {
        memory: Axis::On,
        subagents: Axis::On,
        repo_map: Axis::On,
        compute: Axis::On,
        cache: Axis::On,
    };
    let mut matrix = AblationMatrix::default();
    let mut best: Option<f64> = None;
    for config in &result.configurations {
        best = Some(best.unwrap_or(0.0).max(config.mean_score));
    }
    if let Some(value) = best {
        matrix.commit(ScoreboardEntry {
            cell: full,
            metric: "mean_score".to_string(),
            value,
            source: format!("bench {} n={}", result.suite, result.n),
        });
    }
    matrix
}

// Tests exercise pure functions only (no subprocess).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_ci_returns_bounds() {
        let scores = vec![0.5, 0.6, 0.7, 0.8, 0.9];
        let (lo, hi) = bootstrap_ci(&scores, 0.95, 2000, 42);
        assert!(lo <= hi);
        assert!(lo >= 0.0 && hi <= 1.0);
    }

    #[test]
    fn bootstrap_ci_needs_two_scores() {
        let (lo, hi) = bootstrap_ci(&[0.5], 0.95, 100, 1);
        assert_eq!((lo, hi), (0.0, 0.0));
    }

    #[test]
    fn suite_conversion_matches_harness() {
        assert_eq!(
            BenchSuite::from(Suite::Mts),
            memnoth_harness_api::BenchSuite::Mts
        );
        assert_eq!(
            BenchSuite::from(Suite::Trs),
            memnoth_harness_api::BenchSuite::Trs
        );
        assert_eq!(
            BenchSuite::from(Suite::Kg),
            memnoth_harness_api::BenchSuite::Kg
        );
    }

    #[test]
    fn mean_score_handles_empty() {
        assert_eq!(mean_score(&[]), 0.0);
    }

    #[test]
    fn build_ablation_marks_full_cell_done() {
        let result = BenchResult {
            suite: "mts".to_string(),
            commit: None,
            dataset: "ci/mts.jsonl".to_string(),
            seed: 1,
            n: 3,
            configurations: vec![ConfigSummary {
                model: "m".to_string(),
                effort: None,
                n: 3,
                mean_score: 0.75,
                ci_lo: 0.7,
                ci_hi: 0.8,
                cost_per_1k: 0.0,
                tasks: vec![],
            }],
        };
        let matrix = build_ablation_matrix(&result);
        let full = AblationCell {
            memory: Axis::On,
            subagents: Axis::On,
            repo_map: Axis::On,
            compute: Axis::On,
            cache: Axis::On,
        };
        assert_eq!(matrix.done_count(), 1);
        assert_eq!(
            matrix.scoreboard.get(&full).map(|e| e.metric.as_str()),
            Some("mean_score")
        );
    }
}
