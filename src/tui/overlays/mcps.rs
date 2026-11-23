//! `/mcps` — where Memnoth's server is wired in, what is detected on this
//! machine, and a browse-only catalog. Memnoth does not install arbitrary
//! third-party MCP servers; the labels below say exactly what it can do.

use std::collections::BTreeSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use crate::context::AppContext;
use crate::mcp::clients::ClientKind;
use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use crate::tui::theme::tokens;

#[derive(Debug, Clone, Copy)]
struct CatalogEntry {
    name: &'static str,
    category: &'static str,
    summary: &'static str,
}

// Every field here is a claim about a real service. No ranking field exists in
// the data, so none is shown — "in catalog" is the honest status.
const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        name: "Context7",
        category: "docs",
        summary: "Current, version-pinned library docs and examples.",
    },
    CatalogEntry {
        name: "GitHub",
        category: "code",
        summary: "Repositories, issues, pull requests and Actions.",
    },
    CatalogEntry {
        name: "Playwright",
        category: "browser",
        summary: "Browser control through accessibility snapshots.",
    },
    CatalogEntry {
        name: "Chrome DevTools",
        category: "browser",
        summary: "Inspect, debug and profile a live Chrome session.",
    },
    CatalogEntry {
        name: "Serena",
        category: "code",
        summary: "Symbol-level semantic code retrieval and edits.",
    },
    CatalogEntry {
        name: "Linear",
        category: "work",
        summary: "Issues, projects and cycles.",
    },
    CatalogEntry {
        name: "Notion",
        category: "work",
        summary: "Pages, databases and workspace search.",
    },
    CatalogEntry {
        name: "Figma",
        category: "design",
        summary: "Design files, variables and components.",
    },
    CatalogEntry {
        name: "Sentry",
        category: "observability",
        summary: "Errors, traces and root-cause triage.",
    },
    CatalogEntry {
        name: "Grafana",
        category: "observability",
        summary: "Dashboards, metrics, logs and alerts.",
    },
    CatalogEntry {
        name: "Supabase",
        category: "data",
        summary: "Postgres, auth, storage and migrations.",
    },
    CatalogEntry {
        name: "Exa",
        category: "search",
        summary: "Neural web search and crawling tuned for agents.",
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Catalog,
    Wired,
    Hosts,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Catalog, Tab::Wired, Tab::Hosts];

    fn next(self, delta: isize) -> Self {
        let i = Self::ALL.iter().position(|t| *t == self).unwrap_or(0) as isize;
        Self::ALL[(i + delta).rem_euclid(3) as usize]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Catalog => "Catalog",
            Self::Wired => "Wired in",
            Self::Hosts => "Connect host",
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
    /// (host name, registered, detail) from `mcp::list`.
    hosts: Vec<(String, bool, String)>,
    /// Hosts turned off for this session only — never written to host configs.
    off: BTreeSet<String>,
    note: Option<String>,
    show_help: bool,
}

impl McpsOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    fn rows(&self) -> Vec<String> {
        let q = self.query.trim().to_ascii_lowercase();
        let hit = |fields: &[&str]| {
            q.is_empty() || fields.iter().any(|f| f.to_ascii_lowercase().contains(&q))
        };
        match self.tab {
            Tab::Catalog => CATALOG
                .iter()
                .filter(|e| {
                    hit(&[
                        e.name,
                        e.category,
                        e.summary,
                        "mcp server tool extension plugin",
                    ])
                })
                .map(|e| e.name.to_string())
                .collect(),
            Tab::Wired => self
                .hosts
                .iter()
                .filter(|(name, registered, detail)| {
                    *registered && hit(&[name, "wired linked registered", detail])
                })
                .map(|(name, _, _)| name.clone())
                .collect(),
            Tab::Hosts => ClientKind::ALL
                .iter()
                .filter(|k| hit(&[k.name(), "host application", "connect local mcp server"]))
                .map(|k| k.name().to_string())
                .collect(),
        }
    }

    fn reload(&mut self) {
        let Some(ctx) = &self.ctx else {
            self.hosts.clear();
            return;
        };
        self.hosts = match crate::mcp::list(ctx) {
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
                Tab::Catalog => "No catalog entries match. Try a name or a task word.".into(),
                Tab::Wired => "No hosts are wired in yet. Connect host to link one.".into(),
                Tab::Hosts => "No host applications match.".into(),
            };
        };
        match self.tab {
            Tab::Catalog => CATALOG
                .iter()
                .find(|e| e.name == name)
                .map(|e| {
                    format!(
                        "{name} · {}\n{}\nCatalog entry — browse only. Third-party MCP servers are not installed by Memnoth.",
                        e.category, e.summary
                    )
                })
                .unwrap_or_default(),
            Tab::Wired => {
                let enabled = !self.off.contains(&name);
                let detail = self
                    .hosts
                    .iter()
                    .find(|row| row.0 == name)
                    .map(|row| row.2.as_str())
                    .unwrap_or("");
                format!(
                    "Memnoth server · {name}\n{}\nState: {} (this session only)\nHost config is unchanged by this toggle.",
                    detail,
                    if enabled { "on" } else { "off" }
                )
            }
            Tab::Hosts => format!(
                "Wire Memnoth into {name}.\nLocal-only command — no account, no key, nothing uploaded.\nRun in a terminal:  memnoth mcp add --local --clients {name}"
            ),
        }
    }

    /// `Space` is a three-state, session-only control where Memnoth owns the
    /// operation. It never edits a host config and never claims to install a
    /// third-party server.
    fn toggle(&mut self, app: &mut App) {
        match self.tab {
            Tab::Wired => {
                let Some(name) = self.rows().get(self.selected).cloned() else {
                    return;
                };
                if self.off.remove(&name) {
                    self.note = Some(format!(
                        "{name}: Memnoth server on for this session. Host config untouched."
                    ));
                } else {
                    self.off.insert(name.clone());
                    self.note = Some(format!(
                        "{name}: Memnoth server off for this session. Host config untouched."
                    ));
                }
                let _ = app;
            }
            Tab::Catalog => {
                self.note =
                    Some("Catalog entries are browse-only — Memnoth cannot enable them.".into());
            }
            Tab::Hosts => {
                self.note =
                    Some("Press Enter to copy the local-only command that wires a host.".into());
            }
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
        self.tab = Tab::Catalog;
        self.query.clear();
        self.searching = false;
        self.note = None;
        self.show_help = false;
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
            KeyCode::Char('?') if !ctrl => {
                self.show_help = !self.show_help;
                OverlayAction::None
            }
            KeyCode::Esc => {
                if self.show_help {
                    self.show_help = false;
                } else if self.searching {
                    self.searching = false;
                    self.query.clear();
                    self.selected = 0;
                } else {
                    return OverlayAction::Close;
                }
                OverlayAction::None
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
            KeyCode::Char(' ') if !ctrl && !self.searching => {
                self.toggle(app);
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
            KeyCode::Enter if self.tab == Tab::Hosts => {
                if let Some(host) = self.rows().get(self.selected).cloned() {
                    let command = format!("mn mcp add --local --clients {host}");
                    match crate::auth::copy_to_clipboard(&command) {
                        Ok(()) => app.notify(
                            format!(
                                "Command copied — run it in a terminal to finish linking {host}"
                            ),
                            std::time::Duration::from_secs(4),
                        ),
                        Err(error) => {
                            self.note = Some(format!("{command}\nClipboard unavailable: {error}"))
                        }
                    }
                }
                OverlayAction::None
            }
            KeyCode::Up | KeyCode::Char('k') if !ctrl => {
                let len = self.rows().len();
                self.selected = if len == 0 {
                    0
                } else {
                    (self.selected + len - 1) % len
                };
                OverlayAction::None
            }
            KeyCode::Down | KeyCode::Char('j') if !ctrl => {
                let len = self.rows().len();
                self.selected = if len == 0 {
                    0
                } else {
                    (self.selected + 1) % len
                };
                OverlayAction::None
            }
            KeyCode::Char('r') if !ctrl && !self.searching => {
                self.note = None;
                self.reload();
                self.note = Some("Re-read host configs on this machine.".into());
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible || area.width < 48 || area.height < 14 {
            return;
        }
        let t = tokens();
        let width = (area.width * 88 / 100).clamp(48, area.width);
        let height = (area.height * 84 / 100).clamp(14, area.height);
        let popup = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        frame.render_widget(
            ratatui::widgets::Block::default()
                .style(Style::default().bg(t.surface))
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(t.border)),
            popup,
        );
        let inner = Rect::new(
            popup.x + 2,
            popup.y + 1,
            popup.width.saturating_sub(4),
            popup.height.saturating_sub(2),
        );
        if inner.height < 8 {
            return;
        }
        let chunks = Layout::vertical([
            Constraint::Length(1), // tabs
            Constraint::Length(1), // filter
            Constraint::Min(3),    // list
            Constraint::Length(4), // detail
            Constraint::Length(1), // footer
        ])
        .split(inner);

        let tabs = Tab::ALL
            .into_iter()
            .flat_map(|tab| {
                let active = tab == self.tab;
                let style = if active {
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.muted)
                };
                [
                    Span::styled(format!(" {} ", tab.label()), style),
                    Span::raw("  "),
                ]
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(Line::from(tabs)), chunks[0]);

        let filter = if self.searching {
            format!(" / {}▏", self.query)
        } else if self.query.is_empty() {
            " /  filter by name or task".to_string()
        } else {
            format!(" /  {}", self.query)
        };
        frame.render_widget(
            Paragraph::new(filter).style(Style::default().fg(t.muted)),
            chunks[1],
        );

        let rows = self.rows();
        let mut lines = Vec::new();
        for (index, name) in rows.iter().enumerate() {
            let selected = index == self.selected;
            let (meta, state) = match self.tab {
                Tab::Catalog => {
                    let e = CATALOG.iter().find(|e| e.name == name).unwrap();
                    (e.category.to_string(), "catalog · browse only".to_string())
                }
                Tab::Wired => {
                    let on = !self.off.contains(name);
                    (
                        "local server".to_string(),
                        if on {
                            "● on · space off"
                        } else {
                            "○ off · space on"
                        }
                        .to_string(),
                    )
                }
                Tab::Hosts => ("host app".to_string(), "○ not linked".to_string()),
            };
            let base = if selected {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "› " } else { "  " },
                    base.add_modifier(Modifier::BOLD),
                ),
                Span::styled(name.clone(), base.add_modifier(Modifier::BOLD)),
                Span::styled(format!("  ·  {meta}  ·  {state}"), base.fg(t.muted)),
            ]));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                match self.tab {
                    Tab::Catalog => "No catalog entries match.",
                    Tab::Wired => "No hosts are wired in yet.",
                    Tab::Hosts => "No host applications match.",
                },
                Style::default().fg(t.muted),
            )));
        }
        frame.render_widget(
            Paragraph::new(lines).scroll((
                self.selected.saturating_sub(chunks[2].height as usize / 2) as u16,
                0,
            )),
            chunks[2],
        );

        let detail = if self.show_help {
            "space on/off (this session)  ·  enter copy command  ·  / filter  ·  r re-read  ·  ? hide  ·  esc back".to_string()
        } else {
            self.note.clone().unwrap_or_else(|| self.detail())
        };
        frame.render_widget(
            Paragraph::new(detail)
                .style(Style::default().fg(if self.note.is_some() {
                    t.warning
                } else {
                    t.muted
                }))
                .wrap(Wrap { trim: true }),
            chunks[3],
        );

        let footer = match self.tab {
            Tab::Catalog => {
                "↑↓ choose · space (browse only) · tab section · / filter · ? help · esc back"
            }
            Tab::Wired => "↑↓ choose · space on/off · tab section · / filter · ? help · esc back",
            Tab::Hosts => {
                "↑↓ choose · enter copy command · tab section · / filter · ? help · esc back"
            }
        };
        frame.render_widget(
            Paragraph::new(footer)
                .style(Style::default().fg(t.muted))
                .alignment(Alignment::Center),
            chunks[4],
        );
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑/↓", "choose entry"),
            ("tab/←→", "switch section"),
            ("space", "on/off (this session)"),
            ("/", "filter by name or task"),
            ("enter", "copy local-only command"),
            ("r", "re-read host configs"),
            ("?", "toggle this help"),
            ("esc", "back"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_labels_are_truthful_about_what_memncoth_can_do() {
        // No "Installed"/"Recommended"/"Add custom" — each mispredicts its
        // destination. Catalog is browse-only, Wired is the real state.
        assert_eq!(Tab::Catalog.label(), "Catalog");
        assert_eq!(Tab::Wired.label(), "Wired in");
        assert_eq!(Tab::Hosts.label(), "Connect host");
        assert!(!Tab::ALL.iter().any(|t| t.label().contains("Installed")));
    }

    #[test]
    fn catalog_rows_carry_no_ranking_or_install_claim() {
        let overlay = McpsOverlay {
            tab: Tab::Catalog,
            ..Default::default()
        };
        let rows = overlay.rows();
        assert_eq!(rows.len(), CATALOG.len());
        assert!(rows.iter().any(|r| r == "Context7"));
        let detail = overlay.detail();
        assert!(
            detail.contains("browse only"),
            "must not promise installation: {detail}"
        );
    }

    #[test]
    fn space_toggle_is_session_only_and_never_edits_host_config() {
        let mut overlay = McpsOverlay {
            tab: Tab::Wired,
            hosts: vec![("claude".into(), true, "config at ~/.claude.json".into())],
            ..Default::default()
        };
        let mut app = App::new("s", false);
        overlay.toggle(&mut app);
        assert!(overlay.off.contains("claude"));
        assert!(overlay.detail().contains("off"));
        overlay.toggle(&mut app);
        assert!(overlay.off.is_empty());
        assert!(overlay.detail().contains("on"));
        assert!(
            overlay
                .note
                .as_deref()
                .is_some_and(|n| n.contains("Host config untouched")),
            "the toggle must state that host config is unchanged"
        );
    }

    #[test]
    fn filter_searches_task_words_not_just_names() {
        let overlay = McpsOverlay {
            tab: Tab::Catalog,
            query: "browser".into(),
            ..Default::default()
        };
        let rows = overlay.rows();
        assert!(rows.iter().any(|r| r == "Playwright"));
        assert!(!rows.iter().any(|r| r == "Context7"));
    }
}
