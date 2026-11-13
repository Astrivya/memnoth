//! `/plan` overlay: read-only plan mode for the current session.
//!
//! Shows the plan artifact against the current session — task, mode, and the
//! artifact reference. Read-only by construction: nothing here edits the
//! session, the vault, or the graph. `Esc` returns to the session behind it.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// `/plan`: read-only plan mode, showing the plan artifact for the session.
#[derive(Debug)]
pub struct PlanOverlay {
    visible: bool,
    task: String,
    lines: Vec<String>,
}

impl PlanOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            task: String::new(),
            lines: Vec::new(),
        }
    }

    /// Seed the overlay with a task and its plan lines, e.g. from a verb's
    /// outcome text.
    pub fn with_task(mut self, task: String, lines: Vec<String>) -> Self {
        self.task = task;
        self.lines = lines;
        self
    }

    fn plan_lines(&self) -> Vec<ListItem<'_>> {
        let mut items: Vec<ListItem> = vec![ListItem::new(Line::from(vec![
            Span::styled("plan", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" — "),
            if self.task.is_empty() {
                Span::raw("no task specified; plan mode is active")
            } else {
                Span::styled(
                    format!("task: {}", self.task),
                    Style::default().fg(Color::Cyan),
                )
            },
        ]))];
        items.push(ListItem::new(Line::from(Span::raw("  mode: read-only"))));
        items.push(ListItem::new(Line::from(Span::raw(
            "  artifact: session plan",
        ))));
        for line in &self.lines {
            items.push(ListItem::new(Line::from(Span::raw(line))));
        }
        items
    }
}

impl Default for PlanOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl Overlay for PlanOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Plan
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
    }

    fn on_open(&mut self, _app: &mut App) {}

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, _app: &mut App) -> OverlayAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => OverlayAction::Close,
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }

        frame.render_widget(Clear, area);

        let block = Block::default()
            .title(" Plan ")
            .borders(Borders::ALL)
            .title_alignment(Alignment::Center);
        frame.render_widget(block.clone(), area);

        let inner = area.inner(ratatui::layout::Margin {
            vertical: 1,
            horizontal: 1,
        });
        if inner.height < 2 {
            return;
        }

        let items = self.plan_lines();
        let list = List::new(items);
        frame.render_stateful_widget(list, inner, &mut std::default::Default::default());

        let footer = Paragraph::new("Esc: close  ·  read-only: nothing is written")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Right)
            .block(Block::default().borders(Borders::NONE));
        let footer_area = ratatui::layout::Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(1),
            width: area.width,
            height: 1,
        };
        frame.render_widget(footer, footer_area);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![("/plan", "Open plan overlay"), ("Esc/q", "Close overlay")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_overlay_default_is_hidden() {
        let overlay = PlanOverlay::new();
        assert!(!overlay.is_visible());
    }

    #[test]
    fn plan_overlay_show_sets_visible() {
        let mut overlay = PlanOverlay::new();
        overlay.show();
        assert!(overlay.is_visible());
    }

    #[test]
    fn plan_overlay_kind_is_plan() {
        let overlay = PlanOverlay::new();
        assert_eq!(overlay.kind(), OverlayKind::Plan);
    }
}
