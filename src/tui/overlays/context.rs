//! `/context`: what this workspace's vault currently believes, and what it
//! recently recorded.
//!
//! Everything here is read, never invented. The two lists are the same reads
//! `memnoth vault asof` and `memnoth vault timeline` answer with — one
//! implementation behind the verb and the screen, so the overlay cannot drift
//! from the command a person would otherwise type.
//!
//! A state with no rows (no vault yet, an unreadable vault, an empty vault)
//! renders as words: an empty table would say "nothing is known", and "nothing
//! could be read" is not that.
//!
//! Two tabs were removed rather than kept: a coordination list no local surface
//! writes (agent presence lives in the cloud), and actions no key could perform
//! (pin, copy, compact). A screen that only shows what it read has no such
//! problem.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};

use crate::context::AppContext;
use crate::output::ErrorOutcome;
use crate::provider;
use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// How many entities and events one open reads. The overlay is a glance, not an
/// export: `memnoth vault asof --limit` is where a longer list comes from.
const LIMIT: usize = 200;

/// What the overlay is showing.
#[derive(Debug, Default)]
pub struct ContextOverlay {
    visible: bool,
    tabs: Vec<ContextTab>,
    active_tab: usize,
    /// Live entities, as `vault asof` reports them.
    beliefs: Vec<Belief>,
    /// Recent entity versions, as `vault timeline` reports them.
    activity: Vec<Activity>,
    /// Why a list is empty, when the reason is worth saying.
    note: Option<String>,
    /// What the next request would spend, one labelled row per fact: window,
    /// budget, each admitted slot, each drop with its reason, the prefix hash.
    /// Rebuilt from the assembler on every open, never cached in the session.
    prompt: Vec<(String, String)>,
    selected: usize,
    list_state: ListState,
    detail_view: bool,
    detail_content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextTab {
    Beliefs,
    Activity,
    Prompt,
}

/// One entity the vault believes right now.
#[derive(Debug, Clone)]
struct Belief {
    name: String,
    kind: String,
    confidence: f64,
    summary: String,
    updated: String,
    /// The observations that make the entity true, newest first.
    facts: Vec<String>,
}

/// One version of one entity: the activity feed.
#[derive(Debug, Clone)]
struct Activity {
    at: String,
    name: String,
    kind: String,
    /// `active` or `closed`, as the timeline reports it.
    state: &'static str,
    summary: String,
}

impl ContextOverlay {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            tabs: vec![
                ContextTab::Beliefs,
                ContextTab::Activity,
                ContextTab::Prompt,
            ],
            list_state: state,
            ..Default::default()
        }
    }

    /// The tab the overlay is showing.
    fn tab(&self) -> ContextTab {
        self.tabs
            .get(self.active_tab)
            .copied()
            .unwrap_or(ContextTab::Beliefs)
    }

    /// Read the vault and replace both lists.
    ///
    /// Two independent reads: a timeline that fails must not empty a belief
    /// list that succeeded, so each failure paints its own tab.
    fn reload(&mut self, ctx: &AppContext) {
        self.note = None;

        match crate::vault::asof(ctx, None, None, LIMIT) {
            Ok(outcome) => self.beliefs = beliefs_from(&outcome.data),
            Err(error) => {
                self.beliefs.clear();
                self.note = Some(describe(&error));
            }
        }

        match crate::vault::timeline(ctx, LIMIT, None, None, false) {
            Ok(outcome) => self.activity = activity_from(&outcome.data),
            Err(error) => {
                self.activity.clear();
                // The belief read's message wins when both fail: for a missing
                // vault they say the same thing, and repeating it twice reads
                // as two problems.
                if self.note.is_none() {
                    self.note = Some(describe(&error));
                }
            }
        }

        if self.beliefs.is_empty() && self.note.is_none() {
            self.note = Some(format!(
                "the vault holds nothing yet — record a fact with\n  /vault remember \
                 <name> -- <fact>\n\n({})",
                ctx.vault_path.display()
            ));
        }

        self.prompt = prompt_rows(ctx);
        self.clamp();
    }

    fn rows(&self) -> usize {
        match self.tab() {
            ContextTab::Beliefs => self.beliefs.len(),
            ContextTab::Activity => self.activity.len(),
            ContextTab::Prompt => self.prompt.len(),
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
            ContextTab::Prompt => prompt_detail(self),
            ContextTab::Beliefs => match self.beliefs.get(self.selected) {
                Some(belief) => {
                    let mut text = format!(
                        "{}\n\nkind        {}\nconfidence  {:.2}\nupdated     {}\n\n{}",
                        belief.name, belief.kind, belief.confidence, belief.updated, belief.summary
                    );
                    if belief.facts.is_empty() {
                        text.push_str("\n\nno observation recorded for it yet");
                    } else {
                        text.push_str("\n\nobservations");
                        for fact in &belief.facts {
                            text.push_str(&format!("\n  · {fact}"));
                        }
                    }
                    text.push_str(&format!(
                        "\n\n/vault history {} — every version it ever had",
                        belief.name
                    ));
                    text
                }
                None => "nothing selected".to_string(),
            },
            ContextTab::Activity => match self.activity.get(self.selected) {
                Some(row) => format!(
                    "{}\n\nkind        {}\nwhen        {}\nstate       {}\n\n{}",
                    row.name, row.kind, row.at, row.state, row.summary
                ),
                None => "nothing selected".to_string(),
            },
        };
    }

    /// A line explaining a screen that has no rows, and therefore needs words.
    fn state_text(&self) -> String {
        if let Some(note) = &self.note {
            return note.clone();
        }
        match self.tab() {
            ContextTab::Activity => {
                "no recorded versions — /vault remember records one".to_string()
            }
            ContextTab::Prompt => {
                "the prompt has no slots yet — ask a question, then /context".to_string()
            }
            ContextTab::Beliefs => "nothing is believed right now".to_string(),
        }
    }

    fn render_beliefs(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .beliefs
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
                let summary = if b.summary.trim().is_empty() {
                    b.facts
                        .first()
                        .map(String::as_str)
                        .unwrap_or("")
                        .to_string()
                } else {
                    b.summary.clone()
                };
                let lines = vec![
                    Line::from(vec![
                        Span::styled(format!("[{}] ", b.kind), Style::default().fg(Color::Cyan)),
                        Span::styled(&b.name, style),
                        Span::styled(format!("  {:.2}", b.confidence), style.fg(Color::DarkGray)),
                    ]),
                    Line::from(vec![
                        Span::styled("    ", Style::default()),
                        Span::styled(ellipsize(&summary, 120), style.fg(Color::Gray)),
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
                    .title(format!(" Vault Beliefs ({}) ", self.beliefs.len())),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_activity(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .activity
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let selected = i == self.selected;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let state_color = if row.state == "active" {
                    Color::Green
                } else {
                    Color::DarkGray
                };
                let lines = vec![
                    Line::from(vec![
                        Span::styled(row.at.clone(), Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("  {}", row.name), style),
                        Span::styled(
                            format!("  [{}]", row.kind),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(format!("  {}", row.state), style.fg(state_color)),
                    ]),
                    Line::from(vec![
                        Span::styled("    ", Style::default()),
                        Span::styled(ellipsize(&row.summary, 120), style.fg(Color::Gray)),
                    ]),
                ];
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Recent Versions ({}) ", self.activity.len())),
            )
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
        self.active_tab = 0;
        self.selected = 0;
        self.detail_view = false;
        self.detail_content.clear();
    }

    /// Reading needs the app's context, which `show` does not have.
    fn on_open(&mut self, app: &mut App) {
        match app.ctx.clone() {
            Some(ctx) => self.reload(&ctx),
            None => {
                self.note =
                    Some("no workspace context — open this from a session, not a test".to_string());
                self.clamp();
            }
        }
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
            // `r` rereads the vault. Nothing is said in the transcript: the
            // list changing is the answer, and a note that says "refreshed"
            // would be noise on every press.
            KeyCode::Char('r') if !ctrl => {
                if let Some(ctx) = app.ctx.clone() {
                    self.reload(&ctx);
                    self.detail_view = false;
                    self.detail_content.clear();
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

        let tab_titles: Vec<&str> = self
            .tabs
            .iter()
            .map(|t| match t {
                ContextTab::Beliefs => "Beliefs",
                ContextTab::Activity => "Activity",
                ContextTab::Prompt => "Prompt",
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
                            ContextTab::Beliefs => " Vault Beliefs ",
                            ContextTab::Activity => " Recent Versions ",
                            ContextTab::Prompt => " Prompt ",
                        }),
                )
                .wrap(Wrap { trim: true });
            frame.render_widget(note, chunks[1]);
        } else {
            match self.tab() {
                ContextTab::Beliefs => self.render_beliefs(frame, chunks[1]),
                ContextTab::Activity => self.render_activity(frame, chunks[1]),
                ContextTab::Prompt => render_prompt(self, frame, chunks[1]),
            }
        }

        let help_text = if self.detail_view {
            "esc: back"
        } else {
            "↑↓: move · enter: detail · tab: next tab · r: reread the vault · esc: close"
        };
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(help, chunks[2]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "/context",
                "What the next request spends, and what the vault believes",
            ),
            ("Tab / Shift+Tab", "Switch Beliefs / Activity / Prompt"),
            ("↑/↓", "Move"),
            ("Enter", "Detail"),
            ("R", "Reread the vault"),
            ("Esc", "Close / back from detail"),
        ]
    }
}

/// Entities, plus the observations that make each one true.
fn beliefs_from(data: &serde_json::Value) -> Vec<Belief> {
    let mut facts: Vec<(String, String)> = Vec::new();
    for fact in data["facts"].as_array().into_iter().flatten() {
        let entity = fact["entity"].as_str().unwrap_or_default().to_string();
        let content = fact["content"].as_str().unwrap_or_default().to_string();
        if !entity.is_empty() && !content.trim().is_empty() {
            facts.push((entity, content));
        }
    }

    data["entities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entity| {
            let name = entity["name"].as_str()?.to_string();
            Some(Belief {
                facts: facts
                    .iter()
                    .filter(|(owner, _)| *owner == name)
                    .map(|(_, content)| content.clone())
                    .collect(),
                kind: entity["kind"].as_str().unwrap_or("unknown").to_string(),
                confidence: entity["confidence"].as_f64().unwrap_or(0.0),
                summary: entity["summary"].as_str().unwrap_or_default().to_string(),
                updated: entity["valid_from_iso"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_default(),
                name,
            })
        })
        .collect()
}

/// Recent entity versions, newest first as the timeline reports them.
fn activity_from(data: &serde_json::Value) -> Vec<Activity> {
    data["facts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|fact| Activity {
            at: fact["valid_from"]
                .as_i64()
                .and_then(stamp)
                .unwrap_or_default(),
            name: fact["name"].as_str().unwrap_or_default().to_string(),
            kind: fact["kind"].as_str().unwrap_or("unknown").to_string(),
            state: if fact["valid_to"].is_null() {
                "active"
            } else {
                "closed"
            },
            summary: fact["summary"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

/// Epoch milliseconds as a local-looking stamp, or `None` when the value is not
/// a date this build can format.
fn stamp(ms: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|when| when.format("%Y-%m-%d %H:%M UTC").to_string())
}

/// A failure as a screen can show it: what went wrong, and the fix if there is
/// one. `ErrorOutcome` already carries both, so nothing is invented here.
fn describe(error: &ErrorOutcome) -> String {
    let mut text = error.message.clone();
    if let Some(cause) = &error.cause {
        text.push_str(&format!("\n\n{cause}"));
    }
    if let Some(fix) = &error.fix {
        text.push_str(&format!("\n\n{fix}"));
    }
    text
}

/// Keep one drawn row inside the pane's width.
fn ellipsize(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The prompt report, read from the assembler rather than from a count kept
/// beside it. Every row is a fact the next request would act on.
fn prompt_rows(ctx: &AppContext) -> Vec<(String, String)> {
    let settings = match provider::Settings::load(&ctx.config) {
        Ok(settings) => settings,
        Err(error) => {
            return vec![(
                "notice".to_string(),
                format!("cannot resolve the provider: {error}"),
            )];
        }
    };
    let window = settings.context_window();
    let budget = settings.prompt_budget();
    let assembly = provider::assemble_system_prompt_for(&ctx.root, Some(budget));
    let receipt = &assembly.receipt;

    let mut rows: Vec<(String, String)> = vec![
        ("window".to_string(), format!("{window} tokens")),
        ("request budget".to_string(), format!("{budget} tokens")),
        (
            "committed".to_string(),
            format!(
                "{} tokens · {} pieces",
                receipt.total_tokens,
                receipt.pieces.len()
            ),
        ),
    ];

    for piece in &receipt.pieces {
        let layer = ["Global", "Project", "Session", "Turn"]
            .get(piece.layer as usize)
            .copied()
            .unwrap_or("unknown");
        rows.push((
            format!("slot {}", piece.slot),
            format!("{} tokens · {layer}", piece.tokens),
        ));
    }

    // A missing slot is stated, not omitted: the whole point of the screen is
    // to show what the budget is *not* spending.
    for slot in [
        "identity",
        "policy",
        "decisions",
        "session",
        "memory",
        "graph",
        "tools_hint",
    ] {
        if !receipt.pieces.iter().any(|piece| piece.slot == slot) {
            rows.push((
                format!("slot {slot}"),
                "not in this prompt — that layer is not wired yet".to_string(),
            ));
        }
    }

    for (slot, reason) in &receipt.dropped {
        rows.push((format!("dropped {slot}"), format!("{reason:?}")));
    }

    rows.push((
        "prefix hash".to_string(),
        memnoth_context::assemble::hash_label(receipt.prefix_hash),
    ));
    rows
}

fn render_prompt(overlay: &mut ContextOverlay, frame: &mut Frame, area: Rect) {
    let items: Vec<ListItem> = overlay
        .prompt
        .iter()
        .enumerate()
        .map(|(index, (name, value))| {
            let style = if index == overlay.selected {
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{name:<16}"), Style::default().fg(Color::Cyan)),
                Span::styled(value.clone(), style),
            ]))
            .style(style)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Prompt ({}) ", overlay.prompt.len())),
        )
        .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
    frame.render_stateful_widget(list, area, &mut overlay.list_state);
}

fn prompt_detail(overlay: &ContextOverlay) -> String {
    match overlay.prompt.get(overlay.selected) {
        Some((name, value)) => format!("{name}\n\n{value}"),
        None => "nothing selected".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn belief(name: &str) -> Belief {
        Belief {
            name: name.to_string(),
            kind: "decision".to_string(),
            confidence: 0.9,
            summary: format!("{name} is the decided thing"),
            updated: "2026-01-02 03:04 UTC".to_string(),
            facts: vec![format!("{name} was recorded")],
        }
    }

    #[test]
    fn every_tab_is_a_local_read() {
        let overlay = ContextOverlay::new();
        assert_eq!(
            overlay.tabs,
            vec![
                ContextTab::Beliefs,
                ContextTab::Activity,
                ContextTab::Prompt,
            ],
            "no tab may exist that no local read can fill: beliefs and activity come from the vault, prompt from the assembler"
        );
    }

    #[test]
    fn an_empty_list_is_words_not_an_empty_table() {
        let overlay = ContextOverlay::new();
        assert!(overlay.state_text().contains("nothing is believed"));
    }

    #[test]
    fn a_read_failure_shows_its_reason_and_its_fix() {
        let mut overlay = ContextOverlay::new();
        overlay.note = Some(describe(
            &ErrorOutcome::new(crate::output::ExitCode::Unexpected, "failed to open vault")
                .with_cause("no such file")
                .with_fix("run `memnoth vault init` to create the vault"),
        ));
        let text = overlay.state_text();
        assert!(text.contains("failed to open vault"), "{text}");
        assert!(text.contains("no such file"), "{text}");
        assert!(text.contains("vault init"), "{text}");
    }

    #[test]
    fn navigating_stops_at_the_last_belief_and_the_first() {
        let mut overlay = ContextOverlay::new();
        overlay.beliefs = vec![belief("a"), belief("b")];
        overlay.clamp();

        overlay.select(1);
        assert_eq!(overlay.selected, 1);
        overlay.select(9);
        assert_eq!(overlay.selected, 1, "there is no third row");
        overlay.select(0);
        assert_eq!(overlay.selected, 0);
    }

    #[test]
    fn a_selection_survives_a_reload_that_shrinks_the_list() {
        let mut overlay = ContextOverlay::new();
        overlay.beliefs = vec![belief("a"), belief("b"), belief("c")];
        overlay.selected = 2;
        overlay.beliefs.truncate(1);
        overlay.clamp();
        assert_eq!(overlay.selected, 0);
    }

    #[test]
    fn the_entities_read_is_the_shape_the_vault_returns() {
        let data = serde_json::json!({
            "entities": [
                { "name": "architecture", "kind": "decision", "summary": "event sourcing",
                  "confidence": 0.75, "valid_from_iso": "2026-01-15 14:32 UTC" }
            ],
            "facts": [
                { "entity": "architecture", "content": "replay from the log" },
                { "entity": "something-else", "content": "not this entity" }
            ]
        });
        let beliefs = beliefs_from(&data);
        assert_eq!(beliefs.len(), 1);
        assert_eq!(beliefs[0].name, "architecture");
        assert_eq!(beliefs[0].kind, "decision");
        assert_eq!(beliefs[0].confidence, 0.75);
        assert_eq!(beliefs[0].facts, vec!["replay from the log".to_string()]);
    }

    #[test]
    fn activity_marks_a_closed_version_as_closed() {
        let data = serde_json::json!({
            "facts": [
                { "name": "a", "kind": "fact", "valid_from": 0, "valid_to": null },
                { "name": "b", "kind": "fact", "valid_from": 0, "valid_to": 10 }
            ]
        });
        let rows = activity_from(&data);
        assert_eq!(rows[0].state, "active");
        assert_eq!(rows[1].state, "closed");
    }
}
