//! `/evidence` — the five-page `mn-evidence/1` overlay.
//!
//! Belief → Author → Approval → Commit → Impact, one page at a time. The
//! overlay renders the envelope the `evidence` verb composed; it never queries
//! a store itself and never invents a field. At 80x24 the page tabs, the state
//! reason, at most a screenful of rows and the key hints all stay visible.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use serde_json::Value;

use crate::output::ellipsize;
use crate::tui::app::App;
use crate::tui::commands::evidence::{PAGE_IDS, run};
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// One page of the chain, kept beside the key that jumps straight to it.
const PAGE_JUMPS: [(&str, usize); 3] = [("l", 1), ("c", 3), ("i", 4)];

#[derive(Debug, Default)]
pub struct EvidenceOverlay {
    visible: bool,
    subject: String,
    document: Option<Value>,
    page: usize,
    selected: usize,
    show_reason: bool,
    note: Option<String>,
}

impl EvidenceOverlay {
    /// An empty overlay, used by the factory and the reachability test.
    pub fn new() -> Self {
        Self::default()
    }

    /// Opened by the verb with the subject already parsed.
    pub fn with_subject(subject: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            ..Default::default()
        }
    }

    /// Fill the envelope from the same composer the headless verb uses.
    fn load(&mut self, app: &App) {
        let Some(ctx) = app.ctx.as_ref() else {
            self.note = Some("no workspace context — evidence reads local stores".into());
            return;
        };
        match run(ctx, &self.subject) {
            Ok(outcome) => {
                self.document = Some(outcome.data);
                self.page = 0;
                self.selected = 0;
                self.note = None;
            }
            Err(error) => self.note = Some(format!("{error:#}")),
        }
    }

    fn pages(&self) -> Vec<&Value> {
        self.document
            .as_ref()
            .and_then(|document| document["pages"].as_array())
            .map(|pages| pages.iter().collect())
            .unwrap_or_default()
    }

    fn current(&self) -> Option<&Value> {
        self.pages().get(self.page).copied()
    }

    fn rows(&self) -> Vec<&Value> {
        self.current()
            .and_then(|page| page["rows"].as_array())
            .map(|rows| rows.iter().collect())
            .unwrap_or_default()
    }

    fn set_page(&mut self, page: usize) {
        self.page = page.min(PAGE_IDS.len().saturating_sub(1));
        self.selected = 0;
    }

    fn next_page(&mut self) {
        self.set_page((self.page + 1) % PAGE_IDS.len());
    }

    fn previous_page(&mut self) {
        self.set_page((self.page + PAGE_IDS.len() - 1) % PAGE_IDS.len());
    }
}

impl Overlay for EvidenceOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Evidence
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
    }

    fn on_open(&mut self, app: &mut App) {
        if self.document.is_none() && !self.subject.is_empty() {
            self.load(app);
        }
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, _app: &mut App) -> OverlayAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return OverlayAction::Close,
            KeyCode::Tab | KeyCode::Char(']') => self.next_page(),
            KeyCode::Char('[') => self.previous_page(),
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                let count = self.rows().len();
                if count > 0 {
                    self.selected = (self.selected + 1).min(count - 1);
                }
            }
            KeyCode::Char('v') => self.show_reason = !self.show_reason,
            KeyCode::Char(ch) => {
                for (jump, page) in PAGE_JUMPS {
                    if ch == jump.chars().next().unwrap_or('\0') {
                        self.set_page(page);
                        break;
                    }
                }
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title(format!(
                " Evidence · {} ",
                if self.subject.is_empty() {
                    "—"
                } else {
                    &self.subject
                }
            ))
            .borders(Borders::ALL);
        frame.render_widget(block, area);

        let inner = area.inner(ratatui::layout::Margin {
            vertical: 1,
            horizontal: 1,
        });
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // page tabs
                Constraint::Length(2), // state + reason
                Constraint::Min(0),    // rows
                Constraint::Length(1), // keys
            ])
            .split(inner);

        // Page tabs: current page marked, all five names always present.
        let tabs = PAGE_IDS
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let label = format!(" {} ", id);
                let style = if index == self.page {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::REVERSED)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Span::styled(label, style)
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(Line::from(tabs)), chunks[0]);

        let page = self.current();
        let state = page
            .and_then(|page| page["state"].as_str())
            .unwrap_or("not_recorded");
        let reason = page
            .and_then(|page| page["state_reason"].as_str())
            .unwrap_or("the evidence envelope has not been composed yet");
        let state_color = match state {
            "measured" => Color::Green,
            "partial" => Color::Yellow,
            "stale" => Color::LightRed,
            _ => Color::DarkGray,
        };
        let state_line = Line::from(vec![
            Span::styled(
                format!(" {state} "),
                Style::default()
                    .fg(state_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                ellipsize(reason, chunks[1].width.saturating_sub(4) as usize),
                Style::default().fg(Color::Gray),
            ),
        ]);
        let mut state_lines = vec![state_line];
        if self.show_reason
            && let Some(note) = &self.note
        {
            state_lines.push(Line::from(Span::styled(
                ellipsize(note, chunks[1].width.saturating_sub(2) as usize),
                Style::default().fg(Color::Yellow),
            )));
        }
        frame.render_widget(
            Paragraph::new(state_lines).wrap(Wrap { trim: true }),
            chunks[1],
        );

        let rows = self.rows();
        let visible = chunks[2].height as usize;
        let start = self.selected.saturating_sub(visible.saturating_sub(1));
        let lines = rows
            .iter()
            .skip(start)
            .take(visible)
            .enumerate()
            .map(|(offset, row)| {
                let index = start + offset;
                let row_state = row["state"].as_str().unwrap_or("not_recorded");
                let selected = index == self.selected;
                let label = row["label"].as_str().unwrap_or("?");
                let value = match row["value"].as_u64() {
                    Some(value) => format!(" ({value})"),
                    None => String::new(),
                };
                let width = chunks[2].width.saturating_sub(16) as usize;
                let text = format!("[{row_state}] {}{}", ellipsize(label, width), value);
                Line::from(Span::styled(
                    text,
                    if selected {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    },
                ))
            })
            .collect::<Vec<_>>();
        let empty = if rows.is_empty() {
            vec![Line::from(Span::styled(
                "  nothing recorded for this page",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            Vec::new()
        };
        let body = if lines.is_empty() { empty } else { lines };
        frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), chunks[2]);

        let keys = "tab/] next · [ prev · ↑↓ row · v detail · l/c/i jump · esc close";
        frame.render_widget(
            Paragraph::new(ellipsize(keys, chunks[3].width as usize))
                .style(Style::default().fg(Color::DarkGray)),
            chunks[3],
        );
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("tab / ]", "next page"),
            ("[", "previous page"),
            ("↑ / ↓", "move row"),
            ("v", "toggle state reason"),
            ("l", "author page"),
            ("c", "commit page"),
            ("i", "impact page"),
            ("esc", "close"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_five_pages_and_all_keys_are_offered() {
        let overlay = EvidenceOverlay::with_subject("rate_limit");
        assert_eq!(overlay.kind(), OverlayKind::Evidence);
        let help: Vec<&str> = overlay.help_text().iter().map(|(key, _)| *key).collect();
        for key in ["tab / ]", "[", "↑ / ↓", "v", "l", "c", "i", "esc"] {
            assert!(help.contains(&key), "missing key {key}");
        }
        assert_eq!(PAGE_IDS.len(), 5);
    }

    #[test]
    fn page_navigation_wraps_and_resets_selection() {
        let mut overlay = EvidenceOverlay::with_subject("x");
        overlay.selected = 4;
        overlay.previous_page();
        assert_eq!(overlay.page, 4);
        assert_eq!(overlay.selected, 0);
        overlay.next_page();
        assert_eq!(overlay.page, 0);
        overlay.set_page(99);
        assert_eq!(overlay.page, 4);
    }

    #[test]
    fn esc_closes_and_jump_keys_choose_a_page() {
        let mut overlay = EvidenceOverlay::with_subject("x");
        overlay.show();
        assert!(overlay.is_visible());
        for (key, page) in [('l', 1), ('c', 3), ('i', 4)] {
            let action = overlay.handle_key(
                KeyEvent::from(KeyCode::Char(key)),
                &mut App::new("s", false),
            );
            assert_eq!(action, OverlayAction::None);
            assert_eq!(overlay.page, page, "key {key}");
        }
        let action = overlay.handle_key(KeyEvent::from(KeyCode::Esc), &mut App::new("s", false));
        assert_eq!(action, OverlayAction::Close);
        overlay.hide();
        assert!(!overlay.is_visible());
    }

    #[test]
    fn renders_eighty_by_twentyfour_with_no_document() {
        let mut overlay = EvidenceOverlay::with_subject("rate_limit");
        overlay.show();
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))
            .expect("test terminal");
        let app = App::new("s", false);
        terminal
            .draw(|frame| overlay.render(frame, frame.area(), &app))
            .expect("render");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.area, Rect::new(0, 0, 80, 24));
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
        }
        assert!(text.contains("Evidence"));
        assert!(text.contains("not_recorded"));
        assert!(
            text.contains("esc close"),
            "key hint was clipped off 80 columns"
        );
    }
}
