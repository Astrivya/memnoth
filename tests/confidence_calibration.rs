//! ALG09 integration tests for empirical confidence calibration.
//!
//! The module is included by source path so these tests grade the same code the
//! CLI will dispatch to once `src/confidence_calibration.rs` is declared in
//! `lib.rs`. No model, no network — only labelled fixtures and arithmetic.

#[path = "../src/confidence_calibration.rs"]
mod confidence_calibration;

use std::collections::BTreeSet;

use confidence_calibration::{
    AbstentionReason, AbstentionThreshold, CalibrationStatus, LabelledObservation, Provenance,
    QueryClass, Refusal, StratumKey, StratumOutcome, calibrate, calibrate_with,
};

fn key(language: &str, class: QueryClass) -> StratumKey {
    StratumKey::new(language, class)
}

fn prov(by: &str, at: &str, fixture: &str) -> Provenance {
    Provenance::new(by, at, fixture)
}

fn labelled_set(
    prefix: &str,
    stratum: StratumKey,
    n: usize,
    correct: usize,
    predicted: f64,
    by: &str,
    at: &str,
) -> Vec<LabelledObservation> {
    assert!(correct <= n);
    (0..n)
        .map(|i| LabelledObservation {
            id: format!("{prefix}-{i}"),
            stratum: stratum.clone(),
            predicted_confidence: predicted,
            outcome: Some(i < correct),
            provenance: prov(by, at, "labelled-fixture-v1"),
        })
        .collect()
}

#[test]
fn calibration_from_a_labelled_set() {
    // Two strata with genuinely different correctness rates in the labelled
    // fixtures. Each has 40 labels, comfortably over the default floor of 20.
    let exact = key("rust", QueryClass::Exact);
    let lexical = key("rust", QueryClass::Lexical);
    let mut labels = labelled_set("exact", exact.clone(), 40, 38, 0.92, "verifier", "t1");
    labels.extend(labelled_set(
        "lex",
        lexical.clone(),
        40,
        20,
        0.70,
        "verifier",
        "t1",
    ));

    let report = calibrate(&labels).expect("enough labels to calibrate");
    assert_eq!(report.status, CalibrationStatus::Full);
    assert_eq!(report.label, "full");
    assert_eq!(report.calibrated_strata, 2);
    assert_eq!(report.abstained_strata, 0);
    assert_eq!(report.labelled_count, 80);
    assert_eq!(report.excluded_unlabelled, 0);
    assert!(report.method.contains("empirical"));
    assert!(report.summary.contains("full"));

    let p_exact = report.probability_for(&exact).expect("exact calibrated");
    let p_lex = report
        .probability_for(&lexical)
        .expect("lexical calibrated");
    // Probability is the observed correctness rate, not the self-reported one.
    assert!((p_exact.value - 0.95).abs() < 1e-12, "{}", p_exact.value);
    assert!((p_lex.value - 0.50).abs() < 1e-12, "{}", p_lex.value);
    assert!(p_exact.value > p_lex.value, "strata must not be pooled");
    assert_eq!(p_exact.correct, 38);
    assert_eq!(p_exact.sample_size, 40);
    assert!(p_exact.lower <= p_exact.value && p_exact.value <= p_exact.upper);
    assert!(p_lex.lower <= p_lex.value && p_lex.value <= p_lex.upper);
    assert!(p_exact.upper - p_exact.lower < 0.5);

    // Self-reported confidence is carried separately for the calibration gap;
    // it is a different field from the empirical probability.
    if let StratumOutcome::Calibrated {
        mean_predicted_confidence,
        ..
    } = &report.strata[0].outcome
    {
        assert!((mean_predicted_confidence - 0.92).abs() < 1e-12);
    } else {
        panic!("exact stratum should be Calibrated");
    }
}

#[test]
fn insufficient_data_refuses_to_claim_calibration() {
    // Empty input.
    match calibrate(&[]) {
        Err(Refusal::InsufficientData { detail }) => {
            assert!(detail.contains("no labelled observations"))
        }
        other => panic!("expected InsufficientData for empty input, got {other:?}"),
    }

    // Three labels, below the default floor of twenty.
    let exact = key("rust", QueryClass::Exact);
    let few = labelled_set("few", exact, 3, 3, 0.9, "verifier", "t1");
    match calibrate(&few) {
        Err(Refusal::InsufficientData { detail }) => {
            assert!(detail.contains("0 of 1 strata"), "{detail}");
            assert!(detail.contains("3 labelled"), "{detail}");
        }
        other => panic!("expected InsufficientData for 3 labels, got {other:?}"),
    }

    // A pile of unlabelled rows is not data: no probability may be invented.
    let unlabelled: Vec<LabelledObservation> = (0..50)
        .map(|i| LabelledObservation {
            id: format!("u-{i}"),
            stratum: key("python", QueryClass::Likely),
            predicted_confidence: 0.99,
            outcome: None,
            provenance: prov("importer", "t2", "unlabelled.csv"),
        })
        .collect();
    match calibrate(&unlabelled) {
        Err(Refusal::InsufficientData { detail }) => {
            assert!(detail.contains("fabricated"), "{detail}");
        }
        other => panic!("expected InsufficientData for unlabelled rows, got {other:?}"),
    }
}

#[test]
fn provenance_is_not_probability() {
    let exact = key("rust", QueryClass::Exact);
    let mut first = labelled_set("a", exact.clone(), 40, 36, 0.9, "alice", "2026-01-01");
    let mut second = labelled_set("b", exact.clone(), 40, 36, 0.9, "bob", "2026-11-21");

    // WHO and WHEN recorded the labels changes nothing about the probability.
    // Even the fixture ids differ; only outcomes determine the number.
    for (i, obs) in second.iter_mut().enumerate() {
        obs.provenance = prov("carol", "2027-05-05", &format!("other-fixture-{i}"));
    }
    let r1 = calibrate(&first).expect("first set calibrates");
    let r2 = calibrate(&second).expect("second set calibrates");
    let p1 = r1.probability_for(&exact).unwrap();
    let p2 = r2.probability_for(&exact).unwrap();
    assert_eq!(p1, p2, "provenance must not move a probability");

    // Provenance is reported, but as its own field, separate from strata.
    assert!(!r1.provenance.is_empty());
    assert_eq!(r1.provenance[0].recorded_by, "alice");
    assert_eq!(r2.provenance[0].recorded_by, "carol");
    let value = serde_json::to_value(&r1).unwrap();
    assert!(value.get("provenance").is_some());
    assert!(value.get("strata").is_some());
    assert!(value["provenance"][0].get("value").is_none());
    // StratumOutcome is an externally tagged enum: the calibrated variant
    // carries the probability, and provenance is a separate top-level field.
    assert!(
        value["strata"][0]["outcome"]["Calibrated"]["probability"]
            .get("value")
            .is_some()
    );

    // Flip every outcome: the probability changes, the provenance does not.
    for obs in &mut first {
        obs.outcome = Some(!obs.outcome.unwrap());
    }
    let flipped = calibrate(&first).unwrap();
    let pf = flipped.probability_for(&exact).unwrap();
    assert!((pf.value - 0.1).abs() < 1e-12, "{}", pf.value);
    assert_eq!(flipped.provenance, r1.provenance);

    // Self-reported confidence is not the probability either: a set with the
    // same outcomes but different claimed confidences yields the same number.
    for obs in &mut second {
        obs.predicted_confidence = 0.1;
    }
    let r3 = calibrate(&second).unwrap();
    assert_eq!(r3.probability_for(&exact).unwrap().value, p2.value);
}

#[test]
fn abstention_threshold_is_enforced() {
    let exact = key("rust", QueryClass::Exact);
    let few = labelled_set("few", exact.clone(), 5, 2, 0.9, "verifier", "t1");

    // Default policy: below the minimum label count.
    let report = calibrate_with(&few, &AbstentionThreshold::default());
    assert!(matches!(report, Err(Refusal::InsufficientData { .. })));

    // Inspect the stratum decision directly by adding one calibrated stratum
    // so the call returns a report instead of refusing outright.
    let mut mixed = few.clone();
    mixed.extend(labelled_set(
        "good",
        key("rust", QueryClass::Likely),
        40,
        36,
        0.9,
        "verifier",
        "t1",
    ));
    let report = calibrate(&mixed).expect("one stratum still calibrates");
    let abstain = report.strata.iter().find(|row| row.key == exact).unwrap();
    match &abstain.outcome {
        StratumOutcome::Abstain {
            reason: AbstentionReason::InsufficientLabels { have, need },
            sample_size,
        } => {
            assert_eq!(*have, 5);
            assert_eq!(*need, 20);
            assert_eq!(*sample_size, 5, "real denominator must be reported");
        }
        other => panic!("expected InsufficientLabels abstention, got {other:?}"),
    }
    assert!(report.probability_for(&exact).is_none());

    // A tight width threshold abstains on imprecision even with enough labels.
    let tight = AbstentionThreshold {
        min_labels: 2,
        max_interval_width: 0.1,
    };
    let precise = labelled_set("wide", exact.clone(), 5, 2, 0.9, "verifier", "t1");
    let report = calibrate_with(&precise, &tight).unwrap_err();
    assert!(matches!(report, Refusal::InsufficientData { .. }));
    let mut with_good = precise.clone();
    with_good.extend(labelled_set(
        "good2",
        key("go", QueryClass::Exact),
        200,
        196,
        0.98,
        "verifier",
        "t1",
    ));
    let report = calibrate_with(&with_good, &tight).unwrap();
    let wide = report.strata.iter().find(|row| row.key == exact).unwrap();
    match &wide.outcome {
        StratumOutcome::Abstain {
            reason: AbstentionReason::Imprecise { width, max_width },
            sample_size,
        } => {
            assert!(*width > 0.1);
            assert_eq!(*max_width, 0.1);
            assert_eq!(*sample_size, 5);
        }
        other => panic!("expected Imprecise abstention, got {other:?}"),
    }
    // The large, precise stratum clears the same threshold.
    assert!(
        report
            .probability_for(&key("go", QueryClass::Exact))
            .is_some()
    );
}

#[test]
fn partial_evidence_degrades_honestly() {
    let good = key("rust", QueryClass::Exact);
    let weak = key("python", QueryClass::Lexical);
    let mut labels = labelled_set("good", good.clone(), 40, 38, 0.95, "verifier", "t1");
    // Only five labels in this stratum: it cannot be calibrated.
    labels.extend(labelled_set(
        "weak",
        weak.clone(),
        5,
        1,
        0.6,
        "verifier",
        "t1",
    ));
    // One unlabelled row with a very high claimed confidence: excluded.
    labels.push(LabelledObservation {
        id: "unlabelled-1".to_string(),
        stratum: weak.clone(),
        predicted_confidence: 0.99,
        outcome: None,
        provenance: prov("importer", "t2", "unlabelled.csv"),
    });

    let report = calibrate(&labels).expect("one stratum still calibrates");
    assert_eq!(report.status, CalibrationStatus::Degraded);
    assert!(report.label.contains("degraded"));
    assert!(report.summary.contains("degraded"));
    assert_eq!(report.calibrated_strata, 1);
    assert_eq!(report.abstained_strata, 1);
    assert_eq!(report.excluded_unlabelled, 1);
    assert!(report.probability_for(&good).is_some());
    assert!(
        report.probability_for(&weak).is_none(),
        "no fabricated probability for an under-supported stratum"
    );
    assert!(
        report
            .provenance
            .iter()
            .all(|p| p.fixture != "unlabelled.csv")
    );

    // The abstained stratum still appears with its real denominator.
    let weak_row = report.strata.iter().find(|row| row.key == weak).unwrap();
    assert!(matches!(
        &weak_row.outcome,
        StratumOutcome::Abstain { sample_size: 5, .. }
    ));
}

#[test]
fn unlabelled_rows_are_excluded_not_fabricated() {
    let exact = key("rust", QueryClass::Exact);
    let mut labels = labelled_set("good", exact.clone(), 40, 36, 0.9, "verifier", "t1");
    for i in 0..10 {
        labels.push(LabelledObservation {
            id: format!("no-truth-{i}"),
            stratum: exact.clone(),
            predicted_confidence: 0.99,
            outcome: None,
            provenance: prov("importer", "t2", "unlabelled.csv"),
        });
    }

    let report = calibrate(&labels).unwrap();
    assert_eq!(report.excluded_unlabelled, 10);
    let p = report.probability_for(&exact).unwrap();
    assert_eq!(p.sample_size, 40, "unlabelled rows are not counted as data");
    assert_eq!(p.correct, 36);
    assert!((p.value - 0.9).abs() < 1e-12);
}

#[test]
fn malformed_input_and_policy_are_refused() {
    let exact = key("rust", QueryClass::Exact);
    let mut dup = labelled_set("d", exact.clone(), 40, 36, 0.9, "v", "t");
    dup[1].id = dup[0].id.clone();
    assert!(matches!(calibrate(&dup), Err(Refusal::Invalid { .. })));

    let mut bad_lang = labelled_set("b", exact.clone(), 40, 36, 0.9, "v", "t");
    bad_lang[0].stratum.language.clear();
    assert!(matches!(calibrate(&bad_lang), Err(Refusal::Invalid { .. })));

    let mut bad_conf = labelled_set("c", exact.clone(), 40, 36, 0.9, "v", "t");
    bad_conf[0].predicted_confidence = f64::NAN;
    assert!(matches!(calibrate(&bad_conf), Err(Refusal::Invalid { .. })));

    let labels = labelled_set("ok", exact, 40, 36, 0.9, "v", "t");
    assert!(matches!(
        calibrate_with(
            &labels,
            &AbstentionThreshold {
                min_labels: 1,
                max_interval_width: 0.5,
            }
        ),
        Err(Refusal::Invalid { .. })
    ));
    assert!(matches!(
        calibrate_with(
            &labels,
            &AbstentionThreshold {
                min_labels: 20,
                max_interval_width: 0.0,
            }
        ),
        Err(Refusal::Invalid { .. })
    ));
}

#[test]
fn provenance_records_are_deduplicated_and_deterministic() {
    let exact = key("rust", QueryClass::Exact);
    // All forty rows share one identical provenance record; the report must
    // present it once and present provenance as a separate concern.
    let labels = labelled_set("same", exact, 40, 36, 0.9, "verifier", "t1");
    let a = calibrate(&labels).unwrap();
    let b = calibrate(&labels).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.provenance.len(), 1);
    let recorded: BTreeSet<_> = a.provenance.iter().map(|p| p.recorded_by.clone()).collect();
    assert_eq!(recorded.len(), 1);
}
