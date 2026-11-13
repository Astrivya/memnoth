//! Agent result cards: inline delegation cards in the parent timeline.
//!
//! Each card shows status, task, live activity, files, tests, belief
//! conflicts, receipt, and actions: `[Enter] open`, `[Space] peek`,
//! `[m] merge`, `[p] receipt`.

use ratatui::text::{Line, Span};

use crate::tui::agents::{AgentStatus, ChildInfo};

/// One result card for a child agent in the parent timeline.
#[derive(Debug, Clone)]
pub struct AgentCard {
    pub child: ChildInfo,
    pub activity: String,
    pub files: Vec<String>,
    pub tests: Vec<String>,
    pub belief_conflicts: Vec<String>,
    pub receipt: Option<String>,
}

impl AgentCard {
    pub fn new(child: ChildInfo) -> Self {
        Self {
            child,
            activity: String::new(),
            files: Vec::new(),
            tests: Vec::new(),
            belief_conflicts: Vec::new(),
            receipt: None,
        }
    }

    /// Build the parent-timeline card from the result a child actually
    /// returned. The card is a projection of `AgentResult`; it must not invent
    /// activity, files, tests, conflicts, or a receipt the run did not carry.
    ///
    /// Worktree and belief-conflict evidence are passed separately because
    /// they are gathered by the parent from disk and the vault, whereas the
    /// activity, command, and receipt fields come from the child result.
    pub fn from_result(
        child: ChildInfo,
        result: &memnoth_agent::result::AgentResult,
        worktree_files: &[String],
        belief_conflicts: Vec<String>,
    ) -> Self {
        let mut card = Self::new(child);
        card.activity = result.summary.clone();
        card.files = unique_nonempty(result.changed_files.iter().chain(worktree_files.iter()));
        card.tests = result
            .commands_run
            .iter()
            .filter(|command| command_is_test(&command.argv))
            .map(|command| format!("{} (exit {})", command.argv.join(" "), command.exit))
            .collect();
        card.belief_conflicts = belief_conflicts;
        card.receipt =
            Some(serde_json::to_string_pretty(result).unwrap_or_else(|_| result.summary.clone()));
        card
    }

    /// The card as transcript text: the timeline stores strings, so this is how
    /// a card lands in the parent transcript rather than only in the deck.
    pub fn text(&self) -> String {
        self.render()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Render the card as lines for the transcript.
    pub fn render(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let glyph = self.child.status.glyph();
        let color = match self.child.status {
            AgentStatus::Running => Some(Color::Cyan),
            AgentStatus::Done => Some(Color::Green),
            AgentStatus::Failed => Some(Color::Red),
            AgentStatus::Waiting => Some(Color::Yellow),
            _ => None,
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", glyph),
                color.map(|c| Style::default().fg(c)).unwrap_or_default(),
            ),
            Span::styled(
                format!(" {} — {}", self.child.agent_id, self.child.task),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));

        if !self.activity.is_empty() {
            lines.push(Line::from(Span::raw(format!(
                "    activity: {}",
                self.activity
            ))));
        }
        if !self.files.is_empty() {
            lines.push(Line::from(Span::raw(format!(
                "    files: {}",
                self.files.join(", ")
            ))));
        }
        if !self.tests.is_empty() {
            lines.push(Line::from(Span::raw(format!(
                "    tests: {}",
                self.tests.join(", ")
            ))));
        }
        if !self.belief_conflicts.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("    conflicts: {}", self.belief_conflicts.len()),
                Style::default().fg(Color::Red),
            )));
        }
        if self.receipt.is_some() {
            lines.push(Line::from(Span::raw("    receipt: [p] view")));
        }
        lines.push(Line::from(Span::raw(
            "    [Enter] open  [Space] peek  [m] merge  [p] receipt",
        )));

        lines
    }
}

use ratatui::style::{Color, Modifier, Style};

fn unique_nonempty<'a>(values: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let mut unique = Vec::new();
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !unique.iter().any(|existing| existing == value) {
            unique.push(value.to_string());
        }
    }
    unique
}

fn command_is_test(argv: &[String]) -> bool {
    let Some(command) = argv.first().map(String::as_str) else {
        return false;
    };
    let command = command.to_ascii_lowercase();
    let arguments: Vec<String> = argv[1..]
        .iter()
        .map(|argument| argument.to_ascii_lowercase())
        .collect();
    arguments.iter().any(|argument| {
        argument == "test"
            || argument == "tests"
            || argument.ends_with("/test")
            || argument.ends_with("\\test")
    }) || command == "test"
        || command == "tests"
        || command.ends_with("/test")
        || command.ends_with("\\test")
        || command.contains("test")
        || command.contains("nextest")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child() -> ChildInfo {
        ChildInfo {
            agent_id: "abc123".into(),
            parent_id: None,
            root_id: Some("abc123".into()),
            session_id: Some("session".into()),
            role: "worker".into(),
            task: "fix the parser".into(),
            status: AgentStatus::Running,
            spawn_seq: 1,
            depth: 0,
            model: None,
            worktree: None,
            unread: true,
        }
    }

    fn result() -> memnoth_agent::result::AgentResult {
        memnoth_agent::result::AgentResult {
            summary: "scout ran 2 step(s): read_file, grep".to_string(),
            findings: Vec::new(),
            changed_files: vec!["src/request.rs".to_string()],
            commands_run: vec![
                memnoth_agent::CommandRun {
                    argv: vec!["cargo".to_string(), "test".to_string()],
                    exit: 0,
                    tail: "3 passed".to_string(),
                },
                memnoth_agent::CommandRun {
                    argv: vec!["cargo".to_string(), "clippy".to_string()],
                    exit: 0,
                    tail: "clean".to_string(),
                },
            ],
            receipts: Vec::new(),
            proposed_beliefs: Vec::new(),
            cost: memnoth_agent::Cost {
                tokens: 12,
                usd: 0.001,
            },
            turns: 2,
            exit: memnoth_agent::Exit::Completed,
        }
    }

    #[test]
    fn a_card_is_projected_from_the_child_result() {
        let card = AgentCard::from_result(
            child(),
            &result(),
            &["src/request.rs".to_string(), "src/retry.rs".to_string()],
            vec!["request stayed open before 2026-11-13".to_string()],
        );

        assert_eq!(card.activity, "scout ran 2 step(s): read_file, grep");
        assert_eq!(card.files, vec!["src/request.rs", "src/retry.rs"]);
        assert_eq!(card.tests, vec!["cargo test (exit 0)"]);
        assert_eq!(
            card.belief_conflicts,
            vec!["request stayed open before 2026-11-13"]
        );
        let receipt = card.receipt.expect("the result carries its receipt");
        assert!(receipt.contains("\"summary\""), "{receipt}");
        assert!(receipt.contains("12"), "{receipt}");
    }

    #[test]
    fn a_card_names_the_child_and_its_task_as_transcript_text() {
        let card = AgentCard::new(child());
        let text = card.text();
        assert!(text.contains("abc123"), "{text}");
        assert!(text.contains("fix the parser"), "{text}");
        assert!(
            text.contains("\u{258c}") || text.contains("?"),
            "a status glyph: {text}"
        );
    }
}
