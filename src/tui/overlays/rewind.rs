//! Rewind overlay: navigate session history, replay turns, branch from points.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

#[derive(Debug, Default)]
pub struct RewindOverlay {
    visible: bool,
    tabs: Vec<RewindTab>,
    active_tab: usize,
    sessions: Vec<SessionEntry>,
    turns: Vec<TurnEntry>,
    checkpoints: Vec<CheckpointEntry>,
    selected: usize,
    list_state: ListState,
    detail_view: bool,
    detail_content: String,
    mode: RewindMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RewindMode {
    #[default]
    Browse,
    ConfirmBranch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RewindTab {
    Sessions,
    Turns,
    Checkpoints,
    Branches,
}

#[derive(Debug, Clone)]
struct SessionEntry {
    name: String,
    created: String,
    last_active: String,
    message_count: usize,
    is_current: bool,
}

#[derive(Debug, Clone)]
struct CheckpointEntry {
    id: String,
    timestamp: String,
    label: String,
    turn_count: usize,
    token_count: u32,
}

#[derive(Debug, Clone)]
struct TurnEntry {
    index: usize,
    timestamp: String,
    role: String,
    preview: String,
    tokens: Option<u32>,
    cost: Option<f64>,
}

impl RewindOverlay {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            tabs: vec![
                RewindTab::Sessions,
                RewindTab::Turns,
                RewindTab::Checkpoints,
                RewindTab::Branches,
            ],
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

    fn load_mock_data(&mut self) {
        self.sessions = vec![
            SessionEntry {
                name: "session".to_string(),
                created: "2024-01-15 10:00".to_string(),
                last_active: "2m ago".to_string(),
                message_count: 42,
                is_current: true,
            },
            SessionEntry {
                name: "feature-auth".to_string(),
                created: "2024-01-14 14:30".to_string(),
                last_active: "1d ago".to_string(),
                message_count: 18,
                is_current: false,
            },
            SessionEntry {
                name: "bugfix-search".to_string(),
                created: "2024-01-13 09:15".to_string(),
                last_active: "2d ago".to_string(),
                message_count: 7,
                is_current: false,
            },
        ];

        self.turns = vec![
            TurnEntry {
                index: 41,
                timestamp: "14:32".to_string(),
                role: "assistant".to_string(),
                preview: "Implemented the search overlay with vault and graph results".to_string(),
                tokens: Some(1240),
                cost: Some(0.0024),
            },
            TurnEntry {
                index: 40,
                timestamp: "14:31".to_string(),
                role: "user".to_string(),
                preview: "/search event sourcing architecture".to_string(),
                tokens: None,
                cost: None,
            },
            TurnEntry {
                index: 39,
                timestamp: "14:30".to_string(),
                role: "assistant".to_string(),
                preview: "Created decision: use event sourcing for session replay".to_string(),
                tokens: Some(890),
                cost: Some(0.0018),
            },
            TurnEntry {
                index: 38,
                timestamp: "14:29".to_string(),
                role: "user".to_string(),
                preview: "/vault remember architecture --kind decision \"Use event sourcing...\""
                    .to_string(),
                tokens: None,
                cost: None,
            },
            TurnEntry {
                index: 37,
                timestamp: "14:25".to_string(),
                role: "assistant".to_string(),
                preview: "Refactored the diff viewer to use memnoth-ui DiffViewer widget"
                    .to_string(),
                tokens: Some(2100),
                cost: Some(0.0042),
            },
        ];
    }

    fn render_checkpoints(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .checkpoints
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let selected = i == self.selected;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let lines = vec![
                    Line::from(vec![
                        Span::styled(&c.id, style),
                        Span::styled(format!("  {}", c.label), style.fg(Color::Cyan)),
                        Span::styled(
                            format!("  {} turns, {} tok", c.turn_count, c.token_count),
                            style.fg(Color::DarkGray),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("    ", Style::default()),
                        Span::styled(&c.timestamp, style.fg(Color::Gray)),
                    ]),
                ];
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Checkpoints "),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
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

                let current_mark = if s.is_current { " ●" } else { "" };
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
                            format!("Created: {}  Last: {}", s.created, s.last_active),
                            style.fg(Color::Gray),
                        ),
                    ]),
                ];
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Sessions "))
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

                let role_color = match t.role.as_str() {
                    "user" => Color::Cyan,
                    "assistant" => Color::Green,
                    _ => Color::Gray,
                };

                let tokens_str = t.tokens.map_or("".to_string(), |v| format!("  {} tok", v));
                let cost_str = t.cost.map_or("".to_string(), |v| format!("  ${:.4}", v));

                let lines = vec![
                    Line::from(vec![
                        Span::styled(
                            format!("#{} ", t.index),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(&t.timestamp, Style::default().fg(Color::Gray)),
                        Span::styled(
                            format!("  [{}]", t.role),
                            Style::default().fg(role_color).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(tokens_str, style.fg(Color::DarkGray)),
                        Span::styled(cost_str, style.fg(Color::Yellow)),
                    ]),
                    Line::from(vec![
                        Span::styled("    ", Style::default()),
                        Span::styled(&t.preview, style.fg(Color::Gray)),
                    ]),
                ];
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Turns "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_branches(&mut self, frame: &mut Frame, area: Rect) {
        let branches = vec![
            ("main", "feature-auth", "3 turns diverged"),
            ("main", "bugfix-search", "1 turn diverged"),
        ];

        let items: Vec<ListItem> = branches
            .iter()
            .enumerate()
            .map(|(i, (base, name, desc))| {
                let selected = i == self.selected;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let lines = vec![Line::from(vec![
                    Span::styled(*name, style),
                    Span::styled(format!("  (from {})", base), style.fg(Color::Cyan)),
                    Span::styled(format!("  {}", desc), style.fg(Color::DarkGray)),
                ])];
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Branches "))
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
        self.load_mock_data();
        self.active_tab = 0;
        self.selected = 0;
        self.list_state.select(Some(0));
        self.detail_view = false;
        self.mode = RewindMode::Browse;
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

        // Handle confirmation mode
        if self.mode == RewindMode::ConfirmBranch {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        "Branched from selected turn".to_string(),
                    );
                    self.mode = RewindMode::Browse;
                    self.detail_view = false;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.mode = RewindMode::Browse;
                    self.detail_view = false;
                }
                _ => {}
            }
            return OverlayAction::None;
        }

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
                if !self.detail_view {
                    self.active_tab = (self.active_tab + 1) % self.tabs.len();
                    self.selected = 0;
                    self.list_state.select(Some(0));
                }
            }
            KeyCode::BackTab => {
                if !self.detail_view {
                    self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
                    self.selected = 0;
                    self.list_state.select(Some(0));
                }
            }
            KeyCode::Up => {
                if !self.detail_view && self.selected > 0 {
                    self.selected -= 1;
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::Down => {
                if !self.detail_view {
                    let len = match self.tab() {
                        RewindTab::Sessions => self.sessions.len(),
                        RewindTab::Turns => self.turns.len(),
                        RewindTab::Checkpoints => self.checkpoints.len(),
                        RewindTab::Branches => 0,
                    };
                    if self.selected + 1 < len {
                        self.selected += 1;
                        self.list_state.select(Some(self.selected));
                    }
                }
            }
            KeyCode::PageUp => {
                if !self.detail_view {
                    self.selected = self.selected.saturating_sub(10);
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::PageDown => {
                if !self.detail_view {
                    let len = match self.tab() {
                        RewindTab::Sessions => self.sessions.len(),
                        RewindTab::Turns => self.turns.len(),
                        RewindTab::Checkpoints => self.checkpoints.len(),
                        RewindTab::Branches => 0,
                    };
                    self.selected = (self.selected + 10).min(len.saturating_sub(1));
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::Home => {
                if !self.detail_view {
                    self.selected = 0;
                    self.list_state.select(Some(0));
                }
            }
            KeyCode::End => {
                if !self.detail_view {
                    let len = match self.tab() {
                        RewindTab::Sessions => self.sessions.len(),
                        RewindTab::Turns => self.turns.len(),
                        RewindTab::Checkpoints => self.checkpoints.len(),
                        RewindTab::Branches => 0,
                    };
                    self.selected = len.saturating_sub(1);
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::Enter => {
                if !self.detail_view {
                    match self.tab() {
                        RewindTab::Sessions => {
                            if let Some(s) = self.sessions.get(self.selected) {
                                if !s.is_current {
                                    app.push(
                                        crate::tui::app::EntryRole::Info,
                                        format!("Resuming session: {}", s.name),
                                    );
                                    return OverlayAction::Close;
                                }
                            }
                        }
                        RewindTab::Turns => {
                            self.detail_view = true;
                            if let Some(t) = self.turns.get(self.selected) {
                                self.detail_content = format!(
                                    "Turn #{}\nTime: {}\nRole: {}\nTokens: {}\nCost: ${:.4}\n\n{}",
                                    t.index,
                                    t.timestamp,
                                    t.role,
                                    t.tokens.map_or("N/A".to_string(), |v| v.to_string()),
                                    t.cost.unwrap_or(0.0),
                                    t.preview
                                );
                            }
                        }
                        RewindTab::Checkpoints => {
                            self.detail_view = true;
                            if let Some(c) = self.checkpoints.get(self.selected) {
                                self.detail_content = format!(
                                    "Checkpoint: {}\nLabel: {}\nTime: {}\nTurns: {}\nTokens: {}",
                                    c.id, c.label, c.timestamp, c.turn_count, c.token_count
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('b') => {
                // Branch from current turn
                if !self.detail_view && self.tab() == RewindTab::Turns {
                    self.mode = RewindMode::ConfirmBranch;
                    self.detail_view = true;
                    if let Some(t) = self.turns.get(self.selected) {
                        self.detail_content = format!(
                            "Branch from turn #{}?\n\nThis will create a new session starting from this point.\n\ny: confirm · n: cancel",
                            t.index
                        );
                    }
                }
            }
            KeyCode::Char('c') if ctrl => {
                // Copy
                if !self.detail_view {
                    match self.tab() {
                        RewindTab::Sessions => {
                            if let Some(s) = self.sessions.get(self.selected) {
                                app.push(
                                    crate::tui::app::EntryRole::Info,
                                    format!("Copied session: {}", s.name),
                                );
                            }
                        }
                        RewindTab::Turns => {
                            if let Some(t) = self.turns.get(self.selected) {
                                app.push(
                                    crate::tui::app::EntryRole::Info,
                                    format!("Copied turn #{}", t.index),
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('p') => {
                // Pin
                if !self.detail_view {
                    match self.tab() {
                        RewindTab::Sessions => {
                            if let Some(s) = self.sessions.get(self.selected) {
                                app.push(
                                    crate::tui::app::EntryRole::Info,
                                    format!("Pinned session: {}", s.name),
                                );
                            }
                        }
                        RewindTab::Turns => {
                            if let Some(t) = self.turns.get(self.selected) {
                                app.push(
                                    crate::tui::app::EntryRole::Info,
                                    format!("Pinned turn #{}", t.index),
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('r') => {
                // Resume session
                if !self.detail_view && self.tab() == RewindTab::Sessions {
                    if let Some(s) = self.sessions.get(self.selected) {
                        if !s.is_current {
                            app.push(
                                crate::tui::app::EntryRole::Info,
                                format!("Resuming session: {}", s.name),
                            );
                            return OverlayAction::Close;
                        }
                    }
                }
            }
            KeyCode::Char('d') => {
                // Delete session
                if !self.detail_view && self.tab() == RewindTab::Sessions {
                    if let Some(s) = self.sessions.get(self.selected) {
                        if !s.is_current {
                            app.push(
                                crate::tui::app::EntryRole::Info,
                                format!("Deleted session: {}", s.name),
                            );
                            self.sessions.remove(self.selected);
                            self.selected =
                                self.selected.min(self.sessions.len().saturating_sub(1));
                            self.list_state.select(if self.sessions.is_empty() {
                                None
                            } else {
                                Some(self.selected)
                            });
                        }
                    }
                }
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

        // Tabs
        let tab_titles: Vec<&str> = self
            .tabs
            .iter()
            .map(|t| match t {
                RewindTab::Sessions => "Sessions",
                RewindTab::Turns => "Turns",
                RewindTab::Checkpoints => "Checkpoints",
                RewindTab::Branches => "Branches",
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

        // Content
        if self.detail_view {
            let detail = Paragraph::new(self.detail_content.clone())
                .style(Style::default().fg(Color::White))
                .block(Block::default().borders(Borders::ALL).title(" Detail "))
                .wrap(Wrap { trim: true });
            frame.render_widget(detail, chunks[1]);
        } else {
            match self.tab() {
                RewindTab::Sessions => self.render_sessions(frame, chunks[1]),
                RewindTab::Turns => self.render_turns(frame, chunks[1]),
                RewindTab::Checkpoints => self.render_checkpoints(frame, chunks[1]),
                RewindTab::Branches => self.render_branches(frame, chunks[1]),
            }
        }

        // Help footer
        let help_text = if self.mode == RewindMode::ConfirmBranch {
            "y: confirm branch · n/esc: cancel"
        } else if self.detail_view {
            "esc: back · c: copy · p: pin · b: branch from turn"
        } else {
            match self.tab() {
                RewindTab::Sessions => {
                    "↑↓: navigate · enter: resume · r: resume · d: delete · c: copy · p: pin · tab: next tab · esc: close"
                }
                RewindTab::Turns => {
                    "↑↓: navigate · enter: detail · b: branch · c: copy · p: pin · tab: next tab · esc: close"
                }
                RewindTab::Checkpoints => {
                    "↑↓: navigate · enter: detail · c: copy · tab: next tab · esc: close"
                }
                RewindTab::Branches => "tab: next tab · esc: close",
            }
        };
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(help, chunks[2]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/rewind", "Open rewind overlay"),
            ("Tab", "Switch tabs (Sessions/Turns/Checkpoints/Branches)"),
            ("↑/↓", "Navigate"),
            (
                "Enter",
                "Resume session / View turn detail / View checkpoint detail",
            ),
            ("B", "Branch from selected turn"),
            ("R", "Resume session"),
            ("D", "Delete session"),
            ("Ctrl+C", "Copy entry"),
            ("P", "Pin entry"),
            ("Esc", "Close overlay"),
        ]
    }
}
