//! Command palette overlay: beautiful dropdown of all slash commands with fuzzy search.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use memnoth_ui::palette::{Palette, PaletteConfig, PaletteItem, PaletteRow};

#[derive(Debug, Default)]
pub struct PaletteOverlay {
    visible: bool,
    palette: Palette,
    selected: usize,
    list_state: ListState,
}

impl PaletteOverlay {
    pub fn new() -> Self {
        let config = build_palette_config();
        let mut palette = Palette::with_config(config);
        palette.set_filter(String::new());
        Self {
            visible: false,
            palette,
            selected: 0,
            list_state: ListState::default(),
        }
    }
}

/// Every slash command, in one list.
///
/// The modal palette and the inline dropdown both read this: a command that
/// exists in one surface but not the other is a command people stop finding.
///
/// Rows registered in the command registry are merged in, so a command that
/// gains a `palette` line in `memnoth-command` appears in the session without
/// editing this file. The local rows below cover the session-only verbs that
/// have no CLI counterpart yet.
pub(crate) fn commands() -> Vec<PaletteItem> {
    super::command_rows::palette_items()
}

/// Whether a command needs an argument before it can run.
///
/// The usage string carries the answer: `<...>` is required, `[...]` is
/// optional. That keeps one source of truth for the palette, the inline
/// dropdown and the tests, instead of a second hand-maintained list that drifts.
pub(crate) fn needs_arguments(item: &PaletteItem) -> bool {
    item.command.contains('<') || item.description.contains('<')
}

/// The line a pick puts in the prompt.
///
/// The usage names the argument (`/vault recall <query>`), but typing that
/// placeholder would leave `<query>` in the line for the person to delete. The
/// verb goes in; the argument stays theirs to type.
pub(crate) fn insertable(command: &str) -> String {
    let end = command
        .find('<')
        .or_else(|| command.find('['))
        .unwrap_or(command.len());
    command[..end].trim_end().to_string()
}

fn build_palette_config() -> PaletteConfig {
    let items = commands();

    let rows = vec![PaletteRow {
        label: "Version".into(),
        value: env!("CARGO_PKG_VERSION").into(),
        note: "memnoth".into(),
    }];

    PaletteConfig {
        title: "memnoth".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        subtitle: "Command Palette - type to filter, Up/Down navigate, Enter to run, Esc to close"
            .into(),
        rows,
        items,
        footer_hint: "Type to filter  |  Up/Down navigate  |  Enter run  |  Esc close".into(),
    }
}

impl Overlay for PaletteOverlay {
    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.palette.set_filter(String::new());
        self.selected = 0;
        self.list_state.select(Some(0));
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        match key.code {
            KeyCode::Esc => OverlayAction::Close,
            KeyCode::Up => {
                self.palette.select_previous();
                self.list_state.select(Some(self.palette.selected_index()));
                OverlayAction::None
            }
            KeyCode::Down => {
                self.palette.select_next();
                self.list_state.select(Some(self.palette.selected_index()));
                OverlayAction::None
            }
            KeyCode::PageUp => {
                for _ in 0..10 {
                    self.palette.select_previous();
                }
                self.list_state.select(Some(self.palette.selected_index()));
                OverlayAction::None
            }
            KeyCode::PageDown => {
                for _ in 0..10 {
                    self.palette.select_next();
                }
                self.list_state.select(Some(self.palette.selected_index()));
                OverlayAction::None
            }
            // Home and End mean the ends of the list, not one row: a key that
            // moves one row is indistinguishable from the arrow it duplicates.
            KeyCode::Home => {
                self.palette.select_first();
                self.list_state.select(Some(self.palette.selected_index()));
                OverlayAction::None
            }
            KeyCode::End => {
                self.palette.select_last();
                self.list_state.select(Some(self.palette.selected_index()));
                OverlayAction::None
            }
            KeyCode::Enter => {
                let chosen = {
                    let items = self.palette.filtered_items();
                    items
                        .get(self.palette.selected_index())
                        .map(|item| (item.command.clone(), needs_arguments(item)))
                };
                // Nothing matched: the words that were typed are the command.
                // They go into the prompt unchanged and unsent, because a
                // spelling the palette could not resolve is exactly the one a
                // person wants to finish or fix.
                match chosen {
                    Some((command, needs_more)) => {
                        // The palette fills the line and lets the session submit
                        // it; it never runs a command itself, so the transcript
                        // shows the same line as if it had been typed. Only a
                        // command with a required argument stops at the prompt;
                        // `/kg status` runs on Enter instead of looking stuck.
                        app.input = if command == "!cmd" {
                            "!".to_string()
                        } else if command == "@file" {
                            "@".to_string()
                        } else {
                            let line = insertable(&command);
                            if needs_more { format!("{line} ") } else { line }
                        };
                        app.cursor = app.input.chars().count();
                        app.pending_submit =
                            !needs_more && !matches!(app.input.as_str(), "!" | "@");
                    }
                    None => {
                        let typed = self.palette.filter().trim().to_string();
                        app.input = format!("/{typed}");
                        app.cursor = app.input.chars().count();
                        app.pending_submit = false;
                    }
                }
                OverlayAction::Close
            }
            // A control chord is a command, never text: ctrl-t inside the
            // palette used to append `t` to the filter, which looks like a
            // stuck key. Modified keys fall through to the session.
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL)
                    && !key.modifiers.contains(crossterm::event::KeyModifiers::ALT) =>
            {
                let mut filter = self.palette.filter().to_string();
                filter.push(ch);
                self.palette.set_filter(filter);
                self.list_state.select(Some(0));
                OverlayAction::None
            }
            KeyCode::Backspace => {
                let mut filter = self.palette.filter().to_string();
                if !filter.is_empty() {
                    filter.pop();
                    self.palette.set_filter(filter);
                    self.list_state.select(Some(0));
                }
                OverlayAction::None
            }
            KeyCode::Tab => {
                self.palette.select_next();
                self.list_state.select(Some(self.palette.selected_index()));
                OverlayAction::None
            }
            KeyCode::Delete => {
                let mut filter = self.palette.filter().to_string();
                if !filter.is_empty() {
                    filter.pop();
                    self.palette.set_filter(filter);
                    self.list_state.select(Some(0));
                }
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }

        let items = self.palette.filtered_items();

        // Centered popup: 70% width, 60% height
        let width = (area.width as f32 * 0.7) as u16;
        let height = (area.height as f32 * 0.6) as u16;
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        // Clear background
        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(format!(
                " {} v{} ",
                self.palette.config.title, self.palette.config.version
            ))
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        // Layout: title + rows + the filter being typed + items + footer.
        // The filter line is not optional: a list that changes while nothing
        // echoes the keystrokes reads as a list that changed on its own.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // title + subtitle
                Constraint::Length(2), // metadata rows
                Constraint::Length(1), // the query
                Constraint::Min(8),    // items list
                Constraint::Length(2), // footer
            ])
            .split(inner);

        // Title + subtitle
        let title_area = chunks[0];
        let title_block = Paragraph::new(vec![
            Line::from(Span::styled(
                self.palette.config.title.as_str(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                self.palette.config.subtitle.as_str(),
                Style::default().fg(Color::DarkGray),
            )),
        ]);
        frame.render_widget(title_block, title_area);

        // Metadata rows
        let rows_area = chunks[1];
        let rows_text = self
            .palette
            .config
            .rows
            .iter()
            .map(|r| {
                Line::from(vec![
                    Span::styled(&r.label, Style::default().fg(Color::Yellow)),
                    Span::raw(": "),
                    Span::raw(&r.value),
                    Span::raw("  "),
                    Span::styled(&r.note, Style::default().fg(Color::DarkGray)),
                ])
            })
            .collect::<Vec<_>>();
        let rows_para = Paragraph::new(rows_text);
        frame.render_widget(rows_para, rows_area);

        // The query, in the line it is typed on, with how many rows it matched.
        let filter = self.palette.filter();
        let count = if filter.is_empty() {
            format!("{} commands", items.len())
        } else {
            format!("{} of {}", items.len(), self.palette.total())
        };
        let filter_line = Line::from(vec![
            Span::styled(
                " > ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if filter.is_empty() {
                    "type to filter".to_string()
                } else {
                    filter.to_string()
                },
                if filter.is_empty() {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
            Span::styled(format!("   {count}"), Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(filter_line), chunks[2]);

        // Items list
        let list_area = chunks[3];
        // An empty filter is an answer, not a blank panel: the palette stays on
        // screen and says so, so a keystroke is never swallowed by an invisible
        // modal.
        if items.is_empty() {
            let empty = Paragraph::new(vec![
                Line::from(Span::styled(
                    " no matching command",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(Span::styled(
                    format!(
                        " \"{}\" · enter runs it as a slash command",
                        self.palette.filter()
                    ),
                    Style::default().fg(Color::DarkGray),
                )),
            ]);
            frame.render_widget(empty, list_area);
        }
        let list_items: Vec<ListItem> = items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let is_selected = i == self.palette.selected_index();
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {:<25} ", item.command),
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(item.description.as_str(), Style::default().fg(Color::Gray)),
                ]))
                .style(style)
            })
            .collect();

        let list = List::new(list_items)
            .highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, list_area, &mut self.list_state);

        // Footer hint
        let footer_area = chunks[4];
        let footer = Paragraph::new(Line::from(Span::styled(
            self.palette.config.footer_hint.as_str(),
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(footer, footer_area);
    }

    fn kind(&self) -> OverlayKind {
        OverlayKind::Palette
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn typing_into_the_palette_filters_instead_of_reaching_the_prompt() {
        let mut app = App::new("s", false);
        let mut palette = PaletteOverlay::new();
        palette.show();

        for ch in "vault".chars() {
            let action = palette.handle_key(key(KeyCode::Char(ch)), &mut app);
            assert!(matches!(action, OverlayAction::None));
        }

        assert_eq!(palette.palette.filter(), "vault");
        assert!(app.input.is_empty(), "the prompt must stay untouched");
        let items = palette.palette.filtered_items();
        assert!(!palette.palette.config.items.is_empty());
        assert!(!items.is_empty(), "the filter must narrow the list");
        assert!(
            items.len() < palette.palette.config.items.len(),
            "the list is narrowed, not just reordered"
        );
    }

    #[test]
    fn a_subcommand_with_arguments_is_inserted_and_left_editable() {
        let mut app = App::new("s", false);
        let mut palette = PaletteOverlay::new();
        palette.show();
        for ch in "vault recall".chars() {
            palette.handle_key(key(KeyCode::Char(ch)), &mut app);
        }

        let action = palette.handle_key(key(KeyCode::Enter), &mut app);
        assert!(matches!(action, OverlayAction::Close));
        assert_eq!(app.input, "/vault recall ");
        assert!(!app.pending_submit, "an argument is still missing");
    }

    #[test]
    fn the_selected_command_without_arguments_runs_on_enter() {
        let mut app = App::new("s", false);
        let mut palette = PaletteOverlay::new();
        palette.show();
        for ch in "help".chars() {
            palette.handle_key(key(KeyCode::Char(ch)), &mut app);
        }

        palette.handle_key(key(KeyCode::Enter), &mut app);
        assert_eq!(app.input, "/help");
        assert!(app.pending_submit);
    }

    #[test]
    fn an_unmatched_query_is_kept_as_the_command_to_run() {
        let mut app = App::new("s", false);
        let mut palette = PaletteOverlay::new();
        palette.show();
        for ch in "zzz".chars() {
            palette.handle_key(key(KeyCode::Char(ch)), &mut app);
        }

        palette.handle_key(key(KeyCode::Enter), &mut app);
        assert_eq!(app.input, "/zzz");
        assert!(!app.pending_submit, "the text stays editable");
        assert!(palette.palette.filtered_items().is_empty());
    }

    /// The projection's dedupe key: first two words.
    fn command_key(command: &str) -> String {
        command
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }

    #[test]
    fn the_palette_offers_each_verb_once_and_every_row_runs() {
        let items = commands();
        let keys: Vec<String> = items
            .iter()
            .map(|item| command_key(&item.command))
            .collect();
        let unique: std::collections::HashSet<&String> = keys.iter().collect();
        assert_eq!(keys.len(), unique.len(), "the palette offers a verb twice");
        assert!(!items.is_empty(), "the palette is empty");

        for item in &items {
            assert!(
                !matches!(
                    crate::tui::app::parse_line(&item.command),
                    crate::tui::app::Parsed::Unknown(_)
                ),
                "{} is offered but cannot be parsed",
                item.command
            );
        }
    }

    #[test]
    fn a_required_argument_is_read_from_the_command_line() {
        let item = PaletteItem {
            command: "/vault asof [--at WHEN]".into(),
            description: "What was believed at an instant".into(),
        };
        assert!(!needs_arguments(&item), "brackets are optional");
        let required = PaletteItem {
            command: "/vault explain <name>".into(),
            description: "Everything known about an entity".into(),
        };
        assert!(needs_arguments(&required));
    }
}
