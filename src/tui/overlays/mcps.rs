//! `/mcps`: discover MCP services and see where Memnoth is registered.
//!
//! The recommended list is a discovery catalog, not an installer. Memnoth's
//! current MCP client writes its own server into supported host applications;
//! it does not launch arbitrary third-party MCP servers inside the agent.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::context::AppContext;
use crate::mcp::clients::ClientKind;
use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

#[derive(Debug, Clone, Copy)]
struct Recommendation {
    name: &'static str,
    category: &'static str,
    summary: &'static str,
}

const PICKS: &[Recommendation] = &[
    Recommendation {
        name: "GitHub",
        category: "CODE",
        summary: "Work with repositories, issues, and pull requests.",
    },
    Recommendation {
        name: "Context7",
        category: "CODE",
        summary: "Look up current library documentation while coding.",
    },
    Recommendation {
        name: "Linear",
        category: "WORK",
        summary: "Find and update projects, issues, and cycles.",
    },
    Recommendation {
        name: "Notion",
        category: "WORK",
        summary: "Search and work with pages in a Notion workspace.",
    },
    Recommendation {
        name: "Figma",
        category: "DESIGN",
        summary: "Explore design files and their metadata.",
    },
    Recommendation {
        name: "Datadog",
        category: "OBSERVABILITY",
        summary: "Query logs, metrics, and service context.",
    },
    Recommendation {
        name: "Grafana",
        category: "OBSERVABILITY",
        summary: "Explore dashboards and observability data.",
    },
    Recommendation {
        name: "Composio",
        category: "AUTOMATION",
        summary: "Connect tools and actions across external services.",
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Recommended,
    Installed,
    Add,
}

impl Tab {
    fn next(self, delta: isize) -> Self {
        let value = (self as isize + delta).rem_euclid(3);
        match value {
            0 => Self::Recommended,
            1 => Self::Installed,
            _ => Self::Add,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Recommended => "Recommended",
            Self::Installed => "Installed",
            Self::Add => "Add custom",
        }
    }
}

#[derive(Debug, Default)]
pub struct McpsOverlay {
    visible: bool,
    tab: Tab,
    selected: usize,
    query: String,
    searching: bool,
    ctx: Option<AppContext>,
    installed: Vec<(String, bool, String)>,
    note: Option<String>,
}

impl McpsOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    fn rows(&self) -> Vec<String> {
        match self.tab {
            Tab::Recommended => PICKS
                .iter()
                .filter(|item| self.matches(item.name, item.category, item.summary))
                .map(|item| item.name.to_string())
                .collect(),
            Tab::Installed => self
                .installed
                .iter()
                .filter(|(name, registered, detail)| {
                    *registered && self.matches(name, "registered", detail)
                })
                .map(|(name, _, _)| name.clone())
                .collect(),
            Tab::Add => ClientKind::ALL
                .iter()
                .filter(|kind| {
                    self.matches(
                        kind.name(),
                        "host application",
                        "connect Memnoth's MCP server",
                    )
                })
                .map(|kind| kind.name().to_string())
                .collect(),
        }
    }

    fn matches(&self, name: &str, category: &str, summary: &str) -> bool {
        let q = self.query.trim();
        q.is_empty()
            || format!("{name} {category} {summary}")
                .to_lowercase()
                .contains(&q.to_lowercase())
    }

    fn reload(&mut self) {
        let Some(ctx) = &self.ctx else {
            self.installed.clear();
            return;
        };
        self.installed = match crate::mcp::list(ctx) {
            Ok(report) => report
                .clients
                .into_iter()
                .map(|row| (row.client, row.registered, row.detail))
                .collect(),
            Err(error) => {
                self.note = Some(format!("could not read host configs: {error:#}"));
                Vec::new()
            }
        };
        self.selected = self.selected.min(self.rows().len().saturating_sub(1));
    }

    fn detail(&self) -> String {
        let Some(name) = self.rows().get(self.selected).cloned() else {
            return match self.tab {
                Tab::Recommended => "No recommendations match this search.".into(),
                Tab::Installed => {
                    "No Memnoth registrations found. Use Add custom to connect a supported host."
                        .into()
                }
                Tab::Add => "No supported hosts match this search.".into(),
            };
        };
        match self.tab {
            Tab::Recommended => PICKS.iter().find(|item| item.name == name).map(|item| format!("{}  ·  {}\n{}\nDiscovery only: third-party MCP installation in Memnoth is not supported yet.", item.category, item.name, item.summary)).unwrap_or_default(),
            Tab::Installed => self.installed.iter().find(|row| row.0 == name).map(|row| format!("Memnoth server  ·  {}\n{}", if row.1 { "registered" } else { "not registered" }, row.2)).unwrap_or_default(),
            Tab::Add => format!("Connect Memnoth's local MCP server to {name}.\nRun in a terminal:  memnoth mcp add --local --clients {name}"),
        }
    }
}

impl Overlay for McpsOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Mcps
    }
    fn is_visible(&self) -> bool {
        self.visible
    }
    fn show(&mut self) {
        self.visible = true;
        self.selected = 0;
        self.tab = Tab::Recommended;
        self.note = None;
    }
    fn on_open(&mut self, app: &mut App) {
        self.ctx = app.ctx.clone();
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
            KeyCode::Esc => {
                if self.searching {
                    self.searching = false;
                    self.query.clear();
                    self.selected = 0;
                    OverlayAction::None
                } else {
                    OverlayAction::Close
                }
            }
            KeyCode::Tab | KeyCode::Right => {
                self.tab = self.tab.next(1);
                self.selected = 0;
                self.note = None;
                OverlayAction::None
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.tab = self.tab.next(-1);
                self.selected = 0;
                self.note = None;
                OverlayAction::None
            }
            KeyCode::Char('/') if !ctrl && !self.searching => {
                self.searching = true;
                self.query.clear();
                self.selected = 0;
                OverlayAction::None
            }
            KeyCode::Char(ch) if self.searching && !ctrl => {
                self.query.push(ch);
                self.selected = 0;
                OverlayAction::None
            }
            KeyCode::Backspace if self.searching => {
                self.query.pop();
                self.selected = 0;
                OverlayAction::None
            }
            KeyCode::Enter if self.searching => {
                self.searching = false;
                OverlayAction::None
            }
            KeyCode::Enter if self.tab == Tab::Add => {
                if let Some(client) = self.rows().get(self.selected) {
                    let command = format!("memnoth mcp add --local --clients {client}");
                    match crate::auth::copy_to_clipboard(&command) {
                        Ok(()) => app.notify(
                            format!("Copied setup command for {client}"),
                            std::time::Duration::from_secs(3),
                        ),
                        Err(error) => {
                            self.note = Some(format!("{command}\nClipboard unavailable: {error}"))
                        }
                    }
                }
                OverlayAction::None
            }
            KeyCode::Up | KeyCode::Char('k') if !ctrl => {
                self.selected = self.selected.saturating_sub(1);
                OverlayAction::None
            }
            KeyCode::Down | KeyCode::Char('j') if !ctrl => {
                self.selected = (self.selected + 1).min(self.rows().len().saturating_sub(1));
                OverlayAction::None
            }
            KeyCode::Char('r') if !ctrl && !self.searching => {
                self.note = None;
                self.reload();
                self.note = Some("Re-read MCP host registrations.".into());
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible || area.width < 44 || area.height < 12 {
            return;
        }
        let width = (area.width * 84 / 100).clamp(44, area.width);
        let height = (area.height * 82 / 100).clamp(12, area.height);
        let popup = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .title(" MCPs ")
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        if inner.height < 7 {
            return;
        }
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Min(3),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ]
                .as_ref(),
            )
            .split(inner);
        let tabs = [Tab::Recommended, Tab::Installed, Tab::Add]
            .into_iter()
            .flat_map(|tab| {
                let active = tab == self.tab;
                [
                    Span::styled(
                        format!(" {} ", tab.label()),
                        if active {
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        },
                    ),
                    Span::raw("  "),
                ]
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(Line::from(tabs)), chunks[0]);
        let search = if self.searching {
            format!("/ {}▏", self.query)
        } else if self.query.is_empty() {
            "  /  Search MCPs".into()
        } else {
            format!("  /  {}", self.query)
        };
        frame.render_widget(
            Paragraph::new(search).style(Style::default().fg(Color::Gray)),
            chunks[1],
        );

        let rows = self.rows();
        let mut lines = Vec::new();
        for (index, name) in rows.iter().enumerate() {
            let selected = index == self.selected;
            let (meta, status) = match self.tab {
                Tab::Recommended => {
                    let item = PICKS.iter().find(|item| item.name == name).unwrap();
                    (item.category, "recommended")
                }
                Tab::Installed => {
                    let row = self.installed.iter().find(|row| &row.0 == name).unwrap();
                    (
                        "Memnoth server",
                        if row.1 { "registered" } else { "not connected" },
                    )
                }
                Tab::Add => ("supported host", "connect Memnoth"),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "› " } else { "  " },
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    name.clone(),
                    if selected {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
                Span::styled(
                    format!("  ·  {meta}  ·  {status}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "  Nothing found",
                Style::default().fg(Color::DarkGray),
            )));
        }
        frame.render_widget(
            Paragraph::new(lines).scroll((
                self.selected.saturating_sub(chunks[2].height as usize / 2) as u16,
                0,
            )),
            chunks[2],
        );
        let detail = self.note.clone().unwrap_or_else(|| self.detail());
        frame.render_widget(
            Paragraph::new(detail)
                .style(Style::default().fg(if self.note.is_some() {
                    Color::Yellow
                } else {
                    Color::Gray
                }))
                .wrap(Wrap { trim: true }),
            chunks[3],
        );
        let footer = match self.tab {
            Tab::Recommended => "↑↓ choose  ·  Tab section  ·  / search  ·  Esc close",
            Tab::Installed => "↑↓ choose  ·  r refresh  ·  Tab section  ·  Esc close",
            Tab::Add => "↑↓ choose  ·  Enter setup command  ·  Tab section  ·  Esc close",
        };
        frame.render_widget(
            Paragraph::new(footer)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center),
            chunks[4],
        );
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑/↓", "choose MCP"),
            ("tab", "switch section"),
            ("/", "search"),
            ("r", "refresh registrations"),
            ("esc", "close"),
        ]
    }
}
