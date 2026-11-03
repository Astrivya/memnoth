//! Context overlay: view and manage vault context, beliefs, and coordination.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

#[derive(Debug, Default)]
pub struct ContextOverlay {
    visible: bool,
    tabs: Vec<ContextTab>,
    active_tab: usize,
    vault_beliefs: Vec<BeliefEntry>,
    coordination: Vec<CoordinationEntry>,
    selected: usize,
    list_state: ListState,
    detail_view: bool,
    detail_content: String,
}

#[derive(Debug, Clone)]
enum ContextTab {
    Beliefs,
    Coordination,
    History,
}

#[derive(Debug, Clone)]
struct BeliefEntry {
    name: String,
    kind: String,
    content: String,
    updated: String,
    version: u64,
    contradicted: bool,
}

#[derive(Debug, Clone)]
struct CoordinationEntry {
    agent: String,
    role: String,
    last_seen: String,
    status: CoordinationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoordinationStatus {
    Active,
    Idle,
    Away,
}

impl ContextOverlay {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            tabs: vec![
                ContextTab::Beliefs,
                ContextTab::Coordination,
                ContextTab::History,
            ],
            list_state: state,
            ..Default::default()
        }
    }

    fn load_mock_data(&mut self) {
        // Mock data - replace with actual vault/coordination reads
        self.vault_beliefs = vec![
            BeliefEntry {
                name: "architecture".to_string(),
                kind: "decision".to_string(),
                content: "Use event sourcing for session replay".to_string(),
                updated: "2024-01-15 14:32".to_string(),
                version: 3,
                contradicted: false,
            },
            BeliefEntry {
                name: "search-indexing".to_string(),
                kind: "decision".to_string(),
                content: "Use tantivy for full-text, embedding for semantic".to_string(),
                updated: "2024-01-10 09:15".to_string(),
                version: 1,
                contradicted: false,
            },
            BeliefEntry {
                name: "rust-version".to_string(),
                kind: "fact".to_string(),
                content: "MSRV is 1.75".to_string(),
                updated: "2024-01-05 16:45".to_string(),
                version: 2,
                contradicted: true,
            },
        ];

        self.coordination = vec![
            CoordinationEntry {
                agent: "memnoth-kg".to_string(),
                role: "indexer".to_string(),
                last_seen: "2m ago".to_string(),
                status: CoordinationStatus::Active,
            },
            CoordinationEntry {
                agent: "memnoth-search".to_string(),
                role: "searcher".to_string(),
                last_seen: "5m ago".to_string(),
                status: CoordinationStatus::Idle,
            },
            CoordinationEntry {
                agent: "human-dev".to_string(),
                role: "developer".to_string(),
                last_seen: "just now".to_string(),
                status: CoordinationStatus::Active,
            },
        ];
    }

    fn render_beliefs(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .vault_beliefs
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let selected = i == self.selected;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let contradiction_mark = if b.contradicted { " ⚠" } else { "" };
                let lines = vec![
                    Line::from(vec![
                        Span::styled(format!("[{}] ", b.kind), Style::default().fg(Color::Cyan)),
                        Span::styled(&b.name, style),
                        Span::styled(contradiction_mark, style.fg(Color::Red)),
                        Span::styled(format!("  v{}", b.version), style.fg(Color::DarkGray)),
                    ]),
                    Line::from(vec![
                        Span::styled("    ", Style::default()),
                        Span::styled(&b.content, style.fg(Color::Gray)),
                        Span::styled(format!("  ({})", b.updated), style.fg(Color::DarkGray)),
                    ]),
                ];
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Vault Beliefs "),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_coordination(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .coordination
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

                let status_char = match c.status {
                    CoordinationStatus::Active => "●",
                    CoordinationStatus::Idle => "○",
                    CoordinationStatus::Away => "◌",
                };
                let status_color = match c.status {
                    CoordinationStatus::Active => Color::Green,
                    CoordinationStatus::Idle => Color::Yellow,
                    CoordinationStatus::Away => Color::DarkGray,
                };

                let lines = vec![Line::from(vec![
                    Span::styled(
                        format!("{} ", status_char),
                        Style::default().fg(status_color),
                    ),
                    Span::styled(&c.agent, style),
                    Span::styled(format!(" ({})", c.role), style.fg(Color::Cyan)),
                    Span::styled(format!("  {}", c.last_seen), style.fg(Color::DarkGray)),
                ])];
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Coordination "),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_history(&mut self, frame: &mut Frame, area: Rect) {
        let history_items = vec![
            (
                "2024-01-15 14:32",
                "architecture",
                "v3",
                "Updated decision: event sourcing for replay",
            ),
            (
                "2024-01-10 09:15",
                "search-indexing",
                "v1",
                "Created: tantivy + embeddings",
            ),
            (
                "2024-01-05 16:45",
                "rust-version",
                "v2",
                "Correction: MSRV is 1.75 not 1.70",
            ),
        ];

        let items: Vec<ListItem> = history_items
            .iter()
            .enumerate()
            .map(|(i, (time, name, version, desc))| {
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
                        Span::styled(*time, Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("  {}", name), style),
                        Span::styled(format!("  {}", version), style.fg(Color::Cyan)),
                    ]),
                    Line::from(vec![
                        Span::styled("    ", Style::default()),
                        Span::styled(*desc, style.fg(Color::Gray)),
                    ]),
                ];
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" History "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }
}

impl Overlay for ContextOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Context
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
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

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
                    let len = match self.active_tab {
                        0 => self.vault_beliefs.len(),
                        1 => self.coordination.len(),
                        _ => 0,
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
                    let len = match self.active_tab {
                        0 => self.vault_beliefs.len(),
                        1 => self.coordination.len(),
                        _ => 0,
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
                    let len = match self.active_tab {
                        0 => self.vault_beliefs.len(),
                        1 => self.coordination.len(),
                        _ => 0,
                    };
                    self.selected = len.saturating_sub(1);
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::Enter => {
                if !self.detail_view {
                    self.detail_view = true;
                    match self.active_tab {
                        0 => {
                            if let Some(b) = self.vault_beliefs.get(self.selected) {
                                self.detail_content = format!(
                                    "Name: {}\nKind: {}\nVersion: {}\nUpdated: {}\nContradicted: {}\n\n{}",
                                    b.name,
                                    b.kind,
                                    b.version,
                                    b.updated,
                                    if b.contradicted { "YES" } else { "no" },
                                    b.content
                                );
                            }
                        }
                        1 => {
                            if let Some(c) = self.coordination.get(self.selected) {
                                self.detail_content = format!(
                                    "Agent: {}\nRole: {}\nLast seen: {}\nStatus: {:?}",
                                    c.agent, c.role, c.last_seen, c.status
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('c') if ctrl => {
                // Copy current entry
                if !self.detail_view {
                    match self.active_tab {
                        0 => {
                            if let Some(b) = self.vault_beliefs.get(self.selected) {
                                app.push(
                                    crate::tui::app::EntryRole::Info,
                                    format!("Copied belief: {}", b.name),
                                );
                            }
                        }
                        1 => {
                            if let Some(c) = self.coordination.get(self.selected) {
                                app.push(
                                    crate::tui::app::EntryRole::Info,
                                    format!("Copied agent: {}", c.agent),
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('p') => {
                // Pin current entry
                if !self.detail_view {
                    match self.active_tab {
                        0 => {
                            if let Some(b) = self.vault_beliefs.get(self.selected) {
                                app.push(
                                    crate::tui::app::EntryRole::Info,
                                    format!("Pinned belief: {}", b.name),
                                );
                            }
                        }
                        1 => {
                            if let Some(c) = self.coordination.get(self.selected) {
                                app.push(
                                    crate::tui::app::EntryRole::Info,
                                    format!("Pinned agent: {}", c.agent),
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('r') => {
                // Refresh
                self.load_mock_data();
                app.push(
                    crate::tui::app::EntryRole::Info,
                    "Context refreshed".to_string(),
                );
            }
            KeyCode::Char('x') if ctrl => {
                // Compact context
                app.push(
                    crate::tui::app::EntryRole::Info,
                    "Context compact triggered (would summarize and prune)".to_string(),
                );
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
                ContextTab::Beliefs => "Beliefs",
                ContextTab::Coordination => "Coordination",
                ContextTab::History => "History",
            })
            .collect();
        let tabs = Tabs::new(tab_titles)
            .block(Block::default().borders(Borders::ALL).title(" Context "))
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
            match self.active_tab {
                0 => self.render_beliefs(frame, chunks[1]),
                1 => self.render_coordination(frame, chunks[1]),
                2 => self.render_history(frame, chunks[1]),
                _ => {}
            }
        }

        // Help footer
        let help_text = if self.detail_view {
            "esc: back · c: copy · p: pin"
        } else {
            "↑↓: navigate · enter: detail · tab: next tab · c: copy · p: pin · r: refresh · Ctrl+X: compact · esc: close"
        };
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(help, chunks[2]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/context", "Open context overlay"),
            ("Tab / Shift+Tab", "Switch tabs"),
            ("↑/↓", "Navigate entries"),
            ("Enter", "View detail"),
            ("Esc", "Close / Back from detail"),
            ("Ctrl+C", "Copy entry"),
            ("P", "Pin entry"),
            ("R", "Refresh"),
            ("Ctrl+X", "Compact context"),
        ]
    }
}
