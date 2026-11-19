//! Full-screen, read-only project browsing over the authorized `projects.list`
//! result. Assignment is a separate, explicit action (`u`).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use serde_json::Value;

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

#[derive(Debug, Clone)]
struct Project {
    id: String,
    name: String,
    visibility: Option<String>,
    access: Option<String>,
    repos: Option<i64>,
    members: Option<i64>,
}

impl Project {
    fn from_value(value: &Value) -> Option<Self> {
        let id = value.get("id")?.as_str()?.to_owned();
        let name = value.get("name")?.as_str()?.to_owned();
        Some(Self {
            id,
            name,
            visibility: value
                .get("visibility")
                .and_then(Value::as_str)
                .map(str::to_owned),
            access: value
                .get("access_level")
                .and_then(Value::as_str)
                .map(str::to_owned),
            repos: value.get("repos").and_then(Value::as_i64),
            members: value.get("member_count").and_then(Value::as_i64),
        })
    }
}

/// A full-screen list/detail view constructed from exactly the list command's
/// already-authorized response. It performs no fetch and no assignment on open.
#[derive(Debug)]
pub(crate) struct ProjectBrowserOverlay {
    visible: bool,
    projects: Vec<Project>,
    filter: String,
    selected: usize,
    scroll: usize,
    detail: bool,
}

impl ProjectBrowserOverlay {
    pub(crate) fn from_answer(data: &Value) -> Option<Self> {
        let projects = data
            .get("projects")?
            .as_array()?
            .iter()
            .filter_map(Project::from_value)
            .collect::<Vec<_>>();
        (!projects.is_empty()).then_some(Self {
            visible: true,
            projects,
            filter: String::new(),
            selected: 0,
            scroll: 0,
            detail: false,
        })
    }

    fn matches(&self) -> Vec<usize> {
        let query = self.filter.to_lowercase();
        self.projects
            .iter()
            .enumerate()
            .filter(|(_, project)| {
                query.is_empty()
                    || project.name.to_lowercase().contains(&query)
                    || project.id.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_project(&self) -> Option<&Project> {
        self.matches()
            .get(self.selected)
            .and_then(|index| self.projects.get(*index))
    }

    fn select(&mut self, next: usize, viewport: usize) {
        let count = self.matches().len();
        self.selected = next.min(count.saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if viewport > 0 && self.selected >= self.scroll + viewport {
            self.scroll = self.selected + 1 - viewport;
        }
    }

    fn use_selected(&self, app: &mut App) -> OverlayAction {
        if let Some(project) = self.selected_project() {
            app.input = format!("/projects use {}", project.id);
            app.cursor = app.input.chars().count();
            app.pending_submit = true;
            OverlayAction::Close
        } else {
            OverlayAction::None
        }
    }

    fn detail_lines(project: &Project) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(Span::styled(
            project.name.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))];
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("id  ", Style::default().fg(Color::DarkGray)),
            Span::styled(project.id.clone(), Style::default().fg(Color::Gray)),
        ]));
        for (label, value) in [
            ("visibility", project.visibility.clone()),
            ("your access", project.access.clone()),
            ("repositories", project.repos.map(|n| n.to_string())),
            ("members", project.members.map(|n| n.to_string())),
        ] {
            if let Some(value) = value {
                lines.push(Line::from(vec![
                    Span::styled(format!("{label}  "), Style::default().fg(Color::DarkGray)),
                    Span::styled(value, Style::default().fg(Color::Gray)),
                ]));
            }
        }
        lines
    }
}

impl Overlay for ProjectBrowserOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Popover
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.detail {
            return match key.code {
                KeyCode::Esc | KeyCode::Backspace => {
                    self.detail = false;
                    OverlayAction::None
                }
                KeyCode::Enter if ctrl => self.use_selected(app),
                _ => OverlayAction::None,
            };
        }

        match key.code {
            KeyCode::Esc => OverlayAction::Close,
            KeyCode::Enter if ctrl => self.use_selected(app),
            KeyCode::Enter => {
                if self.selected_project().is_some() {
                    self.detail = true;
                }
                OverlayAction::None
            }
            KeyCode::Up | KeyCode::Char('k') if !ctrl && self.filter.is_empty() => {
                self.select(self.selected.saturating_sub(1), 0);
                OverlayAction::None
            }
            KeyCode::Down | KeyCode::Char('j') if !ctrl && self.filter.is_empty() => {
                self.select(self.selected.saturating_add(1), 0);
                OverlayAction::None
            }
            KeyCode::PageUp if self.filter.is_empty() => {
                self.select(self.selected.saturating_sub(10), 0);
                OverlayAction::None
            }
            KeyCode::PageDown if self.filter.is_empty() => {
                self.select(self.selected.saturating_add(10), 0);
                OverlayAction::None
            }
            KeyCode::Home if self.filter.is_empty() => {
                self.selected = 0;
                self.scroll = 0;
                OverlayAction::None
            }
            KeyCode::End if self.filter.is_empty() => {
                self.selected = self.matches().len().saturating_sub(1);
                OverlayAction::None
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.selected = 0;
                self.scroll = 0;
                OverlayAction::None
            }
            KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.filter.push(ch);
                self.selected = 0;
                self.scroll = 0;
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible || area.width < 24 || area.height < 7 {
            return;
        }
        frame.render_widget(Clear, area);
        frame.render_widget(Block::default().borders(Borders::NONE), area);
        let title = if self.detail { "project" } else { "projects" };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("memnoth / ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    title,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("   {} listed", self.projects.len()),
                    Style::default().fg(Color::DarkGray),
                ),
            ])),
            Rect::new(area.x + 2, area.y, area.width.saturating_sub(4), 1),
        );

        let footer_y = area.bottom().saturating_sub(1);
        let footer = if self.detail {
            "Ctrl+Enter use for this chat · Esc projects"
        } else {
            "type search · ↑↓ move · Enter inspect · Ctrl+Enter use · Esc chat"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(footer, Style::default().fg(Color::DarkGray))),
            Rect::new(area.x + 2, footer_y, area.width.saturating_sub(4), 1),
        );
        let body = Rect::new(
            area.x + 2,
            area.y + 2,
            area.width.saturating_sub(4),
            area.height.saturating_sub(4),
        );

        if self.detail {
            if let Some(project) = self.selected_project() {
                frame.render_widget(
                    Paragraph::new(Self::detail_lines(project)).wrap(Wrap { trim: true }),
                    body,
                );
            }
            return;
        }

        let query_line = Line::from(vec![
            Span::styled("/ ", Style::default().fg(Color::Cyan)),
            Span::styled(
                if self.filter.is_empty() {
                    "filter projects…"
                } else {
                    &self.filter
                },
                if self.filter.is_empty() {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
        ]);
        frame.render_widget(
            Paragraph::new(query_line),
            Rect::new(body.x, body.y, body.width, 1),
        );
        let list_area = Rect::new(
            body.x,
            body.y + 2,
            body.width,
            body.height.saturating_sub(2),
        );
        let matches = self.matches();
        let visible = list_area.height as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if visible > 0 && self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
        if matches.is_empty() {
            frame.render_widget(
                Paragraph::new(if self.filter.is_empty() {
                    "No project rows were returned."
                } else {
                    "No projects match this filter. Clear the search with Backspace."
                })
                .style(Style::default().fg(Color::DarkGray)),
                list_area,
            );
            return;
        }
        let lines = matches
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(visible)
            .map(|(visible_index, index)| {
                let project = &self.projects[*index];
                let selected = visible_index + self.scroll == self.selected;
                let mut facts = Vec::new();
                if let Some(visibility) = &project.visibility {
                    facts.push(visibility.clone());
                }
                if let Some(access) = &project.access {
                    facts.push(access.clone());
                }
                if let Some(repos) = project.repos {
                    facts.push(format!("{repos} repos"));
                }
                let detail = facts.join(" · ");
                let mut spans = vec![
                    Span::styled(
                        if selected { "› " } else { "  " },
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        project.name.clone(),
                        if selected {
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::Gray)
                        },
                    ),
                ];
                if !detail.is_empty() {
                    spans.push(Span::styled(
                        format!("   {detail}"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                Line::from(spans)
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), list_area);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("type", "filter projects by name or id"),
            (
                "enter",
                "inspect the selected project; does not change assignment",
            ),
            ("ctrl+enter", "use the selected project for this chat"),
            ("esc", "return to chat"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    fn overlay() -> ProjectBrowserOverlay {
        ProjectBrowserOverlay::from_answer(&json!({"projects": [
            {"id":"p-1", "name":"Alpha", "visibility":"org", "access_level":"read", "repos":2, "member_count":5},
            {"id":"p-2", "name":"Beta"}
        ]})).expect("project list")
    }

    #[test]
    fn enter_inspects_without_changing_chat_assignment_or_draft() {
        let mut app = App::new("s", false);
        app.input = "keep this draft".into();
        app.cursor = 5;
        let mut overlay = overlay();
        assert_eq!(
            overlay.handle_key(KeyEvent::from(KeyCode::Enter), &mut app),
            OverlayAction::None
        );
        assert!(overlay.detail);
        assert_eq!(app.input, "keep this draft");
        assert_eq!(app.cursor, 5);
        assert!(!app.pending_submit);
    }

    #[test]
    fn detail_only_shows_returned_facts_and_explicit_use_runs_existing_command() {
        let mut app = App::new("s", false);
        let mut overlay = overlay();
        overlay.handle_key(KeyEvent::from(KeyCode::Down), &mut app);
        overlay.handle_key(KeyEvent::from(KeyCode::Enter), &mut app);
        let detail = ProjectBrowserOverlay::detail_lines(overlay.selected_project().unwrap())
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(detail.contains("Beta"), "{detail}");
        assert!(detail.contains("id  p-2"), "{detail}");
        assert!(
            !detail.contains("visibility"),
            "absent fields stay absent: {detail}"
        );
        assert!(
            !detail.contains("repositories"),
            "absent fields stay absent: {detail}"
        );
        assert_eq!(
            overlay.handle_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
                &mut app
            ),
            OverlayAction::Close
        );
        assert_eq!(app.input, "/projects use p-2");
        assert!(app.pending_submit);
    }

    #[test]
    fn full_screen_browser_has_no_dialog_border_and_preserves_row_facts() {
        let mut overlay = overlay();
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|frame| overlay.render(frame, frame.area(), &App::new("s", false)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("projects"), "{text}");
        assert!(text.contains("Alpha"), "{text}");
        assert!(text.contains("org · read · 2 repos"), "{text}");
        assert!(!text.contains('┌'), "no mini-window border: {text}");
    }
}
