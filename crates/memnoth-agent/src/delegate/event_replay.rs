//! Pure, in-memory replay of persisted child-event logs (NDJSON).
//!
//! This is the loader layer only. It reads text that was already written to a
//! child JSONL file and returns a typed, auditable picture of what the log
//! actually contains. It never writes to disk, never starts a process, never
//! applies an edit, never resumes a run, and never fabricates reasoning text.
//!
//! The truth rules enforced here:
//!
//! - An absent history means "not recorded", never "no tools ran".
//! - Unknown event kinds are preserved verbatim. They are never treated as a
//!   success, a terminal state, or reasoning.
//! - `tool.use` is a fact observed at a tool boundary. It is not a tool result
//!   and not reasoning, and neither is stored in the child log.
//! - A `tool.result` with no matching `tool.use` is reported as an issue, not
//!   as a silent success.
//! - An unknown schema `version` is reported as an [`OpenItem::UnknownVersion`]
//!   (OPEN), never as "no events" and never as a clean replay.
//! - A malformed line is reported and skipped; every other line is still
//!   returned. Bad input never panics.
//!
//! [`is_complete`](ReplayResult::is_complete) answers only "did this replay
//! resolve cleanly?". It makes no claim that any tool ran, that a run is still
//! alive, or that any side effect may be applied.

use std::collections::{BTreeMap, BTreeSet};

use crate::delegate::ChildEvent;
use crate::delegate::event_schema::{EVENT_SCHEMA_VERSION, TOOL_USE};

/// Event kind for a tool result.
///
/// The delegator deliberately does not persist results in the child log, but
/// an additive producer may. Recognising the kind lets the replay report an
/// orphan result instead of silently counting it as a successful call.
pub const TOOL_RESULT: &str = "tool.result";

/// Alternate (underscore) spelling accepted for a tool result.
const TOOL_RESULT_ALT: &str = "tool_result";

/// A typed problem found while replaying a log.
///
/// An issue means the log is inconsistent or unresolvable as written. It is
/// distinct from an [`OpenItem`], which is a fact the loader deliberately
/// leaves OPEN because only a person or a future writer can close it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayIssue {
    /// Two events share the same stable event id.
    DuplicateId {
        /// Run the events belong to.
        run_id: String,
        /// The repeated id.
        id: String,
        /// Sequence number carried by the later event.
        seq: u64,
    },
    /// Two events in one run carry the same sequence number.
    DuplicateSeq {
        /// Run the events belong to.
        run_id: String,
        /// The repeated sequence number.
        seq: u64,
    },
    /// Sequence numbers skip forward, so at least one event is missing.
    SeqGap {
        /// Run the events belong to.
        run_id: String,
        /// The sequence number that should have come next.
        expected: u64,
        /// The sequence number that was found.
        found: u64,
    },
    /// A sequence number went backwards.
    SeqNotMonotonic {
        /// Run the events belong to.
        run_id: String,
        /// The previously observed sequence number.
        previous: u64,
        /// The smaller sequence number that was found.
        found: u64,
    },
    /// A parent id points at a run that has no events in this log.
    OrphanParent {
        /// The child whose link cannot be resolved.
        run_id: String,
        /// The missing parent run id.
        parent_id: String,
    },
    /// A run records itself as its own parent.
    SelfParent {
        /// The run with the invalid link.
        run_id: String,
    },
    /// Following parent links from this run loops back on itself.
    CycleInTree {
        /// A run on the cycle.
        run_id: String,
    },
    /// A `tool.result` has no preceding `tool.use` in the same run.
    ResultWithoutCall {
        /// Run the result was found in.
        run_id: String,
        /// The result detail (the tool name, when supplied).
        detail: String,
    },
    /// The run identity is empty, so the event cannot be linked to a run.
    MissingRunIdentity {
        /// One-based line number of the offending line.
        line: usize,
    },
    /// A line was not valid JSON and was skipped.
    MalformedLine {
        /// One-based line number in the input.
        line: usize,
        /// A bounded snippet of the raw line.
        snippet: String,
    },
}

/// A fact the loader cannot close, left explicitly OPEN for a reader to
/// surface. An open item is never converted into a success or a terminal
/// state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenItem {
    /// An event uses a schema version this reader does not understand.
    UnknownVersion {
        /// Run the event belongs to.
        run_id: String,
        /// Stable event id, empty on legacy lines.
        event_id: String,
        /// The version number that was found.
        version: u32,
    },
    /// A parent run is referenced but no events for it were recorded.
    ///
    /// This is the "not recorded" case. It is deliberately not phrased as
    /// "no tools ran", because an absent history proves nothing about work.
    HistoryNotRecorded {
        /// The run whose history is absent.
        run_id: String,
    },
}

/// One reconstructed node of the run tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayRun {
    /// The run id this node represents.
    pub run_id: String,
    /// Parent run id, when a parent was recorded.
    pub parent_id: Option<String>,
    /// The root of the tree, reached by following parent links.
    pub root_id: String,
    /// Direct children of this run, in discovery order.
    pub children: Vec<String>,
    /// Indices into [`ReplayResult::events`] for this run, in log order.
    pub event_indices: Vec<usize>,
}

/// The pure result of replaying one NDJSON log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayResult {
    /// Reconstructed run tree, keyed by run id.
    pub runs: BTreeMap<String, ReplayRun>,
    /// Every parsed event, in input order. Unknown kinds are included as-is.
    pub events: Vec<ChildEvent>,
    /// Integrity problems found while replaying.
    pub issues: Vec<ReplayIssue>,
    /// Facts left OPEN because the loader cannot resolve them.
    pub open_items: Vec<OpenItem>,
}

impl ReplayResult {
    /// Whether the replay resolved with no integrity issues and no OPEN items.
    ///
    /// This says nothing about whether tools ran or whether a run is still
    /// active. A log with no events is "not recorded", not proof of no work.
    pub fn is_complete(&self) -> bool {
        self.issues.is_empty() && self.open_items.is_empty()
    }

    /// Look up one reconstructed run by id.
    pub fn run(&self, run_id: &str) -> Option<&ReplayRun> {
        self.runs.get(run_id)
    }

    /// All events belonging to one run, in log order.
    pub fn events_for_run(&self, run_id: &str) -> Vec<&ChildEvent> {
        self.run(run_id)
            .map(|run| {
                run.event_indices
                    .iter()
                    .filter_map(|index| self.events.get(*index))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Parse and validate an NDJSON child-event log without any I/O or side
/// effects.
///
/// Legacy v0 lines (only `event`, `agent_id`, `at_ms`, `detail`) parse with
/// their serde defaults and are never mistaken for errors. Blank lines are
/// ignored; malformed lines become [`ReplayIssue::MalformedLine`] and are
/// skipped while the rest of the log is retained in order.
pub fn replay_events(jsonl: &str) -> ReplayResult {
    let mut result = ReplayResult::default();

    // 1. Parse every line. A bad line costs only itself. `parsed` keeps the
    // one-based source line so diagnostics point at the real file position,
    // even when blank or malformed lines were skipped.
    let mut parsed: Vec<(usize, ChildEvent)> = Vec::new();
    for (index, line) in jsonl.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ChildEvent>(line) {
            Ok(event) => parsed.push((index + 1, event)),
            Err(_) => result.issues.push(ReplayIssue::MalformedLine {
                line: index + 1,
                snippet: snippet(line),
            }),
        }
    }

    result.events = parsed.iter().map(|(_, event)| event.clone()).collect();

    // 2. Stable id uniqueness across the whole log.
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    for (_line, event) in &parsed {
        if event.id.is_empty() {
            // Legacy lines share an empty id; it is not an identity.
            continue;
        }
        if !seen_ids.insert(event.id.clone()) {
            result.issues.push(ReplayIssue::DuplicateId {
                run_id: effective_run_id(event).to_string(),
                id: event.id.clone(),
                seq: event.seq,
            });
        }
    }

    // 3. Group by effective run id and validate per-run ordering and facts.
    // The stored index is a position in `parsed`, which is also its position
    // in `result.events` because the two vectors share one order.
    let mut per_run: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (position, (line, event)) in parsed.iter().enumerate() {
        let run_id = effective_run_id(event);
        if run_id.is_empty() {
            result
                .issues
                .push(ReplayIssue::MissingRunIdentity { line: *line });
        }
        per_run
            .entry(run_id.to_string())
            .or_default()
            .push(position);
    }

    // Outstanding tool uses per run, keyed by tool name (the `detail` field).
    let mut outstanding: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();

    for (run_id, positions) in &per_run {
        let mut last_seq: Option<u64> = None;
        for position in positions {
            let event = &parsed[*position].1;

            // Sequence checks apply only to recorded (v1) sequence numbers;
            // legacy lines carry seq == 0 and have no ordering guarantee.
            if event.seq > 0 {
                match last_seq {
                    None => last_seq = Some(event.seq),
                    Some(previous) if event.seq == previous => {
                        result.issues.push(ReplayIssue::DuplicateSeq {
                            run_id: run_id.clone(),
                            seq: event.seq,
                        });
                    }
                    Some(previous) if event.seq < previous => {
                        result.issues.push(ReplayIssue::SeqNotMonotonic {
                            run_id: run_id.clone(),
                            previous,
                            found: event.seq,
                        });
                        last_seq = Some(event.seq);
                    }
                    Some(previous) if event.seq > previous.saturating_add(1) => {
                        result.issues.push(ReplayIssue::SeqGap {
                            run_id: run_id.clone(),
                            expected: previous.saturating_add(1),
                            found: event.seq,
                        });
                        last_seq = Some(event.seq);
                    }
                    Some(_) => last_seq = Some(event.seq),
                }
            }

            // Tool call/result pairing. A result never becomes a silent
            // success when no use preceded it.
            if event.event == TOOL_USE {
                *outstanding
                    .entry(run_id.clone())
                    .or_default()
                    .entry(event.detail.clone())
                    .or_insert(0) += 1;
            } else if event.event == TOOL_RESULT || event.event == TOOL_RESULT_ALT {
                let uses = outstanding.entry(run_id.clone()).or_default();
                let count = uses.entry(event.detail.clone()).or_insert(0);
                if *count == 0 {
                    result.issues.push(ReplayIssue::ResultWithoutCall {
                        run_id: run_id.clone(),
                        detail: event.detail.clone(),
                    });
                } else {
                    *count -= 1;
                }
            }

            // Unknown versions are OPEN, not an empty history.
            if event.version != 0 && event.version != EVENT_SCHEMA_VERSION {
                result.open_items.push(OpenItem::UnknownVersion {
                    run_id: run_id.clone(),
                    event_id: event.id.clone(),
                    version: event.version,
                });
            }
        }
    }

    // 4. Reconstruct parent links from the additive `parent_id` field.
    let mut parents: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut referenced_parents: BTreeSet<String> = BTreeSet::new();
    for (_line, event) in &parsed {
        let run_id = effective_run_id(event).to_string();
        match parents.get(&run_id) {
            None => {
                parents.insert(run_id, event.parent_id.clone());
            }
            Some(Some(_)) => {
                // A recorded parent wins over a line with no parent.
            }
            Some(None) => {
                if event.parent_id.is_some() {
                    parents.insert(run_id, event.parent_id.clone());
                }
            }
        }
        if let Some(parent) = &event.parent_id {
            referenced_parents.insert(parent.clone());
        }
    }

    let known: BTreeSet<String> = parents.keys().cloned().collect();
    for (run_id, parent) in &parents {
        if let Some(parent_id) = parent {
            if parent_id == run_id {
                result.issues.push(ReplayIssue::SelfParent {
                    run_id: run_id.clone(),
                });
            } else if !known.contains(parent_id) {
                result.issues.push(ReplayIssue::OrphanParent {
                    run_id: run_id.clone(),
                    parent_id: parent_id.clone(),
                });
            }
        }
    }

    // 5. Build the tree nodes with a cycle-guarded walk to each root.
    for run_id in &known {
        let root_id = walk_to_root(run_id, &parents, &mut result.issues);
        let mut children: Vec<String> = parents
            .iter()
            .filter(|(_, parent)| parent.as_deref() == Some(run_id.as_str()))
            .map(|(child, _)| child.clone())
            .collect();
        children.sort();
        let event_indices = per_run.get(run_id).cloned().unwrap_or_default();
        result.runs.insert(
            run_id.clone(),
            ReplayRun {
                run_id: run_id.clone(),
                parent_id: parents.get(run_id).cloned().flatten(),
                root_id,
                children,
                event_indices,
            },
        );
    }

    // 6. A referenced run with no events is "not recorded", never "no tools
    // ran". Report it once per run id.
    for parent in referenced_parents {
        if !known.contains(&parent) {
            result
                .open_items
                .push(OpenItem::HistoryNotRecorded { run_id: parent });
        }
    }

    result
}

/// The run an event belongs to: `run_id` on v1 lines, `agent_id` on legacy
/// lines where `run_id` was left at its default.
fn effective_run_id(event: &ChildEvent) -> &str {
    if event.run_id.is_empty() {
        &event.agent_id
    } else {
        &event.run_id
    }
}

/// Follow parent links to the root, stopping on a missing link or a cycle.
fn walk_to_root(
    start: &str,
    parents: &BTreeMap<String, Option<String>>,
    issues: &mut Vec<ReplayIssue>,
) -> String {
    let mut cursor = start.to_string();
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(cursor.clone()) {
            issues.push(ReplayIssue::CycleInTree {
                run_id: start.to_string(),
            });
            return cursor;
        }
        match parents.get(&cursor).and_then(|parent| parent.clone()) {
            Some(parent) if parents.contains_key(&parent) => cursor = parent,
            _ => return cursor,
        }
    }
}

/// A bounded, single-line preview of a malformed line.
fn snippet(line: &str) -> String {
    let trimmed = line.trim();
    let mut out: String = trimmed.chars().take(80).collect();
    if trimmed.chars().count() > 80 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegate::event_schema::{
        EVENT_SCHEMA_VERSION, EventState, RUN_DONE, RUN_SPAWN, RUN_START, SOURCE_DELEGATOR,
        state_for_kind,
    };

    /// Build a v1 event as the delegator would write it.
    fn v1(run: &str, seq: u64, kind: &str, detail: &str, parent: Option<&str>) -> ChildEvent {
        ChildEvent {
            event: kind.to_string(),
            agent_id: run.to_string(),
            at_ms: seq as i64,
            detail: detail.to_string(),
            id: format!("{run}:{seq}"),
            seq,
            run_id: run.to_string(),
            session_id: Some("s-1".to_string()),
            turn_id: None,
            parent_id: parent.map(str::to_string),
            source: SOURCE_DELEGATOR.to_string(),
            state: state_for_kind(kind),
            version: EVENT_SCHEMA_VERSION,
        }
    }

    /// Build a legacy v0 line with only the four original fields set.
    fn legacy(run: &str, kind: &str, detail: &str) -> ChildEvent {
        ChildEvent {
            event: kind.to_string(),
            agent_id: run.to_string(),
            at_ms: 1,
            detail: detail.to_string(),
            id: String::new(),
            seq: 0,
            run_id: String::new(),
            session_id: None,
            turn_id: None,
            parent_id: None,
            source: String::new(),
            state: EventState::Unknown,
            version: 0,
        }
    }

    fn line(event: &ChildEvent) -> String {
        serde_json::to_string(event).expect("serialize event")
    }

    fn log(events: &[ChildEvent]) -> String {
        events.iter().map(line).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn a_clean_v1_log_replays_into_a_linked_tree() {
        let events = vec![
            v1("root", 1, RUN_SPAWN, "spawned scout", None),
            v1("root", 2, RUN_START, "find it", None),
            v1("root", 3, TOOL_USE, "read_file", None),
            v1("root", 4, RUN_DONE, "done", None),
            v1("child", 1, RUN_SPAWN, "spawned worker", Some("root")),
            v1("child", 2, RUN_START, "work", Some("root")),
            v1("child", 3, RUN_DONE, "done", Some("root")),
        ];
        let result = replay_events(&log(&events));

        assert_eq!(result.events.len(), 7, "every line is preserved");
        assert_eq!(result.runs.len(), 2);
        assert!(result.issues.is_empty(), "{:?}", result.issues);
        assert!(result.open_items.is_empty(), "{:?}", result.open_items);
        assert!(result.is_complete());
        assert_eq!(result.run("root").expect("root").children, ["child"]);
        assert_eq!(
            result.run("child").expect("child").parent_id.as_deref(),
            Some("root")
        );
        assert_eq!(result.run("child").expect("child").root_id, "root");
        assert_eq!(result.events_for_run("root").len(), 4);
    }

    #[test]
    fn a_legacy_v0_only_log_replays_without_inventing_issues() {
        let events = vec![
            legacy("old", RUN_START, "start"),
            legacy("old", TOOL_USE, "grep"),
            legacy("old", RUN_DONE, "done"),
        ];
        let result = replay_events(&log(&events));

        assert_eq!(result.events.len(), 3);
        assert!(
            result.events.iter().all(|event| event.version == 0),
            "legacy lines keep version 0"
        );
        assert_eq!(result.runs.len(), 1);
        assert!(result.issues.is_empty(), "{:?}", result.issues);
        assert!(result.open_items.is_empty(), "{:?}", result.open_items);
        assert!(result.is_complete());
    }

    #[test]
    fn mixed_v0_and_v1_lines_do_not_create_phantom_gaps() {
        // The unsequenced legacy line sits in the middle of the v1 sequence.
        let interleaved = vec![
            v1("mix", 1, RUN_SPAWN, "spawn", None),
            v1("mix", 2, RUN_START, "go", None),
            legacy("mix", TOOL_USE, "legacy_tool"),
            v1("mix", 3, RUN_DONE, "done", None),
        ];
        let result = replay_events(&log(&interleaved));

        assert_eq!(result.events.len(), 4, "the legacy line is not dropped");
        assert!(
            result
                .issues
                .iter()
                .all(|issue| !matches!(issue, ReplayIssue::SeqGap { .. })),
            "{:?}",
            result.issues
        );
        assert!(result.issues.is_empty(), "{:?}", result.issues);
        assert!(result.is_complete());
    }

    #[test]
    fn a_duplicate_event_id_is_reported() {
        let first = v1("dup", 1, RUN_START, "a", None);
        let mut second = v1("dup", 2, TOOL_USE, "b", None);
        second.id = first.id.clone();
        let result = replay_events(&log(&[first, second]));

        assert!(
            result.issues.iter().any(|issue| matches!(
                issue,
                ReplayIssue::DuplicateId { id, .. } if id == "dup:1"
            )),
            "{:?}",
            result.issues
        );
        assert!(!result.is_complete());
    }

    #[test]
    fn a_sequence_gap_is_reported() {
        let result = replay_events(&log(&[
            v1("gap", 1, RUN_START, "a", None),
            v1("gap", 3, TOOL_USE, "c", None),
        ]));

        assert!(
            result.issues.iter().any(|issue| matches!(
                issue,
                ReplayIssue::SeqGap {
                    expected: 2,
                    found: 3,
                    ..
                }
            )),
            "{:?}",
            result.issues
        );
        assert!(!result.is_complete());
    }

    #[test]
    fn an_orphan_parent_is_reported_without_panicking() {
        let result = replay_events(&log(&[v1("child", 1, RUN_SPAWN, "spawn", Some("ghost"))]));

        assert_eq!(result.runs.len(), 1);
        assert!(
            result.issues.iter().any(|issue| matches!(
                issue,
                ReplayIssue::OrphanParent { run_id, parent_id }
                    if run_id == "child" && parent_id == "ghost"
            )),
            "{:?}",
            result.issues
        );
        assert!(
            result.open_items.iter().any(|item| matches!(
                item,
                OpenItem::HistoryNotRecorded { run_id } if run_id == "ghost"
            )),
            "an unrecorded parent is OPEN, not 'no tools ran': {:?}",
            result.open_items
        );
        assert!(!result.is_complete());
    }

    #[test]
    fn an_unknown_version_is_open_not_no_events() {
        let mut event = v1("u", 1, RUN_START, "go", None);
        event.version = 7;
        let result = replay_events(&log(&[event]));

        assert_eq!(result.events.len(), 1, "the event still exists");
        assert!(
            result
                .open_items
                .iter()
                .any(|item| matches!(item, OpenItem::UnknownVersion { version: 7, .. })),
            "{:?}",
            result.open_items
        );
        assert!(!result.is_complete());
    }

    #[test]
    fn a_result_without_a_call_is_an_issue_not_a_success() {
        let orphan = replay_events(&log(&[
            v1("t", 1, RUN_START, "go", None),
            v1("t", 2, TOOL_RESULT, "orphan result", None),
        ]));
        assert!(
            orphan.issues.iter().any(|issue| matches!(
                issue,
                ReplayIssue::ResultWithoutCall { detail, .. } if detail == "orphan result"
            )),
            "{:?}",
            orphan.issues
        );
        assert!(!orphan.is_complete());

        let matched = replay_events(&log(&[
            v1("t", 1, TOOL_USE, "read_file", None),
            v1("t", 2, TOOL_RESULT, "read_file", None),
        ]));
        assert!(
            !matched
                .issues
                .iter()
                .any(|issue| matches!(issue, ReplayIssue::ResultWithoutCall { .. })),
            "a paired result is fine: {:?}",
            matched.issues
        );
    }

    #[test]
    fn malformed_lines_are_reported_without_discarding_good_ones() {
        let good_first = v1("m", 1, RUN_START, "a", None);
        let good_last = v1("m", 2, TOOL_USE, "b", None);
        let input = format!(
            "{}\n{{not json}}\n{}\n",
            line(&good_first),
            line(&good_last)
        );
        let result = replay_events(&input);

        assert_eq!(result.events.len(), 2, "both valid lines survive");
        assert_eq!(result.events[0].seq, 1);
        assert_eq!(result.events[1].seq, 2);
        assert!(
            result
                .issues
                .iter()
                .any(|issue| matches!(issue, ReplayIssue::MalformedLine { line: 2, .. })),
            "{:?}",
            result.issues
        );
        assert!(
            !result
                .issues
                .iter()
                .any(|issue| matches!(issue, ReplayIssue::SeqGap { .. })),
            "the bad line must not create a gap: {:?}",
            result.issues
        );
        assert!(!result.is_complete());
    }

    #[test]
    fn unknown_kinds_are_preserved_without_becoming_success() {
        let mut event = v1("k", 1, "some.future.kind", "payload", None);
        event.state = EventState::Unknown;
        let result = replay_events(&log(&[event]));

        assert_eq!(result.events[0].event, "some.future.kind");
        assert_eq!(result.events[0].state, EventState::Unknown);
        assert!(result.issues.is_empty(), "{:?}", result.issues);
        assert!(result.open_items.is_empty(), "{:?}", result.open_items);
    }

    #[test]
    fn self_parent_and_cycles_are_reported_without_hanging() {
        let self_parent = replay_events(&log(&[v1("a", 1, RUN_SPAWN, "spawn", Some("a"))]));
        assert!(
            self_parent
                .issues
                .iter()
                .any(|issue| matches!(issue, ReplayIssue::SelfParent { .. })),
            "{:?}",
            self_parent.issues
        );

        let cycle = replay_events(&log(&[
            v1("a", 1, RUN_SPAWN, "spawn", Some("b")),
            v1("b", 1, RUN_SPAWN, "spawn", Some("a")),
        ]));
        assert!(
            cycle
                .issues
                .iter()
                .any(|issue| matches!(issue, ReplayIssue::CycleInTree { .. })),
            "{:?}",
            cycle.issues
        );
    }
}
