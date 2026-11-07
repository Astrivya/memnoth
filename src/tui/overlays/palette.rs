//! Command palette overlay: beautiful dropdown of all slash commands with fuzzy search.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use std::collections::HashSet;

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
    merge(local_commands(), registry_items())
}

/// Registry rows, in stable command order.
fn registry_items() -> Vec<PaletteItem> {
    let registry = memnoth_command::build_p0_registry().build();
    let mut items: Vec<PaletteItem> = registry
        .all()
        .filter(|spec| !spec.slash.hidden)
        .filter_map(|spec| {
            spec.slash.palette.as_ref().map(|line| PaletteItem {
                command: format!("/{}", line.trim().trim_start_matches('/')),
                description: spec.slash.description.clone(),
            })
        })
        .collect();
    items.sort_by(|a, b| a.command.cmp(&b.command));
    items
}

/// Merge two surfaces without offering the same verb twice.
///
/// Two rows are the same command when their first two words match, so a local
/// `/vault asof` and a registered `/vault asof [--at WHEN]` collapse into the
/// first row that mentions the verb. The local rows come first because their
/// descriptions are written for the session.
fn merge(local: Vec<PaletteItem>, registry: Vec<PaletteItem>) -> Vec<PaletteItem> {
    let mut seen = HashSet::new();
    let mut merged = Vec::with_capacity(local.len() + registry.len());
    for item in local.into_iter().chain(registry) {
        if seen.insert(command_key(&item.command)) {
            merged.push(item);
        }
    }
    merged
}

/// The identity of a palette command: its first two words, lowercased.
fn command_key(command: &str) -> String {
    let mut words = command.split_whitespace();
    let head = words.next().unwrap_or_default();
    match words.next() {
        Some(second) => format!("{head} {second}").to_lowercase(),
        None => head.to_lowercase(),
    }
}

fn local_commands() -> Vec<PaletteItem> {
    vec![
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
            description: "Roll back the conversation; files are left untouched".into(),
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
            command: "/commands".into(),
            description: "Browse every command, grouped by area".into(),
        },
        PaletteItem {
            command: "/new".into(),
            description: "Start a fresh conversation".into(),
        },
        PaletteItem {
            command: "/sessions".into(),
            description: "Browse saved conversations, with a preview".into(),
        },
        PaletteItem {
            command: "/tasks".into(),
            description: "Show the background-job rail".into(),
        },
        PaletteItem {
            command: "/tasks run <command>".into(),
            description: "Start a background job".into(),
        },
        PaletteItem {
            command: "/tasks status".into(),
            description: "List background jobs".into(),
        },
        PaletteItem {
            command: "/tasks logs [id]".into(),
            description: "Show a job's output".into(),
        },
        PaletteItem {
            command: "/tasks cancel <id>".into(),
            description: "Stop a running job".into(),
        },
        PaletteItem {
            command: "/home".into(),
            description: "The session brief: what is here, what is missing".into(),
        },
        PaletteItem {
            command: "/clear".into(),
            description: "Clear transcript".into(),
        },
        PaletteItem {
            command: "/quit".into(),
            description: "Exit session".into(),
        },
        // Vault
        PaletteItem {
            command: "/vault remember".into(),
            description: "Record a fact/decision/lesson - /vault remember <name> <value>".into(),
        },
        PaletteItem {
            command: "/vault recall".into(),
            description: "Search vault (FTS + graph + temporal) - /vault recall <query>".into(),
        },
        PaletteItem {
            command: "/vault status".into(),
            description: "Vault path, size, namespace, counts".into(),
        },
        PaletteItem {
            command: "/vault asof".into(),
            description: "What was believed at an instant - /vault asof [--at WHEN]".into(),
        },
        PaletteItem {
            command: "/vault history".into(),
            description: "Every version of one belief - /vault history <name>".into(),
        },
        PaletteItem {
            command: "/vault contradictions".into(),
            description: "Explicitly revoked beliefs - /vault contradictions [name]".into(),
        },
        PaletteItem {
            command: "/vault forget".into(),
            description: "Revoke a belief, keep record - /vault forget <name> --reason <why>"
                .into(),
        },
        PaletteItem {
            command: "/vault correct".into(),
            description:
                "Supersede with a stated reason - /vault correct <name> <value> --reason <why>"
                    .into(),
        },
        PaletteItem {
            command: "/vault export".into(),
            description: "Export vault to JSON/Markdown - /vault export [file]".into(),
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
            description: "Blast radius of a symbol - /kg impact <symbol>".into(),
        },
        PaletteItem {
            command: "/kg search".into(),
            description: "Lexical symbol search - /kg search <query>".into(),
        },
        PaletteItem {
            command: "/kg explore".into(),
            description: "Definition + snippet + refs - /kg explore <symbol>".into(),
        },
        PaletteItem {
            command: "/kg refs".into(),
            description: "Find references - /kg refs <symbol>".into(),
        },
        PaletteItem {
            command: "/kg callers".into(),
            description: "Functions calling symbol - /kg callers <symbol>".into(),
        },
        PaletteItem {
            command: "/kg callees".into(),
            description: "Functions called by symbol - /kg callees <symbol>".into(),
        },
        PaletteItem {
            command: "/kg def".into(),
            description: "Jump to definition - /kg def <symbol>".into(),
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
        // The tenant surface: the console's Organization, Projects, Teams and
        // API-keys pages, reachable from the session. The four heads carry no
        // `<` on purpose — a bare `/projects` answers with the list instead of
        // looking stuck — and the destructive subcommands (`/projects delete`,
        // `/keys revoke-all`) are named as verbs rather than spelled out, since
        // a row that pre-fills `--yes` is a row that invites a mistake.
        PaletteItem {
            command: "/projects".into(),
            description: "The org's projects - create, rename, delete".into(),
        },
        PaletteItem {
            command: "/keys".into(),
            description: "Org API keys - list, mint, revoke".into(),
        },
        PaletteItem {
            command: "/keys create".into(),
            description: "Mint a key - the secret is shown once, never in the transcript".into(),
        },
        PaletteItem {
            command: "/org".into(),
            description: "The organization: members, invites, roles".into(),
        },
        PaletteItem {
            command: "/org invite".into(),
            description: "Invite by email - the link and code are shown once".into(),
        },
        PaletteItem {
            command: "/teams".into(),
            description: "Org teams - list, create, join by code".into(),
        },
        PaletteItem {
            command: "/teams create <name>".into(),
            description: "Start a team - /teams create <name>".into(),
        },
        PaletteItem {
            command: "/teams members".into(),
            description: "A team's people and projects - pick the team from a list".into(),
        },
        PaletteItem {
            command: "/teams members <id>".into(),
            description: "A team's people and projects - /teams members <id>".into(),
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
        PaletteItem {
            command: "/login".into(),
            description: "Sign in to memnoth cloud".into(),
        },
        PaletteItem {
            command: "/logout".into(),
            description: "Sign out of this machine".into(),
        },
        PaletteItem {
            command: "/whoami".into(),
            description: "This machine's account, org, role and key expiry".into(),
        },
        PaletteItem {
            command: "/doctor".into(),
            description: "Report what is wired on this machine".into(),
        },
        PaletteItem {
            command: "/theme".into(),
            description: "Switch palette - /theme <default|high-contrast|no-color>".into(),
        },
        PaletteItem {
            command: "/screen-reader".into(),
            description: "Accessible output - /screen-reader <off|plain>".into(),
        },
        PaletteItem {
            command: "/skill add".into(),
            description: "Install a skill - /skill add <path>".into(),
        },
        PaletteItem {
            command: "/skill list".into(),
            description: "List installed skills".into(),
        },
        PaletteItem {
            command: "/skill remove".into(),
            description: "Remove a skill - /skill remove <name>".into(),
        },
        PaletteItem {
            command: "/skill run".into(),
            description: "Run a skill - /skill run <name> [KEY=VALUE...]".into(),
        },
        PaletteItem {
            command: "/skill edit".into(),
            description: "Edit a skill - /skill edit <name>".into(),
        },
        // Vault: the full command surface, not just the seven that were here.
        PaletteItem {
            command: "/vault init".into(),
            description: "Create the temporal vault".into(),
        },
        PaletteItem {
            command: "/vault link".into(),
            description:
                "Relate two entities - /vault link <source> <target> --kind <kind> --reason <why>"
                    .into(),
        },
        PaletteItem {
            command: "/vault unlink".into(),
            description:
                "Remove a relation - /vault unlink <source> <target> --kind <kind> --reason <why>"
                    .into(),
        },
        PaletteItem {
            command: "/vault search".into(),
            description: "Search beliefs - /vault search <query>".into(),
        },
        PaletteItem {
            command: "/vault import".into(),
            description: "Import JSON or Markdown - /vault import [file]".into(),
        },
        PaletteItem {
            command: "/vault gc".into(),
            description: "Close stale observations - /vault gc [--keep N]".into(),
        },
        PaletteItem {
            command: "/vault prune".into(),
            description: "Prune superseded beliefs - /vault prune [--keep-days N]".into(),
        },
        PaletteItem {
            command: "/vault reindex".into(),
            description: "Rebuild search indexes".into(),
        },
        PaletteItem {
            command: "/vault stats".into(),
            description: "Counts, growth and size".into(),
        },
        PaletteItem {
            command: "/vault backup".into(),
            description: "Snapshot the vault - /vault backup <file>".into(),
        },
        PaletteItem {
            command: "/vault restore".into(),
            description: "Restore from a snapshot - /vault restore <file>".into(),
        },
        PaletteItem {
            command: "/vault audit".into(),
            description: "Chronological activity feed".into(),
        },
        PaletteItem {
            command: "/vault merge".into(),
            description: "Fold one entity into another - /vault merge <from> <into> --reason <why>"
                .into(),
        },
        PaletteItem {
            command: "/vault duplicates".into(),
            description: "Entities that look like the same thing".into(),
        },
        PaletteItem {
            command: "/vault top".into(),
            description: "Most connected entities - /vault top [--limit N] [--kind K]".into(),
        },
        PaletteItem {
            command: "/vault timeline".into(),
            description: "Facts in time order - /vault timeline [--at WHEN]".into(),
        },
        PaletteItem {
            command: "/vault explain".into(),
            description: "Everything known about an entity - /vault explain <name>".into(),
        },
        PaletteItem {
            command: "/vault graph".into(),
            description: "Relations around an entity - /vault graph <name> [--depth N]".into(),
        },
        // Code graph: the fourteen commands the dispatcher used to refuse.
        PaletteItem {
            command: "/kg callpath".into(),
            description: "Shortest path between symbols - /kg callpath <from> <to>".into(),
        },
        PaletteItem {
            command: "/kg outline".into(),
            description: "Structural outline of a file - /kg outline <file>".into(),
        },
        PaletteItem {
            command: "/kg tree".into(),
            description: "Symbol tree for a directory - /kg tree <dir>".into(),
        },
        PaletteItem {
            command: "/kg review".into(),
            description: "Graph-aware diff review".into(),
        },
        PaletteItem {
            command: "/kg tests-for".into(),
            description: "Tests covering a symbol - /kg tests-for <symbol>".into(),
        },
        PaletteItem {
            command: "/kg owners".into(),
            description: "CODEOWNERS and blame - /kg owners <path>".into(),
        },
        PaletteItem {
            command: "/kg dead-code".into(),
            description: "Unreferenced exports".into(),
        },
        PaletteItem {
            command: "/kg cycles".into(),
            description: "Import cycles".into(),
        },
        PaletteItem {
            command: "/kg hotspots".into(),
            description: "Churn x complexity hotspots".into(),
        },
        PaletteItem {
            command: "/kg unused-exports".into(),
            description: "Unused public exports".into(),
        },
        PaletteItem {
            command: "/kg deps".into(),
            description: "Import graph for a file - /kg deps <file>".into(),
        },
        PaletteItem {
            command: "/kg query".into(),
            description: "Graph DSL query - /kg query <expr>".into(),
        },
        PaletteItem {
            command: "/kg watch".into(),
            description: "Reindex as files change".into(),
        },
        PaletteItem {
            command: "/kg serve".into(),
            description: "Local HTTP/JSON graph API".into(),
        },
        // Worktrees and parallel agents.
        PaletteItem {
            command: "/worktree create".into(),
            description: "Isolated worktree - /worktree create <name>".into(),
        },
        PaletteItem {
            command: "/worktree list".into(),
            description: "List worktrees".into(),
        },
        PaletteItem {
            command: "/worktree clean".into(),
            description: "Remove a worktree - /worktree clean <name>".into(),
        },
        PaletteItem {
            command: "/worktree diff".into(),
            description: "Diff two worktrees - /worktree diff <from> [to]".into(),
        },
        PaletteItem {
            command: "/batch".into(),
            description: "Fan a task across worktrees - /batch <task>".into(),
        },
        PaletteItem {
            command: "/parallel".into(),
            description: "Run a command in every worktree - /parallel <command> [targets]".into(),
        },
        // Automation, CI and local services.
        PaletteItem {
            command: "/automation hooks install".into(),
            description: "Pre-commit graph indexing".into(),
        },
        PaletteItem {
            command: "/automation action init".into(),
            description: "Generate a GitHub Actions workflow".into(),
        },
        PaletteItem {
            command: "/automation ci detect".into(),
            description: "Detect the CI environment".into(),
        },
        PaletteItem {
            command: "/automation ci index".into(),
            description: "Index the graph in CI".into(),
        },
        PaletteItem {
            command: "/docs".into(),
            description: "Generate command documentation".into(),
        },
        PaletteItem {
            command: "/serve".into(),
            description: "Editor HTTP API - /serve [--port N]".into(),
        },
        PaletteItem {
            command: "/watch".into(),
            description: "Watch files and reindex incrementally".into(),
        },
    ]
}

/// Whether a command needs an argument before it can run.
///
/// The usage string carries the answer: `<...>` is required, `[...]` is
/// optional. That keeps one source of truth for the palette, the inline
/// dropdown and the tests, instead of a second hand-maintained list that drifts.
pub(crate) fn needs_arguments(item: &PaletteItem) -> bool {
    item.command.contains('<') || item.description.contains('<')
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
                        } else if needs_more {
                            format!("{command} ")
                        } else {
                            command
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

    #[test]
    fn every_registered_palette_line_reaches_the_palette_once() {
        let items = commands();
        let keys: Vec<String> = items.iter().map(|i| command_key(&i.command)).collect();
        let unique: HashSet<&String> = keys.iter().collect();
        assert_eq!(keys.len(), unique.len(), "the palette offers a verb twice");

        let registry = memnoth_command::build_p0_registry().build();
        let registered: Vec<String> = registry
            .all()
            .filter(|spec| !spec.slash.hidden)
            .filter_map(|spec| spec.slash.palette.clone())
            .collect();
        assert!(
            !registered.is_empty(),
            "no registry rows are marked for the palette"
        );
        for line in registered {
            let command = format!("/{}", line.trim().trim_start_matches('/'));
            assert!(
                items
                    .iter()
                    .any(|item| command_key(&item.command) == command_key(&command)),
                "{command} is registered but missing from the palette"
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
