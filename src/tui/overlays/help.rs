//! Help overlay: contextual keybinding reference and command documentation.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use memnoth_ui::keybindings::{KeyBinding, KeyBindingGroup, KeyBindingHelp, default_memnoth_help};

#[derive(Debug, Default)]
pub struct HelpOverlay {
    visible: bool,
    tabs: Vec<HelpTab>,
    active_tab: usize,
    global_help: KeyBindingHelp,
    selected: usize,
    list_state: ListState,
    search_mode: bool,
    search_query: String,
    search_cursor: usize,
    filtered_bindings: Vec<(usize, usize, KeyBinding)>, // (group_idx, binding_idx, binding)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpTab {
    Global,
    Overlays,
    Commands,
    Shortcuts,
}

impl HelpOverlay {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            tabs: vec![
                HelpTab::Global,
                HelpTab::Overlays,
                HelpTab::Commands,
                HelpTab::Shortcuts,
            ],
            list_state: state,
            global_help: default_memnoth_help(),
            ..Default::default()
        }
    }

    fn build_overlay_help() -> KeyBindingHelp {
        let mut help = KeyBindingHelp::new();

        // Search overlay
        let mut search = KeyBindingGroup::new("Search Overlay");
        search.add_binding(KeyBinding::new(
            "Enter",
            "Execute search",
            "Run the search query",
        ));
        search.add_binding(KeyBinding::new(
            "Tab",
            "Switch mode",
            "Toggle between query input and results",
        ));
        search.add_binding(KeyBinding::new(
            "↑/↓",
            "Navigate",
            "Move through search results",
        ));
        search.add_binding(KeyBinding::new(
            "Enter",
            "Open result",
            "Open the selected result",
        ));
        search.add_binding(KeyBinding::new(
            "Ctrl+C",
            "Copy",
            "Copy selected result to clipboard",
        ));
        search.add_binding(KeyBinding::new("P", "Pin", "Pin selected result"));
        search.add_binding(KeyBinding::new("Esc", "Close", "Close the search overlay"));
        help.add_group(search);

        // Context overlay
        let mut context = KeyBindingGroup::new("Context Overlay");
        context.add_binding(KeyBinding::new(
            "Tab",
            "Next tab",
            "Switch between Beliefs/Coordination/History",
        ));
        context.add_binding(KeyBinding::new(
            "Shift+Tab",
            "Prev tab",
            "Switch tabs backwards",
        ));
        context.add_binding(KeyBinding::new("↑/↓", "Navigate", "Move through entries"));
        context.add_binding(KeyBinding::new(
            "Enter",
            "Detail",
            "View detailed information",
        ));
        context.add_binding(KeyBinding::new("Ctrl+C", "Copy", "Copy entry"));
        context.add_binding(KeyBinding::new("P", "Pin", "Pin entry"));
        context.add_binding(KeyBinding::new("R", "Refresh", "Reload context data"));
        context.add_binding(KeyBinding::new(
            "Esc",
            "Close/Back",
            "Close overlay or return from detail",
        ));
        help.add_group(context);

        // Diff overlay
        let mut diff = KeyBindingGroup::new("Diff Overlay");
        diff.add_binding(KeyBinding::new(
            "↑/↓",
            "Navigate hunks",
            "Move between diff hunks",
        ));
        diff.add_binding(KeyBinding::new(
            "←/→",
            "Scroll",
            "Horizontal scroll within hunk",
        ));
        diff.add_binding(KeyBinding::new(
            "A",
            "Accept hunk",
            "Accept the current hunk",
        ));
        diff.add_binding(KeyBinding::new(
            "R",
            "Reject hunk",
            "Reject the current hunk",
        ));
        diff.add_binding(KeyBinding::new("Shift+A", "Accept all", "Accept all hunks"));
        diff.add_binding(KeyBinding::new("Shift+R", "Reject all", "Reject all hunks"));
        diff.add_binding(KeyBinding::new(
            "N/n",
            "Next/Prev conflict",
            "Jump to next/previous conflict",
        ));
        diff.add_binding(KeyBinding::new(
            "S",
            "Side-by-side",
            "Toggle side-by-side view",
        ));
        diff.add_binding(KeyBinding::new(
            "W",
            "Whitespace",
            "Toggle whitespace visibility",
        ));
        diff.add_binding(KeyBinding::new("L", "Line numbers", "Toggle line numbers"));
        diff.add_binding(KeyBinding::new("?", "Help", "Show diff viewer help"));
        diff.add_binding(KeyBinding::new("Esc", "Close", "Close the diff overlay"));
        help.add_group(diff);

        // Rewind overlay
        let mut rewind = KeyBindingGroup::new("Rewind Overlay");
        rewind.add_binding(KeyBinding::new(
            "Tab",
            "Next tab",
            "Switch between Sessions/Turns/Branches",
        ));
        rewind.add_binding(KeyBinding::new("↑/↓", "Navigate", "Move through entries"));
        rewind.add_binding(KeyBinding::new(
            "Enter",
            "Detail/Resume",
            "View turn detail or resume session",
        ));
        rewind.add_binding(KeyBinding::new(
            "B",
            "Branch",
            "Create new session from turn",
        ));
        rewind.add_binding(KeyBinding::new("R", "Resume", "Resume selected session"));
        rewind.add_binding(KeyBinding::new("D", "Delete", "Delete session"));
        rewind.add_binding(KeyBinding::new("Ctrl+C", "Copy", "Copy entry"));
        rewind.add_binding(KeyBinding::new("P", "Pin", "Pin entry"));
        rewind.add_binding(KeyBinding::new(
            "Esc",
            "Close/Back",
            "Close overlay or return from detail",
        ));
        help.add_group(rewind);

        // Model overlay
        let mut model = KeyBindingGroup::new("Model Overlay");
        model.add_binding(KeyBinding::new(
            "Tab",
            "Focus",
            "Tab bar / models (All / Connected / Favorites / Reasoning)",
        ));
        model.add_binding(KeyBinding::new(
            "↑/↓",
            "Navigate",
            "Move through models; ↑ at the top reaches the tabs",
        ));
        model.add_binding(KeyBinding::new(
            "←/→",
            "Zone arrows",
            "Reasoning effort on a model, tabs on the tab bar",
        ));
        model.add_binding(KeyBinding::new(
            "Enter",
            "Use",
            "Use this model now, and save it",
        ));
        model.add_binding(KeyBinding::new(
            "type",
            "Search",
            "Typing filters — no prefix key needed",
        ));
        model.add_binding(KeyBinding::new(
            "Alt+1..6, Alt+0",
            "Provider",
            "Filter by provider, alt-0 clears",
        ));
        model.add_binding(KeyBinding::new(
            "Alt+s, Ctrl+s",
            "Sort",
            "Sort field, then direction",
        ));
        model.add_binding(KeyBinding::new(
            "Alt+f",
            "Favorite",
            "Pin the model in the Favorites tab",
        ));
        model.add_binding(KeyBinding::new(
            "Alt+c",
            "Compare",
            "Compare against the current model",
        ));
        model.add_binding(KeyBinding::new(
            "Alt+i",
            "Details",
            "Context window, price and reasoning",
        ));
        model.add_binding(KeyBinding::new(
            "Alt+r, Ctrl+r",
            "Refresh",
            "Fetch the provider's live model list",
        ));
        model.add_binding(KeyBinding::new("Esc", "Close", "Close the model overlay"));
        help.add_group(model);

        // Permissions overlay
        let mut perms = KeyBindingGroup::new("Permissions Overlay");
        perms.add_binding(KeyBinding::new(
            "Tab",
            "Next tab",
            "Switch between Tools/Policies/Requests/Audit",
        ));
        perms.add_binding(KeyBinding::new("↑/↓", "Navigate", "Move through entries"));
        perms.add_binding(KeyBinding::new(
            "Enter",
            "Detail",
            "View detailed information",
        ));
        perms.add_binding(KeyBinding::new(
            "A",
            "Allow/Approve",
            "Allow tool or approve request",
        ));
        perms.add_binding(KeyBinding::new("D", "Deny", "Deny tool or deny request"));
        perms.add_binding(KeyBinding::new("K", "Ask", "Set tool to ask permission"));
        perms.add_binding(KeyBinding::new("E", "Enable", "Enable/disable policy"));
        perms.add_binding(KeyBinding::new("Ctrl+C", "Copy", "Copy entry"));
        perms.add_binding(KeyBinding::new("R", "Refresh", "Reload permissions data"));
        perms.add_binding(KeyBinding::new(
            "Esc",
            "Close/Back",
            "Close overlay or return from detail",
        ));
        help.add_group(perms);

        help
    }

    fn build_command_help() -> KeyBindingHelp {
        // One projection: the same rows the palette and the dropdown draw.
        // Help keeps no second table of command names.
        let mut help = KeyBindingHelp::new();
        let mut group = KeyBindingGroup::new("Slash Commands");
        for row in crate::tui::overlays::command_rows::rows() {
            let chip = row.chip();
            group.add_binding(KeyBinding::new(row.command, chip, row.description));
        }
        help.add_group(group);
        help
    }

    fn build_shortcut_help() -> KeyBindingHelp {
        let mut help = KeyBindingHelp::new();

        let mut global = KeyBindingGroup::new("Global Keys");
        global.add_binding(KeyBinding::new("Ctrl+C", "Quit", "Exit the session"));
        global.add_binding(KeyBinding::new("Ctrl+D", "Quit", "Exit the session"));
        global.add_binding(KeyBinding::new("Ctrl+L", "Clear", "Clear the transcript"));
        global.add_binding(KeyBinding::new(
            "↑/↓",
            "History",
            "Navigate command history",
        ));
        global.add_binding(KeyBinding::new(
            "/",
            "Commands",
            "Type a slash to list commands under the prompt",
        ));
        global.add_binding(KeyBinding::new(
            "PgUp/PgDn",
            "Scroll",
            "Scroll transcript by page",
        ));
        global.add_binding(KeyBinding::new("End", "Bottom", "Jump to newest output"));
        global.add_binding(KeyBinding::new("←/→", "Edit", "Move cursor in input line"));
        global.add_binding(KeyBinding::new(
            "Home/Ctrl+A",
            "Line start",
            "Move to start of input",
        ));
        global.add_binding(KeyBinding::new(
            "End/Ctrl+E",
            "Line end",
            "Move to end of input",
        ));
        global.add_binding(KeyBinding::new(
            "Esc",
            "Clear input",
            "Clear the input line",
        ));
        help.add_group(global);

        let mut overlay_nav = KeyBindingGroup::new("Overlay Navigation");
        overlay_nav.add_binding(KeyBinding::new("Esc", "Close", "Close current overlay"));
        overlay_nav.add_binding(KeyBinding::new("Tab", "Next", "Next overlay / next tab"));
        overlay_nav.add_binding(KeyBinding::new("Shift+Tab", "Prev", "Previous tab"));
        overlay_nav.add_binding(KeyBinding::new(
            "Enter",
            "Action",
            "Primary action (varies by overlay)",
        ));
        overlay_nav.add_binding(KeyBinding::new("?", "Help", "Show overlay-specific help"));
        help.add_group(overlay_nav);

        help
    }

    /// The tab the cursor is on. `active_tab` stays an index because `Tabs`
    /// takes one; this is where that index becomes a variant.
    fn tab(&self) -> HelpTab {
        self.tabs
            .get(self.active_tab)
            .copied()
            .unwrap_or(HelpTab::Global)
    }

    fn update_filtered(&mut self) {
        self.filtered_bindings.clear();
        if self.search_query.is_empty() {
            return;
        }
        let query = self.search_query.to_lowercase();

        // Search global help
        for (gi, group) in self.global_help.groups.iter().enumerate() {
            for (bi, binding) in group.bindings.iter().enumerate() {
                let text = format!("{} {} {}", binding.key, binding.action, binding.description)
                    .to_lowercase();
                if text.contains(&query) {
                    self.filtered_bindings.push((gi, bi, binding.clone()));
                }
            }
        }

        // Search overlay help
        let overlay_help = Self::build_overlay_help();
        for (gi, group) in overlay_help.groups.iter().enumerate() {
            for (bi, binding) in group.bindings.iter().enumerate() {
                let text = format!("{} {} {}", binding.key, binding.action, binding.description)
                    .to_lowercase();
                if text.contains(&query) {
                    self.filtered_bindings.push((gi + 100, bi, binding.clone()));
                }
            }
        }

        // Search command help
        let command_help = Self::build_command_help();
        for (gi, group) in command_help.groups.iter().enumerate() {
            for (bi, binding) in group.bindings.iter().enumerate() {
                let text = format!("{} {} {}", binding.key, binding.action, binding.description)
                    .to_lowercase();
                if text.contains(&query) {
                    self.filtered_bindings.push((gi + 200, bi, binding.clone()));
                }
            }
        }

        // Search shortcut help
        let shortcut_help = Self::build_shortcut_help();
        for (gi, group) in shortcut_help.groups.iter().enumerate() {
            for (bi, binding) in group.bindings.iter().enumerate() {
                let text = format!("{} {} {}", binding.key, binding.action, binding.description)
                    .to_lowercase();
                if text.contains(&query) {
                    self.filtered_bindings.push((gi + 300, bi, binding.clone()));
                }
            }
        }

        self.list_state
            .select(if self.filtered_bindings.is_empty() {
                None
            } else {
                Some(0)
            });
        self.selected = 0;
    }

    fn render_global_help(&mut self, frame: &mut Frame, area: Rect) {
        let mut all_bindings = Vec::new();
        for group in &self.global_help.groups {
            for binding in &group.bindings {
                all_bindings.push((group.name.clone(), binding.clone()));
            }
        }

        let items: Vec<ListItem> = all_bindings
            .iter()
            .enumerate()
            .map(|(i, (group_name, binding))| {
                let selected = i == self.selected;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let key = Span::styled(
                    format!("{}  ", binding.key),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
                let action = Span::styled(binding.action.clone(), style);
                let description = Span::styled(
                    format!("  — {}", binding.description),
                    style.fg(Color::Gray),
                );
                let group = Span::styled(format!("  [{}]", group_name), style.fg(Color::DarkGray));
                // One row per binding only when the terminal can show the
                // description whole. A `List` clips, so at 80 columns the tail of
                // every long binding used to vanish mid-word; the second line
                // keeps the words and costs one row.
                let width = binding.key.chars().count()
                    + 2
                    + binding.action.chars().count()
                    + 2
                    + binding.description.chars().count()
                    + 3
                    + group_name.chars().count()
                    + 2;
                // The block's borders and padding eat two columns, so a line
                // that fits the outer rect still clips inside it.
                let usable = area.width.saturating_sub(2) as usize;
                // Four columns of slack: a row that recomputes to exactly the
                // usable width still loses its last glyph to the border.
                let lines = if width.saturating_add(4) > usable {
                    vec![
                        Line::from(vec![key, action]),
                        Line::from(vec![
                            Span::styled("    ", Style::default()),
                            description,
                            group,
                        ]),
                    ]
                } else {
                    vec![Line::from(vec![key, action, description, group])]
                };
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Global Keybindings "),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_overlay_help(&mut self, frame: &mut Frame, area: Rect) {
        let overlay_help = Self::build_overlay_help();
        let mut all_bindings = Vec::new();
        for group in &overlay_help.groups {
            for binding in &group.bindings {
                all_bindings.push((group.name.clone(), binding.clone()));
            }
        }

        let items: Vec<ListItem> = all_bindings
            .iter()
            .enumerate()
            .map(|(i, (group_name, binding))| {
                let selected = i == self.selected;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let key = Span::styled(
                    format!("{}  ", binding.key),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
                let action = Span::styled(binding.action.clone(), style);
                let description = Span::styled(
                    format!("  — {}", binding.description),
                    style.fg(Color::Gray),
                );
                let group = Span::styled(format!("  [{}]", group_name), style.fg(Color::DarkGray));
                // One row per binding only when the terminal can show the
                // description whole. A `List` clips, so at 80 columns the tail of
                // every long binding used to vanish mid-word; the second line
                // keeps the words and costs one row.
                let width = binding.key.chars().count()
                    + 2
                    + binding.action.chars().count()
                    + 2
                    + binding.description.chars().count()
                    + 3
                    + group_name.chars().count()
                    + 2;
                let lines = if width > area.width as usize {
                    vec![
                        Line::from(vec![key, action]),
                        Line::from(vec![
                            Span::styled("    ", Style::default()),
                            description,
                            group,
                        ]),
                    ]
                } else {
                    vec![Line::from(vec![key, action, description, group])]
                };
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Overlay Keybindings "),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_command_help(&mut self, frame: &mut Frame, area: Rect) {
        let command_help = Self::build_command_help();
        let mut all_bindings = Vec::new();
        for group in &command_help.groups {
            for binding in &group.bindings {
                all_bindings.push((group.name.clone(), binding.clone()));
            }
        }

        let items: Vec<ListItem> = all_bindings
            .iter()
            .enumerate()
            .map(|(i, (group_name, binding))| {
                let selected = i == self.selected;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let key = Span::styled(
                    format!("{}  ", binding.key),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
                let action = Span::styled(binding.action.clone(), style);
                let description = Span::styled(
                    format!("  — {}", binding.description),
                    style.fg(Color::Gray),
                );
                let group = Span::styled(format!("  [{}]", group_name), style.fg(Color::DarkGray));
                // One row per binding only when the terminal can show the
                // description whole. A `List` clips, so at 80 columns the tail of
                // every long binding used to vanish mid-word; the second line
                // keeps the words and costs one row.
                let width = binding.key.chars().count()
                    + 2
                    + binding.action.chars().count()
                    + 2
                    + binding.description.chars().count()
                    + 3
                    + group_name.chars().count()
                    + 2;
                let lines = if width > area.width as usize {
                    vec![
                        Line::from(vec![key, action]),
                        Line::from(vec![
                            Span::styled("    ", Style::default()),
                            description,
                            group,
                        ]),
                    ]
                } else {
                    vec![Line::from(vec![key, action, description, group])]
                };
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Slash Commands "),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_shortcut_help(&mut self, frame: &mut Frame, area: Rect) {
        let shortcut_help = Self::build_shortcut_help();
        let mut all_bindings = Vec::new();
        for group in &shortcut_help.groups {
            for binding in &group.bindings {
                all_bindings.push((group.name.clone(), binding.clone()));
            }
        }

        let items: Vec<ListItem> = all_bindings
            .iter()
            .enumerate()
            .map(|(i, (group_name, binding))| {
                let selected = i == self.selected;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let key = Span::styled(
                    format!("{}  ", binding.key),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
                let action = Span::styled(binding.action.clone(), style);
                let description = Span::styled(
                    format!("  — {}", binding.description),
                    style.fg(Color::Gray),
                );
                let group = Span::styled(format!("  [{}]", group_name), style.fg(Color::DarkGray));
                // One row per binding only when the terminal can show the
                // description whole. A `List` clips, so at 80 columns the tail of
                // every long binding used to vanish mid-word; the second line
                // keeps the words and costs one row.
                let width = binding.key.chars().count()
                    + 2
                    + binding.action.chars().count()
                    + 2
                    + binding.description.chars().count()
                    + 3
                    + group_name.chars().count()
                    + 2;
                let lines = if width > area.width as usize {
                    vec![
                        Line::from(vec![key, action]),
                        Line::from(vec![
                            Span::styled("    ", Style::default()),
                            description,
                            group,
                        ]),
                    ]
                } else {
                    vec![Line::from(vec![key, action, description, group])]
                };
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Shortcuts "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_search_results(&mut self, frame: &mut Frame, area: Rect) {
        if self.filtered_bindings.is_empty() {
            let empty = Paragraph::new("No matches found")
                .style(Style::default().fg(Color::DarkGray))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Search Results "),
                );
            frame.render_widget(empty, area);
            return;
        }

        let items: Vec<ListItem> = self
            .filtered_bindings
            .iter()
            .enumerate()
            .map(|(i, (gi, _bi, binding))| {
                let selected = i == self.selected;
                let style = if selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let group_name = if *gi < 100 {
                    "Global"
                } else if *gi < 200 {
                    "Overlays"
                } else if *gi < 300 {
                    "Commands"
                } else {
                    "Shortcuts"
                };

                let key = Span::styled(
                    format!("{}  ", binding.key),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
                let action = Span::styled(binding.action.clone(), style);
                let description = Span::styled(
                    format!("  — {}", binding.description),
                    style.fg(Color::Gray),
                );
                let group = Span::styled(format!("  [{}]", group_name), style.fg(Color::DarkGray));
                // One row per binding only when the terminal can show the
                // description whole. A `List` clips, so at 80 columns the tail of
                // every long binding used to vanish mid-word; the second line
                // keeps the words and costs one row.
                let width = binding.key.chars().count()
                    + 2
                    + binding.action.chars().count()
                    + 2
                    + binding.description.chars().count()
                    + 3
                    + group_name.chars().count()
                    + 2;
                let lines = if width > area.width as usize {
                    vec![
                        Line::from(vec![key, action]),
                        Line::from(vec![
                            Span::styled("    ", Style::default()),
                            description,
                            group,
                        ]),
                    ]
                } else {
                    vec![Line::from(vec![key, action, description, group])]
                };
                ListItem::new(lines).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Search Results "),
            )
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }
}

impl Overlay for HelpOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Help
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.active_tab = 0;
        self.selected = 0;
        self.list_state.select(Some(0));
        self.search_mode = false;
        self.search_query.clear();
    }

    fn hide(&mut self) {
        self.visible = false;
        self.search_mode = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Handle search mode
        if self.search_mode {
            match key.code {
                KeyCode::Esc => {
                    self.search_mode = false;
                    self.search_query.clear();
                    self.search_cursor = 0;
                    self.update_filtered();
                }
                KeyCode::Enter => {
                    self.search_mode = false;
                }
                KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                    self.search_query.insert(self.search_cursor, ch);
                    self.search_cursor += 1;
                    self.update_filtered();
                }
                KeyCode::Backspace => {
                    if self.search_cursor > 0 {
                        self.search_cursor -= 1;
                        self.search_query.remove(self.search_cursor);
                        self.update_filtered();
                    }
                }
                KeyCode::Delete => {
                    if self.search_cursor < self.search_query.len() {
                        self.search_query.remove(self.search_cursor);
                        self.update_filtered();
                    }
                }
                KeyCode::Left => {
                    if self.search_cursor > 0 {
                        self.search_cursor -= 1;
                    }
                }
                KeyCode::Right => {
                    if self.search_cursor < self.search_query.len() {
                        self.search_cursor += 1;
                    }
                }
                KeyCode::Up => {
                    if self.selected > 0 {
                        self.selected -= 1;
                        self.list_state.select(Some(self.selected));
                    }
                }
                KeyCode::Down if self.selected + 1 < self.filtered_bindings.len() => {
                    self.selected += 1;
                    self.list_state.select(Some(self.selected));
                }
                _ => {}
            }
            return OverlayAction::None;
        }

        match key.code {
            KeyCode::Esc => return OverlayAction::Close,
            KeyCode::Char('q') => return OverlayAction::Close,
            KeyCode::Char('/') => {
                self.search_mode = true;
                self.search_query.clear();
                self.search_cursor = 0;
            }
            KeyCode::Tab => {
                self.active_tab = (self.active_tab + 1) % self.tabs.len();
                self.selected = 0;
                self.list_state.select(Some(0));
            }
            KeyCode::BackTab => {
                self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
                self.selected = 0;
                self.list_state.select(Some(0));
            }
            KeyCode::Up => {
                let _len = match self.tab() {
                    HelpTab::Global => self
                        .global_help
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                    HelpTab::Overlays => Self::build_overlay_help()
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                    HelpTab::Commands => Self::build_command_help()
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                    HelpTab::Shortcuts => Self::build_shortcut_help()
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                };
                if self.selected > 0 {
                    self.selected -= 1;
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::Down => {
                let len = match self.tab() {
                    HelpTab::Global => self
                        .global_help
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                    HelpTab::Overlays => Self::build_overlay_help()
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                    HelpTab::Commands => Self::build_command_help()
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                    HelpTab::Shortcuts => Self::build_shortcut_help()
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                };
                if self.selected + 1 < len {
                    self.selected += 1;
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(10);
                self.list_state.select(Some(self.selected));
            }
            KeyCode::PageDown => {
                let len = match self.tab() {
                    HelpTab::Global => self
                        .global_help
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                    HelpTab::Overlays => Self::build_overlay_help()
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                    HelpTab::Commands => Self::build_command_help()
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                    HelpTab::Shortcuts => Self::build_shortcut_help()
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                };
                self.selected = (self.selected + 10).min(len.saturating_sub(1));
                self.list_state.select(Some(self.selected));
            }
            KeyCode::Home => {
                self.selected = 0;
                self.list_state.select(Some(0));
            }
            KeyCode::End => {
                let len = match self.tab() {
                    HelpTab::Global => self
                        .global_help
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                    HelpTab::Overlays => Self::build_overlay_help()
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                    HelpTab::Commands => Self::build_command_help()
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                    HelpTab::Shortcuts => Self::build_shortcut_help()
                        .groups
                        .iter()
                        .flat_map(|g| &g.bindings)
                        .count(),
                };
                self.selected = len.saturating_sub(1);
                self.list_state.select(Some(self.selected));
            }
            KeyCode::Char('c') if ctrl => {
                // Copy selected binding
                app.push(
                    crate::tui::app::EntryRole::Info,
                    "Copied binding".to_string(),
                );
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }

        frame.render_widget(Clear, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),                                    // Tabs
                Constraint::Length(if self.search_mode { 3 } else { 0 }), // Search bar
                Constraint::Min(0),                                       // Content
                Constraint::Length(2),                                    // Help footer
            ])
            .split(area);

        // Tabs
        let tab_titles: Vec<&str> = self
            .tabs
            .iter()
            .map(|t| match t {
                HelpTab::Global => "Global",
                HelpTab::Overlays => "Overlays",
                HelpTab::Commands => "Commands",
                HelpTab::Shortcuts => "Shortcuts",
            })
            .collect();
        let tabs = Tabs::new(tab_titles)
            .block(Block::default().borders(Borders::ALL).title(" Help "))
            .select(self.active_tab)
            .style(Style::default().fg(Color::Gray))
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(tabs, chunks[0]);

        // Content is always the third chunk. The search bar is chunks[1] and
        // collapses to zero height when it is closed, so using it here drew the
        // whole list into a zero-height rect and the overlay looked empty.
        let content_area = chunks[2];

        // Search bar
        if self.search_mode {
            let search_text = format!("/{}_", self.search_query);
            let search = Paragraph::new(search_text)
                .style(Style::default().fg(Color::Yellow))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Search (Esc to cancel) "),
                );
            frame.render_widget(search, chunks[1]);

            // Show cursor
            frame.set_cursor_position((
                chunks[1].x + 2 + self.search_cursor as u16,
                chunks[1].y + 1,
            ));
        }

        // Content
        if self.search_mode && !self.search_query.is_empty() {
            self.render_search_results(frame, content_area);
        } else {
            match self.tab() {
                HelpTab::Global => self.render_global_help(frame, content_area),
                HelpTab::Overlays => self.render_overlay_help(frame, content_area),
                HelpTab::Commands => self.render_command_help(frame, content_area),
                HelpTab::Shortcuts => self.render_shortcut_help(frame, content_area),
            }
        }

        // Help footer
        let help_text = if self.search_mode {
            "type to filter · ↑↓: navigate · enter: exit search · esc: clear search"
        } else {
            "↑↓: navigate · pgup/pgdn: page · home/end: top/bottom · /: search · tab: next tab · c: copy · esc: close"
        };
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(help, chunks[chunks.len() - 1]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/help", "Open help overlay"),
            ("Tab", "Switch tabs (Global/Overlays/Commands/Shortcuts)"),
            ("↑/↓", "Navigate"),
            ("PgUp/PgDn", "Page up/down"),
            ("Home/End", "First/last"),
            ("/", "Search/filter"),
            ("Ctrl+C", "Copy binding"),
            ("Esc", "Close overlay"),
        ]
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(help: &mut HelpOverlay) -> String {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let app = App::new("s", false);
        terminal
            .draw(|frame| help.render(frame, frame.area(), &app))
            .expect("draw must not fail");
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
    fn every_tab_draws_its_bindings_and_not_just_its_title() {
        let mut help = HelpOverlay::new();
        help.show();

        // The list used to be drawn into the collapsed search-bar chunk, so the
        // overlay showed its tabs over an empty panel.
        let text = screen(&mut help);
        assert!(text.contains("previous/next input"), "{text}");

        help.handle_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut App::new("s", false),
        );
        let text = screen(&mut help);
        assert!(text.contains("Search Overlay"), "{text}");

        help.handle_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut App::new("s", false),
        );
        let text = screen(&mut help);
        assert!(text.contains("/vault remember"), "{text}");

        help.handle_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut App::new("s", false),
        );
        let text = screen(&mut help);
        assert!(text.contains("Ctrl+L"), "{text}");
    }

    #[test]
    fn escape_closes_the_overlay() {
        let mut help = HelpOverlay::new();
        help.show();
        let action = help.handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut App::new("s", false),
        );
        assert!(matches!(action, OverlayAction::Close));
    }
}
