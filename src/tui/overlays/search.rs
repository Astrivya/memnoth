//! Search overlay: interactive search across vault and code graph.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

#[derive(Debug, Default)]
pub struct SearchOverlay {
    visible: bool,
    query: String,
    cursor: usize,
    results: Vec<SearchResult>,
    selected: usize,
    list_state: ListState,
    mode: SearchMode,
    loading: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SearchMode {
    Query,
    Results,
}

#[derive(Debug, Clone)]
struct SearchResult {
    source: SearchSource,
    title: String,
    snippet: String,
    score: f32,
    path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchSource {
    Vault,
    Graph,
}

impl SearchOverlay {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            list_state: state,
            ..Default::default()
        }
    }

    fn run_search(&mut self, app: &mut App) {
        // In a real implementation, this would call the vault and graph search
        // For now, populate with mock data to demonstrate the UI
        self.loading = true;
        self.results.clear();

        if self.query.trim().is_empty() {
            self.loading = false;
            return;
        }

        // Mock results - replace with actual search calls
        self.results.push(SearchResult {
            source: SearchSource::Vault,
            title: format!("Vault: {}", self.query),
            snippet: "This is a vault search result snippet showing relevant context..."
                .to_string(),
            score: 0.95,
            path: Some("vault/decisions/architecture.md".to_string()),
        });
        self.results.push(SearchResult {
            source: SearchSource::Graph,
            title: format!("Graph: {}", self.query),
            snippet: "Symbol definition found in src/main.rs:42".to_string(),
            score: 0.87,
            path: Some("src/main.rs".to_string()),
        });
        self.results.push(SearchResult {
            source: SearchSource::Vault,
            title: "Related decision".to_string(),
            snippet: "Decision about search indexing strategy from 2024-01-15".to_string(),
            score: 0.72,
            path: Some("vault/decisions/search.md".to_string()),
        });

        self.loading = false;
        self.list_state.select(Some(0));
        self.mode = SearchMode::Results;
    }
}

impl Overlay for SearchOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Search
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.mode = SearchMode::Query;
        self.query.clear();
        self.cursor = 0;
        self.results.clear();
        self.list_state.select(Some(0));
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match (self.mode, key.code) {
            // Query mode
            (SearchMode::Query, KeyCode::Esc) => return OverlayAction::Close,
            (SearchMode::Query, KeyCode::Enter) => {
                self.run_search(app);
                return OverlayAction::None;
            }
            (SearchMode::Query, KeyCode::Char(ch))
                if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.query.insert(self.cursor, ch);
                self.cursor += 1;
                return OverlayAction::None;
            }
            (SearchMode::Query, KeyCode::Backspace) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.query.remove(self.cursor);
                }
                return OverlayAction::None;
            }
            (SearchMode::Query, KeyCode::Delete) => {
                if self.cursor < self.query.len() {
                    self.query.remove(self.cursor);
                }
                return OverlayAction::None;
            }
            (SearchMode::Query, KeyCode::Left) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                return OverlayAction::None;
            }
            (SearchMode::Query, KeyCode::Right) => {
                if self.cursor < self.query.len() {
                    self.cursor += 1;
                }
                return OverlayAction::None;
            }
            (SearchMode::Query, KeyCode::Home) => {
                self.cursor = 0;
                return OverlayAction::None;
            }
            (SearchMode::Query, KeyCode::End) => {
                self.cursor = self.query.len();
                return OverlayAction::None;
            }
            (SearchMode::Query, KeyCode::Tab) => {
                self.mode = SearchMode::Results;
                return OverlayAction::None;
            }

            // Results mode
            (SearchMode::Results, KeyCode::Esc) => return OverlayAction::Close,
            (SearchMode::Results, KeyCode::Char('q')) => return OverlayAction::Close,
            (SearchMode::Results, KeyCode::Up) => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.list_state.select(Some(self.selected));
                }
                return OverlayAction::None;
            }
            (SearchMode::Results, KeyCode::Down) => {
                if self.selected + 1 < self.results.len() {
                    self.selected += 1;
                    self.list_state.select(Some(self.selected));
                }
                return OverlayAction::None;
            }
            (SearchMode::Results, KeyCode::PageUp) => {
                self.selected = self.selected.saturating_sub(10);
                self.list_state.select(Some(self.selected));
                return OverlayAction::None;
            }
            (SearchMode::Results, KeyCode::PageDown) => {
                self.selected = (self.selected + 10).min(self.results.len().saturating_sub(1));
                self.list_state.select(Some(self.selected));
                return OverlayAction::None;
            }
            (SearchMode::Results, KeyCode::Home) => {
                self.selected = 0;
                self.list_state.select(Some(0));
                return OverlayAction::None;
            }
            (SearchMode::Results, KeyCode::End) => {
                self.selected = self.results.len().saturating_sub(1);
                self.list_state.select(Some(self.selected));
                return OverlayAction::None;
            }
            (SearchMode::Results, KeyCode::Enter) => {
                // Open selected result
                if let Some(result) = self.results.get(self.selected) {
                    if let Some(path) = &result.path {
                        app.push(
                            crate::tui::app::EntryRole::Info,
                            format!("Opening: {}", path),
                        );
                    }
                }
                return OverlayAction::None;
            }
            (SearchMode::Results, KeyCode::Char('c')) if ctrl => {
                // Copy selected result
                if let Some(result) = self.results.get(self.selected) {
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        format!("Copied: {}", result.title),
                    );
                }
                return OverlayAction::None;
            }
            (SearchMode::Results, KeyCode::Char('p')) => {
                // Pin selected result
                if let Some(result) = self.results.get(self.selected) {
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        format!("Pinned: {}", result.title),
                    );
                }
                return OverlayAction::None;
            }
            (SearchMode::Results, KeyCode::BackTab) | (SearchMode::Results, KeyCode::Tab) => {
                self.mode = SearchMode::Query;
                return OverlayAction::None;
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }

        // Clear background
        frame.render_widget(Clear, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Query input
                Constraint::Min(0),    // Results
                Constraint::Length(2), // Help footer
            ])
            .split(area);

        // Query input
        let query_text = if self.mode == SearchMode::Query {
            format!("> {}", self.query)
        } else {
            format!("  {}", self.query)
        };

        let query_style = if self.mode == SearchMode::Query {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Gray)
        };

        let query_widget = Paragraph::new(query_text)
            .style(query_style)
            .block(Block::default().borders(Borders::ALL).title(" Search "));
        frame.render_widget(query_widget, chunks[0]);

        // Show cursor in query mode
        if self.mode == SearchMode::Query {
            frame.set_cursor_position((chunks[0].x + 2 + self.cursor as u16, chunks[0].y + 1));
        }

        // Results
        if self.loading {
            let loading = Paragraph::new("Searching...")
                .style(Style::default().fg(Color::Yellow))
                .block(Block::default().borders(Borders::ALL).title(" Results "));
            frame.render_widget(loading, chunks[1]);
        } else if self.results.is_empty() {
            let empty = Paragraph::new(if self.query.is_empty() {
                "Type a query and press Enter to search"
            } else {
                "No results found"
            })
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" Results "));
            frame.render_widget(empty, chunks[1]);
        } else {
            let items: Vec<ListItem> = self
                .results
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let source_label = match r.source {
                        SearchSource::Vault => "📦",
                        SearchSource::Graph => "🔗",
                    };
                    let selected = i == self.selected;
                    let style = if selected {
                        Style::default()
                            .bg(Color::Blue)
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };

                    let mut lines = vec![
                        Line::from(vec![
                            Span::styled(
                                format!("{}  ", source_label),
                                Style::default().fg(Color::Cyan),
                            ),
                            Span::styled(&r.title, style),
                            Span::styled(
                                format!("  ({:.0}%)", r.score * 100.0),
                                style.fg(Color::DarkGray),
                            ),
                        ]),
                        Line::from(vec![
                            Span::styled("    ", Style::default()),
                            Span::styled(&r.snippet, style.fg(Color::Gray)),
                        ]),
                    ];
                    if let Some(path) = &r.path {
                        lines.push(Line::from(vec![
                            Span::styled("    ", Style::default()),
                            Span::styled(
                                path,
                                style.fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                            ),
                        ]));
                    }
                    ListItem::new(lines).style(style)
                })
                .collect();

            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" Results "))
                .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
            frame.render_stateful_widget(list, chunks[1], &mut self.list_state);
        }

        // Help footer
        let help_text = match self.mode {
            SearchMode::Query => "enter: search · esc: close · tab: results",
            SearchMode::Results => {
                "↑↓: navigate · enter: open · c: copy · p: pin · esc: close · tab: query"
            }
        };
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(help, chunks[2]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/search", "Open search overlay"),
            ("Enter", "Execute search (query mode)"),
            ("↑/↓", "Navigate results"),
            ("Enter", "Open selected result"),
            ("Ctrl+C", "Copy result"),
            ("P", "Pin result"),
            ("Esc", "Close overlay"),
            ("Tab", "Switch query/results"),
        ]
    }
}
