//! EV-01 boundary tests for the one hidden `/evidence` verb.
//!
//! These are pure: the registry projection, the parser and the envelope
//! composer are all in-process. No PTY, no real vault, no network.

use memnoth::cli::Verb;
use memnoth::tui::app::{Parsed, parse_line};
use memnoth::tui::commands::evidence::{EvidenceInput, SCHEMA, compose};
use serde_json::json;

#[test]
fn evidence_is_one_hidden_registry_row_on_the_memory_surface() {
    let registry = memnoth_command::build_p0_registry().build();
    let ids: Vec<&str> = registry.all().map(|spec| spec.id.as_str()).collect();
    let spec = registry
        .all()
        .find(|spec| spec.id == "evidence")
        .unwrap_or_else(|| panic!("evidence registry row missing; ids: {ids:?}"));
    assert_eq!(spec.cli_path, vec!["evidence".to_string()]);
    assert!(spec.slash.hidden, "the verb is hidden from --help");
    assert_eq!(spec.screen(), "memory");
    assert_eq!(spec.slash.palette.as_deref(), Some("evidence <subject>"));
    assert!(spec.related.iter().any(|id| id == "why"));
    assert!(spec.related.iter().any(|id| id == "lineage"));
    assert!(spec.related.iter().any(|id| id == "impact"));
}

#[test]
fn clap_parses_the_same_single_verb() {
    use clap::CommandFactory;
    let names: Vec<String> = memnoth::cli::Cli::command()
        .get_subcommands()
        .map(|verb| verb.get_name().to_string())
        .collect();
    assert_eq!(names.iter().filter(|name| *name == "evidence").count(), 1);
}

#[test]
fn slash_line_parses_to_the_verb_not_a_second_list() {
    match parse_line("/evidence rate_limit") {
        Parsed::Run(Verb::Evidence(args)) => assert_eq!(args.subject, "rate_limit"),
        other => panic!("expected the evidence verb, got {other:?}"),
    }
    assert!(matches!(
        parse_line("/evidence"),
        Parsed::Usage(message) if message.contains("/evidence <subject>")
    ));
}

#[test]
fn envelope_has_the_five_pages_in_chain_order() {
    let document = compose(EvidenceInput::default());
    assert_eq!(document["schema"], json!(SCHEMA));
    let pages = document["pages"].as_array().expect("pages");
    let ids: Vec<&str> = pages
        .iter()
        .map(|page| page["id"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(ids, ["belief", "author", "approval", "commit", "impact"]);
    assert_eq!(document["links"].as_array().map(Vec::len), Some(4));
    assert_eq!(document["fence"].as_array().map(Vec::len), Some(10));
}

#[test]
fn only_the_four_honest_states_are_used() {
    let document = compose(EvidenceInput {
        belief: Some(json!({"entities": [{"facts": [
            {"content": "a", "provenance": {"user": "B"}, "recorded_at": 1i64}
        ]}]})),
        author: Some(
            json!({"edges": [{"source": "a", "kind": "k", "target": "b", "by": "audit"}]}),
        ),
        receipt: Some(json!({
            "authority": {"approvals": [{"tool": "edit", "subject": "x", "approved_by": "B", "at_ms": 1i64}]},
            "change": {"commit": "abc"}
        })),
        verification: Some(json!({
            "ok": true, "commit": "abc", "bound_commit": "abc",
            "checks": [{"name": "schema", "ok": true, "detail": "ok"}]
        })),
        impact: Some(json!({"precision": "lexical", "exact": 2, "likely": 3})),
        ..Default::default()
    });
    for page in document["pages"].as_array().expect("pages") {
        let state = page["state"].as_str().expect("state");
        assert!(
            ["measured", "partial", "not_recorded", "stale"].contains(&state),
            "{state}"
        );
    }
    for link in document["links"].as_array().expect("links") {
        let state = link["state"].as_str().expect("state");
        assert!(
            ["measured", "partial", "not_recorded", "stale"].contains(&state),
            "{state}"
        );
    }
}

#[test]
fn session_only_approvals_are_never_receipt_sourced() {
    let document = compose(EvidenceInput {
        session_approvals: 3,
        ..Default::default()
    });
    let approval = &document["pages"][2];
    assert_eq!(approval["state"], json!("not_recorded"));
    for row in approval["rows"].as_array().expect("rows") {
        assert_eq!(row["receipt_sourced"], json!(false), "{row}");
        assert_eq!(row["state"], json!("not_recorded"));
    }
}

#[test]
fn a_missing_commit_is_stale() {
    let document = compose(EvidenceInput {
        receipt: Some(json!({"change": {"commit": "abc"}})),
        verification: Some(json!({
            "ok": false, "commit": "abc", "bound_commit": "abc",
            "checks": [
                {"name": "commit", "ok": true, "detail": "bound"},
                {"name": "commit-exists", "ok": false, "detail": "not in repository"}
            ]
        })),
        ..Default::default()
    });
    assert_eq!(document["pages"][3]["state"], json!("stale"));
}

#[test]
fn a_lexical_blast_radius_is_never_measured() {
    let document = compose(EvidenceInput {
        impact: Some(json!({
            "precision": "lexical", "exact": 9, "likely": 12,
            "impacted_tests": null,
            "impacted_tests_reason": "test index did not answer"
        })),
        ..Default::default()
    });
    assert_eq!(document["pages"][4]["state"], json!("partial"));
}
