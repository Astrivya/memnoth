//! Context pane: token budget grid with compact action, vault beliefs, and coordination.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Tabs, Wrap,
};

use crate::tui::app::App;

/// Token budget information
#[derive(Debug, Clone, Default)]
pub struct TokenBudget {
    pub used: usize,
    pub limit: usize,
    pub reserved: usize,
    pub available: usize,
}

impl TokenBudget {
    pub fn usage_percent(&self) -> f64 {
        if self.limit == 0 {
            0.0
        } else {
            self.used as f64 / self.limit as f64 * 100.0
        }
    }

    pub fn is_warning(&self) -> bool {
        self.usage_percent() > 75.0
    }

    pub fn is_critical(&self) -> bool {
        self.usage_percent() > 90.0
    }
}

/// Context pane showing token budget, vault beliefs, and coordination
#[derive(Debug, Default)]
pub struct ContextPane {
    pub visible: bool,
    pub tabs: Vec<ContextTab>,
    pub active_tab: usize,
    pub list_state: ListState,
    pub token_budget: TokenBudget,
    pub compact_requested: bool,
    pub vault_beliefs: Vec<BeliefEntry>,
    pub coordination: Vec<CoordinationEntry>,
    pub selected: usize,
    pub detail_view: bool,
    pub detail_content: String,
    pub show_token_grid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextTab {
    Overview,
    Beliefs,
    Coordination,
    Budget,
}

#[derive(Debug, Clone)]
struct BeliefEntry {
    name: String,
    kind: String,
    content: String,
    updated: String,
    version: u64,
    contradicted: bool,
    tokens: usize,
}

#[derive(Debug, Clone)]
struct CoordinationEntry {
    agent: String,
    role: String,
    last_seen: String,
    status: CoordinationStatus,
    tokens_used: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoordinationStatus {
    Active,
    Idle,
    Away,
}

impl ContextPane {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            visible: true,
            tabs: vec![
                ContextTab::Overview,
                ContextTab::Beliefs,
                ContextTab::Coordination,
                ContextTab::Budget,
            ],
            active_tab: 0,
            list_state: state,
            token_budget: TokenBudget::default(),
            compact_requested: false,
            vault_beliefs: Vec::new(),
            coordination: Vec::new(),
            selected: 0,
            detail_view: false,
            detail_content: String::new(),
            show_token_grid: true,
        }
    }

    /// Load data from vault and coordination
    pub fn load_data(&mut self, app: &App) {
        self.load_mock_data();
        self.update_token_budget(app);
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
                tokens: 45,
            },
            BeliefEntry {
                name: "search-indexing".to_string(),
                kind: "decision".to_string(),
                content: "Use tantivy for full-text, embedding for semantic".to_string(),
                updated: "2024-01-10 09:15".to_string(),
                version: 1,
                contradicted: false,
                tokens: 62,
            },
            BeliefEntry {
                name: "rust-version".to_string(),
                kind: "fact".to_string(),
                content: "MSRV is 1.75".to_string(),
                updated: "2024-01-05 16:45".to_string(),
                version: 2,
                contradicted: true,
                tokens: 18,
            },
        ];

        self.coordination = vec![
            CoordinationEntry {
                agent: "memnoth-kg".to_string(),
                role: "indexer".to_string(),
                last_seen: "2m ago".to_string(),
                status: CoordinationStatus::Active,
                tokens_used: 1240,
            },
            CoordinationEntry {
                agent: "memnoth-search".to_string(),
                role: "searcher".to_string(),
                last_seen: "5m ago".to_string(),
                status: CoordinationStatus::Idle,
                tokens_used: 890,
            },
            CoordinationEntry {
                agent: "human-dev".to_string(),
                role: "developer".to_string(),
                last_seen: "just now".to_string(),
                status: CoordinationStatus::Active,
                tokens_used: 0,
            },
        ];
    }

    fn update_token_budget(&mut self, app: &App) {
        // Estimate token usage from transcript
        let total_chars: usize = app.entries.iter().map(|e| e.text.len()).sum();
        let estimated_tokens = total_chars / 4; // Rough estimate
        self.token_budget = TokenBudget {
            used: estimated_tokens,
            limit: 128_000,
            reserved: 8_000,
            available: 128_000 - estimated_tokens - 8_000,
        };
    }

    /// Handle key events for the context pane
    pub fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> bool {
        if !self.visible {
            return false;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Tab => {
                self.active_tab = (self.active_tab + 1) % self.tabs.len();
                self.selected = 0;
                self.list_state.select(Some(0));
                true
            }
            KeyCode::BackTab => {
                self.active_tab = if self.active_tab == 0 {
                    self.tabs.len() - 1
                } else {
                    self.active_tab - 1
                };
                self.selected = 0;
                self.list_state.select(Some(0));
                true
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.list_state.select(Some(self.selected));
                }
                true
            }
            KeyCode::Down => {
                let max = match self.active_tab {
                    0 => 3, // Overview items
                    1 => self.vault_beliefs.len().saturating_sub(1),
                    2 => self.coordination.len().saturating_sub(1),
                    3 => 4, // Budget items
                    _ => 0,
                };
                if self.selected < max {
                    self.selected += 1;
                    self.list_state.select(Some(self.selected));
                }
                true
            }
            KeyCode::Char('c') if ctrl => {
                // Compact action
                self.compact_requested = true;
                app.push(
                    crate::tui::app::EntryRole::Info,
                    "Context compact requested".to_string(),
                );
                true
            }
            KeyCode::Char('g') => {
                self.show_token_grid = !self.show_token_grid;
                true
            }
            KeyCode::Enter => {
                self.detail_view = !self.detail_view;
                if self.detail_view {
                    self.update_detail_content();
                }
                true
            }
            KeyCode::Esc => {
                if self.detail_view {
                    self.detail_view = false;
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn update_detail_content(&mut self) {
        self.detail_content = match self.active_tab {
            1 => {
                if let Some(b) = self.vault_beliefs.get(self.selected) {
                    format!(
                        "Name: {}\nKind: {}\nVersion: {}\nUpdated: {}\nContradicted: {}\nTokens: {}\n\n{}",
                        b.name, b.kind, b.version, b.updated, b.contradicted, b.tokens, b.content
                    )
                } else {
                    String::new()
                }
            }
            2 => {
                if let Some(c) = self.coordination.get(self.selected) {
                    format!(
                        "Agent: {}\nRole: {}\nStatus: {:?}\nLast seen: {}\nTokens used: {}",
                        c.agent, c.role, c.status, c.last_seen, c.tokens_used
                    )
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        };
    }

    /// Render the context pane
    pub fn render(&mut self, frame: &mut Frame, area: Rect, app: &App) {
        if !self.visible {
            return;
        }

        // Update token budget
        self.update_token_budget(app);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Tabs
                Constraint::Min(0),    // Content
                Constraint::Length(3), // Footer with compact button
            ])
            .split(area);

        // Render tabs
        self.render_tabs(frame, chunks[0]);

        // Render content based on active tab
        match self.active_tab {
            0 => self.render_overview(frame, chunks[1]),
            1 => self.render_beliefs(frame, chunks[1]),
            2 => self.render_coordination(frame, chunks[1]),
            3 => self.render_budget(frame, chunks[1]),
            _ => {}
        }

        // Render footer with compact button
        self.render_footer(frame, chunks[2]);
    }

    fn render_tabs(&self, frame: &mut Frame, area: Rect) {
        let titles = vec!["Overview", "Beliefs", "Coordination", "Budget"];
        let tabs = titles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let style = if i == self.active_tab {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Span::styled(format!(" {} ", t), style)
            })
            .collect::<Vec<_>>();

        let tabs_widget = Paragraph::new(Line::from(tabs))
            .block(Block::default().borders(Borders::BOTTOM))
            .style(Style::default().fg(Color::White));
        frame.render_widget(tabs_widget, area);
    }

    fn render_overview(&mut self, frame: &mut Frame, area: Rect) {
        let budget = &self.token_budget;
        let usage_color = if budget.is_critical() {
            Color::Red
        } else if budget.is_warning() {
            Color::Yellow
        } else {
            Color::Green
        };

        let overview_items = vec![
            ("Vault Beliefs", self.vault_beliefs.len().to_string()),
            (
                "Active Agents",
                self.coordination
                    .iter()
                    .filter(|c| c.status == CoordinationStatus::Active)
                    .count()
                    .to_string(),
            ),
            (
                "Total Tokens Used",
                format!("{} / {}", budget.used, budget.limit),
            ),
            ("Available", budget.available.to_string()),
        ];

        let items: Vec<ListItem> = overview_items
            .iter()
            .enumerate()
            .map(|(i, (label, value))| {
                let selected = i == self.selected;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{}: ", label), style.fg(Color::Cyan)),
                    Span::styled(value, style),
                ]))
                .style(style)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Overview "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
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
                        Span::styled(
                            format!("  v{}  {} tok", b.version, b.tokens),
                            style.fg(Color::DarkGray),
                        ),
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
                    Span::styled(
                        format!("  {} tok  {}", c.tokens_used, c.last_seen),
                        style.fg(Color::DarkGray),
                    ),
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

    fn render_budget(&mut self, frame: &mut Frame, area: Rect) {
        let budget = &self.token_budget;
        let usage_color = if budget.is_critical() {
            Color::Red
        } else if budget.is_warning() {
            Color::Yellow
        } else {
            Color::Green
        };

        let budget_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Token gauge
                Constraint::Length(8), // Grid
                Constraint::Min(0),    // Details
            ])
            .split(area);

        // Token gauge
        let gauge = Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Token Budget "),
            )
            .gauge_style(Style::default().fg(usage_color))
            .ratio(budget.used as f64 / budget.limit as f64)
            .label(format!(
                "{}% ({}/{})",
                budget.usage_percent() as u32,
                budget.used,
                budget.limit
            ));
        frame.render_widget(gauge, budget_chunks[0]);

        // Token grid (visual representation)
        if self.show_token_grid {
            self.render_token_grid(frame, budget_chunks[1], budget);
        }

        // Details
        let details = vec![
            Line::from(vec![
                Span::styled("Used: ", Style::default().fg(Color::Cyan)),
                Span::styled(budget.used.to_string(), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("Reserved: ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    budget.reserved.to_string(),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("Available: ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    budget.available.to_string(),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("Limit: ", Style::default().fg(Color::Cyan)),
                Span::styled(budget.limit.to_string(), Style::default().fg(Color::White)),
            ]),
        ];
        let details_widget = Paragraph::new(details)
            .block(Block::default().borders(Borders::ALL).title(" Details "))
            .wrap(Wrap { trim: true });
        frame.render_widget(details_widget, budget_chunks[2]);
    }

    fn render_token_grid(&self, frame: &mut Frame, area: Rect, budget: &TokenBudget) {
        // Create a visual grid showing token usage
        let grid_width = area.width as usize;
        let grid_height = area.height as usize;
        let total_cells = grid_width * grid_height;
        let used_cells = ((budget.used as f64 / budget.limit as f64) * total_cells as f64) as usize;

        let usage_color = if budget.is_critical() {
            Color::Red
        } else if budget.is_warning() {
            Color::Yellow
        } else {
            Color::Green
        };

        let mut lines = Vec::new();
        for row in 0..grid_height {
            let mut spans = Vec::new();
            for col in 0..grid_width {
                let cell_idx = row * grid_width + col;
                let cell_char = if cell_idx < used_cells { "█" } else { "░" };
                let cell_color = if cell_idx < used_cells {
                    usage_color
                } else {
                    Color::DarkGray
                };
                spans.push(Span::styled(cell_char, Style::default().fg(cell_color)));
            }
            lines.push(Line::from(spans));
        }

        let grid = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Token Grid (█ = used) "),
        );
        frame.render_widget(grid, area);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let footer_text = if self.compact_requested {
            "Compacting context... (Ctrl+C to cancel)"
        } else {
            "Tab: switch tabs · ↑↓: navigate · Enter: detail · Ctrl+C: compact context · G: toggle grid · Esc: close detail"
        };
        let footer = Paragraph::new(footer_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(footer, area);
    }
}
