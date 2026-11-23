//! Integration tests for the honest paired/held-out benchmark statistics.
//!
//! The module is included by source path so these tests grade the same code the
//! CLI will dispatch to once `src/bench_stats.rs` is declared in `lib.rs`.

#[path = "../src/bench_stats.rs"]
mod bench_stats;

use bench_stats::{
    DEFAULT_BOOTSTRAP, DEFAULT_CI_LEVEL, DEFAULT_N_RUNS, PairedBench, Refusal, RunScores,
    TaskScore, Verdict, bootstrap_ci, compare,
};

fn task(id: &str, score: f64) -> TaskScore {
    TaskScore {
        task_id: id.to_string(),
        score: Some(score),
    }
}

fn run(index: usize, scores: Vec<TaskScore>) -> RunScores {
    RunScores { run: index, scores }
}

/// `n_runs` candidate runs that beat the baseline by `advantage` on every task.
fn winning_candidate(task_ids: &[&str], n_runs: usize, advantage: f64) -> Vec<RunScores> {
    (0..n_runs)
        .map(|r| {
            run(
                r,
                task_ids
                    .iter()
                    .map(|id| task(id, 5.0 + r as f64 + advantage))
                    .collect(),
            )
        })
        .collect()
}

fn baseline(task_ids: &[&str], n_runs: usize) -> Vec<RunScores> {
    (0..n_runs)
        .map(|r| {
            run(
                r,
                task_ids.iter().map(|id| task(id, 5.0 + r as f64)).collect(),
            )
        })
        .collect()
}

fn bench(
    task_ids: &[&str],
    candidate_runs: Vec<RunScores>,
    baseline_runs: Vec<RunScores>,
    held_out: &[&str],
) -> PairedBench {
    PairedBench {
        candidate_name: "candidate".to_string(),
        baseline_name: "baseline".to_string(),
        n_runs: candidate_runs.len(),
        task_ids: task_ids.iter().map(|id| id.to_string()).collect(),
        candidate_runs,
        baseline_runs,
        held_out: held_out.iter().map(|id| id.to_string()).collect(),
        seed: 20261121,
    }
}

#[test]
fn known_ci_result() {
    // N=3 is the committed paired-repetition default.
    assert_eq!(DEFAULT_N_RUNS, 3);

    // Golden values produced by this exact xorshift64 + modulo-index algorithm
    // (seed 20261121, 10,000 resamples): the interval must be reproducible.
    let ci = bootstrap_ci(
        &[0.0, 1.0, 2.0, 3.0, 4.0],
        DEFAULT_CI_LEVEL,
        DEFAULT_BOOTSTRAP,
        20261121,
    );
    assert_eq!(ci.level, 0.95);
    assert!((ci.lower - 0.8).abs() < 1e-9, "lower = {}", ci.lower);
    assert!((ci.upper - 3.2).abs() < 1e-9, "upper = {}", ci.upper);

    // A constant sample has a degenerate interval, whatever the seed.
    let constant = bootstrap_ci(
        &[2.5, 2.5, 2.5, 2.5, 2.5],
        DEFAULT_CI_LEVEL,
        DEFAULT_BOOTSTRAP,
        7,
    );
    assert_eq!((constant.lower, constant.upper), (2.5, 2.5));

    // An interval needs at least two observations; refuse by reporting NaN
    // rather than inventing bounds.
    let singleton = bootstrap_ci(&[2.5], DEFAULT_CI_LEVEL, 100, 1);
    assert!(singleton.lower.is_nan() && singleton.upper.is_nan());
}

#[test]
fn paired_comparison_wins_and_unpaired_is_refused() {
    let ids = ["t1", "t2", "t3"];
    let paired = bench(
        &ids,
        winning_candidate(&ids, 3, 1.0),
        baseline(&ids, 3),
        &ids,
    );
    let report = compare(&paired).expect("paired comparison should compute");
    assert_eq!(report.verdict, Verdict::Win);
    assert!(report.baseline_matched);
    assert!(report.runs_complete);
    assert_eq!(report.method, "paired, held-out");
    assert!(report.summary.contains("paired, held-out"));
    assert!((report.mean_difference - 1.0).abs() < 1e-9);
    assert!(report.ci.lower > 0.0);

    // Same three tasks, but run 0 measures the baseline on a different task id.
    // The two sides are no longer paired: refuse instead of averaging.
    let mut broken_baseline = baseline(&ids, 3);
    broken_baseline[0].scores[2].task_id = "other".to_string();
    let unpaired = bench(&ids, winning_candidate(&ids, 3, 1.0), broken_baseline, &ids);
    match compare(&unpaired) {
        Err(Refusal::Unpaired { .. }) => {}
        other => panic!("expected Unpaired refusal, got {other:?}"),
    }
}

#[test]
fn missing_data_is_refused_not_zero_filled() {
    let ids = ["t1", "t2", "t3"];
    let mut candidate = winning_candidate(&ids, 3, 1.0);
    candidate[1].scores[0].score = None;
    let input = bench(&ids, candidate, baseline(&ids, 3), &ids);

    match compare(&input) {
        Err(Refusal::MissingData { detail }) => {
            assert!(
                detail.contains("zero"),
                "refusal must say why no zero was substituted: {detail}"
            );
        }
        other => panic!("expected MissingData refusal, got {other:?}"),
    }

    // A non-finite value is missing data too, not a legitimate score.
    let mut nan_bench = bench(
        &ids,
        winning_candidate(&ids, 3, 1.0),
        baseline(&ids, 3),
        &ids,
    );
    nan_bench.baseline_runs[2].scores[1].score = Some(f64::NAN);
    assert!(matches!(
        compare(&nan_bench),
        Err(Refusal::MissingData { .. })
    ));
}

#[test]
fn incomplete_run_is_labelled_with_its_real_denominator() {
    // Only two of the three expected tasks were run, on BOTH sides (so the
    // comparison is still paired). It must be reported, labelled incomplete,
    // and never awarded a win.
    let present = ["t1", "t2"];
    let all = ["t1", "t2", "t3"];
    let bench = bench(
        &all,
        winning_candidate(&present, 3, 1.0),
        baseline(&present, 3),
        &present,
    );
    let report = compare(&bench).expect("incomplete but paired run should report");

    assert!(!report.runs_complete);
    assert!(!report.baseline_matched);
    assert_eq!(report.verdict, Verdict::NoWin);
    assert!(report.label.contains("incomplete-run"));
    assert_eq!(report.tasks_per_run, 2);
    assert_eq!(report.expected_tasks_per_run, 3);
    assert_eq!(report.denominator, 2);
    assert!(report.summary.contains("incomplete-run"));
}
