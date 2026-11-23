//! Source-level contract tests for the immutable eval manifest guard in
//! `src/eval.rs`.
//!
//! The guard lives in a `#![cfg(test)]` module whose surrounding code reaches
//! private `crate::` items, so an integration crate cannot link it directly.
//! The behavioural tests (complete accepted, each field rejected, secret
//! redacted) run inline next to the validator; these tests pin its *contract*
//! at the source so the required-field set cannot quietly shrink and the
//! report writer cannot stop routing publishable results through the guard.

const EVAL_RS: &str = include_str!("../src/eval.rs");

/// The slice between two markers, with a clear message if either is missing.
fn between<'a>(src: &'a str, start: &str, end: &str) -> &'a str {
    let after = src
        .find(start)
        .unwrap_or_else(|| panic!("marker not found in eval.rs: {start}"));
    let tail = &src[after + start.len()..];
    let stop = tail
        .find(end)
        .unwrap_or_else(|| panic!("end marker {end:?} missing after {start}"));
    &tail[..stop]
}

/// The body of `MANIFEST_REQUIRED_FIELDS`, the single source of truth for what
/// a publishable result must carry.
fn required_fields_block() -> &'static str {
    between(
        EVAL_RS,
        "pub(crate) const MANIFEST_REQUIRED_FIELDS: &[&str] = &[",
        "];",
    )
}

/// Every field the release plan says a real eval run must carry.
const MANDATED_FIELDS: &[&str] = &[
    "dataset_sha256",
    "model_id",
    "model_version",
    "temperature",
    "judge_id",
    "judge_version",
    "reader_id",
    "status",
    "sample_count",
    "trials",
    "per_task",
    "failures",
    "observed_date",
];

#[test]
fn manifest_declares_every_mandated_field() {
    let block = required_fields_block();
    for field in MANDATED_FIELDS {
        assert!(
            block.contains(&format!("\"{field}\"")),
            "`{field}` is missing from MANIFEST_REQUIRED_FIELDS: {block}"
        );
    }
    // `full`/`subset` status is expressed by the `status` field's value domain,
    // which the validator pins to exactly those two values.
    let violations = between(EVAL_RS, "fn manifest_violations(", "/// The gate:");
    assert!(violations.contains("\"full\" | \"subset\""));
}

#[test]
fn report_writer_refuses_an_incomplete_manifest() {
    let writer = between(
        EVAL_RS,
        "fn record_report(kind: &str",
        "/// One judged answer.",
    );
    assert!(
        writer.contains("guard_publishable_manifest"),
        "the report writer must route publishable results through the manifest guard"
    );
    assert!(
        writer.contains("guard_publishable_manifest(manifest)?"),
        "the guard's verdict must refuse emission, not merely be consulted"
    );
}

#[test]
fn builder_redacts_secrets_through_provenance_value() {
    let builder = between(EVAL_RS, "pub(crate) fn eval_manifest(", "/// Every reason");
    assert!(
        builder.contains("provenance_value"),
        "config must be captured through the existing redactor, not copied raw"
    );
}

#[test]
fn temperature_is_checked_and_not_just_present() {
    let validator = between(EVAL_RS, "fn manifest_violations(", "/// The gate:");
    assert!(
        validator.contains("temperature") && validator.contains("0.0"),
        "presence alone is not enough: the validator must require temperature 0"
    );
}
