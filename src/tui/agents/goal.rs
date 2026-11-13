//! `/goal` overlay: persistent completion condition for the current session.
//!
//! Shows the goal condition that the evaluator checks after each turn, or a
//! "no condition set" state when none is active. Nothing is written to the
//! vault or the graph; the condition lives in session state only. `Esc`
//! returns to the session behind it.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// `/goal`: persistent completion condition, shown as an overlay.
#[derive(Debug)]
pub struct GoalOverlay {
    visible: bool,
    condition: Option<String>,
}

impl GoalOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            condition: None,
        }
    }

    /// Seed the overlay with a condition, e.g. from a verb's outcome.
    pub fn with_condition(mut self, condition: Option<String>) -> Self {
        self.condition = condition;
        self
    }

    fn goal_lines(&self) -> Vec<ListItem<'static>> {
        let mut items: Vec<ListItem> = vec![ListItem::new(Line::from(vec![
            Span::styled("goal", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" — "),
            match &self.condition {
                None => Span::raw("no condition set"),
                Some(c) if c == "clear" => Span::raw("goal cleared"),
                Some(c) => {
                    Span::styled(format!("condition: {c}"), Style::default().fg(Color::Cyan))
                }
            },
        ]))];
        items.push(ListItem::new(Line::from(Span::raw(
            "  evaluator: runs after each turn",
        ))));
        items.push(ListItem::new(Line::from(Span::raw(
            "  scope: session state only",
        ))));
        items
    }
}

impl Default for GoalOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl Overlay for GoalOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Goal
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
            .title(" Goal ")
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

        let items = self.goal_lines();
        let list = List::new(items);
        frame.render_stateful_widget(list, inner, &mut std::default::Default::default());

        let footer = Paragraph::new("Esc: close  ·  condition checked each turn")
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
        vec![("/goal", "Open goal overlay"), ("Esc/q", "Close overlay")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_overlay_default_is_hidden() {
        let overlay = GoalOverlay::new();
        assert!(!overlay.is_visible());
    }

    #[test]
    fn goal_overlay_show_sets_visible() {
        let mut overlay = GoalOverlay::new();
        overlay.show();
        assert!(overlay.is_visible());
    }

    #[test]
    fn goal_overlay_kind_is_goal() {
        let overlay = GoalOverlay::new();
        assert_eq!(overlay.kind(), OverlayKind::Goal);
    }
}
