//! Fixture-driven fault injection for `replay_events`.
//!
//! Each fixture under `tests/fixtures/` is a complete NDJSON log. The tests
//! here assert the exact issue/open-item category the log must produce, that
//! every good line survives verbatim and in log order, that replay never
//! panics, and that `is_complete()` is true only for the clean fixtures.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use memnoth_agent::delegate::event_replay::{OpenItem, ReplayIssue, ReplayResult, replay_events};

const CLEAN: &str = include_str!("fixtures/agent_events_clean.jsonl");
const TRUNCATED_TAIL: &str = include_str!("fixtures/agent_events_truncated_tail.jsonl");
const INTERLEAVED: &str = include_str!("fixtures/agent_events_interleaved_runs.jsonl");
const DUPLICATE_IDS: &str = include_str!("fixtures/agent_events_duplicate_ids.jsonl");
const ORPHAN_PARENT: &str = include_str!("fixtures/agent_events_orphan_parent.jsonl");
const LATE_RESULT: &str = include_str!("fixtures/agent_events_late_tool_result.jsonl");
const UNKNOWN_VERSION: &str = include_str!("fixtures/agent_events_unknown_version.jsonl");
const INTERRUPTED_SAVE: &str = include_str!("fixtures/agent_events_interrupted_save.jsonl");
const MIXED_V0_V1: &str = include_str!("fixtures/agent_events_mixed_v0_v1.jsonl");
const CRLF_BOM: &str = include_str!("fixtures/agent_events_crlf_bom.jsonl");
const BULK_10000: &str = include_str!("fixtures/agent_events_bulk_10000.jsonl");

/// Replay a fixture, failing explicitly if the parser unwinds on bad input.
fn replay_without_panic(input: &str) -> ReplayResult {
    match catch_unwind(AssertUnwindSafe(|| replay_events(input))) {
        Ok(result) => result,
        Err(_) => panic!("replay_events panicked on a fixture"),
    }
}

/// Every good line is preserved as `(id, seq, kind)`, in source order.
fn assert_events_in_order(result: &ReplayResult, expected: &[(&str, u64, &str)]) {
    let actual: Vec<(&str, u64, &str)> = result
        .events
        .iter()
        .map(|event| (event.id.as_str(), event.seq, event.event.as_str()))
        .collect();
    assert_eq!(actual.as_slice(), expected);
}

#[test]
fn clean_control_is_complete() {
    let result = replay_without_panic(CLEAN);

    assert_events_in_order(
        &result,
        &[
            ("root:1", 1, "run.spawn"),
            ("root:2", 2, "run.start"),
            ("root:3", 3, "tool.use"),
            ("root:4", 4, "run.done"),
            ("child:1", 1, "run.spawn"),
            ("child:2", 2, "run.start"),
            ("child:3", 3, "run.done"),
        ],
    );
    assert_eq!(result.runs.len(), 2);
    assert_eq!(result.run("root").expect("root").children, ["child"]);
    assert_eq!(
        result.run("child").expect("child").parent_id.as_deref(),
        Some("root")
    );
    assert_eq!(result.run("child").expect("child").root_id, "root");
    assert!(result.issues.is_empty(), "{:?}", result.issues);
    assert!(result.open_items.is_empty(), "{:?}", result.open_items);
    assert!(result.is_complete());
}

#[test]
fn truncated_tail_costs_only_its_own_line() {
    let result = replay_without_panic(TRUNCATED_TAIL);

    assert_events_in_order(
        &result,
        &[
            ("t:1", 1, "run.start"),
            ("t:2", 2, "tool.use"),
            ("t:3", 3, "run.done"),
        ],
    );
    assert_eq!(result.issues.len(), 1, "{:?}", result.issues);
    assert!(
        result.issues.iter().any(
            |issue| matches!(issue, ReplayIssue::MalformedLine { line: 4, snippet } if !snippet.is_empty())
        ),
        "{:?}",
        result.issues
    );
    assert!(
        !result
            .issues
            .iter()
            .any(|issue| matches!(issue, ReplayIssue::SeqGap { .. })),
        "a truncated line must not invent a sequence gap: {:?}",
        result.issues
    );
    assert!(result.open_items.is_empty(), "{:?}", result.open_items);
    assert!(!result.is_complete());
}

#[test]
fn interleaved_runs_group_without_false_gaps() {
    let result = replay_without_panic(INTERLEAVED);

    assert_events_in_order(
        &result,
        &[
            ("a:1", 1, "run.spawn"),
            ("b:1", 1, "run.spawn"),
            ("a:2", 2, "run.start"),
            ("b:2", 2, "run.start"),
            ("a:3", 3, "tool.use"),
            ("b:3", 3, "tool.use"),
            ("a:4", 4, "tool.result"),
            ("b:4", 4, "tool.result"),
            ("a:5", 5, "run.done"),
            ("b:5", 5, "run.done"),
        ],
    );
    assert_eq!(result.runs.len(), 2);
    let ids_for = |run: &str| -> Vec<&str> {
        result
            .events_for_run(run)
            .iter()
            .map(|event| event.id.as_str())
            .collect()
    };
    assert_eq!(ids_for("a"), ["a:1", "a:2", "a:3", "a:4", "a:5"]);
    assert_eq!(ids_for("b"), ["b:1", "b:2", "b:3", "b:4", "b:5"]);
    assert_eq!(result.run("a").expect("a").children, ["b"]);
    assert_eq!(result.run("b").expect("b").root_id, "a");
    assert!(result.issues.is_empty(), "{:?}", result.issues);
    assert!(result.open_items.is_empty(), "{:?}", result.open_items);
    assert!(result.is_complete());
}

#[test]
fn duplicate_ids_are_the_only_issue() {
    let result = replay_without_panic(DUPLICATE_IDS);

    assert_events_in_order(
        &result,
        &[
            ("d:1", 1, "run.start"),
            ("d:2", 2, "tool.use"),
            ("d:1", 3, "run.done"),
        ],
    );
    assert_eq!(
        result.issues,
        vec![ReplayIssue::DuplicateId {
            run_id: "d".into(),
            id: "d:1".into(),
            seq: 3,
        }]
    );
    assert!(result.open_items.is_empty(), "{:?}", result.open_items);
    assert!(!result.is_complete());
}

#[test]
fn orphan_parent_is_an_issue_and_an_open_history() {
    let result = replay_without_panic(ORPHAN_PARENT);

    assert_events_in_order(&result, &[("c:1", 1, "run.spawn"), ("c:2", 2, "run.start")]);
    assert_eq!(result.runs.len(), 1);
    assert_eq!(
        result.issues,
        vec![ReplayIssue::OrphanParent {
            run_id: "c".into(),
            parent_id: "ghost".into(),
        }]
    );
    assert_eq!(
        result.open_items,
        vec![OpenItem::HistoryNotRecorded {
            run_id: "ghost".into(),
        }]
    );
    assert_eq!(result.run("c").expect("c").root_id, "c");
    assert!(!result.is_complete());
}

#[test]
fn late_tool_result_is_an_issue_not_a_success() {
    let result = replay_without_panic(LATE_RESULT);

    assert_events_in_order(
        &result,
        &[("t:1", 1, "run.start"), ("t:2", 2, "tool.result")],
    );
    assert_eq!(
        result.issues,
        vec![ReplayIssue::ResultWithoutCall {
            run_id: "t".into(),
            detail: "orphan_result".into(),
        }]
    );
    assert!(result.open_items.is_empty(), "{:?}", result.open_items);
    assert!(!result.is_complete());
}

#[test]
fn unknown_version_is_open_without_discarding_the_event() {
    let result = replay_without_panic(UNKNOWN_VERSION);

    assert_events_in_order(&result, &[("u:1", 1, "run.start"), ("u:2", 2, "run.done")]);
    assert!(result.issues.is_empty(), "{:?}", result.issues);
    assert_eq!(
        result.open_items,
        vec![OpenItem::UnknownVersion {
            run_id: "u".into(),
            event_id: "u:1".into(),
            version: 7,
        }]
    );
    assert!(!result.is_complete());
}

#[test]
fn interrupted_save_ignores_the_blank_line_and_blames_the_partial_one() {
    let result = replay_without_panic(INTERRUPTED_SAVE);

    assert_events_in_order(&result, &[("s:1", 1, "run.start"), ("s:2", 2, "tool.use")]);
    assert_eq!(
        result.issues,
        vec![ReplayIssue::MalformedLine {
            line: 4,
            snippet: r#"{"event":"run.done","agent_id":"s","at_ms":3,"id":"s:3","#.into(),
        }]
    );
    assert!(result.open_items.is_empty(), "{:?}", result.open_items);
    assert!(!result.is_complete());
}

#[test]
fn mixed_v0_and_v1_stays_clean_and_preserves_order() {
    let result = replay_without_panic(MIXED_V0_V1);

    assert_events_in_order(
        &result,
        &[
            ("mix:1", 1, "run.spawn"),
            ("", 0, "tool.use"),
            ("mix:2", 2, "run.done"),
        ],
    );
    assert_eq!(result.events[1].agent_id, "mix");
    assert_eq!(result.events[0].version, 1);
    assert_eq!(result.events[1].version, 0);
    assert_eq!(result.events[2].version, 1);
    assert_eq!(result.runs.len(), 1);
    assert!(result.issues.is_empty(), "{:?}", result.issues);
    assert!(result.open_items.is_empty(), "{:?}", result.open_items);
    assert!(result.is_complete());
}

#[test]
fn crlf_and_bom_are_handled_without_leaking_control_bytes() {
    let result = replay_without_panic(CRLF_BOM);

    // The BOM makes line 1 invalid JSON; the CRLF line after it parses fine.
    assert_events_in_order(&result, &[("e:2", 2, "run.done")]);
    assert_eq!(result.issues.len(), 1, "{:?}", result.issues);
    let snippet = match &result.issues[0] {
        ReplayIssue::MalformedLine { line: 1, snippet } => {
            assert!(!snippet.contains('\r'), "CRLF must not reach the snippet");
            snippet
        }
        other => panic!("expected a malformed first line, got {other:?}"),
    };
    assert!(
        snippet.starts_with('\u{feff}'),
        "the BOM stays on the failed first line: {snippet:?}"
    );
    assert_eq!(result.events[0].detail, "finish");
    assert!(result.open_items.is_empty(), "{:?}", result.open_items);
    assert!(!result.is_complete());
}

#[test]
fn ten_thousand_events_replay_in_bounded_work() {
    let started = Instant::now();
    let result = replay_without_panic(BULK_10000);
    let elapsed = started.elapsed();

    assert_eq!(result.events.len(), 10_000);
    assert_eq!(result.events.first().expect("first").id, "bulk:1");
    assert_eq!(result.events.last().expect("last").id, "bulk:10000");
    assert_eq!(result.events.first().expect("first").seq, 1);
    assert_eq!(result.events.last().expect("last").seq, 10_000);
    assert_eq!(result.runs.len(), 1);
    assert!(result.issues.is_empty(), "{:?}", result.issues);
    assert!(result.open_items.is_empty(), "{:?}", result.open_items);
    assert!(result.is_complete());
    assert!(
        elapsed < Duration::from_secs(30),
        "10,000 events took {elapsed:?}; replay is doing unbounded work"
    );
}
