//! `/permissions` — what the chat agent may and may not do.
//!
//! This screen used to be fiction: eight invented tools (`vault_read`,
//! `shell_exec`, `network_request` — none of which exist), invented "last used
//! 2m ago" columns, and toggles that moved a widget and nothing else. It sat
//! exactly where a person goes to audit the agent's power, which made it the
//! most expensive lie in the product.
//!
//! It is now three facts and one write:
//!
//! * The rows are `provider::tool_descriptions()` — the tools the model is
//!   actually offered, with the sentences it actually reads.
//! * "asks first" is `provider::requires_approval`: a write is confirmed every
//!   time, and no setting here can make it silent.
//! * Denying a tool writes `[tools] denied = […]` to `config.toml`, and
//!   `Toolbox::call` refuses that tool on its next call — it re-reads the file,
//!   so this screen works mid-session instead of next launch.
//!
//! Nothing is claimed that the code does not do, and the footer says which
//! decision belongs to whom.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::context::AppContext;
use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// One tool as this screen tells it.
#[derive(Debug, Clone)]
struct ToolRow {
    name: &'static str,
    description: &'static str,
    /// A write asks the person before it runs; no setting lifts that.
    asks_first: bool,
    /// The person denied it outright.
    denied: bool,
}

impl ToolRow {
    fn state(&self) -> &'static str {
        if self.denied {
            "denied"
        } else if self.asks_first {
            "asks first"
        } else {
            "allowed"
        }
    }
}

/// The screen.
#[derive(Debug)]
pub struct PermissionsOverlay {
    visible: bool,
    rows: Vec<ToolRow>,
    selected: usize,
    scroll: usize,
    /// Where the denial is written. Copied from the app on open.
    path: Option<PathBuf>,
    ctx: Option<AppContext>,
    /// What the last save did, in the same voice as the rest of the session.
    note: Option<String>,
}

impl PermissionsOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            path: None,
            ctx: None,
            note: None,
        }
    }

    /// Read the tool set and the denials that are in force right now.
    fn reload(&mut self) {
        let denied = self
            .ctx
            .as_ref()
            .map(AppContext::denied_tools)
            .unwrap_or_default();
        self.rows = crate::provider::tool_descriptions()
            .into_iter()
            .map(|(name, description)| ToolRow {
                name,
                description,
                asks_first: crate::provider::requires_approval(name),
                denied: denied.iter().any(|denied| denied == name),
            })
            .collect();
        self.clamp();
    }

    fn clamp(&mut self) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(self.rows.len() - 1);
    }

    fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    /// Deny or allow the selected tool, in the config file the gate reads.
    ///
    /// Both directions write; a toggle that only changed the row would be the
    /// bug this screen was rewritten to remove.
    fn toggle(&mut self, app: &mut App) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let name = row.name;
        let deny = !row.denied;

        let tools = app.config.tools.get_or_insert_with(Default::default);
        if deny {
            if !tools.denied.iter().any(|denied| denied == name) {
                tools.denied.push(name.to_string());
            }
        } else {
            tools.denied.retain(|denied| denied != name);
        }
        if tools.denied.is_empty() {
            // An empty list is the default; writing it out leaves a section
            // that says nothing and a diff on a file the user did not edit.
            app.config.tools = None;
        }

        let path = self.path.clone().unwrap_or_else(|| app.config_path.clone());
        self.note = Some(match app.config.save(&path) {
            Ok(()) => {
                // The overlay's own copy has to agree with the file it just
                // wrote, or the next `r` contradicts what is on screen. The row
                // is set here as well as reloaded, because `ctx` is how `reload`
                // reads the denials and a screen opened without one would show a
                // successful save and an unchanged row.
                if let Some(row) = self.rows.iter_mut().find(|row| row.name == name) {
                    row.denied = deny;
                }
                if let Some(ctx) = self.ctx.as_mut() {
                    ctx.config = app.config.clone();
                }
                if deny {
                    format!("{name} denied — saved to {}", path.display())
                } else {
                    format!("{name} allowed again — saved to {}", path.display())
                }
            }
            Err(error) => format!("could not save {}: {error:#}", path.display()),
        });
        // Only a context that holds the file can re-derive the list; with none,
        // `reload` would read "no denials" and undo the row just set.
        if self.ctx.is_some() {
            self.reload();
        }
    }

    /// The line under the list: what the selected decision means.
    fn detail_text(&self) -> String {
        let Some(row) = self.rows.get(self.selected) else {
            return String::new();
        };
        let meaning = if row.denied {
            "the agent is refused before it runs; the model is told why, and cannot lift it"
        } else if row.asks_first {
            "every call is shown to you first; a refusal is final and no setting makes it silent"
        } else {
            "the agent may call it without asking; it reads nothing it was not given"
        };
        format!("{} — {meaning}", row.name)
    }
}

impl Default for PermissionsOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl Overlay for PermissionsOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Permissions
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.selected = 0;
        self.scroll = 0;
    }

    fn on_open(&mut self, app: &mut App) {
        self.ctx = app.ctx.clone();
        self.path = Some(app.config_path.clone());
        self.note = None;
        self.reload();
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => OverlayAction::Close,
            KeyCode::Up | KeyCode::Char('k') if !ctrl => {
                self.move_by(-1);
                OverlayAction::None
            }
            KeyCode::Down | KeyCode::Char('j') if !ctrl => {
                self.move_by(1);
                OverlayAction::None
            }
            KeyCode::PageUp => {
                self.move_by(-8);
                OverlayAction::None
            }
            KeyCode::PageDown => {
                self.move_by(8);
                OverlayAction::None
            }
            KeyCode::Home => {
                self.selected = 0;
                OverlayAction::None
            }
            KeyCode::End => {
                self.selected = self.rows.len().saturating_sub(1);
                OverlayAction::None
            }
            // Space and enter do the same thing, and both are named in the
            // footer: a screen about permission should not hide its own action.
            KeyCode::Char(' ') | KeyCode::Enter => {
                self.toggle(app);
                OverlayAction::None
            }
            KeyCode::Char('d') if !ctrl => {
                if let Some(row) = self.rows.get(self.selected)
                    && !row.denied
                {
                    self.selected = self
                        .rows
                        .iter()
                        .position(|candidate| candidate.name == row.name)
                        .unwrap_or(self.selected);
                    self.toggle(app);
                }
                OverlayAction::None
            }
            KeyCode::Char('a') if !ctrl => {
                if let Some(row) = self.rows.get(self.selected)
                    && row.denied
                {
                    self.toggle(app);
                }
                OverlayAction::None
            }
            KeyCode::Char('r') if !ctrl => {
                // Re-read the file: another window (or a script) may have
                // changed it, and this screen must not show a stale decision.
                if let Some(ctx) = self.ctx.as_mut() {
                    ctx.config = crate::context::Config::load(&ctx.config_path)
                        .unwrap_or_else(|_| ctx.config.clone());
                }
                self.reload();
                self.note = Some("re-read the config".to_string());
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible || area.width < 40 || area.height < 10 {
            return;
        }
        // A popover, like the palette: the transcript behind it is context, and
        // a decision is made in a window over it rather than in a page of its
        // own.
        let width = (area.width as u32 * 82 / 100).clamp(46, area.width as u32) as u16;
        let height = (area.height as u32 * 78 / 100).clamp(12, area.height as u32) as u16;
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let popup = Rect::new(x, y, width, height);
        frame.render_widget(Clear, popup);

        let block = Block::default()
            .title(" permissions — what the agent may call ")
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        if inner.height < 5 || inner.width < 12 {
            return;
        }

        let header_area = Rect::new(inner.x, inner.y, inner.width, 1);
        let detail_area = Rect::new(inner.x, inner.y + inner.height - 2, inner.width, 2);
        let body_area = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(3),
        );

        let denied = self.rows.iter().filter(|row| row.denied).count();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {} tools", self.rows.len()),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    if denied == 0 {
                        " · nothing denied".to_string()
                    } else {
                        format!(" · {denied} denied")
                    },
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    "  · writes always ask; this screen adds denials",
                    Style::default().fg(Color::DarkGray),
                ),
            ])),
            header_area,
        );

        if self.rows.is_empty() {
            frame.render_widget(
                Paragraph::new("no tools are registered — this build offers the agent nothing")
                    .wrap(Wrap { trim: true })
                    .alignment(Alignment::Left),
                body_area,
            );
        } else {
            // Two drawn lines per tool (name + state, then what it does), so the
            // scroll is kept in drawn lines and cannot drift against the rows.
            let pages = (body_area.height as usize).max(2);
            let first = self.selected * 2;
            if first < self.scroll {
                self.scroll = first;
            } else if first + 1 >= self.scroll + pages {
                self.scroll = (first + 2).saturating_sub(pages);
            }

            let mut lines: Vec<Line<'static>> = Vec::new();
            for (index, row) in self.rows.iter().enumerate() {
                let selected = index == self.selected;
                let (glyph, state_style) = if row.denied {
                    ("✗ ", Style::default().fg(Color::Red))
                } else if row.asks_first {
                    ("? ", Style::default().fg(Color::Yellow))
                } else {
                    ("✓ ", Style::default().fg(Color::Green))
                };
                let name_style = if selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        if selected { "› " } else { "  " },
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(glyph, state_style),
                    Span::styled(row.name, name_style),
                    Span::styled(format!("  {}", row.state()), state_style),
                ]));
                lines.push(Line::from(Span::styled(
                    format!("      {}", row.description),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            frame.render_widget(
                Paragraph::new(lines).scroll((self.scroll as u16, 0)),
                body_area,
            );
        }

        let detail = self.note.clone().unwrap_or_else(|| self.detail_text());
        let detail_style = if self.note.is_some() {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Gray)
        };
        frame.render_widget(
            Paragraph::new(detail)
                .wrap(Wrap { trim: true })
                .style(detail_style),
            detail_area,
        );
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑/↓", "choose a tool"),
            ("space", "deny or allow it"),
            ("d / a", "deny / allow"),
            ("r", "re-read config.toml"),
            ("esc", "close"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay() -> PermissionsOverlay {
        let mut overlay = PermissionsOverlay::new();
        overlay.show();
        // `reload` reads `provider::tool_descriptions` only; no context is
        // needed for the list itself, only for the denials.
        overlay.reload();
        overlay
    }

    #[test]
    fn the_rows_are_the_tools_the_agent_actually_has() {
        let overlay = overlay();
        let names: Vec<&str> = overlay.rows.iter().map(|row| row.name).collect();

        // The real set, from `provider::tool_names()`.
        for real in crate::provider::tool_names() {
            assert!(names.contains(&real), "{real} is missing from the screen");
        }
        // The fiction this screen used to show, by name.
        for invented in ["vault_read", "shell_exec", "network_request", "file_write"] {
            assert!(
                !names.contains(&invented),
                "{invented} does not exist and must not be listed"
            );
        }
    }

    #[test]
    fn the_writes_are_marked_asks_first_and_reads_are_not() {
        let overlay = overlay();
        for row in &overlay.rows {
            assert_eq!(
                row.asks_first,
                crate::provider::requires_approval(row.name),
                "{} disagrees with the gate",
                row.name
            );
        }
        let edit = overlay
            .rows
            .iter()
            .find(|row| row.name == "edit_file")
            .expect("edit_file is a tool");
        assert!(edit.asks_first);
        assert_eq!(edit.state(), "asks first");
    }

    #[test]
    fn a_denial_is_written_to_the_config_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mut app = App::new("s", false);
        app.config_path = config_path.clone();

        let mut overlay = overlay();
        overlay.path = Some(config_path.clone());
        let index = overlay
            .rows
            .iter()
            .position(|row| row.name == "edit_file")
            .expect("edit_file");
        overlay.selected = index;

        overlay.toggle(&mut app);
        assert!(overlay.rows[index].denied, "the row has to follow the file");
        let saved = crate::context::Config::load(&config_path).expect("config parses");
        assert_eq!(
            saved.tools.as_ref().map(|tools| tools.denied.clone()),
            Some(vec!["edit_file".to_string()])
        );
        assert!(
            overlay.note.as_deref().unwrap_or("").contains("denied"),
            "the screen says what it did: {:?}",
            overlay.note
        );

        // And back off again: the empty list is removed, not written as `[]`.
        overlay.toggle(&mut app);
        assert!(!overlay.rows[index].denied);
        let saved = crate::context::Config::load(&config_path).expect("config parses");
        assert!(saved.tools.is_none(), "an empty denial list is the default");
    }

    #[test]
    fn the_detail_line_says_which_decision_belongs_to_whom() {
        let overlay = overlay();
        let text = overlay.detail_text();
        assert!(!text.is_empty());
        assert!(
            text.contains("asks") || text.contains("refused") || text.contains("may call"),
            "{text}"
        );
    }
}
