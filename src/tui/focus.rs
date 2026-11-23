//! Focus overlay: one child owns the composer.
//!
//! Child transcript, `composer → <agent>` target chip, per-child draft and
//! scroll, `Esc` back to the exact parent state, `Tab`/`Shift+Tab` sibling
//! cycling, `Ctrl+↑` parent.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::tui::app::App;
use crate::tui::agents::{AgentStatus, ChildInfo};
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use crate::tui::theme::tokens;

/// Focus: one child agent with its transcript and composer target chip.
#[derive(Debug)]
pub struct AgentFocus {
    visible: bool,
    child: Option<ChildInfo>,
    transcript: Vec<String>,
    scroll: usize,
}

impl AgentFocus {
    pub fn new() -> Self {
        Self {
            visible: false,
            child: None,
            transcript: Vec::new(),
            scroll: 0,
        }
    }

    pub fn with_child(mut self, child: ChildInfo) -> Self {
        self.child = Some(child);
        self
    }
}

impl Default for AgentFocus {
    fn default() -> Self {
        Self::new()
    }
}

/// Accent ink, bold so the cue survives NO_COLOR. This is both the focused
/// child's status line and the selected transcript row; neither paints a band
/// behind the text.
fn emphasis_style() -> Style {
    let t = tokens();
    Style::default()
        .fg(t.accent)
        .add_modifier(Modifier::BOLD)
}

impl Overlay for AgentFocus {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Focus
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
        self.child = None;
        self.transcript.clear();
        self.scroll = 0;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        match key.code {
            KeyCode::Esc => {
                // Restore exact parent state: cursor, scroll, filter, draft.
                OverlayAction::PopOverlay
            }
            KeyCode::Tab if key.modifiers == KeyModifiers::NONE => {
                OverlayAction::CycleSibling(1)
            }
            KeyCode::Tab if key.modifiers == KeyModifiers::SHIFT => {
                OverlayAction::CycleSibling(-1)
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                OverlayAction::FocusParent
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        let child_label = self
            .child
            .as_ref()
            .map(|c| c.task.clone())
            .unwrap_or_else(|| "(no child)".into());

        let status_line = if let Some(c) = &self.child {
            format!(
                "composer → {} · {} · {}",
                c.agent_id,
                c.status.glyph(),
                c.task
            )
        } else {
            "composer → main".to_string()
        };

        let block = Block::default()
            .title(format!(" Focus › {}", child_label))
            .borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let status = Paragraph::new(status_line).style(emphasis_style());
        frame.render_widget(status, Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        });

        let transcript_area = Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: inner.height.saturating_sub(2),
        };
        let items: Vec<ListItem> = self
            .transcript
            .iter()
            .enumerate()
            .map(|(i, line)| {
                ListItem::new(Line::from(Span::raw(line))).style(
                    if i == self.scroll {
                        emphasis_style()
                    } else {
                        Style::default()
                    },
                )
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(self.scroll.min(items.len().saturating_sub(1))));
        frame.render_stateful_widget(
            List::new(items),
            transcript_area,
            &mut state,
        );

        let composer = Paragraph::new("composer → ").block(
            Block::default().borders(Borders::TOP),
        );
        frame.render_widget(composer, Rect {
            x: inner.x,
            y: inner.y + inner.height - 1,
            width: inner.width,
            height: 1,
        });
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("Esc", "return to parent"),
            ("Tab", "next sibling"),
            ("Shift+Tab", "previous sibling"),
            ("Ctrl+↑", "focus parent"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    #[test]
    fn no_raw_colours_in_production_render_code() {
        let source = include_str!("focus.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            !production.contains("Color::"),
            "production render code must use semantic tokens, not raw Color::"
        );
    }

    #[test]
    fn status_and_selection_use_accent_ink_with_no_fill() {
        let t = tokens();
        let style = emphasis_style();
        assert_eq!(style.fg, Some(t.accent));
        assert_eq!(style.bg, None, "no selected-bg band");
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn every_colour_in_the_focus_overlay_comes_from_the_tokens() {
        let t = tokens();
        let allowed = [
            t.canvas,
            t.surface,
            t.text,
            t.muted,
            t.border,
            t.accent,
            t.success,
            t.warning,
            t.error,
            t.selected_bg,
            t.selected_fg,
            Color::Reset,
        ];
        let mut focus = AgentFocus::new();
        focus.show();
        focus.transcript = vec!["first".to_string(), "second".to_string()];
        focus.scroll = 0;
        let app = App::new("s", false);
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| focus.render(frame, Rect::new(0, 0, 40, 8), &app))
            .expect("draw must not fail");
        for cell in terminal.backend().buffer().content() {
            assert!(
                allowed.contains(&cell.fg) && allowed.contains(&cell.bg),
                "a colour outside the token set reached the overlay: {cell:?}"
            );
        }
    }
}
