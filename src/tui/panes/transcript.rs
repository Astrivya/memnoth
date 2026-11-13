//! Transcript pane: the main conversation view with scroll and follow modes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use crate::tui::app::{App, EntryRole};

/// One entry's recorded time as `HH:MM`.
///
/// Local time, because this is read next to a wall clock rather than compared
/// across machines. A negative or zero `at_ms` (a session header with no time)
/// clamps to the epoch rather than panicking on a `Duration`.
fn clock(at_ms: i64) -> String {
    chrono::DateTime::<chrono::Local>::from(
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(at_ms.max(0) as u64),
    )
    .format("%H:%M")
    .to_string()
}

/// Transcript pane showing the conversation history
#[derive(Debug, Default)]
pub struct TranscriptPane {
    pub visible: bool,
    pub list_state: ListState,
    pub scroll: usize,
    pub follow: bool,
    pub show_timestamps: bool,
    pub max_width: usize,
}

impl TranscriptPane {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            visible: true,
            list_state: state,
            scroll: 0,
            follow: true,
            show_timestamps: true,
            max_width: 120,
        }
    }

    /// Handle key events for the transcript pane
    pub fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> bool {
        if !self.visible {
            return false;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let _shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Up => {
                self.follow = false;
                if self.scroll > 0 {
                    self.scroll -= 1;
                    self.update_selection(app);
                }
                true
            }
            KeyCode::Down => {
                if self.scroll + 1 < app.entries.len().saturating_sub(1) {
                    self.scroll += 1;
                    self.update_selection(app);
                } else {
                    self.follow = true;
                    self.scroll = app.entries.len().saturating_sub(1);
                    self.update_selection(app);
                }
                true
            }
            KeyCode::PageUp => {
                self.follow = false;
                self.scroll = self.scroll.saturating_sub(20);
                self.update_selection(app);
                true
            }
            KeyCode::PageDown => {
                self.scroll = (self.scroll + 20).min(app.entries.len().saturating_sub(1));
                if self.scroll >= app.entries.len().saturating_sub(1) {
                    self.follow = true;
                }
                self.update_selection(app);
                true
            }
            KeyCode::Home => {
                self.follow = false;
                self.scroll = 0;
                self.update_selection(app);
                true
            }
            KeyCode::End => {
                self.follow = true;
                self.scroll = app.entries.len().saturating_sub(1);
                self.update_selection(app);
                true
            }
            KeyCode::Char('t') if ctrl => {
                self.show_timestamps = !self.show_timestamps;
                true
            }
            KeyCode::Char('f') if ctrl => {
                self.follow = !self.follow;
                if self.follow {
                    self.scroll = app.entries.len().saturating_sub(1);
                    self.update_selection(app);
                }
                true
            }
            _ => false,
        }
    }

    fn update_selection(&mut self, app: &App) {
        if !app.entries.is_empty() {
            self.list_state
                .select(Some(self.scroll.min(app.entries.len() - 1)));
        }
    }

    /// Render the transcript pane
    pub fn render(&mut self, frame: &mut Frame, area: Rect, app: &App) {
        if !self.visible {
            return;
        }

        let items: Vec<ListItem> = app
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let selected = i == self.scroll;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let (role_style, role_glyph) = style_for(entry.role);
                let mut lines = Vec::new();

                // The row's own recorded time, in local time to the minute.
                // A stamp reading differently from the same turn in the session
                // file would be a second truth, so this formats the entry's
                // `at_ms` and nothing else.
                if self.show_timestamps {
                    lines.push(Line::from(Span::styled(
                        clock(entry.at_ms),
                        Style::default().fg(Color::DarkGray),
                    )));
                }

                // Role and content
                let content_lines = wrap_text(&entry.text, self.max_width);
                for (line_idx, line) in content_lines.iter().enumerate() {
                    if line_idx == 0 {
                        lines.push(Line::from(vec![
                            Span::styled(role_glyph, role_style),
                            Span::styled(" ", Style::default()),
                            Span::styled(line.to_string(), style),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::styled("  ", Style::default()),
                            Span::styled(line.to_string(), style),
                        ]));
                    }
                }

                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(format!(
                " Transcript {} ",
                if self.follow { "●" } else { "○" }
            )))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));

        // Calculate scrollbar state
        let list_height = area.height.saturating_sub(2) as usize; // Account for borders
        let mut scrollbar_state = ScrollbarState::default()
            .content_length(app.entries.len())
            .viewport_content_length(list_height)
            .position(self.scroll);

        frame.render_stateful_widget(list, area, &mut self.list_state);

        // Render scrollbar
        let scrollbar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        };
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"))
            .track_symbol(Some("│"))
            .thumb_symbol("█");
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}

/// Style for entry role
fn style_for(role: EntryRole) -> (Style, &'static str) {
    match role {
        EntryRole::User => (
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            "▶",
        ),
        EntryRole::Answer => (
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            "▶",
        ),
        EntryRole::Info => (
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            "ℹ",
        ),
        EntryRole::Error => (
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            "✗",
        ),
        // The glyph is the state: running, then done in place.
        EntryRole::Tool => (Style::default().fg(Color::Cyan), "▌"),
        EntryRole::ToolDone => (Style::default().fg(Color::Green), "✓"),
        EntryRole::Thinking => (
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(ratatui::style::Modifier::ITALIC),
            "▸",
        ),
    }
}

/// Break text into lines of at most `width` characters.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        if current.len() + word.len() + 1 > width && !current.is_empty() {
            lines.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}
