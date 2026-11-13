//! `/rewind`: the sessions on this machine, and the turns of this one.
//!
//! Both lists are reads of things that exist: `SessionStore::list` is the same
//! listing `memnoth session list` prints, and the Turns tab is this session's
//! own transcript, already on screen.
//!
//! What was removed, and why: a Checkpoints tab (nothing creates a checkpoint,
//! so every row in it was invented), a Branches tab (same), and the resume /
//! delete / pin / copy keys (no key here could perform them — deleting a
//! session file from a picker with no confirmation is the opposite of safe).
//! Switching sessions is `memnoth --resume <name>`: a process cannot swap the
//! session it is recording, so the screen says that instead of pretending.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};

use crate::session::SessionStore;
use crate::tui::app::{App, EntryRole};
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

#[derive(Debug, Default)]
pub struct RewindOverlay {
    visible: bool,
    tabs: Vec<RewindTab>,
    active_tab: usize,
    /// Sessions on disk, newest first.
    sessions: Vec<SessionEntry>,
    /// This session's transcript, oldest first.
    turns: Vec<TurnEntry>,
    /// Why a list is empty, when the reason is worth saying.
    note: Option<String>,
    selected: usize,
    list_state: ListState,
    detail_view: bool,
    detail_content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RewindTab {
    Sessions,
    Turns,
}

#[derive(Debug, Clone)]
struct SessionEntry {
    name: String,
    model: String,
    created: String,
    message_count: usize,
    path: String,
    is_current: bool,
}

#[derive(Debug, Clone)]
struct TurnEntry {
    index: usize,
    role: &'static str,
    text: String,
}

impl RewindOverlay {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            tabs: vec![RewindTab::Sessions, RewindTab::Turns],
            list_state: state,
            ..Default::default()
        }
    }

    /// The tab the cursor is on. `active_tab` is an index so `Tabs` can be
    /// handed it directly; this is the one place that index becomes a variant.
    fn tab(&self) -> RewindTab {
        self.tabs
            .get(self.active_tab)
            .copied()
            .unwrap_or(RewindTab::Sessions)
    }

    /// Read the sessions on disk and this session's transcript.
    fn load(&mut self, app: &App) {
        self.note = None;

        match app.ctx.as_ref() {
            Some(ctx) => {
                let store = SessionStore::new(&ctx.sessions_dir);
                self.sessions = store
                    .list()
                    .into_iter()
                    .map(|summary| SessionEntry {
                        is_current: summary.name == app.name,
                        created: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
                            summary.created_ms,
                        )
                        .map(|when| when.format("%Y-%m-%d %H:%M UTC").to_string())
                        .unwrap_or_else(|| "unknown".to_string()),
                        name: summary.name,
                        model: summary.model,
                        message_count: summary.messages,
                        path: summary.path,
                    })
                    .collect();
            }
            None => {
                self.sessions.clear();
                self.note = Some(
                    "no workspace context — the session list lives under the workspace's sessions \
                     directory"
                        .to_string(),
                );
            }
        }

        // This session's own transcript: the same rows the pane is drawing.
        self.turns = app
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| TurnEntry {
                index,
                role: match entry.role {
                    EntryRole::User => "you",
                    EntryRole::Answer => "answer",
                    EntryRole::Error => "error",
                    EntryRole::Info => "info",
                    EntryRole::Tool => "tool",
                    EntryRole::ToolDone => "tool done",
                    EntryRole::Thinking => "thinking",
                },
                text: entry.text.clone(),
            })
            .collect();

        self.clamp();
    }

    fn rows(&self) -> usize {
        match self.tab() {
            RewindTab::Sessions => self.sessions.len(),
            RewindTab::Turns => self.turns.len(),
        }
    }

    fn clamp(&mut self) {
        let len = self.rows();
        if len == 0 {
            self.selected = 0;
            self.list_state.select(None);
        } else {
            self.selected = self.selected.min(len - 1);
            self.list_state.select(Some(self.selected));
        }
    }

    fn select(&mut self, index: usize) {
        if self.rows() == 0 {
            return;
        }
        self.selected = index.min(self.rows() - 1);
        self.list_state.select(Some(self.selected));
    }

    fn open_detail(&mut self) {
        self.detail_view = true;
        self.detail_content = match self.tab() {
            RewindTab::Sessions => match self.sessions.get(self.selected) {
                Some(session) => format!(
                    "{}\n\nmodel      {}\ncreated    {}\nmessages   {}\nfile       {}\n\n{}",
                    session.name,
                    if session.model.is_empty() {
                        "unknown (the file did not parse)"
                    } else {
                        session.model.as_str()
                    },
                    session.created,
                    session.message_count,
                    session.path,
                    if session.is_current {
                        "this is the session you are in."
                    } else {
                        "open it from a shell: `mn --resume <name>`.\n\
                         A session cannot be switched from inside one — the file being \
                         recorded is this one."
                    }
                ),
                None => "nothing selected".to_string(),
            },
            RewindTab::Turns => match self.turns.get(self.selected) {
                Some(turn) => format!("#{} · {}\n\n{}", turn.index, turn.role, turn.text),
                None => "nothing selected".to_string(),
            },
        };
    }

    /// A line explaining a screen that has no rows.
    fn state_text(&self) -> String {
        if let Some(note) = &self.note {
            return note.clone();
        }
        match self.tab() {
            RewindTab::Sessions => {
                "no sessions on disk yet — this one is written when its first answer lands"
                    .to_string()
            }
            RewindTab::Turns => "this session has no turns yet".to_string(),
        }
    }

    fn render_sessions(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .sessions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let selected = i == self.selected;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let current_mark = if s.is_current { " ● here" } else { "" };
                let lines = vec![
                    Line::from(vec![
                        Span::styled(&s.name, style),
                        Span::styled(current_mark, style.fg(Color::Green)),
                        Span::styled(
                            format!("  ({} msgs)", s.message_count),
                            style.fg(Color::DarkGray),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("    ", Style::default()),
                        Span::styled(
                            format!(
                                "{}  {}",
                                s.created,
                                if s.model.is_empty() {
                                    "unreadable"
                                } else {
                                    s.model.as_str()
                                }
                            ),
                            style.fg(Color::Gray),
                        ),
                    ]),
                ];
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Sessions ({}) ", self.sessions.len())),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_turns(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .turns
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let selected = i == self.selected;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let role_color = match t.role {
                    "you" => Color::Cyan,
                    "answer" => Color::Green,
                    "error" => Color::Red,
                    _ => Color::Gray,
                };

                // One drawn row per entry: a wrapped line would make the
                // selection highlight meaningless.
                let lines = vec![Line::from(vec![
                    Span::styled(
                        format!("#{} ", t.index),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("[{}] ", t.role),
                        Style::default().fg(role_color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(ellipsize(&t.text, 100), style.fg(Color::Gray)),
                ])];
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" This Session's Turns ({}) ", self.turns.len())),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }
}

impl Overlay for RewindOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Rewind
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.active_tab = 0;
        self.selected = 0;
        self.detail_view = false;
        self.detail_content.clear();
    }

    /// Reading needs the app: the session's paths and its transcript.
    fn on_open(&mut self, app: &mut App) {
        self.load(app);
    }

    fn hide(&mut self) {
        self.visible = false;
        self.detail_view = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => {
                if self.detail_view {
                    self.detail_view = false;
                    self.detail_content.clear();
                } else {
                    return OverlayAction::Close;
                }
            }
            KeyCode::Tab => {
                if !self.detail_view && !self.tabs.is_empty() {
                    self.active_tab = (self.active_tab + 1) % self.tabs.len();
                    self.selected = 0;
                    self.clamp();
                }
            }
            KeyCode::BackTab => {
                if !self.detail_view && !self.tabs.is_empty() {
                    self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
                    self.selected = 0;
                    self.clamp();
                }
            }
            KeyCode::Up => {
                if !self.detail_view && self.selected > 0 {
                    self.select(self.selected - 1);
                }
            }
            KeyCode::Down => {
                if !self.detail_view && self.selected + 1 < self.rows() {
                    self.select(self.selected + 1);
                }
            }
            KeyCode::PageUp => {
                if !self.detail_view {
                    self.select(self.selected.saturating_sub(10));
                }
            }
            KeyCode::PageDown => {
                if !self.detail_view {
                    self.select(self.selected.saturating_add(10));
                }
            }
            KeyCode::Home => {
                if !self.detail_view {
                    self.select(0);
                }
            }
            KeyCode::End => {
                if !self.detail_view {
                    self.select(self.rows().saturating_sub(1));
                }
            }
            KeyCode::Enter => {
                if !self.detail_view {
                    self.open_detail();
                }
            }
            // `r` rereads both lists. The screen changing is the answer; a
            // transcript line for every press would be noise.
            KeyCode::Char('r') if !ctrl => {
                self.load(app);
                self.detail_view = false;
                self.detail_content.clear();
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }

        frame.render_widget(Clear, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Tabs
                Constraint::Min(0),    // Content
                Constraint::Length(2), // Help footer
            ])
            .split(area);

        let tab_titles: Vec<&str> = self
            .tabs
            .iter()
            .map(|t| match t {
                RewindTab::Sessions => "Sessions",
                RewindTab::Turns => "Turns",
            })
            .collect();
        let tabs = Tabs::new(tab_titles)
            .block(Block::default().borders(Borders::ALL).title(" Rewind "))
            .select(self.active_tab)
            .style(Style::default().fg(Color::Gray))
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(tabs, chunks[0]);

        if self.detail_view {
            let detail = Paragraph::new(self.detail_content.clone())
                .style(Style::default().fg(Color::White))
                .block(Block::default().borders(Borders::ALL).title(" Detail "))
                .wrap(Wrap { trim: false });
            frame.render_widget(detail, chunks[1]);
        } else if self.rows() == 0 {
            let note = Paragraph::new(self.state_text())
                .style(Style::default().fg(Color::Gray))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(match self.tab() {
                            RewindTab::Sessions => " Sessions ",
                            RewindTab::Turns => " This Session's Turns ",
                        }),
                )
                .wrap(Wrap { trim: true });
            frame.render_widget(note, chunks[1]);
        } else {
            match self.tab() {
                RewindTab::Sessions => self.render_sessions(frame, chunks[1]),
                RewindTab::Turns => self.render_turns(frame, chunks[1]),
            }
        }

        let help_text = if self.detail_view {
            "esc: back"
        } else {
            "↑↓: move · enter: detail · tab: next tab · r: reread · esc: close"
        };
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(help, chunks[2]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/rewind", "Sessions on disk, and this session's turns"),
            ("Tab / Shift+Tab", "Switch Sessions / Turns"),
            ("↑/↓", "Move"),
            ("Enter", "Detail"),
            ("R", "Reread"),
            ("Esc", "Close / back from detail"),
        ]
    }
}

/// One drawn line, cut to the pane's width.
fn ellipsize(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(index: usize, role: &'static str, text: &str) -> TurnEntry {
        TurnEntry {
            index,
            role,
            text: text.to_string(),
        }
    }

    #[test]
    fn only_the_tabs_this_overlay_can_fill_exist() {
        let overlay = RewindOverlay::new();
        assert_eq!(
            overlay.tabs,
            vec![RewindTab::Sessions, RewindTab::Turns],
            "checkpoints and branches were invented by the old screen"
        );
    }

    #[test]
    fn an_empty_session_list_is_words_not_an_empty_table() {
        let overlay = RewindOverlay::new();
        assert!(overlay.state_text().contains("no sessions on disk"));
    }

    /// The overlay opens on Sessions — the tab with rows a person can act on —
    /// so a test about turns has to say which tab it is looking at.
    fn on_turns(turns: Vec<TurnEntry>) -> RewindOverlay {
        let mut overlay = RewindOverlay::new();
        overlay.active_tab = overlay
            .tabs
            .iter()
            .position(|tab| *tab == RewindTab::Turns)
            .expect("a Turns tab exists");
        overlay.turns = turns;
        overlay.clamp();
        overlay
    }

    #[test]
    fn turns_come_from_the_transcript_that_is_on_screen() {
        let mut overlay = on_turns(vec![
            turn(0, "you", "/help"),
            turn(1, "answer", "the commands are below"),
        ]);
        overlay.select(1);
        overlay.open_detail();
        assert!(overlay.detail_view);
        assert!(overlay.detail_content.contains("the commands are below"));
        assert!(overlay.detail_content.contains("answer"));
    }

    #[test]
    fn selection_stops_at_the_last_row() {
        let mut overlay = on_turns(vec![turn(0, "you", "a"), turn(1, "you", "b")]);
        overlay.select(7);
        assert_eq!(overlay.selected, 1);
        overlay.select(0);
        assert_eq!(overlay.selected, 0);
    }

    #[test]
    fn a_title_with_a_newline_still_draws_one_row() {
        assert_eq!(ellipsize("first line\nsecond", 40), "first line");
        assert_eq!(ellipsize("abcdef", 4), "abc…");
    }
}
