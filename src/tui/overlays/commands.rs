//! `/commands`: the whole registry, grouped, on one page.
//!
//! The dropdown under the prompt is for the command someone is about to run, so
//! it shows a handful of rows and leans on the filter for the rest. This is the
//! other question — *what is in here?* — and it needs the opposite shape: every
//! row, under the area it belongs to, in a page tall enough to read a group at a
//! time. Both read the same list (`palette::commands`), so a command cannot be
//! in one surface and missing from the other.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use std::collections::BTreeMap;

use crate::tui::app::App;
use crate::tui::overlays::palette::{commands, needs_arguments};
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use crate::tui::slash::display_name;
use memnoth_ui::palette::PaletteItem;

/// The headings, in the order they are drawn, keyed by a command's first word.
///
/// The key is the verb's own name (`/vault …` is under Vault) rather than a
/// second table of classifications: adding `/kg something` needs no edit here,
/// and anything whose first word is not a tool area — `/help`, `/clear`, the
/// one-word session verbs — is a session command by construction.
const GROUPS: [(&str, &str); 6] = [
    ("", "Session"),
    ("vault", "Vault"),
    ("kg", "Code graph"),
    ("tasks", "Tasks"),
    ("worktree", "Worktrees"),
    ("automation", "Automation"),
];

/// Which heading a command is drawn under.
fn group_of(command: &str) -> &'static str {
    let head = command
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default();
    GROUPS
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(head))
        .map(|(_, title)| *title)
        .unwrap_or("Session")
}

/// The browser.
#[derive(Debug)]
pub struct CommandsOverlay {
    visible: bool,
    /// What was typed to narrow the list; empty lists everything.
    filter: String,
    /// Every command, in registry order: the group is computed when drawing.
    items: Vec<PaletteItem>,
    /// Index into the *filtered* list, never into `items`.
    selected: usize,
    /// First line on screen, in drawn (wrapped) lines.
    scroll: usize,
    /// Whether the next frame should put the selection at the top. A group jump
    /// sets it so the jump lands on the heading, which is the thing that says
    /// where the jump went.
    reveal_top: bool,
}

impl CommandsOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            filter: String::new(),
            items: commands(),
            selected: 0,
            scroll: 0,
            reveal_top: false,
        }
    }

    /// Move the highlight to the first command under the group whose heading
    /// starts with `ch` — the heading letter, so `V` is Vault — and say whether
    /// there was a group to move to.
    fn jump(&mut self, ch: char) -> bool {
        let matches = self.matches();
        let Some(index) = matches.iter().position(|item| {
            group_of(&item.command)
                .chars()
                .next()
                .is_some_and(|first| first.eq_ignore_ascii_case(&ch))
        }) else {
            return false;
        };
        self.selected = index;
        self.reveal_top = true;
        true
    }

    /// The commands the filter leaves, in registry order.
    fn matches(&self) -> Vec<&PaletteItem> {
        let needle = self.filter.trim().to_lowercase();
        self.items
            .iter()
            .filter(|item| {
                needle.is_empty() || item.command.to_lowercase().contains(needle.as_str())
            })
            .collect()
    }

    /// How many commands the filter leaves. The whole registry when empty.
    pub fn total(&self) -> usize {
        self.matches().len()
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    fn select_next(&mut self) {
        let last = self.total().saturating_sub(1);
        self.selected = (self.selected + 1).min(last);
    }

    fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn select_first(&mut self) {
        self.selected = 0;
    }

    fn select_last(&mut self) {
        self.selected = self.total().saturating_sub(1);
    }

    /// Move the highlight by a page, clamped at both ends.
    fn page(&mut self, down: bool, rows: usize) {
        for _ in 0..rows {
            if down {
                self.select_next();
            } else {
                self.select_previous();
            }
        }
    }

    /// Draw the list as wrapped lines, and say where each command starts so the
    /// highlight and the scroll can be kept in the same coordinate system.
    ///
    /// Returns the lines and, for every filtered command, `(first, last)` line
    /// index inclusive. Headings are lines too: the scroll counts what is on
    /// screen, not what is in the registry.
    fn layout(
        &self,
        width: usize,
        runs: &BTreeMap<String, usize>,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        let matches = self.matches();
        let column = matches
            .iter()
            .map(|item| item.command.chars().count())
            .max()
            .unwrap_or(0)
            .min(width.saturating_sub(12));
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut spans: Vec<(usize, usize)> = vec![(0, 0); matches.len()];
        let mut group = "";
        for (index, item) in matches.iter().enumerate() {
            let here = group_of(&item.command);
            if here != group {
                if !lines.is_empty() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(Span::styled(
                    format!(" {here}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
                group = here;
            }
            let selected = index == self.selected;
            let name = if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Green)
            };
            let text = if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            let first = lines.len();
            // The command is padded to one column so the eye runs down the names;
            // the description starts where the widest name ends, and wraps under
            // that column so a long sentence is read, not clipped.
            let mut row: Vec<(String, Style)> = vec![(format!(" {} ", item.command), name)];
            let pad = column.saturating_sub(item.command.chars().count()) + 1;
            row.push((" ".repeat(pad), Style::default()));
            // How often this machine has run it, in its own narrow column so the
            // counts line up under each other. A command never run gets blank
            // space rather than `0×`: the absence is the information, and a
            // column of zeroes is noise to read past.
            let count = runs.get(display_name(&item.command)).copied().unwrap_or(0);
            let tally = if count == 0 {
                "    ".to_string()
            } else {
                format!("{count:>3}×")
            };
            row.push((format!("{tally} "), Style::default().fg(Color::DarkGray)));
            for word in item.description.split_whitespace() {
                row.push((format!("{word} "), text));
            }
            // One wrapped line per `width` characters, keeping the styles that
            // were set above: the row is built as spans, then broken.
            let mut line: Vec<Span> = Vec::new();
            let mut used = 0usize;
            for (chunk, style) in row {
                for piece in chunk.split_inclusive(' ') {
                    let len = piece.chars().count();
                    if used > 0 && used + len > width {
                        lines.push(Line::from(std::mem::take(&mut line)));
                        used = 0;
                        // Continuations start under the description column, so a
                        // wrapped sentence reads as one paragraph. Name, its
                        // padding, and the five columns of the tally column.
                        let indent = " ".repeat(column + 7);
                        line.push(Span::styled(indent.clone(), Style::default()));
                        used += indent.chars().count();
                    }
                    line.push(Span::styled(piece.to_string(), style));
                    used += len;
                }
            }
            if !line.is_empty() {
                lines.push(Line::from(line));
            }
            spans[index] = (first, lines.len().saturating_sub(1));
        }
        (lines, spans)
    }
}

impl Default for CommandsOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl Overlay for CommandsOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Commands
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.filter.clear();
        self.selected = 0;
        self.scroll = 0;
        self.reveal_top = false;
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }
        match key.code {
            KeyCode::Esc => OverlayAction::Close,
            KeyCode::Up => {
                self.select_previous();
                OverlayAction::None
            }
            KeyCode::Down => {
                self.select_next();
                OverlayAction::None
            }
            KeyCode::Tab => {
                self.select_next();
                OverlayAction::None
            }
            KeyCode::PageUp => {
                self.page(false, 10);
                OverlayAction::None
            }
            KeyCode::PageDown => {
                self.page(true, 10);
                OverlayAction::None
            }
            KeyCode::Home => {
                self.select_first();
                OverlayAction::None
            }
            KeyCode::End => {
                self.select_last();
                OverlayAction::None
            }
            // Enter fills the prompt with the highlighted command and closes:
            // the browser is for finding a command, not for running one unseen.
            // A command with a required argument stops at the prompt with the
            // space already there, which is the one thing that makes the next
            // keystroke land in the right place.
            KeyCode::Enter => {
                let chosen = self
                    .matches()
                    .get(self.selected)
                    .map(|item| (item.command.clone(), needs_arguments(item)));
                if let Some((command, needs_more)) = chosen {
                    app.input = if needs_more {
                        format!("{command} ")
                    } else {
                        command
                    };
                    app.cursor = app.input.chars().count();
                    app.pending_submit = !needs_more;
                }
                OverlayAction::Close
            }
            KeyCode::Backspace => {
                if self.filter.pop().is_some() {
                    self.selected = 0;
                    self.scroll = 0;
                }
                OverlayAction::None
            }
            // A control chord is a command, never text: ctrl-t in the browser
            // must not append `t` to the filter.
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                // Uppercase is the heading's own letter, and a plain letter is
                // the first character of a filter. One key cannot do both, and
                // `v` has to keep typing `/vault` — so the shift is what tells
                // "take me to Vault" from "narrow to vault".
                if ch.is_uppercase() && self.jump(ch) {
                    return OverlayAction::None;
                }
                self.filter.push(ch);
                self.selected = 0;
                self.scroll = 0;
                self.reveal_top = false;
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, app: &App) {
        if !self.visible || area.width < 24 || area.height < 8 {
            return;
        }
        // The browser takes most of the screen: it exists because the dropdown
        // cannot show the registry, so a small box would be the same problem
        // with a different title.
        let width = (area.width as u32 * 94 / 100) as u16;
        let height = area.height.saturating_sub(2);
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let popup = Rect::new(x, y, width, height);
        frame.render_widget(Clear, popup);

        let block = Block::default()
            .title(" commands — the whole registry ")
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        if inner.height < 4 || inner.width < 8 {
            return;
        }

        // The filter line, then the list, then the hint.
        let filter_area = Rect::new(inner.x, inner.y, inner.width, 1);
        let hint_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
        let list_area = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(2),
        );

        let count = if self.filter.is_empty() {
            format!(" {} commands", self.total())
        } else {
            format!(" {} of {}", self.total(), self.items.len())
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " > ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if self.filter.is_empty() {
                        "type to filter".to_string()
                    } else {
                        self.filter.clone()
                    },
                    if self.filter.is_empty() {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
                Span::styled(count, Style::default().fg(Color::DarkGray)),
            ])),
            filter_area,
        );

        let (lines, spans) = self.layout(list_area.width as usize, app.slash.runs());
        let pages = list_area.height as usize;
        // Keep the highlight on screen by moving the window, never the selection:
        // a heading that scrolls past takes its group with it, so the counts stay
        // in step with what is drawn.
        if let Some((first, last)) = spans.get(self.selected).copied() {
            if std::mem::take(&mut self.reveal_top) {
                // A jump lands on the heading — the blank line above it and the
                // title itself — not merely on the row it moved to.
                self.scroll = first.saturating_sub(2);
            } else if first < self.scroll {
                self.scroll = first;
            } else if last >= self.scroll + pages {
                self.scroll = last + 1 - pages;
            }
        }
        self.scroll = self.scroll.min(lines.len().saturating_sub(1));

        if lines.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" no command matches \"{}\"", self.filter),
                    Style::default().fg(Color::Yellow),
                ))),
                list_area,
            );
        } else {
            frame.render_widget(
                Paragraph::new(lines).scroll((self.scroll as u16, 0)),
                list_area,
            );
        }

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " S/V/C/T/W/A jump to a group  |  type to filter  |  Up/Down move  |  Enter puts it in the prompt  |  Esc close",
                Style::default().fg(Color::DarkGray),
            )))
            .alignment(Alignment::Left),
            hint_area,
        );
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("type", "filter the registry"),
            ("shift+letter", "jump to that group"),
            ("up/down", "move"),
            ("enter", "put the command in the prompt"),
            ("esc", "close"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened() -> CommandsOverlay {
        let mut overlay = CommandsOverlay::new();
        overlay.show();
        overlay
    }

    #[test]
    fn the_browser_lists_the_whole_registry() {
        let overlay = opened();
        assert_eq!(
            overlay.total(),
            commands().len(),
            "the browser is the surface with no row limit"
        );
        assert!(
            overlay.total() > 5,
            "the dropdown's limit does not apply here"
        );
    }

    #[test]
    fn every_command_lands_under_a_heading() {
        // Every group name the layout can produce is one of the declared ones, so
        // a new area of the registry shows up under Session instead of vanishing.
        let titles: Vec<&str> = GROUPS.iter().map(|(_, title)| *title).collect();
        for item in commands() {
            let group = group_of(&item.command);
            assert!(titles.contains(&group), "{} → {group}", item.command);
        }
        assert_eq!(group_of("/vault recall"), "Vault");
        assert_eq!(group_of("/kg status"), "Code graph");
        assert_eq!(group_of("/help"), "Session");
    }

    #[test]
    fn typing_narrows_instead_of_reaching_the_prompt() {
        let mut app = App::new("s", false);
        let mut overlay = opened();
        for ch in "vault".chars() {
            overlay.handle_key(KeyEvent::from(KeyCode::Char(ch)), &mut app);
        }
        assert_eq!(overlay.filter(), "vault");
        assert!(app.input.is_empty(), "the prompt must stay untouched");
        assert!(overlay.total() < commands().len());
    }

    #[test]
    fn enter_puts_the_command_in_the_prompt() {
        let mut app = App::new("s", false);
        let mut overlay = opened();
        for ch in "help".chars() {
            overlay.handle_key(KeyEvent::from(KeyCode::Char(ch)), &mut app);
        }
        let action = overlay.handle_key(KeyEvent::from(KeyCode::Enter), &mut app);
        assert!(matches!(action, OverlayAction::Close));
        assert_eq!(app.input, "/help");
        assert!(app.pending_submit);
    }

    #[test]
    fn a_command_that_needs_an_argument_stops_with_room_to_type_it() {
        let mut app = App::new("s", false);
        let mut overlay = opened();
        for ch in "vault recall".chars() {
            overlay.handle_key(KeyEvent::from(KeyCode::Char(ch)), &mut app);
        }
        overlay.handle_key(KeyEvent::from(KeyCode::Enter), &mut app);
        assert_eq!(app.input, "/vault recall ");
        assert!(!app.pending_submit, "the argument is still missing");
    }

    #[test]
    fn the_highlight_stops_at_both_ends_of_a_long_list() {
        let mut overlay = opened();
        let last = overlay.total() - 1;
        for _ in 0..(last + 20) {
            overlay.select_next();
        }
        assert_eq!(overlay.selected, last);
        for _ in 0..(last + 20) {
            overlay.select_previous();
        }
        assert_eq!(overlay.selected, 0);
    }

    #[test]
    fn a_command_that_was_run_carries_its_count() {
        let mut runs = BTreeMap::new();
        runs.insert("/vault status".to_string(), 12);
        let overlay = opened();
        let (lines, _) = overlay.layout(120, &runs);
        let text: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("12×"),
            "the count is drawn beside the command"
        );
        assert_eq!(
            text.matches('×').count(),
            1,
            "only the command that was run has a count"
        );
    }

    #[test]
    fn a_heading_letter_jumps_to_that_group_and_leaves_the_filter_alone() {
        let mut app = App::new("s", false);
        let mut overlay = opened();

        // Shift+V is Vault. A plain `v` is still the start of a filter.
        overlay.handle_key(KeyEvent::from(KeyCode::Char('V')), &mut app);
        assert!(overlay.filter().is_empty(), "a jump is not a filter");
        let first = overlay.matches()[overlay.selected].command.clone();
        assert!(first.starts_with("/vault"), "landed on {first}");

        overlay.handle_key(KeyEvent::from(KeyCode::Char('C')), &mut app);
        let first = overlay.matches()[overlay.selected].command.clone();
        assert!(first.starts_with("/kg"), "C is the code graph, got {first}");

        // A letter with no group of its own is a character in the filter.
        overlay.handle_key(KeyEvent::from(KeyCode::Char('Z')), &mut app);
        assert_eq!(overlay.filter(), "Z");
    }

    #[test]
    fn the_drawn_lines_cover_every_command() {
        // Sixty columns is a narrow terminal; the wrapped layout must still give
        // every command a line of its own rather than dropping the tail.
        let mut overlay = opened();
        let (lines, spans) = overlay.layout(60, &BTreeMap::new());
        assert_eq!(spans.len(), commands().len());
        let last = spans.last().copied().expect("at least one command");
        assert!(last.1 < lines.len(), "the last command has drawn lines");
        // The selection is the row the window follows, so the first command must
        // start directly under its heading.
        overlay.selected = 0;
        assert_eq!(spans[0].0, 1, "the heading is line 0, the row is line 1");
    }
}
