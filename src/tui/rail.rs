//! Persistent rail: `now`, `agents`, `team`, `work` — shown at 100+ columns.
//!
//! Below 100 columns there is no rail; the header carries `N agents · N% ·
//! local`. At 100: `now` and `agents`. At 120: `team`. Stable section order,
//! no focus-swapped rail, no empty section. `team · local` appears only with
//! real local data. D28: no team screens before M8 — hidden, no placeholder.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// One row of rail state.
#[derive(Debug, Clone)]
pub struct RailRow {
    pub section: RailSection,
    pub label: String,
    pub value: String,
    pub state: RailState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailSection {
    Now,
    Agents,
    Team,
    Work,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailState {
    Ok,
    Waiting,
    Warning,
}

impl RailState {
    fn glyph(self) -> &'static str {
        match self {
            RailState::Ok => "✓",
            RailState::Waiting => "…",
            RailState::Warning => "!",
        }
    }
    fn color(self) -> Color {
        match self {
            RailState::Ok => Color::Green,
            RailState::Waiting => Color::Yellow,
            RailState::Warning => Color::Red,
        }
    }
}

/// The persistent rail: drawn on the right edge at 100+ columns.
#[derive(Debug, Default)]
pub struct Rail {
    rows: Vec<RailRow>,
    /// Whether team data is real (not a placeholder). D28: hidden until M8.
    pub team_visible: bool,
}

impl Rail {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            team_visible: false,
        }
    }

    /// Set the rail rows from live session state. Empty sections are dropped.
    pub fn set_rows(&mut self, rows: Vec<RailRow>) {
        self.rows = rows;
    }

    /// Whether the rail should be drawn: width >= 100 and at least one row
    /// that would actually be drawn. Team rows hidden before M8 do not count:
    /// a rail with only hidden rows is an empty rail.
    pub fn visible(&self, width: u16) -> bool {
        width >= 100
            && self
                .rows
                .iter()
                .any(|row| row.section != RailSection::Team || self.team_visible)
    }

    /// Render the rail on the right edge of `area`.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if !self.visible(area.width) {
            return;
        }

        let rail_width = 26u16.min(area.width.saturating_sub(4));
        if rail_width == 0 {
            return;
        }

        let rail_area = Rect {
            x: area.x + area.width - rail_width,
            y: area.y,
            width: rail_width,
            height: area.height,
        };

        let block = Block::default()
            .title(" rail ")
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(Color::DarkGray));
        frame.render_widget(block, rail_area);
        let mut lines: Vec<Line> = Vec::new();

        for row in &self.rows {
            if row.section == RailSection::Team && !self.team_visible {
                continue;
            }
            let state_span = Span::styled(
                format!(" {} ", row.state.glyph()),
                Style::default().fg(row.state.color()),
            );
            let label_span =
                Span::styled(format!("{}: ", row.label), Style::default().fg(Color::Gray));
            let value_span = Span::raw(row.value.clone());
            lines.push(Line::from(vec![state_span, label_span, value_span]));
        }

        if lines.is_empty() {
            return;
        }

        frame.render_widget(Paragraph::new(lines), rail_area);
    }

    /// Header chips below 100 columns: `N agents · N% · local`.
    pub fn header_chips(width: u16, agents: usize, context_percent: u8) -> Option<String> {
        if width >= 100 {
            return None;
        }
        Some(format!("{} agents · {}% · local", agents, context_percent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_hidden_below_100_columns() {
        let rail = Rail::new();
        assert!(!rail.visible(99));
    }

    #[test]
    fn rail_visible_at_100_columns() {
        let mut rail = Rail::new();
        rail.set_rows(vec![RailRow {
            section: RailSection::Now,
            label: "turn".into(),
            value: "3".into(),
            state: RailState::Ok,
        }]);
        assert!(rail.visible(100));
    }

    #[test]
    fn header_chips_below_100() {
        let chips = Rail::header_chips(80, 2, 45);
        assert_eq!(chips.as_deref(), Some("2 agents · 45% · local"));
    }

    #[test]
    fn header_chips_none_at_100() {
        let chips = Rail::header_chips(100, 2, 45);
        assert_eq!(chips, None);
    }

    #[test]
    fn team_hidden_before_m8() {
        let mut rail = Rail::new();
        rail.team_visible = false;
        rail.set_rows(vec![RailRow {
            section: RailSection::Team,
            label: "team".into(),
            value: "local".into(),
            state: RailState::Ok,
        }]);
        assert!(!rail.visible(120));
    }
}
