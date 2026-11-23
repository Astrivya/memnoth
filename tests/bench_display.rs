//! Integration tests for the honest benchmark-number rendering helpers.
//!
//! The module is included by source path (the same pattern `bench_stats.rs`
//! uses) so these tests grade the exact code the CLI will dispatch to once
//! `src/bench_display.rs` is declared in `lib.rs`.

#[path = "../src/bench_display.rs"]
mod bench_display;

use bench_display::{Observation, comparison, is_tiered, reliability_ceiling};

fn observation() -> Observation {
    Observation {
        solved: 47,
        total: 500,
        ci: Some((7.1, 12.1)),
        ci_method: "bootstrap percentile, 95%",
        dataset: "LongMemEval-S v1.2 (500q)".to_owned(),
        scorer: "judge-v3".to_owned(),
        provenance: "self-measured on local harness".to_owned(),
        seed: Some(0),
        observed_at: "2026-11-23".to_owned(),
        metric: "strict exact match, mean over 1 run".to_owned(),
    }
}

#[test]
fn missing_or_invalid_field_refuses_to_render() {
    let base = observation();
    assert!(
        !base.render().starts_with("REFUSED"),
        "complete figure must render"
    );

    let mut missing_dataset = base.clone();
    missing_dataset.dataset.clear();
    let refusal = missing_dataset.render();
    assert!(refusal.starts_with("REFUSED"), "{refusal}");
    assert!(refusal.contains("dataset"), "{refusal}");

    let mut missing_ci = base.clone();
    missing_ci.ci = None;
    assert!(missing_ci.render().starts_with("REFUSED"));

    let mut missing_scorer = base.clone();
    missing_scorer.scorer.clear();
    assert!(missing_scorer.render().starts_with("REFUSED"));

    let mut missing_provenance = base.clone();
    missing_provenance.provenance.clear();
    assert!(missing_provenance.render().starts_with("REFUSED"));

    let mut missing_seed = base.clone();
    missing_seed.seed = None;
    assert!(missing_seed.render().starts_with("REFUSED"));

    let mut missing_date = base.clone();
    missing_date.observed_at.clear();
    assert!(missing_date.render().starts_with("REFUSED"));

    let mut missing_metric = base.clone();
    missing_metric.metric.clear();
    assert!(missing_metric.render().starts_with("REFUSED"));

    let mut empty_ci_method = base.clone();
    empty_ci_method.ci_method = "";
    assert!(empty_ci_method.render().starts_with("REFUSED"));

    let mut zero_total = base.clone();
    zero_total.total = 0;
    assert!(zero_total.render().starts_with("REFUSED"));

    let mut impossible = base.clone();
    impossible.solved = impossible.total + 1;
    assert!(impossible.render().starts_with("REFUSED"));
}

#[test]
fn render_carries_denominator_ci_dataset_date_and_provenance() {
    let out = observation().render();
    for needle in [
        "47/500",
        "7.1",
        "12.1",
        "bootstrap percentile",
        "LongMemEval-S v1.2 (500q)",
        "judge-v3",
        "2026-11-23",
        "SELF-MEASURED",
        "self-measured on local harness",
        "seed 0",
        "strict exact match",
    ] {
        assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
    }

    assert!(
        out.lines().count() >= 7,
        "expected a multi-line label:\n{out}"
    );
    assert!(
        !out.contains("100%"),
        "a clean run is not 100% reliable:\n{out}"
    );
    assert!(!out.contains("#1"), "no ordinal ranks:\n{out}");
    for line in out.lines() {
        assert!(
            !line.trim().ends_with('%'),
            "a bare percentage line is never publishable: {line}"
        );
    }
}

#[test]
fn overlapping_ci_is_one_tier_not_an_ordinal_rank() {
    let a = observation();
    let mut b = a.clone();
    b.solved = 48;
    b.ci = Some((8.0, 13.0)); // overlaps [7.1, 12.1]

    assert!(is_tiered(&a, &b));
    let tied = comparison(Some(&a), &b).expect("baseline supplied");
    assert!(tied.contains("one tier"), "{tied}");
    assert!(
        !tied.contains("#1") && !tied.contains("#2") && !tied.contains("#3"),
        "{tied}"
    );

    let mut c = a.clone();
    c.ci = Some((20.0, 25.0)); // disjoint from [7.1, 12.1]
    assert!(!is_tiered(&a, &c));
    let distinct = comparison(Some(&a), &c).expect("baseline supplied");
    assert!(distinct.contains("do not overlap"), "{distinct}");

    let mut no_ci = a.clone();
    no_ci.ci = None;
    assert!(!is_tiered(&a, &no_ci));
}

#[test]
fn missing_baseline_produces_no_comparison_line() {
    let candidate = observation();
    assert!(comparison(None, &candidate).is_none());

    // An incomplete baseline is "missing" too: omit the line rather than
    // fabricate a delta such as "Δ +0%".
    let mut broken = candidate.clone();
    broken.dataset.clear();
    assert!(comparison(Some(&broken), &candidate).is_none());
}

#[test]
fn reliability_ceiling_uses_rule_of_three() {
    let ceiling = reliability_ceiling(10);
    assert!((ceiling - 0.30).abs() < 1e-12, "{ceiling}");
    assert!((reliability_ceiling(100) - 0.03).abs() < 1e-12);
    assert_eq!(reliability_ceiling(0), 1.0);

    // A flawless 10/10 result still only supports a 30% error-rate bound.
    let mut perfect = observation();
    perfect.solved = 10;
    perfect.total = 10;
    perfect.ci = Some((0.7, 1.0));
    let out = perfect.render();
    assert!(out.contains("3/10"), "{out}");
    assert!(out.contains("0 errors in 10 trials"), "{out}");
}
