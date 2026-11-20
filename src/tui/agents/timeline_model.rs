//! Pure replay-timeline model for the agent TUI.
//!
//! This module turns a [`ReplayResult`] into typed, renderable rows. It holds
//! no terminal I/O, no clock, no mutation of the replay, and no side effects:
//! building a timeline never replays a run, never applies an edit, never
//! resumes a process, and never fabricates reasoning text.
//!
//! The truth rules from the replay loader are carried through verbatim:
//!
//! - An absent history stays "not recorded"; it is never phrased as "no tools
//!   ran" and is never turned into a success or a terminal state.
//! - Unknown event kinds keep their original `kind` string. They are Facts,
//!   not successes, not reasoning, and not terminal states.
//! - `tool.use` is a [`RowKind::ToolBoundary`]: a fact observed at a tool
//!   boundary, never a tool result and never reasoning.
//! - Replay issues become warning rows and open items become explicit `OPEN`
//!   rows. Neither is silently dropped.
//!
//! Expansion state is per row and is changed only by the pure methods on
//! [`ReplayTimeline`]. Nothing here reads a key, writes to a screen, or touches
//! the filesystem.

use memnoth_agent::delegate::ChildEvent;
use memnoth_agent::delegate::event_replay::{OpenItem, ReplayIssue, ReplayResult};
use memnoth_agent::delegate::event_schema::{EventState, TOOL_USE, state_for_kind};

/// The category a renderable row belongs to.
///
/// This is deliberately a small, closed set so a renderer cannot mistake an
/// integrity warning or an deliberately-OPEN fact for a recorded event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A recorded fact from an event (spawn, start, done, failed, …).
    Fact,
    /// A `tool.use` boundary. A fact about tool *use*, never a result.
    ToolBoundary,
    /// An integrity problem found while replaying. Render as a warning.
    Issue,
    /// A fact the loader left deliberately OPEN. Render as an explicit row.
    OpenItem,
}

impl RowKind {
    /// Compact text marker used when a row is drawn at narrow widths.
    ///
    /// Text as well as glyph so warnings and OPEN rows stay legible with a
    /// screen reader and in a monochrome theme.
    pub fn marker(self) -> &'static str {
        match self {
            RowKind::Fact => "·",
            RowKind::ToolBoundary => "↳",
            RowKind::Issue => "!",
            RowKind::OpenItem => "OPEN",
        }
    }

    /// Whether this row warns about a replay integrity problem.
    pub fn is_warning(self) -> bool {
        matches!(self, RowKind::Issue)
    }

    /// Whether this row is an explicit, unresolved OPEN item.
    pub fn is_open(self) -> bool {
        matches!(self, RowKind::OpenItem)
    }
}

/// One renderable row of the replay timeline.
///
/// Every row carries the fields a renderer needs without going back to the
/// replay: identity (`id`, `seq`), `kind` verbatim, typed `state`, `source`,
/// occurrence time (`ts`), and the `row_kind` that decides how it is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRow {
    /// Stable event id, empty on legacy lines and on diagnostic rows.
    pub id: String,
    /// Per-run sequence, 0 on legacy lines and on diagnostic rows.
    pub seq: u64,
    /// Event kind preserved verbatim, or a synthetic `replay.*` diagnostic.
    pub kind: String,
    /// Typed lifecycle state carried by the event; `Unknown` for diagnostics.
    pub state: EventState,
    /// Producer label, empty on legacy lines and on diagnostic rows.
    pub source: String,
    /// Occurrence time in epoch milliseconds, 0 when not recorded.
    pub ts: i64,
    /// Run the row belongs to, falling back to the legacy `agent_id`.
    pub run_id: String,
    /// The category this row renders under.
    pub row_kind: RowKind,
    /// Safe, single-line collapsed label.
    pub label: String,
    /// Expanded-only detail: ids, state, and the reason a row exists.
    pub detail: String,
    /// Whether this row has anything to reveal when expanded.
    pub expandable: bool,
}

impl TimelineRow {
    /// Whether this row renders as a warning.
    pub fn is_warning(&self) -> bool {
        self.row_kind.is_warning()
    }

    /// Whether this row renders as an explicit OPEN item.
    pub fn is_open(&self) -> bool {
        self.row_kind.is_open()
    }

    /// A shortened label for narrow terminals.
    ///
    /// Truncates on character boundaries and appends an ellipsis when the
    /// label is shortened. A width of zero yields an empty string; a width
    /// larger than the label yields the label unchanged. It never panics.
    pub fn narrow_label(&self, width: usize) -> String {
        if width == 0 {
            return String::new();
        }
        let count = self.label.chars().count();
        if count <= width {
            return self.label.clone();
        }
        let mut out: String = self.label.chars().take(width).collect();
        out.pop();
        out.push('…');
        out
    }
}

/// An immutable projection of a replay into ordered, renderable rows.
///
/// Rows are ordered as the replay presents them: recorded events first in log
/// order, then issues, then open items. Expansion is the only mutable state and
/// lives here rather than on the rows so the rows stay a pure value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayTimeline {
    rows: Vec<TimelineRow>,
    expanded: Vec<bool>,
}

impl ReplayTimeline {
    /// Build a timeline from a replay without changing the replay.
    ///
    /// Empty, legacy, partial, or noisy replays all produce a value; this
    /// constructor never indexes by an untrusted position and never panics.
    pub fn from_replay(replay: &ReplayResult) -> Self {
        let mut rows: Vec<TimelineRow> = replay.events.iter().map(row_from_event).collect();
        rows.extend(replay.issues.iter().map(row_from_issue));
        rows.extend(replay.open_items.iter().map(row_from_open_item));
        let expanded = vec![false; rows.len()];
        Self { rows, expanded }
    }

    /// All rows in render order.
    pub fn rows(&self) -> &[TimelineRow] {
        &self.rows
    }

    /// One row by render position, if it exists.
    pub fn row(&self, index: usize) -> Option<&TimelineRow> {
        self.rows.get(index)
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there is nothing to render.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Number of rows of one kind, for compact status summaries.
    pub fn count(&self, kind: RowKind) -> usize {
        self.rows.iter().filter(|row| row.row_kind == kind).count()
    }

    /// Whether the row at `index` is expanded. Out-of-range is never expanded.
    pub fn is_expanded(&self, index: usize) -> bool {
        self.expanded.get(index).copied().unwrap_or(false)
    }

    /// Set expansion for one row. Returns false when `index` is out of range.
    ///
    /// A row with nothing to reveal cannot be expanded, so this silently
    /// clamps to collapsed for non-expandable rows.
    pub fn set_expanded(&mut self, index: usize, expanded: bool) -> bool {
        let Some(slot) = self.expanded.get_mut(index) else {
            return false;
        };
        let can_expand = self.rows.get(index).is_some_and(|row| row.expandable);
        *slot = expanded && can_expand;
        true
    }

    /// Flip expansion for one row. Returns false when `index` is out of range.
    pub fn toggle_expanded(&mut self, index: usize) -> bool {
        let Some(&current) = self.expanded.get(index) else {
            return false;
        };
        self.set_expanded(index, !current);
        true
    }

    /// Collapse every row without changing the rows themselves.
    pub fn collapse_all(&mut self) {
        self.expanded.fill(false);
    }
}

fn row_from_event(event: &ChildEvent) -> TimelineRow {
    let is_tool_boundary = event.event == TOOL_USE;
    let known_kind = state_for_kind(&event.event) != EventState::Unknown;

    // The original kind string is the label prefix, so an unknown future kind
    // is shown exactly as it arrived instead of being abstracted away.
    let kind = event.event.clone();
    let label = if event.detail.is_empty() {
        kind.clone()
    } else {
        format!("{} · {}", kind, event.detail)
    };

    let mut detail = format!(
        "id: {}\nseq: {}\nstate: {}\nsource: {}\nat: {}ms\nkind: {}",
        event.id,
        event.seq,
        event.state.as_str(),
        event.source,
        event.at_ms,
        kind
    );
    if is_tool_boundary {
        detail.push_str("\ntool boundary only — no result and no reasoning is recorded here");
    } else if !known_kind {
        detail.push_str("\nunknown kind — preserved verbatim, not a success and not reasoning");
    }

    TimelineRow {
        id: event.id.clone(),
        seq: event.seq,
        kind,
        state: event.state,
        source: event.source.clone(),
        ts: event.at_ms,
        run_id: if event.run_id.is_empty() {
            event.agent_id.clone()
        } else {
            event.run_id.clone()
        },
        row_kind: if is_tool_boundary {
            RowKind::ToolBoundary
        } else {
            RowKind::Fact
        },
        label,
        detail,
        expandable: true,
    }
}

fn row_from_issue(issue: &ReplayIssue) -> TimelineRow {
    let description = describe_issue(issue);
    TimelineRow {
        id: String::new(),
        seq: 0,
        kind: "replay.issue".to_string(),
        state: EventState::Unknown,
        source: String::new(),
        ts: 0,
        run_id: issue_run_id(issue).to_string(),
        row_kind: RowKind::Issue,
        label: format!("warning · {description}"),
        detail: format!("{description}\nthis is a replay integrity problem, not a recorded event"),
        expandable: true,
    }
}

fn row_from_open_item(item: &OpenItem) -> TimelineRow {
    let description = describe_open(item);
    TimelineRow {
        id: String::new(),
        seq: 0,
        kind: "replay.open".to_string(),
        state: EventState::Unknown,
        source: String::new(),
        ts: 0,
        run_id: open_run_id(item).to_string(),
        row_kind: RowKind::OpenItem,
        label: format!("OPEN · {description}"),
        detail: format!(
            "{description}\nleft OPEN — loading history never replays a run, resumes a subprocess, or applies an edit"
        ),
        expandable: true,
    }
}

fn issue_run_id(issue: &ReplayIssue) -> &str {
    match issue {
        ReplayIssue::DuplicateId { run_id, .. }
        | ReplayIssue::DuplicateSeq { run_id, .. }
        | ReplayIssue::SeqGap { run_id, .. }
        | ReplayIssue::SeqNotMonotonic { run_id, .. }
        | ReplayIssue::OrphanParent { run_id, .. }
        | ReplayIssue::SelfParent { run_id }
        | ReplayIssue::CycleInTree { run_id }
        | ReplayIssue::ResultWithoutCall { run_id, .. } => run_id,
        ReplayIssue::MissingRunIdentity { .. } | ReplayIssue::MalformedLine { .. } => "",
    }
}

fn open_run_id(item: &OpenItem) -> &str {
    match item {
        OpenItem::UnknownVersion { run_id, .. } | OpenItem::HistoryNotRecorded { run_id } => run_id,
    }
}

fn describe_issue(issue: &ReplayIssue) -> String {
    match issue {
        ReplayIssue::DuplicateId { run_id, id, seq } => {
            format!("duplicate event id {id} in {run_id} (seq {seq})")
        }
        ReplayIssue::DuplicateSeq { run_id, seq } => {
            format!("duplicate sequence {seq} in {run_id}")
        }
        ReplayIssue::SeqGap {
            run_id,
            expected,
            found,
        } => format!("sequence gap in {run_id}: expected {expected}, found {found}"),
        ReplayIssue::SeqNotMonotonic {
            run_id,
            previous,
            found,
        } => format!("sequence went backwards in {run_id}: {previous} to {found}"),
        ReplayIssue::OrphanParent { run_id, parent_id } => {
            format!("parent {parent_id} of {run_id} has no recorded events")
        }
        ReplayIssue::SelfParent { run_id } => {
            format!("{run_id} records itself as its own parent")
        }
        ReplayIssue::CycleInTree { run_id } => format!("parent cycle through {run_id}"),
        ReplayIssue::ResultWithoutCall { run_id, detail } => {
            format!("tool result without a matching call in {run_id}: {detail}")
        }
        ReplayIssue::MissingRunIdentity { line } => {
            format!("missing run identity on line {line}")
        }
        ReplayIssue::MalformedLine { line, snippet } => {
            format!("malformed line {line}: {snippet}")
        }
    }
}

fn describe_open(item: &OpenItem) -> String {
    match item {
        OpenItem::UnknownVersion {
            run_id,
            event_id,
            version,
        } => {
            format!("unknown schema version {version} for {run_id} (event {event_id})")
        }
        OpenItem::HistoryNotRecorded { run_id } => {
            format!("history not recorded for {run_id}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use memnoth_agent::delegate::event_schema::{
        EVENT_SCHEMA_VERSION, RUN_DONE, RUN_SPAWN, RUN_START, SOURCE_DELEGATOR,
    };

    fn v1(run: &str, seq: u64, kind: &str, detail: &str) -> ChildEvent {
        ChildEvent {
            event: kind.to_string(),
            agent_id: run.to_string(),
            at_ms: 1000 + seq as i64,
            detail: detail.to_string(),
            id: format!("{run}:{seq}"),
            seq,
            run_id: run.to_string(),
            session_id: Some("s-1".to_string()),
            turn_id: None,
            parent_id: None,
            source: SOURCE_DELEGATOR.to_string(),
            state: state_for_kind(kind),
            version: EVENT_SCHEMA_VERSION,
        }
    }

    fn legacy(run: &str, kind: &str, detail: &str) -> ChildEvent {
        ChildEvent {
            event: kind.to_string(),
            agent_id: run.to_string(),
            at_ms: 0,
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

    fn replay(
        events: Vec<ChildEvent>,
        issues: Vec<ReplayIssue>,
        open_items: Vec<OpenItem>,
    ) -> ReplayResult {
        ReplayResult {
            runs: BTreeMap::new(),
            events,
            issues,
            open_items,
        }
    }

    #[test]
    fn clean_v1_rows_keep_identity_state_source_and_ts() {
        let input = replay(
            vec![
                v1("root", 1, RUN_SPAWN, "spawned scout"),
                v1("root", 2, RUN_START, "find it"),
                v1("root", 3, TOOL_USE, "read_file"),
                v1("root", 4, RUN_DONE, "done"),
            ],
            vec![],
            vec![],
        );
        let timeline = ReplayTimeline::from_replay(&input);

        assert_eq!(timeline.len(), 4);
        let first = timeline.row(0).expect("first row");
        assert_eq!(first.id, "root:1");
        assert_eq!(first.seq, 1);
        assert_eq!(first.kind, RUN_SPAWN);
        assert_eq!(first.state, EventState::Spawned);
        assert_eq!(first.source, SOURCE_DELEGATOR);
        assert_eq!(first.ts, 1001);
        assert_eq!(first.run_id, "root");
        assert_eq!(first.row_kind, RowKind::Fact);
        assert_eq!(timeline.count(RowKind::Fact), 3);
        assert_eq!(timeline.count(RowKind::ToolBoundary), 1);
        assert!(
            timeline
                .rows()
                .iter()
                .all(|row| !row.is_warning() && !row.is_open())
        );
    }

    #[test]
    fn legacy_v0_rows_default_identity_without_inventing_issues() {
        let input = replay(
            vec![
                legacy("old", RUN_START, "go"),
                legacy("old", TOOL_USE, "grep"),
            ],
            vec![],
            vec![],
        );
        let timeline = ReplayTimeline::from_replay(&input);

        assert_eq!(timeline.len(), 2);
        let row = timeline.row(0).expect("first row");
        assert!(row.id.is_empty());
        assert_eq!(row.seq, 0);
        assert_eq!(row.source, "");
        assert_eq!(row.ts, 0);
        assert_eq!(row.state, EventState::Unknown);
        assert_eq!(row.run_id, "old", "legacy identity falls back to agent_id");
        assert_eq!(row.row_kind, RowKind::Fact);
        assert!(
            timeline.rows().iter().all(|row| !row.is_warning()),
            "a legacy line is not an integrity problem"
        );
    }

    #[test]
    fn issues_render_as_warning_rows() {
        let input = replay(
            vec![],
            vec![
                ReplayIssue::SeqGap {
                    run_id: "r".to_string(),
                    expected: 2,
                    found: 4,
                },
                ReplayIssue::ResultWithoutCall {
                    run_id: "r".to_string(),
                    detail: "orphan".to_string(),
                },
                ReplayIssue::MalformedLine {
                    line: 3,
                    snippet: "{bad".to_string(),
                },
            ],
            vec![],
        );
        let timeline = ReplayTimeline::from_replay(&input);

        assert_eq!(timeline.len(), 3);
        assert_eq!(timeline.count(RowKind::Issue), 3);
        assert!(timeline.rows().iter().all(|row| row.is_warning()));
        assert!(timeline.row(0).unwrap().label.contains("sequence gap"));
        assert!(timeline.row(1).unwrap().label.contains("orphan"));
        assert!(timeline.row(2).unwrap().label.contains("line 3"));
        assert_eq!(timeline.row(0).unwrap().kind, "replay.issue");
        assert_eq!(timeline.row(2).unwrap().run_id, "");
    }

    #[test]
    fn open_items_render_as_explicit_open_rows() {
        let input = replay(
            vec![],
            vec![],
            vec![
                OpenItem::UnknownVersion {
                    run_id: "r".to_string(),
                    event_id: "r:1".to_string(),
                    version: 7,
                },
                OpenItem::HistoryNotRecorded {
                    run_id: "ghost".to_string(),
                },
            ],
        );
        let timeline = ReplayTimeline::from_replay(&input);

        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline.count(RowKind::OpenItem), 2);
        assert!(timeline.rows().iter().all(|row| row.is_open()));
        assert!(
            timeline
                .rows()
                .iter()
                .all(|row| row.label.starts_with("OPEN · "))
        );
        assert_eq!(timeline.row(0).unwrap().kind, "replay.open");
        assert!(timeline.row(0).unwrap().label.contains("version 7"));
        assert_eq!(timeline.row(1).unwrap().run_id, "ghost");
    }

    #[test]
    fn unrecorded_history_is_open_and_never_says_no_tools_ran() {
        let input = replay(
            vec![],
            vec![],
            vec![OpenItem::HistoryNotRecorded {
                run_id: "ghost".to_string(),
            }],
        );
        let timeline = ReplayTimeline::from_replay(&input);
        let row = timeline.row(0).expect("open row");

        assert_eq!(row.row_kind, RowKind::OpenItem);
        assert!(row.label.contains("history not recorded"));
        assert!(!row.label.to_lowercase().contains("no tools ran"));
        assert!(!row.detail.to_lowercase().contains("no tools ran"));
        assert!(!row.label.contains("completed"));
    }

    #[test]
    fn unknown_kinds_are_preserved_verbatim_and_are_not_successes() {
        let input = replay(
            vec![v1("k", 1, "some.future.kind", "payload")],
            vec![],
            vec![],
        );
        let timeline = ReplayTimeline::from_replay(&input);
        let row = timeline.row(0).expect("row");

        assert_eq!(row.kind, "some.future.kind");
        assert!(row.label.contains("some.future.kind"));
        assert!(row.label.contains("payload"));
        assert_eq!(
            row.row_kind,
            RowKind::Fact,
            "an unknown kind is not terminal"
        );
        assert_ne!(row.state, EventState::Completed);
        assert!(row.detail.contains("not a success"));
        assert!(!row.label.to_lowercase().contains("reasoning"));
    }

    #[test]
    fn tool_use_is_a_boundary_row_that_never_claims_a_result() {
        let input = replay(vec![v1("t", 1, TOOL_USE, "grep")], vec![], vec![]);
        let timeline = ReplayTimeline::from_replay(&input);
        let row = timeline.row(0).expect("row");

        assert_eq!(row.row_kind, RowKind::ToolBoundary);
        assert_eq!(row.kind, TOOL_USE);
        assert!(row.label.contains("tool.use"));
        assert!(row.label.contains("grep"));
        assert!(row.detail.contains("no result"));
        assert!(!row.label.contains("completed"));
        assert!(!row.label.contains("success"));
    }

    #[test]
    fn expansion_is_per_row_bounded_and_does_not_mutate_the_replay() {
        let input = replay(
            vec![v1("r", 1, RUN_START, "go"), v1("r", 2, TOOL_USE, "grep")],
            vec![],
            vec![],
        );
        let before = format!("{input:?}");
        let mut timeline = ReplayTimeline::from_replay(&input);

        assert!(!timeline.is_expanded(0));
        assert!(timeline.toggle_expanded(0));
        assert!(timeline.is_expanded(0));
        assert!(!timeline.is_expanded(1), "expansion is per row");
        assert!(!timeline.toggle_expanded(99), "out of range is refused");
        assert!(!timeline.is_expanded(99));
        assert!(!timeline.set_expanded(99, true));

        timeline.collapse_all();
        assert!(
            timeline
                .rows()
                .iter()
                .enumerate()
                .all(|(i, _)| !timeline.is_expanded(i))
        );
        assert_eq!(
            format!("{input:?}"),
            before,
            "building a timeline is a pure read"
        );
    }

    #[test]
    fn narrow_labels_truncate_on_char_boundaries_without_panicking() {
        let timeline = ReplayTimeline::from_replay(&replay(
            vec![v1(
                "r",
                1,
                RUN_START,
                "a very long detail that will not fit",
            )],
            vec![],
            vec![],
        ));
        let row = timeline.row(0).expect("row");

        assert_eq!(row.narrow_label(0), "");
        assert_eq!(row.narrow_label(1).chars().count(), 1);
        assert_eq!(row.narrow_label(3).chars().count(), 3);
        assert!(row.narrow_label(3).ends_with('…'));
        let wide = row.narrow_label(10_000);
        assert_eq!(wide, row.label);
    }

    #[test]
    fn an_empty_replay_produces_no_rows() {
        let timeline = ReplayTimeline::from_replay(&ReplayResult::default());

        assert!(timeline.is_empty());
        assert_eq!(timeline.len(), 0);
        assert!(timeline.row(0).is_none());
        assert!(!timeline.is_expanded(0));
        let mut timeline = timeline;
        assert!(!timeline.toggle_expanded(0));
    }

    #[test]
    fn partial_events_never_panic() {
        let partial = ChildEvent {
            event: String::new(),
            agent_id: String::new(),
            at_ms: -5,
            detail: String::new(),
            id: String::new(),
            seq: 0,
            run_id: String::new(),
            session_id: None,
            turn_id: None,
            parent_id: None,
            source: String::new(),
            state: EventState::Unknown,
            version: 0,
        };
        let timeline = ReplayTimeline::from_replay(&replay(
            vec![partial],
            vec![ReplayIssue::MissingRunIdentity { line: 1 }],
            vec![OpenItem::HistoryNotRecorded {
                run_id: "ghost".to_string(),
            }],
        ));

        assert_eq!(timeline.len(), 3);
        assert_eq!(timeline.row(0).unwrap().ts, -5);
        assert_eq!(timeline.row(0).unwrap().label, "");
        assert!(timeline.row(1).unwrap().is_warning());
        assert!(timeline.row(2).unwrap().is_open());
    }

    #[test]
    fn labels_never_claim_auto_replay_or_fabricated_reasoning() {
        let timeline = ReplayTimeline::from_replay(&replay(
            vec![
                v1("r", 1, RUN_START, "go"),
                v1("r", 2, TOOL_USE, "grep"),
                v1("r", 3, "future.kind", "payload"),
            ],
            vec![ReplayIssue::SeqGap {
                run_id: "r".to_string(),
                expected: 3,
                found: 9,
            }],
            vec![OpenItem::HistoryNotRecorded {
                run_id: "ghost".to_string(),
            }],
        ));
        let labels: Vec<String> = timeline
            .rows()
            .iter()
            .map(|row| row.label.to_lowercase())
            .collect();

        assert!(
            labels.iter().all(|label| !label.contains("thinking")
                && !label.contains("reasoning")
                && !label.contains("auto-replay")
                && !label.contains("applied")),
            "{labels:?}"
        );
    }
}
