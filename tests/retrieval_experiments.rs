//! Integration tests for the offline retrieval experiments (ALG01–06/15).
//!
//! The module is included by source path so these tests grade exactly the code
//! the CLI will dispatch to once `pub mod retrieval_experiments;` is declared
//! in `src/lib.rs`. No model, network, API key or subprocess is involved.

#[path = "../src/retrieval_experiments.rs"]
mod retrieval_experiments;

use retrieval_experiments::{
    Arm, Budget, DecisionReport, DecisionVerdict, FusionWeights, Query, QueryRun, RRF_K,
    RouteChoice, RoutingPolicy, Split, Stage, TaskKind, calibrate_weights, decide, fixture,
    fixture_hash, fixture_no_held_out, learn_policy, mode_recall, route, rrf_fuse, run_experiment,
    weighted_fuse,
};

fn budget() -> Budget {
    Budget::default()
}

fn ids(scores: &[(String, f64)]) -> Vec<&str> {
    scores.iter().map(|(id, _)| id.as_str()).collect()
}

fn run_for<'a>(
    experiment: &'a retrieval_experiments::Experiment,
    query: &str,
    arm: Arm,
) -> &'a QueryRun {
    experiment
        .runs
        .iter()
        .find(|item| item.query_id == query && item.arm == arm)
        .unwrap_or_else(|| panic!("missing run {query} / {arm}"))
}

#[test]
fn all_arms_are_compared_at_an_equal_candidate_and_token_budget() {
    let value = fixture();
    let budget = budget();
    let experiment = run_experiment(&value, &budget);

    assert_eq!(experiment.fixture_id, "mn-retrieval-experiments/1");
    assert_eq!(experiment.budget, budget);
    assert_eq!(experiment.runs.len(), value.queries.len() * Arm::ALL.len());

    for run in &experiment.runs {
        assert!(
            run.ranked.len() <= budget.candidates,
            "{} / {}: {} candidates exceed the budget",
            run.query_id,
            run.arm,
            run.ranked.len()
        );
        assert!(
            run.context_tokens <= budget.tokens,
            "{} / {}: {} context tokens exceed the budget",
            run.query_id,
            run.arm,
            run.context_tokens
        );
        assert_eq!(run.ranked.len(), run.final_candidates);
        assert!((0.0..=1.0).contains(&run.reciprocal_rank));
    }

    // Every arm gets the same context size on the same query because every
    // seeded document has the same token cost and all arms pack to the cap.
    for item in &value.queries {
        let per_arm: Vec<usize> = Arm::ALL
            .iter()
            .map(|&arm| run_for(&experiment, &item.id, arm).context_tokens)
            .collect();
        assert!(
            per_arm.iter().all(|tokens| *tokens == per_arm[0]),
            "{} had unequal context tokens across arms: {per_arm:?}",
            item.id
        );
        assert_eq!(
            per_arm[0],
            budget.candidates * retrieval_experiments::TOKEN_COST
        );
        for arm in Arm::ALL {
            let run = run_for(&experiment, &item.id, arm);
            assert_eq!(run.final_candidates, budget.candidates);
            assert_eq!(run.task, item.kind);
        }
    }
}

#[test]
fn fusion_ranking_is_deterministic_and_calibration_is_offline_only() {
    let lexical: Vec<String> = ["p", "q", "r"].iter().map(|s| (*s).to_string()).collect();
    let graph: Vec<String> = ["q", "s", "p"].iter().map(|s| (*s).to_string()).collect();

    // RRF rewards an id that appears in both arms: q and p outrank s and r.
    let rrf = rrf_fuse(&[lexical.clone(), graph.clone()], RRF_K);
    assert_eq!(ids(&rrf), ["q", "p", "s", "r"]);
    assert!(
        rrf[0].1 > rrf[2].1,
        "a multi-arm hit must outrank a single-arm hit"
    );
    assert_eq!(
        rrf,
        rrf_fuse(&[lexical.clone(), graph.clone()], RRF_K),
        "RRF must be deterministic"
    );

    // A calibrated (unequal-weight) fusion uses 1/rank, so it can reorder the
    // RRF result toward its stronger arm; it is equally reproducible.
    let weights = FusionWeights {
        lexical: 0.8,
        graph: 0.2,
    };
    let fused = weighted_fuse(&[lexical.clone(), graph.clone()], &weights);
    assert_eq!(ids(&fused), ["p", "q", "r", "s"]);
    assert_eq!(fused, weighted_fuse(&[lexical, graph], &weights));

    // The default is the neutral 50/50 starting point.
    assert_eq!(
        FusionWeights::default(),
        FusionWeights {
            lexical: 0.5,
            graph: 0.5,
        }
    );

    // Calibration is normalized and favours the stronger lexical mode.
    let calibrated = calibrate_weights(1.0, 0.5);
    assert!((calibrated.lexical + calibrated.graph - 1.0).abs() < 1e-12);
    assert!(calibrated.lexical > calibrated.graph);

    // Calibration reads tuning data only: deleting or rewriting held-out
    // queries cannot move the weights used by the fusion arm.
    let full = run_experiment(&fixture(), &budget());
    let mut changed = fixture();
    for item in &mut changed.queries {
        if item.split == Split::HeldOut {
            item.text = "totally unrelated words".to_string();
            item.relevant.clear();
        }
    }
    let replayed = run_experiment(&changed, &budget());
    assert_eq!(full.weights, replayed.weights);
    assert_eq!(full.fixture_hash, fixture_hash(&fixture()));
}

#[test]
fn stage_accounting_sums_to_the_recorded_totals() {
    let experiment = run_experiment(&fixture(), &budget());
    assert!(!experiment.runs.is_empty());

    for run in &experiment.runs {
        let recomputed: usize = run.stages.iter().map(|item| item.tokens).sum();
        let last = run.stages.last().expect("every run has stages");
        assert_eq!(
            run.total_tokens, recomputed,
            "{} / {}",
            run.query_id, run.arm
        );
        assert_eq!(run.context_tokens, last.tokens);
        assert_eq!(run.final_candidates, last.emitted);
        assert_eq!(
            last.stage,
            Stage::Budget,
            "the final stage must be the budget pack"
        );
        assert!(run.is_consistent(), "{} / {}", run.query_id, run.arm);
    }

    // Fusion arms account for every stage: lexical, graph, fusion and pack.
    let fusion = run_for(&experiment, "qt2", Arm::WeightedFusion);
    let stages: Vec<Stage> = fusion.stages.iter().map(|item| item.stage).collect();
    assert_eq!(
        stages,
        [Stage::Lexical, Stage::Graph, Stage::Fusion, Stage::Budget]
    );
    let lexical = run_for(&experiment, "qt2", Arm::Bm25);
    assert_eq!(
        lexical.stages.len(),
        2,
        "the lexical control has no graph/fusion stages"
    );
}

#[test]
fn decision_names_a_tuning_best_but_refuses_a_win_without_a_held_out_split() {
    let tuning_only = run_experiment(&fixture_no_held_out(), &budget());
    let report: DecisionReport = decide(&tuning_only);

    assert!(report.held_out_scores.is_empty());
    assert!(report.held_out_best.is_none());
    match &report.verdict {
        DecisionVerdict::RefusedNoHeldOut { best_on_tuning } => {
            assert_eq!(*best_on_tuning, report.tuning_best);
        }
        other => panic!("expected a held-out refusal, got {other:?}"),
    }
    assert!(!report.verdict.claims_win());
    assert!(
        !report.verdict.claims_win() && report.summary.contains("refusing to claim a win"),
        "the report must say plainly why it will not claim a win"
    );

    // With a held-out split the same pipeline may confirm a win, and it
    // always names a best arm from tuning.
    let full: DecisionReport = decide(&run_experiment(&fixture(), &budget()));
    assert!(!matches!(
        &full.verdict,
        DecisionVerdict::RefusedNoHeldOut { .. }
    ));
    assert!(full.held_out_best.is_some());
    assert!(matches!(full.verdict, DecisionVerdict::Win { .. }));
    assert!(full.verdict.claims_win());
    // Every reported field is populated, including the tuning score table.
    assert_eq!(full.fixture_id, "mn-retrieval-experiments/1");
    assert_eq!(full.fixture_hash.len(), 16);
    assert_eq!(full.budget, budget());
    assert_eq!(full.tuning_scores.len(), Arm::ALL.len());
    assert!(full.summary.contains("held-out best"));
}

#[test]
fn routing_picks_a_mode_by_task_type_without_an_online_feedback_loop() {
    let value = fixture();
    let policy: RoutingPolicy = learn_policy(&value, &budget());

    assert_eq!(route(&policy, TaskKind::Lexical), RouteChoice::Lexical);
    assert_eq!(route(&policy, TaskKind::Graph), RouteChoice::Graph);
    assert_eq!(route(&policy, TaskKind::Dense), RouteChoice::Dense);

    // Independently recompute the argmax from mode-level replay scores and
    // check the frozen policy agrees, using the same tie rule (prefer the
    // mode named for the task) that the learner documents.
    for kind in TaskKind::ALL {
        let queries: Vec<&Query> = value
            .queries
            .iter()
            .filter(|item| item.split == Split::Tuning && item.kind == kind)
            .collect();
        let mut best = RouteChoice::Lexical;
        let mut best_score = f64::NEG_INFINITY;
        for choice in RouteChoice::ALL {
            let mean: f64 = queries
                .iter()
                .map(|item| mode_recall(&value, &budget(), item, choice))
                .sum::<f64>()
                / queries.len() as f64;
            let preferred = choice
                == match kind {
                    TaskKind::Lexical => RouteChoice::Lexical,
                    TaskKind::Graph => RouteChoice::Graph,
                    TaskKind::Dense => RouteChoice::Dense,
                };
            if mean > best_score + 1e-9 || ((mean - best_score).abs() <= 1e-9 && preferred) {
                best = choice;
                best_score = mean;
            }
        }
        assert_eq!(route(&policy, kind), best, "policy disagrees for {kind}");
    }

    // No online feedback: editing held-out queries cannot change the policy,
    // and repeated lookups are identical because the policy has no update API.
    let mut changed = fixture();
    for item in &mut changed.queries {
        if item.split == Split::HeldOut {
            item.text = "noise that must not train routing".to_string();
            item.relevant.clear();
        }
    }
    assert_eq!(learn_policy(&changed, &budget()), policy);
    assert_eq!(
        route(&policy, TaskKind::Graph),
        route(&policy, TaskKind::Graph)
    );
}
