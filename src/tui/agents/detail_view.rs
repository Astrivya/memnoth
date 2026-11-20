//! Read-only run detail pages for the Agent Deck.
//!
//! Every line here is projected from a persisted [`ChildEvent`] or a
//! [`ChildInfo`] row. A child log records tool *use* at a boundary, never tool
//! results and never reasoning, so this module never prints a "thinking" or
//! "reasoning" section: the typed [`memnoth_agent::delegate::event_schema::EventState`]
//! and the event kind are the activity. Missing values are named as missing,
//! not rendered as zeroes.

use memnoth_agent::{ChildEvent, delegate::event_schema::RUN_DONE};

use crate::tui::agents::ChildInfo;

/// The four read-only pages of a run's detail view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailPage {
    #[default]
    Overview,
    Activity,
    Result,
    Details,
}

impl DetailPage {
    /// Tab order of the pages.
    pub const ALL: [Self; 4] = [Self::Overview, Self::Activity, Self::Result, Self::Details];

    /// The label used in the page tab strip.
    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Activity => "Activity",
            Self::Result => "Result",
            Self::Details => "Details",
        }
    }

    /// Next page on Tab.
    pub fn next(self) -> Self {
        match self {
            Self::Overview => Self::Activity,
            Self::Activity => Self::Result,
            Self::Result => Self::Details,
            Self::Details => Self::Overview,
        }
    }

    /// Previous page on Shift+Tab.
    pub fn previous(self) -> Self {
        match self {
            Self::Overview => Self::Details,
            Self::Activity => Self::Overview,
            Self::Result => Self::Activity,
            Self::Details => Self::Result,
        }
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }
}

/// Human age of an event, from its recorded timestamp only.
fn age(at_ms: i64) -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default();
    let seconds = now_ms.saturating_sub(at_ms).max(0) / 1_000;
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else {
        format!("{}h ago", seconds / 3_600)
    }
}

/// The `run.done` receipt, when the run actually finished.
fn done_event(events: &[ChildEvent]) -> Option<&ChildEvent> {
    events.iter().find(|event| event.event == RUN_DONE)
}

/// Cost from the `run.done` receipt, when it carries a measured cost.
pub fn cost_from_events(events: &[ChildEvent]) -> Option<String> {
    let done = done_event(events)?;
    let value: serde_json::Value = serde_json::from_str(&done.detail).ok()?;
    let tokens = value.get("cost")?.get("tokens")?.as_u64()?;
    let usd = value.get("cost")?.get("usd")?.as_f64()?;
    Some(format!("{tokens} tokens · ${usd:.4}"))
}

/// The child's own result summary, when a result was recorded.
fn result_summary(events: &[ChildEvent]) -> Option<String> {
    let done = done_event(events)?;
    let value: serde_json::Value = serde_json::from_str(&done.detail).ok()?;
    let summary = value.get("summary")?.as_str()?.trim();
    (!summary.is_empty()).then(|| summary.to_string())
}

/// Overview: why the run exists, its persisted state, the newest *actual*
/// event, and whether a result came back.
pub fn overview_lines(child: &ChildInfo, events: &[ChildEvent]) -> Vec<String> {
    let mut lines = vec![
        "Assignment".to_string(),
        child.task.clone(),
        String::new(),
        format!("state: {} {}", child.status.glyph(), child.status.label()),
        format!("role: {}", child.role),
        format!("parent: {}", child.parent_id.as_deref().unwrap_or("none")),
        String::new(),
        "Now".to_string(),
    ];
    match events.last() {
        None => lines.push("No activity reported yet.".to_string()),
        Some(event) => lines.push(format!(
            "[{}] {}  ·  {}",
            event.state.as_str(),
            event.event,
            age(event.at_ms)
        )),
    }
    lines.push(String::new());
    lines.push("Work produced".to_string());
    match result_summary(events) {
        Some(summary) => lines.extend(summary.lines().map(str::to_string)),
        None => lines.push("No result returned yet.".to_string()),
    }
    lines
}

/// Ordered activity: sequence, typed state, event kind and age. A `tool.use`
/// row shows only the tool name, which is the fact the log records; nothing is
/// labelled reasoning and no result text is presented as a thought.
pub fn activity_lines(events: &[ChildEvent]) -> Vec<String> {
    if events.is_empty() {
        return vec!["No activity reported yet.".to_string()];
    }
    events
        .iter()
        .map(|event| {
            let mut line = format!(
                "{:>3}. [{:<16}] {}  ·  {}",
                event.seq,
                event.state.as_str(),
                event.event,
                age(event.at_ms)
            );
            if event.event == memnoth_agent::delegate::event_schema::TOOL_USE {
                line.push_str(&format!("  ·  tool: {}", event.detail));
            }
            line
        })
        .collect()
}

/// Result: the outcome the child returned, separate from the assignment.
/// A run with no recorded result says "not recorded" rather than inventing one.
pub fn result_lines(events: &[ChildEvent]) -> Vec<String> {
    let Some(done) = done_event(events) else {
        return vec!["not recorded".to_string()];
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&done.detail) else {
        return vec!["outcome recorded but not readable".to_string()];
    };
    let mut lines = vec!["Result".to_string()];
    match value.get("summary").and_then(|summary| summary.as_str()) {
        Some(summary) if !summary.trim().is_empty() => {
            lines.extend(summary.lines().map(str::to_string));
        }
        _ => lines.push("not recorded".to_string()),
    }
    if let Some(exit) = value.get("exit").and_then(|exit| exit.as_str()) {
        lines.push(format!("exit: {exit}"));
    }
    for (field, label) in [
        ("findings", "findings"),
        ("changed_files", "files changed"),
        ("commands_run", "commands run"),
    ] {
        if let Some(count) = value.get(field).and_then(|field| field.as_array()) {
            lines.push(format!("{label}: {}", count.len()));
        }
    }
    lines
}

/// Details: exact identifiers, recorded provenance and verified cost.
pub fn details_lines(child: &ChildInfo, events: &[ChildEvent]) -> Vec<String> {
    let mut lines = vec![
        format!("run: {}", child.agent_id),
        format!("parent: {}", child.parent_id.as_deref().unwrap_or("none")),
        format!(
            "session: {}",
            child.session_id.as_deref().unwrap_or("not recorded")
        ),
        format!("role: {}", child.role),
        format!("spawn: #{}", child.spawn_seq),
        format!("depth: {}", child.depth),
        format!(
            "model: {}",
            child.model.as_deref().unwrap_or("not recorded")
        ),
        format!(
            "worktree: {}",
            child.worktree.as_deref().unwrap_or("not recorded")
        ),
    ];
    match events.first() {
        Some(event) => lines.push(format!(
            "first event: {}  ·  {}",
            event.event,
            age(event.at_ms)
        )),
        None => lines.push("first event: not recorded".to_string()),
    }
    match done_event(events).or_else(|| events.last()) {
        Some(event) => lines.push(format!(
            "last event: {}  ·  {}",
            event.event,
            age(event.at_ms)
        )),
        None => lines.push("last event: not recorded".to_string()),
    }
    lines.push(format!("events: {}", events.len()));
    match cost_from_events(events) {
        Some(cost) => lines.push(format!("cost: {cost}")),
        None => lines.push("cost: not recorded".to_string()),
    }
    lines
}

/// The lines for one page, with the common title removed by the caller.
pub fn page_lines(page: DetailPage, child: &ChildInfo, events: &[ChildEvent]) -> Vec<String> {
    match page {
        DetailPage::Overview => overview_lines(child, events),
        DetailPage::Activity => activity_lines(events),
        DetailPage::Result => result_lines(events),
        DetailPage::Details => details_lines(child, events),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memnoth_agent::delegate::event_schema::{EventState, SOURCE_DELEGATOR, TOOL_USE};

    fn child() -> ChildInfo {
        ChildInfo {
            agent_id: "run-1".to_string(),
            parent_id: None,
            root_id: Some("run-1".to_string()),
            session_id: Some("session-1".to_string()),
            role: "scout".to_string(),
            task: "map the session endpoints".to_string(),
            status: crate::tui::agents::AgentStatus::Running,
            spawn_seq: 1,
            depth: 0,
            model: Some("model-x".to_string()),
            worktree: None,
            unread: true,
        }
    }

    fn event(kind: &str, seq: u64, detail: &str, state: EventState) -> ChildEvent {
        ChildEvent {
            event: kind.to_string(),
            agent_id: "run-1".to_string(),
            at_ms: 1_700_000_000_000,
            detail: detail.to_string(),
            id: format!("run-1:{seq}"),
            seq,
            run_id: "run-1".to_string(),
            session_id: Some("session-1".to_string()),
            turn_id: None,
            parent_id: None,
            source: SOURCE_DELEGATOR.to_string(),
            state,
            version: 1,
        }
    }

    #[test]
    fn deck_detail_pages_render_from_events_without_inventing_reasoning() {
        let events = vec![
            event(
                "run.start",
                1,
                "map the session endpoints",
                EventState::Started,
            ),
            event(TOOL_USE, 2, "read_file src/auth.rs", EventState::ToolUse),
        ];
        let child = child();
        let pages: Vec<String> = DetailPage::ALL
            .iter()
            .flat_map(|page| page_lines(*page, &child, &events))
            .collect();
        let text = pages.join("\n");

        assert!(text.contains("map the session endpoints"), "{text}");
        assert!(text.contains("[tool_use]"), "{text}");
        assert!(text.contains("tool: read_file src/auth.rs"), "{text}");
        assert!(text.contains("run: run-1"), "{text}");
        assert!(text.contains("cost: not recorded"), "{text}");
        assert!(!text.to_lowercase().contains("reasoning"), "{text}");
        assert!(!text.to_lowercase().contains("thinking"), "{text}");
    }

    #[test]
    fn deck_missing_result_is_not_recorded() {
        let events = vec![event(
            "run.start",
            1,
            "map the session endpoints",
            EventState::Started,
        )];
        assert_eq!(result_lines(&events), vec!["not recorded".to_string()]);

        let done = event(
            RUN_DONE,
            2,
            r#"{"summary":"found 3 routes","exit":"completed","findings":[],"changed_files":[],"commands_run":[],"cost":{"tokens":10,"usd":0.01}}"#,
            EventState::Completed,
        );
        let pair = vec![events[0].clone(), done];
        let result = result_lines(&pair).join("\n");
        assert!(result.contains("found 3 routes"), "{result}");
        assert!(result.contains("exit: completed"), "{result}");
        assert!(result.contains("findings: 0"), "{result}");
        assert_eq!(cost_from_events(&pair).unwrap(), "10 tokens · $0.0100");
    }

    #[test]
    fn deck_activity_keeps_sequence_and_typed_state_without_a_result_payload() {
        let events = vec![
            event("run.start", 1, "task", EventState::Started),
            event(TOOL_USE, 2, "cargo test", EventState::ToolUse),
        ];
        let activity = activity_lines(&events).join("\n");
        assert!(activity.contains("1. [started"), "{activity}");
        assert!(activity.contains("2. [tool_use"), "{activity}");
        assert!(activity.contains("tool: cargo test"), "{activity}");
        assert!(
            !activity.contains("{"),
            "result payloads never leak into activity"
        );
    }

    #[test]
    fn deck_detail_page_navigation_cycles_in_order() {
        assert_eq!(DetailPage::Overview.next(), DetailPage::Activity);
        assert_eq!(DetailPage::Details.next(), DetailPage::Overview);
        assert_eq!(DetailPage::Overview.previous(), DetailPage::Details);
        assert_eq!(DetailPage::from_index(2), Some(DetailPage::Result));
    }
}
