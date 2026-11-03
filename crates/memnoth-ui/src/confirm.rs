//! Confirm dialog widget: modal yes/no prompt extracted from draw_approval.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::util::{ellipsize, warning_style};

/// Confirmation dialog response
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmResponse {
    Yes,
    No,
    Cancel,
}

/// Confirm dialog state
#[derive(Debug, Default)]
pub struct ConfirmDialog {
    pub title: String,
    pub message: String,
    pub yes_label: String,
    pub no_label: String,
    pub selected: bool, // true = yes selected, false = no selected
    pub visible: bool,
}

impl ConfirmDialog {
    pub fn new() -> Self {
        Self {
            title: "confirm".to_string(),
            message: String::new(),
            yes_label: "y approve".to_string(),
            no_label: "n or esc reject".to_string(),
            selected: false,
            visible: false,
        }
    }

    pub fn with_message(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            ..Default::default()
        }
    }

    pub fn with_labels(mut self, yes: impl Into<String>, no: impl Into<String>) -> Self {
        self.yes_label = yes.into();
        self.no_label = no.into();
        self
    }

    pub fn show(&mut self) {
        self.visible = true;
        self.selected = false;
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn select_yes(&mut self) {
        self.selected = true;
    }

    pub fn select_no(&mut self) {
        self.selected = false;
    }

    pub fn toggle(&mut self) {
        self.selected = !self.selected;
    }

    pub fn response(&self) -> Option<ConfirmResponse> {
        if !self.visible {
            return None;
        }
        if self.selected {
            Some(ConfirmResponse::Yes)
        } else {
            Some(ConfirmResponse::No)
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        if area.width < 20 || area.height < 6 {
            return;
        }

        let width = ((area.width as u32 * 3 / 4) as u16).clamp(20, area.width);
        let height = ((area.height as u32 * 3 / 5) as u16).clamp(6, area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );

        frame.render_widget(Clear, popup);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(Line::from(Span::styled(
                format!(" {} ", self.title),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));

        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // Message
        let msg_lines: Vec<Line> = self
            .message
            .lines()
            .map(|line| {
                Line::from(Span::styled(
                    ellipsize(line, inner.width as usize),
                    Style::default().fg(Color::White),
                ))
            })
            .collect();

        let msg_height = msg_lines
            .len()
            .min((inner.height as usize).saturating_sub(3));
        let msg_area = Rect::new(inner.x, inner.y, inner.width, msg_height as u16);
        frame.render_widget(
            Paragraph::new(msg_lines).wrap(Wrap { trim: false }),
            msg_area,
        );

        // Yes/No options at bottom
        let option_y = inner.y + inner.height.saturating_sub(1);
        let yes_style = if self.selected {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let no_style = if !self.selected {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };

        let options = Line::from(vec![
            Span::styled(format!(" {} ", self.yes_label), yes_style),
            Span::styled("   ", warning_style()),
            Span::styled(format!(" {} ", self.no_label), no_style),
        ]);
        let options_width = options.width() as u16;
        let options_x = inner.x + (inner.width.saturating_sub(options_width)) / 2;
        frame.render_widget(
            Paragraph::new(options),
            Rect::new(options_x, option_y, options_width, 1),
        );
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<ConfirmResponse> {
        use crossterm::event::KeyCode;
        if !self.visible {
            return None;
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.selected = true;
                Some(ConfirmResponse::Yes)
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.selected = false;
                Some(ConfirmResponse::No)
            }
            KeyCode::Left | KeyCode::Right => {
                self.toggle();
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_confirm(dialog: &mut ConfirmDialog, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| dialog.render(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn confirm_renders_when_visible() {
        let mut dialog = ConfirmDialog::with_message("confirm", "edit src/a.rs?");
        dialog.show();
        let text = render_confirm(&mut dialog, 80, 30);
        assert!(text.contains("confirm"));
        assert!(text.contains("edit src/a.rs"));
    }

    #[test]
    fn confirm_not_rendered_when_hidden() {
        let mut dialog = ConfirmDialog::with_message("confirm", "test");
        dialog.hide();
        let text = render_confirm(&mut dialog, 80, 30);
        assert_eq!(text.trim(), "");
    }

    #[test]
    fn confirm_accept_and_reject() {
        let mut dialog = ConfirmDialog::with_message("confirm", "test");
        dialog.show();

        let resp = dialog.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('y'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(resp, Some(ConfirmResponse::Yes));

        let mut dialog2 = ConfirmDialog::with_message("confirm", "test");
        dialog2.show();
        let resp = dialog2.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('n'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(resp, Some(ConfirmResponse::No));
    }
}
