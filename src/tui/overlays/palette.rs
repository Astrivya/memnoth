//! Command palette overlay: beautiful dropdown of all slash commands with fuzzy search.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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

fn build_palette_config() -> PaletteConfig {
    let items = vec![
        // Navigation
        PaletteItem {
            command: "/search".into(),
            description: "Search vault and code graph".into(),
        },
        PaletteItem {
            command: "/context".into(),
            description: "View token budget and context breakdown".into(),
        },
        PaletteItem {
            command: "/diff".into(),
            description: "Review pending/accepted edits".into(),
        },
        PaletteItem {
            command: "/rewind".into(),
            description: "Roll back conversation + file writes".into(),
        },
        PaletteItem {
            command: "/model".into(),
            description: "Switch model, view pricing, set default".into(),
        },
        PaletteItem {
            command: "/connect".into(),
            description: "Configure model provider connections".into(),
        },
        PaletteItem {
            command: "/permissions".into(),
            description: "Per-tool allow/ask/deny, profiles".into(),
        },
        PaletteItem {
            command: "/help".into(),
            description: "Keybinding reference and docs".into(),
        },
        PaletteItem {
            command: "/feedback".into(),
            description: "Send feedback to operators".into(),
        },
        PaletteItem {
            command: "/palette".into(),
            description: "Show this palette".into(),
        },
        PaletteItem {
            command: "/sessions".into(),
            description: "List/manage saved sessions".into(),
        },
        PaletteItem {
            command: "/clear".into(),
            description: "Clear transcript".into(),
        },
        PaletteItem {
            command: "/quit".into(),
            description: "Exit session".into(),
        },
        PaletteItem {
            command: "/compact".into(),
            description: "Summarise conversation + receipt".into(),
        },
        PaletteItem {
            command: "/fork".into(),
            description: "Branch conversation".into(),
        },
        PaletteItem {
            command: "/plan".into(),
            description: "Plan mode, approve before edits".into(),
        },
        // Vault
        PaletteItem {
            command: "/vault remember".into(),
            description: "Record a fact/decision/lesson".into(),
        },
        PaletteItem {
            command: "/vault recall".into(),
            description: "Search vault (FTS + graph + temporal)".into(),
        },
        PaletteItem {
            command: "/vault status".into(),
            description: "Vault path, size, namespace, counts".into(),
        },
        PaletteItem {
            command: "/vault asof".into(),
            description: "What was believed at an instant".into(),
        },
        PaletteItem {
            command: "/vault history".into(),
            description: "Every version of one belief".into(),
        },
        PaletteItem {
            command: "/vault contradictions".into(),
            description: "Explicitly revoked beliefs".into(),
        },
        PaletteItem {
            command: "/vault forget".into(),
            description: "Revoke a belief, keep record".into(),
        },
        PaletteItem {
            command: "/vault correct".into(),
            description: "Supersede with stated reason".into(),
        },
        PaletteItem {
            command: "/vault export".into(),
            description: "Export vault to JSON/Markdown".into(),
        },
        // Code graph
        PaletteItem {
            command: "/kg index".into(),
            description: "Build/update code graph".into(),
        },
        PaletteItem {
            command: "/kg status".into(),
            description: "Graph staleness info".into(),
        },
        PaletteItem {
            command: "/kg impact".into(),
            description: "Blast radius of a symbol".into(),
        },
        PaletteItem {
            command: "/kg search".into(),
            description: "Lexical symbol search".into(),
        },
        PaletteItem {
            command: "/kg explore".into(),
            description: "Definition + snippet + refs".into(),
        },
        PaletteItem {
            command: "/kg refs".into(),
            description: "Find references".into(),
        },
        PaletteItem {
            command: "/kg callers".into(),
            description: "Functions calling symbol".into(),
        },
        PaletteItem {
            command: "/kg callees".into(),
            description: "Functions called by symbol".into(),
        },
        PaletteItem {
            command: "/kg def".into(),
            description: "Jump to definition".into(),
        },
        PaletteItem {
            command: "/kg diff-impact".into(),
            description: "Changed files → blast radius".into(),
        },
        // Cloud
        PaletteItem {
            command: "/stats".into(),
            description: "Daily activity, tokens, cost, sessions".into(),
        },
        PaletteItem {
            command: "/models".into(),
            description: "List available models".into(),
        },
        PaletteItem {
            command: "/usage".into(),
            description: "Tokens/cost by day·model·project".into(),
        },
        PaletteItem {
            command: "/credits".into(),
            description: "Balance, grants, expiry".into(),
        },
        PaletteItem {
            command: "/sync".into(),
            description: "Push vault/graph to cloud".into(),
        },
        // MCP
        PaletteItem {
            command: "/mcp serve".into(),
            description: "Start MCP server".into(),
        },
        PaletteItem {
            command: "/mcp doctor".into(),
            description: "MCP server health check".into(),
        },
        PaletteItem {
            command: "/mcp add".into(),
            description: "Register MCP client".into(),
        },
        PaletteItem {
            command: "/mcp token".into(),
            description: "Mint MCP bearer token".into(),
        },
        PaletteItem {
            command: "/mcp test".into(),
            description: "Tools/list + real call".into(),
        },
        // System
        PaletteItem {
            command: "/init".into(),
            description: "Generate AGENTS.md".into(),
        },
        PaletteItem {
            command: "/memory".into(),
            description: "Edit project memory file".into(),
        },
        PaletteItem {
            command: "!cmd".into(),
            description: "Shell passthrough (sandbox policy)".into(),
        },
        PaletteItem {
            command: "@file".into(),
            description: "Fuzzy path completion (Tab cycles)".into(),
        },
    ];

    let rows = vec![PaletteRow {
        label: "Version".into(),
        value: env!("CARGO_PKG_VERSION").into(),
        note: "memnoth".into(),
    }];

    PaletteConfig {
        title: "memnoth".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        subtitle: "Command Palette — type to filter, ↑/↓ navigate, Enter to run, Esc to close"
            .into(),
        rows,
        items,
        footer_hint: "Type to filter  •  ↑/↓ navigate  •  Enter run  •  Esc close".into(),
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
            KeyCode::Home => {
                self.palette.select_previous();
                self.list_state.select(Some(self.palette.selected_index()));
                OverlayAction::None
            }
            KeyCode::End => {
                self.palette.select_next();
                self.list_state.select(Some(self.palette.selected_index()));
                OverlayAction::None
            }
            KeyCode::Enter => {
                let chosen = {
                    let items = self.palette.filtered_items();
                    items
                        .get(self.palette.selected_index())
                        .map(|item| item.command.clone())
                };
                // Nothing matched: the words that were typed are the command.
                // They go into the prompt unchanged and unsent, because a
                // spelling the palette could not resolve is exactly the one a
                // person wants to finish or fix.
                let (command, from_menu) = match chosen {
                    Some(cmd) => (cmd, true),
                    None => {
                        let typed = self.palette.filter().trim().to_string();
                        (format!("/{typed}"), false)
                    }
                };
                if command.starts_with('/') || command.starts_with('!') || command.starts_with('@')
                {
                    // The palette fills the line and lets the session submit it;
                    // it never runs a command itself, so the transcript shows
                    // the same line as if it had been typed. A subcommand that
                    // takes an argument stops here with a trailing space, so the
                    // argument can be typed instead of the command being rejected
                    // for the one it is missing.
                    let needs_more = !from_menu || command.contains(' ');
                    app.input = if command.contains(' ') {
                        format!("{command} ")
                    } else {
                        command
                    };
                    app.cursor = app.input.chars().count();
                    app.pending_submit = !needs_more;
                }
                OverlayAction::Close
            }
            KeyCode::Char(ch) => {
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

        // Layout: title row + rows + items list + footer
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // title + subtitle
                Constraint::Length(2), // metadata rows
                Constraint::Min(10),   // items list
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

        // Items list
        let list_area = chunks[2];
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
            .highlight_symbol("► ");
        frame.render_stateful_widget(list, list_area, &mut self.list_state);

        // Footer hint
        let footer_area = chunks[3];
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
}
