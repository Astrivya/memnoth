//! Agent result cards: inline delegation cards in the parent timeline.
//!
//! Each card shows status, task, live activity, files, tests, belief
//! conflicts, receipt, and actions: `[Enter] open`, `[Space] peek`,
//! `[m] merge`, `[p] receipt`.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::agents::{AgentStatus, ChildInfo};
use crate::tui::theme::{Tokens, tokens};

/// One result card for a child agent in the parent timeline.
#[derive(Debug, Clone)]
pub struct AgentCard {
    pub child: ChildInfo,
    pub activity: String,
    pub files: Vec<String>,
    pub tests: Vec<String>,
    pub belief_conflicts: Vec<String>,
    pub receipt: Option<String>,
    /// Tool commands the child actually ran, reduced to their first argv word.
    /// Empty when no result was received; never guessed.
    pub tools_used: Vec<String>,
    /// Number of child runs, when a caller has that data. `None` means unknown,
    /// and no count is invented in that case.
    pub child_count: Option<usize>,
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
            tools_used: Vec::new(),
            child_count: None,
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
        let tools: Vec<String> = result
            .commands_run
            .iter()
            .filter_map(|command| command.argv.first().cloned())
            .collect();
        card.tools_used = unique_nonempty(tools.iter());
        card.receipt =
            Some(serde_json::to_string_pretty(result).unwrap_or_else(|_| result.summary.clone()));
        card
    }

    /// The card as one aggregated transcript line.
    ///
    /// The parent timeline is not a second copy of the child deck. It gets the
    /// state, the identity, the counts and a one-line result summary — never
    /// the file list, command tails, conflicts or receipt payload. Those stay
    /// in the card that the on-demand view renders.
    pub fn text(&self) -> String {
        let mut line = format!(
            "{} {} — {} ({})",
            self.child.status.glyph(),
            self.child.status.label(),
            self.child.role,
            self.child.agent_id
        );
        if !self.child.task.is_empty() {
            line.push_str(&format!(" — {}", one_line(&self.child.task)));
        }
        if let Some(count) = self.child_count {
            let word = if count == 1 { "child" } else { "children" };
            line.push_str(&format!(" · {count} {word}"));
        }
        if !self.tools_used.is_empty() {
            let count = self.tools_used.len();
            line.push_str(&format!(" · {count} {} used", plural(count, "tool")));
        }
        if self.child.status == AgentStatus::Done && !self.activity.is_empty() {
            line.push_str(&format!(" · result: {}", one_line(&self.activity)));
        }
        line
    }

    /// Render the full card as lines for the on-demand view.
    pub fn render(&self) -> Vec<Line<'static>> {
        self.render_with_tokens(tokens())
    }

    fn render_with_tokens(&self, t: Tokens) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let glyph = self.child.status.glyph();
        // The glyph and the literal state word carry the meaning; ink only
        // reinforces it. Under NO_COLOR every token resolves to Reset and the
        // row still reads the same.
        let state_style = match self.child.status {
            AgentStatus::Starting | AgentStatus::Running => Style::default().fg(t.accent),
            AgentStatus::Waiting | AgentStatus::Parked => Style::default().fg(t.warning),
            AgentStatus::Done => Style::default().fg(t.success),
            AgentStatus::Failed | AgentStatus::Killed | AgentStatus::Orphaned => {
                Style::default().fg(t.error)
            }
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {glyph} "), state_style),
            Span::styled(
                format!("{} ", self.child.status.label()),
                state_style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} ({})", self.child.role, self.child.agent_id),
                Style::default().fg(t.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" — {}", one_line(&self.child.task)),
                Style::default().fg(t.text),
            ),
        ]));

        let mut counts = Vec::new();
        if let Some(count) = self.child_count {
            let word = if count == 1 { "child" } else { "children" };
            counts.push(format!("{count} {word}"));
        }
        if !self.tools_used.is_empty() {
            let count = self.tools_used.len();
            counts.push(format!("{count} {} used", plural(count, "tool")));
        }
        if !counts.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("    {}", counts.join(" · ")),
                Style::default().fg(t.muted),
            )));
        }

        if self.child.status == AgentStatus::Done && !self.activity.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("    result: ", Style::default().fg(t.muted)),
                Span::styled(one_line(&self.activity), Style::default().fg(t.text)),
            ]));
        } else if !self.activity.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("    activity: ", Style::default().fg(t.muted)),
                Span::styled(one_line(&self.activity), Style::default().fg(t.text)),
            ]));
        }
        if !self.files.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("    files: ", Style::default().fg(t.muted)),
                Span::styled(self.files.join(", "), Style::default().fg(t.text)),
            ]));
        }
        if !self.tests.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("    tests: ", Style::default().fg(t.muted)),
                Span::styled(self.tests.join(", "), Style::default().fg(t.text)),
            ]));
        }
        if !self.belief_conflicts.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("    conflicts: {}", self.belief_conflicts.len()),
                Style::default().fg(t.error),
            )));
        }
        if self.receipt.is_some() {
            lines.push(Line::from(Span::styled(
                "    receipt: [p] view",
                Style::default().fg(t.muted),
            )));
        }
        lines.push(Line::from(Span::styled(
            "    [Enter] open  [Space] peek  [m] merge  [p] receipt",
            Style::default().fg(t.muted),
        )));

        lines
    }
}

fn plural(count: usize, singular: &str) -> String {
    if count == 1 {
        singular.to_string()
    } else {
        format!("{singular}s")
    }
}

/// Collapse a result summary to one line so a card cannot inflate the parent
/// timeline with a child's multi-line log.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

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
            card.tools_used,
            vec!["cargo"],
            "two commands, one tool: no invented count"
        );
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
        assert!(text.contains("running"), "a literal state word: {text}");
    }

    #[test]
    fn the_parent_timeline_gets_one_aggregate_line_not_a_child_log() {
        let mut info = child();
        info.status = AgentStatus::Done;
        let mut card = AgentCard::from_result(
            info,
            &result(),
            &["src/request.rs".to_string()],
            vec!["a conflict".to_string()],
        );
        card.child_count = Some(3);
        let text = card.text();

        assert_eq!(text.lines().count(), 1, "one row in the parent timeline");
        assert!(text.starts_with("✓ returned"), "{text}");
        assert!(text.contains("worker (abc123)"), "{text}");
        assert!(text.contains(" · 3 children"), "{text}");
        assert!(text.contains(" · 1 tool used"), "{text}");
        assert!(
            text.contains("result: scout ran 2 step(s): read_file, grep"),
            "{text}"
        );
        for internal in [
            "files:",
            "tests:",
            "conflicts:",
            "receipt:",
            "cargo test (exit",
        ] {
            assert!(!text.contains(internal), "child internals leaked: {text}");
        }
        // Unknown counts stay unsaid rather than becoming a fabricated zero.
        let unknown = AgentCard::new(child());
        assert!(!unknown.text().contains("children"));
        assert!(!unknown.text().contains("tool used"));
    }

    #[test]
    fn card_styling_comes_only_from_tokens_and_no_color_keeps_the_cues() {
        let source = include_str!("cards.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            !production.contains("Color::"),
            "production card code must use semantic tokens, not raw Color::"
        );

        let t = Tokens::for_palette(crate::tui::theme::Palette::NoColor);
        let mut info = child();
        info.status = AgentStatus::Failed;
        let card = AgentCard::new(info);
        let lines = card.render_with_tokens(t);
        let spans = &lines[0].spans;
        assert_eq!(spans[0].content.as_ref(), " ✗ ");
        assert_eq!(spans[0].style.fg, Some(t.error));
        assert_eq!(
            spans[1].content.as_ref(),
            "failed ",
            "the literal word survives colour stripping"
        );
        for line in &lines {
            for span in &line.spans {
                assert_eq!(span.style.bg, None, "no card row paints a fill: {span:?}");
            }
        }
        assert!(card.text().starts_with("✗ failed"), "{}", card.text());
    }

    #[test]
    fn a_result_summary_is_collapsed_to_a_single_line() {
        let mut info = child();
        info.status = AgentStatus::Done;
        let mut card = AgentCard::new(info);
        card.activity = "first line\nsecond line\tthird line".to_string();
        let text = card.text();
        assert_eq!(text.lines().count(), 1);
        assert!(
            text.contains("result: first line second line third line"),
            "{text}"
        );
    }
}
