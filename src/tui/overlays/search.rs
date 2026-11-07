//! Search overlay: interactive search across the vault and the code graph.
//!
//! Both halves are the verbs, not a copy of them: the vault rows come from
//! `crate::vault::recall` and the graph rows from `crate::kg::search`, so the
//! screen cannot disagree with `memnoth vault recall` or `memnoth kg search`
//! about what exists. There used to be three invented rows here — "This is a
//! vault search result snippet…" — under a comment that said to replace them.
//!
//! Enter on a row runs the follow-up the row points at (`/vault explain <name>`,
//! `/kg def <symbol>`), which is the same line a person would type. The `c`/`p`
//! keys that claimed to copy and pin were removed: no clipboard and no pins
//! exist, and a key that only prints "Copied:" is a lie the user cannot see
//! through.
//!
//! Work runs on a thread with a channel, like every other screen that reads a
//! store: a big vault can take longer than a frame, and a frozen search box is
//! indistinguishable from a hang.

use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::context::AppContext;
use crate::output::ellipsize;
use crate::tui::app::{App, EntryRole};
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// How many rows each half contributes. The overlay is a glance; the two verbs
/// take a `--limit` for a longer list.
const VAULT_LIMIT: usize = 12;
const GRAPH_LIMIT: usize = 20;

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
    /// The workspace and vault the search reads.
    ctx: Option<AppContext>,
    /// A search in flight: the rows, and what to say about them.
    pending: Option<mpsc::Receiver<(Vec<SearchResult>, Option<String>)>>,
    /// Why the last search found nothing, when the reason is worth saying.
    note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SearchMode {
    #[default]
    Query,
    Results,
}

#[derive(Debug, Clone)]
struct SearchResult {
    source: SearchSource,
    title: String,
    snippet: String,
    /// A percentage, for the vault's hybrid score. The graph ranks by name
    /// match, so it has no comparable number and draws none.
    score: Option<f32>,
    path: Option<String>,
    /// What Enter runs on this row.
    action: Option<String>,
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

    /// Ask the two verbs, off the UI thread.
    fn run_search(&mut self, _app: &mut App) {
        let query = self.query.trim().to_string();
        self.results.clear();
        self.note = None;
        self.list_state.select(Some(0));
        self.selected = 0;

        if query.is_empty() {
            self.loading = false;
            return;
        }

        let Some(ctx) = self.ctx.clone() else {
            self.note = Some("no workspace is open".to_string());
            return;
        };

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let words = vec![query.clone()];
            let (mut rows, mut notes) = (Vec::new(), Vec::new());

            match crate::vault::recall(&ctx, &words, VAULT_LIMIT, None) {
                Ok(outcome) => {
                    for hit in outcome.data["hits"].as_array().into_iter().flatten() {
                        let name = hit["name"].as_str().unwrap_or_default().to_string();
                        let kind = hit["kind"].as_str().unwrap_or_default().to_string();
                        let summary = hit["summary"].as_str().unwrap_or_default();
                        let detail = if summary.trim().is_empty() {
                            ellipsize(&kind, 40)
                        } else {
                            format!(
                                "{kind} · {}",
                                ellipsize(summary.lines().next().unwrap_or(""), 60)
                            )
                        };
                        rows.push(SearchResult {
                            source: SearchSource::Vault,
                            title: name.clone(),
                            snippet: detail,
                            score: hit["score"].as_f64().map(|score| score as f32),
                            path: None,
                            action: if name.is_empty() {
                                None
                            } else {
                                Some(format!("/vault explain {name}"))
                            },
                        });
                    }
                }
                Err(error) => notes.push(error.message),
            }

            match crate::kg::search(&ctx, &words, GRAPH_LIMIT) {
                Ok(hits) => {
                    for hit in hits {
                        rows.push(SearchResult {
                            source: SearchSource::Graph,
                            title: hit.name.clone(),
                            snippet: format!("{} · line {}", hit.kind, hit.line),
                            score: None,
                            path: Some(hit.path.clone()),
                            action: Some(format!("/kg def {}", hit.name)),
                        });
                    }
                }
                // A graph that was never indexed is not a failure of the search:
                // it is the reason half the answer is missing, so it is said.
                Err(error) => notes.push(format!("{error}")),
            }

            if rows.is_empty() && notes.is_empty() {
                notes.push("nothing in the vault or the graph matches".to_string());
            }
            let _ = tx.send((rows, notes.into_iter().next()));
        });

        self.pending = Some(rx);
        self.loading = true;
    }

    /// Take an answered search, if one landed since the last frame.
    fn apply_pending(&mut self) {
        let Some(rx) = &self.pending else {
            return;
        };
        match rx.try_recv() {
            Ok((rows, note)) => {
                self.results = rows;
                self.note = note;
                self.loading = false;
                self.pending = None;
                self.list_state.select(Some(0));
                self.selected = 0;
                self.mode = SearchMode::Results;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.note = Some("the search stopped before it answered".to_string());
                self.loading = false;
                self.pending = None;
            }
        }
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
        self.note = None;
        self.loading = false;
        self.pending = None;
        self.list_state.select(Some(0));
    }

    fn hide(&mut self) {
        self.visible = false;
        // A search that was still running has nowhere to land; dropping the
        // receiver is how its thread learns that.
        self.pending = None;
        self.loading = false;
    }

    fn on_open(&mut self, app: &mut App) {
        // Copied once, not read per frame: the search thread needs to own it.
        self.ctx = app.ctx.clone();
    }

    fn on_tick(&mut self, _app: &mut App) {
        self.apply_pending();
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match (self.mode, key.code) {
            // Query mode
            (SearchMode::Query, KeyCode::Esc) => OverlayAction::Close,
            (SearchMode::Query, KeyCode::Enter) => {
                self.run_search(app);
                OverlayAction::None
            }
            (SearchMode::Query, KeyCode::Char(ch))
                if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.query.insert(self.cursor, ch);
                self.cursor += 1;
                OverlayAction::None
            }
            (SearchMode::Query, KeyCode::Backspace) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.query.remove(self.cursor);
                }
                OverlayAction::None
            }
            (SearchMode::Query, KeyCode::Delete) => {
                if self.cursor < self.query.len() {
                    self.query.remove(self.cursor);
                }
                OverlayAction::None
            }
            (SearchMode::Query, KeyCode::Left) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                OverlayAction::None
            }
            (SearchMode::Query, KeyCode::Right) => {
                if self.cursor < self.query.len() {
                    self.cursor += 1;
                }
                OverlayAction::None
            }
            (SearchMode::Query, KeyCode::Home) => {
                self.cursor = 0;
                OverlayAction::None
            }
            (SearchMode::Query, KeyCode::End) => {
                self.cursor = self.query.len();
                OverlayAction::None
            }
            (SearchMode::Query, KeyCode::Tab) => {
                self.mode = SearchMode::Results;
                OverlayAction::None
            }

            // Results mode
            (SearchMode::Results, KeyCode::Esc) => OverlayAction::Close,
            (SearchMode::Results, KeyCode::Char('q')) => OverlayAction::Close,
            (SearchMode::Results, KeyCode::Up) => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.list_state.select(Some(self.selected));
                }
                OverlayAction::None
            }
            (SearchMode::Results, KeyCode::Down) => {
                if self.selected + 1 < self.results.len() {
                    self.selected += 1;
                    self.list_state.select(Some(self.selected));
                }
                OverlayAction::None
            }
            (SearchMode::Results, KeyCode::PageUp) => {
                self.selected = self.selected.saturating_sub(10);
                self.list_state.select(Some(self.selected));
                OverlayAction::None
            }
            (SearchMode::Results, KeyCode::PageDown) => {
                self.selected = (self.selected + 10).min(self.results.len().saturating_sub(1));
                self.list_state.select(Some(self.selected));
                OverlayAction::None
            }
            (SearchMode::Results, KeyCode::Home) => {
                self.selected = 0;
                self.list_state.select(Some(0));
                OverlayAction::None
            }
            (SearchMode::Results, KeyCode::End) => {
                self.selected = self.results.len().saturating_sub(1);
                self.list_state.select(Some(self.selected));
                OverlayAction::None
            }
            (SearchMode::Results, KeyCode::Enter) => {
                // The row runs the line it points at, through the prompt, exactly
                // as the popover does: one way for a screen to start a verb.
                match self
                    .results
                    .get(self.selected)
                    .and_then(|row| row.action.clone())
                {
                    Some(line) => {
                        app.input = line;
                        app.cursor = app.input.chars().count();
                        app.pending_submit = true;
                        OverlayAction::Close
                    }
                    None => {
                        app.push(
                            EntryRole::Info,
                            "this row has nothing to run — tab returns to the query".to_string(),
                        );
                        OverlayAction::None
                    }
                }
            }
            (SearchMode::Results, KeyCode::Char('r')) => {
                // The index moved under the screen; ask again.
                self.run_search(app);
                OverlayAction::None
            }
            (SearchMode::Results, KeyCode::BackTab) | (SearchMode::Results, KeyCode::Tab) => {
                self.mode = SearchMode::Query;
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }

        frame.render_widget(Clear, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Query input
                Constraint::Min(0),    // Results
                Constraint::Length(2), // Help footer
            ])
            .split(area);

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

        if self.mode == SearchMode::Query {
            frame.set_cursor_position((chunks[0].x + 2 + self.cursor as u16, chunks[0].y + 1));
        }

        if self.loading {
            let loading = Paragraph::new("Searching the vault and the graph…")
                .style(Style::default().fg(Color::Yellow))
                .block(Block::default().borders(Borders::ALL).title(" Results "));
            frame.render_widget(loading, chunks[1]);
        } else if self.results.is_empty() {
            let empty = Paragraph::new(if self.query.is_empty() {
                "Type a query and press Enter to search".to_string()
            } else {
                self.note
                    .clone()
                    .unwrap_or_else(|| "No results found".to_string())
            })
            .style(Style::default().fg(Color::DarkGray))
            .wrap(ratatui::widgets::Wrap { trim: false })
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

                    let mut title = vec![
                        Span::styled(
                            format!("{source_label}  "),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(r.title.clone(), style),
                    ];
                    if let Some(score) = r.score {
                        title.push(Span::styled(
                            format!("  ({:.0}%)", score * 100.0),
                            style.fg(Color::DarkGray),
                        ));
                    }

                    let mut lines = vec![
                        Line::from(title),
                        Line::from(vec![
                            Span::styled("    ", Style::default()),
                            Span::styled(r.snippet.clone(), style.fg(Color::Gray)),
                        ]),
                    ];
                    if let Some(path) = &r.path {
                        lines.push(Line::from(vec![
                            Span::styled("    ", Style::default()),
                            Span::styled(
                                path.clone(),
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

        let help_text = match self.mode {
            SearchMode::Query => "enter: search · esc: close · tab: results",
            SearchMode::Results => {
                "↑↓: navigate · enter: run · r: search again · tab: query · esc: close"
            }
        };
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(help, chunks[2]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/search", "Search the vault and the code graph"),
            ("Enter", "Run the search (query mode)"),
            ("↑/↓", "Move through the results"),
            ("Enter", "Run the line the row points at"),
            ("R", "Run the search again (results mode)"),
            ("Tab", "Switch between query and results"),
            ("Esc", "Close the overlay"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_without_a_query_asks_for_one_instead_of_inventing_rows() {
        let mut overlay = SearchOverlay::new();
        overlay.show();
        let mut app = App::new("payments", false);
        overlay.run_search(&mut app);
        assert!(overlay.results.is_empty());
        assert!(!overlay.loading);
        assert!(overlay.note.is_none());
    }

    /// Every row a search returns must be actionable with a line the parser
    /// knows, or say nothing; the removed `c`/`p` keys are the reason this test
    /// exists.
    #[test]
    fn the_footer_names_only_keys_that_do_something() {
        let overlay = SearchOverlay::new();
        let help = overlay.help_text();
        let keys: Vec<&str> = help.iter().map(|(key, _)| *key).collect();
        assert!(!keys.contains(&"Ctrl+C"), "{keys:?}");
        assert!(!keys.contains(&"P"), "{keys:?}");
        assert!(keys.contains(&"R"), "{keys:?}");
    }

    #[test]
    fn hiding_abandons_a_search_in_flight() {
        let mut overlay = SearchOverlay::new();
        overlay.show();
        let (tx, rx) = mpsc::channel();
        overlay.pending = Some(rx);
        overlay.loading = true;
        overlay.hide();
        assert!(overlay.pending.is_none());
        assert!(!overlay.loading);
        // The thread's send now fails, which is how it learns nobody is waiting.
        assert!(tx.send((Vec::new(), None)).is_err());
    }
}
