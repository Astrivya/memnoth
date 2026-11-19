//! `/teams`: browse the current org's teams and inspect read-only details.
//!
//! Opening a row fetches its authorized detail asynchronously; it does not
//! change team, project, or organization assignment.
//!
//! The list is a request, so it is read on a worker thread and drained in
//! `on_tick`, exactly like the model browser. A synchronous fetch here freezes
//! the frame for the length of the timeout, which is how `/stats` came to look
//! like a hang.
//!
//! Keys:
//!   type       filter by name or id
//!   ↑/↓        move            pgup/pgdn, home/end  travel
//!   enter      inspect the selected team's members and projects
//!   ctrl+r     read the list again
//!   esc        close

use std::sync::mpsc::{self, Receiver};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

/// What the worker thread answers: the team list, and whether more pages exist.
type TeamsReply = Result<(Vec<Team>, bool), String>;
type TeamDetailReply = Result<TeamDetail, String>;

use memnoth_cloud_client::{Team, TeamDetail};

use crate::context::AppContext;
use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// What is known about the org's teams at this instant.
#[derive(Debug)]
enum Loaded {
    /// The request is in flight.
    Loading,
    Ready(Vec<Team>),
    /// The reason it could not be read, as the client worded it.
    Failed(String),
}
#[derive(Debug)]
enum DetailLoaded {
    Loading,
    Ready(TeamDetail),
    Failed(String),
}

/// The picker.
#[derive(Debug)]
pub struct TeamsOverlay {
    visible: bool,
    /// The invocation's paths and credentials, copied from the app on open: the
    /// worker thread needs an owned context, and a context is cheap to clone.
    ctx: Option<AppContext>,
    loaded: Loaded,
    /// Where the worker's answer lands. `None` once it has arrived, so a frame
    /// cannot drain one reply twice.
    rx: Option<Receiver<TeamsReply>>,
    /// Whether the org is a demo tenant, which the header says out loud.
    demo: bool,
    /// What was typed to narrow the list; empty lists every team.
    filter: String,
    /// Index into the *filtered* list, never into the loaded one.
    selected: usize,
    /// First drawn line on screen, in a two-line-per-team coordinate system.
    scroll: usize,
    detail: Option<DetailLoaded>,
    detail_rx: Option<Receiver<TeamDetailReply>>,
    detail_scroll: u16,
}

impl TeamsOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            ctx: None,
            loaded: Loaded::Loading,
            rx: None,
            demo: false,
            filter: String::new(),
            selected: 0,
            scroll: 0,
            detail: None,
            detail_rx: None,
            detail_scroll: 0,
        }
    }

    /// Ask for the list on a thread, so opening the picker never waits on the
    /// network — not even for the first frame.
    fn reload(&mut self) {
        let Some(ctx) = self.ctx.clone() else {
            self.loaded = Loaded::Failed(
                "no workspace context — open this from a session, not a test".to_string(),
            );
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.loaded = Loaded::Loading;
        self.selected = 0;
        self.scroll = 0;
        std::thread::spawn(move || {
            let answer = match crate::cloud::signed_in_client(&ctx) {
                Ok(client) => client
                    .teams()
                    .map(|response| (response.teams, response.demo))
                    .map_err(|error| format!("{error:#}")),
                Err(error) => Err(format!("{error:#}")),
            };
            let _ = tx.send(answer);
        });
    }

    fn open_detail(&mut self, id: String) {
        let Some(ctx) = self.ctx.clone() else {
            self.detail = Some(DetailLoaded::Failed("no workspace context".into()));
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.detail_rx = Some(rx);
        self.detail = Some(DetailLoaded::Loading);
        self.detail_scroll = 0;
        std::thread::spawn(move || {
            let result = match crate::cloud::signed_in_client(&ctx) {
                Ok(client) => client.team(&id).map_err(|error| format!("{error:#}")),
                Err(error) => Err(format!("{error:#}")),
            };
            let _ = tx.send(result);
        });
    }

    /// The teams the filter leaves, in the order the server sent them.
    fn matches(&self) -> Vec<&Team> {
        let Loaded::Ready(teams) = &self.loaded else {
            return Vec::new();
        };
        let needle = self.filter.trim().to_lowercase();
        teams
            .iter()
            .filter(|team| {
                needle.is_empty()
                    || team.name.to_lowercase().contains(needle.as_str())
                    || team.id.to_lowercase().contains(needle.as_str())
            })
            .collect()
    }

    /// How many rows the filter leaves.
    fn total(&self) -> usize {
        self.matches().len()
    }

    /// How many teams the server sent, before the filter: the denominator in
    /// "3 of 7".
    fn loaded_count(&self) -> usize {
        match &self.loaded {
            Loaded::Ready(teams) => teams.len(),
            _ => 0,
        }
    }

    /// Keep the highlight inside the list the filter left.
    fn clamp(&mut self) {
        self.selected = self.selected.min(self.total().saturating_sub(1));
    }

    fn select_next(&mut self) {
        let last = self.total().saturating_sub(1);
        self.selected = (self.selected + 1).min(last);
    }

    fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn page(&mut self, down: bool, rows: usize) {
        for _ in 0..rows {
            if down {
                self.select_next();
            } else {
                self.select_previous();
            }
        }
    }

    /// One team as two lines: the name, then the facts every other command
    /// needs. The marker vocabulary is `src/teams.rs`'s, so the picker and the
    /// list it replaces cannot describe the same team two ways.
    ///
    /// The selection is a glyph as well as a colour: a screen reader, and a
    /// terminal with colour turned off, still say which row Enter will take.
    fn row_lines(team: &Team, selected: bool) -> Vec<Line<'static>> {
        let mut markers = Vec::new();
        if team.is_member {
            markers.push("yours");
        }
        if team.can_manage {
            markers.push("can manage");
        }
        let suffix = if markers.is_empty() {
            String::new()
        } else {
            format!(" · {}", markers.join(" · "))
        };

        let name_style = if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Green)
        };
        let glyph = if selected { "› " } else { "  " };

        vec![
            Line::from(vec![
                Span::styled(
                    glyph,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{}{suffix}", team.name), name_style),
            ]),
            Line::from(Span::styled(
                format!(
                    "    id {} · {} member(s) · {} project(s)",
                    team.id, team.member_count, team.project_count
                ),
                Style::default().fg(Color::DarkGray),
            )),
        ]
    }

    /// A line explaining a screen that has no rows, and therefore needs words.
    ///
    /// `None` when there is a list to draw.
    fn state_text(&self) -> Option<String> {
        match &self.loaded {
            Loaded::Loading => Some("reading the org's teams…".to_string()),
            Loaded::Failed(message) => Some(format!("{message}\n\nctrl+r to try again")),
            Loaded::Ready(teams) if teams.is_empty() => Some(format!(
                "No teams in this org yet.\n\nCreate one with /teams create <name>, or in the \
                 console:\n{}",
                crate::context::console_url("teams")
            )),
            Loaded::Ready(_) => None,
        }
    }

    fn detail_text(detail: &DetailLoaded) -> String {
        match detail {
            DetailLoaded::Loading => "Loading team detail…".to_string(),
            DetailLoaded::Failed(error) => format!("Team detail unavailable\n\n{error}"),
            DetailLoaded::Ready(detail) => {
                let mut lines = vec![
                    detail.team.name.clone(),
                    detail.team.id.clone(),
                    format!(
                        "{} member(s) · {} project(s)",
                        detail.team.member_count, detail.team.project_count
                    ),
                    format!(
                        "You are a member: {} · You can manage: {}",
                        detail.team.is_member, detail.can_manage
                    ),
                    String::new(),
                    "Members".to_string(),
                ];
                if detail.members.is_empty() {
                    lines.push("No member details returned.".into());
                } else {
                    for member in &detail.members {
                        lines.push(format!(
                            "{} · {} · org role {}",
                            if member.name.is_empty() {
                                &member.email
                            } else {
                                &member.name
                            },
                            member.email,
                            member.org_role
                        ));
                    }
                }
                lines.push(String::new());
                lines.push("Projects".to_string());
                if detail.projects.is_empty() {
                    lines.push("No project details returned.".into());
                } else {
                    for project in &detail.projects {
                        lines.push(format!("{} · {}", project.name, project.level));
                    }
                }
                lines.join("\n")
            }
        }
    }
}

impl Default for TeamsOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl Overlay for TeamsOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Teams
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.filter.clear();
        self.selected = 0;
        self.scroll = 0;
        self.detail = None;
        self.detail_rx = None;
        self.detail_scroll = 0;
    }

    fn on_open(&mut self, app: &mut App) {
        self.ctx = app.ctx.clone();
        self.reload();
    }

    /// The answer arrives here: the UI thread is the only one allowed to change
    /// what is drawn, so the worker sends and this reads.
    fn on_tick(&mut self, _app: &mut App) {
        if let Some(rx) = self.detail_rx.take() {
            match rx.try_recv() {
                Ok(Ok(detail)) => self.detail = Some(DetailLoaded::Ready(detail)),
                Ok(Err(error)) => self.detail = Some(DetailLoaded::Failed(error)),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.detail = Some(DetailLoaded::Failed("team detail did not arrive".into()))
                }
                Err(mpsc::TryRecvError::Empty) => self.detail_rx = Some(rx),
            }
        }
        let Some(rx) = self.rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok((teams, demo))) => {
                self.demo = demo;
                self.loaded = Loaded::Ready(teams);
                self.clamp();
            }
            Ok(Err(message)) => self.loaded = Loaded::Failed(message),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.loaded = Loaded::Failed(
                    "the team list did not arrive — press ctrl+r to try again".to_string(),
                );
            }
            Err(mpsc::TryRecvError::Empty) => self.rx = Some(rx),
        }
    }

    fn hide(&mut self) {
        self.visible = false;
        self.detail = None;
        self.detail_rx = None;
        self.detail_scroll = 0;
    }

    fn handle_key(&mut self, key: KeyEvent, _app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }
        // Detail is a distinct browsing state. Never let navigation or typed
        // characters mutate the hidden team list or leak into the chat prompt.
        if self.detail.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.detail = None;
                    self.detail_rx = None;
                    self.detail_scroll = 0;
                }
                KeyCode::Up => self.detail_scroll = self.detail_scroll.saturating_sub(1),
                KeyCode::Down => self.detail_scroll = self.detail_scroll.saturating_add(1),
                KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(8),
                KeyCode::PageDown => self.detail_scroll = self.detail_scroll.saturating_add(8),
                KeyCode::Home => self.detail_scroll = 0,
                _ => {}
            }
            return OverlayAction::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => OverlayAction::Close,
            KeyCode::Char('r') if ctrl => {
                self.reload();
                OverlayAction::None
            }
            KeyCode::Up => {
                self.select_previous();
                OverlayAction::None
            }
            KeyCode::Down => {
                self.select_next();
                OverlayAction::None
            }
            KeyCode::Tab => {
                self.select_next();
                OverlayAction::None
            }
            KeyCode::PageUp => {
                self.page(false, 10);
                OverlayAction::None
            }
            KeyCode::PageDown => {
                self.page(true, 10);
                OverlayAction::None
            }
            KeyCode::Home => {
                self.selected = 0;
                OverlayAction::None
            }
            KeyCode::End => {
                self.selected = self.total().saturating_sub(1);
                OverlayAction::None
            }
            KeyCode::Enter => {
                let chosen = self
                    .matches()
                    .get(self.selected)
                    .map(|team| team.id.clone());
                if let Some(id) = chosen {
                    self.open_detail(id);
                }
                OverlayAction::None
            }
            KeyCode::Backspace => {
                if self.filter.pop().is_some() {
                    self.selected = 0;
                    self.scroll = 0;
                }
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
        if !self.visible || area.width < 24 || area.height < 8 {
            return;
        }
        // Use the full content area: browsing teams is a page in the TUI, not a
        // small dialog that hides the list and leaves an unusable prompt behind.
        let popup = area;
        frame.render_widget(Clear, popup);

        let block = Block::default()
            .title(" teams — pick one ")
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::NONE)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        if inner.height < 4 || inner.width < 8 {
            return;
        }

        if let Some(detail) = &self.detail {
            let content = Self::detail_text(detail);
            frame.render_widget(
                Paragraph::new(content)
                    .wrap(Wrap { trim: true })
                    .scroll((self.detail_scroll, 0)),
                Rect::new(
                    inner.x,
                    inner.y + 2,
                    inner.width,
                    inner.height.saturating_sub(3),
                ),
            );
            frame.render_widget(
                Paragraph::new("Esc back to teams"),
                Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
            );
            return;
        }

        let filter_area = Rect::new(inner.x, inner.y + 2, inner.width, 1);
        let hint_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
        let body_area = Rect::new(
            inner.x,
            inner.y + 3,
            inner.width,
            inner.height.saturating_sub(4),
        );

        let count = if self.filter.is_empty() {
            format!(" {} teams", self.total())
        } else {
            format!(" {} of {}", self.total(), self.loaded_count())
        };
        let demo = if self.demo { " · demo" } else { "" };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " > ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if self.filter.is_empty() {
                        "type to filter".to_string()
                    } else {
                        self.filter.clone()
                    },
                    if self.filter.is_empty() {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
                Span::styled(
                    format!("{count}{demo}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ])),
            filter_area,
        );

        if let Some(text) = self.state_text() {
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: true })
                    .alignment(Alignment::Left),
                body_area,
            );
        } else if self.total() == 0 {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" no team matches \"{}\"", self.filter),
                    Style::default().fg(Color::Yellow),
                ))),
                body_area,
            );
        } else {
            // Two drawn lines per team, so the window is moved in rows and the
            // scroll is kept in lines: one number would drift every other row.
            let pages = (body_area.height as usize).max(2);
            let first = self.selected * 2;
            if first < self.scroll {
                self.scroll = first;
            } else if first + 1 >= self.scroll + pages {
                self.scroll = (first + 2).saturating_sub(pages);
            }

            let mut lines: Vec<Line<'static>> = Vec::new();
            for (index, team) in self.matches().iter().enumerate() {
                lines.extend(Self::row_lines(team, index == self.selected));
            }
            frame.render_widget(
                Paragraph::new(lines).scroll((self.scroll as u16, 0)),
                body_area,
            );
        }

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "↑/↓ scroll  ·  PgUp/PgDn page  ·  Esc back",
                Style::default().fg(Color::DarkGray),
            ))),
            hint_area,
        );
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("type", "filter by name or id"),
            ("up/down", "move"),
            ("enter", "inspect the selected team's details"),
            ("ctrl+r", "read the list again"),
            ("esc", "close"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn team(name: &str, id: &str, members: i64, projects: i64) -> Team {
        Team {
            id: id.to_string(),
            name: name.to_string(),
            project_count: projects,
            member_count: members,
            is_member: false,
            can_manage: false,
            created_by: None,
            created_by_email: None,
            created_at: "2026-03-01T00:00:00Z".to_string(),
        }
    }

    /// A picker with an answer already in it: the tests never open a socket.
    fn ready(teams: Vec<Team>) -> TeamsOverlay {
        let mut overlay = TeamsOverlay::new();
        overlay.show();
        overlay.loaded = Loaded::Ready(teams);
        overlay
    }

    fn drawn(overlay: &TeamsOverlay) -> String {
        overlay
            .matches()
            .iter()
            .enumerate()
            .flat_map(|(index, team)| TeamsOverlay::row_lines(team, index == overlay.selected))
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn team_browser_uses_the_full_area_without_a_dialog_frame() {
        let mut overlay = ready(vec![team("platform", "id-platform", 4, 2)]);
        let backend = TestBackend::new(72, 20);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| overlay.render(frame, frame.area(), &App::new("s", false)))
            .expect("draw team browser");

        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("teams"), "{text}");
        assert!(text.contains("platform"), "{text}");
        assert!(
            !text.contains('┌'),
            "the full-screen browser has no box: {text}"
        );
    }

    #[test]
    fn a_row_carries_the_id_and_states_only_the_markers_that_are_true() {
        let mut mine = team("platform", "4f2a9c10-0000-4000-8000-000000000003", 3, 2);
        mine.is_member = true;
        mine.can_manage = true;
        let overlay = ready(vec![
            mine,
            team("design", "4f2a9c10-0000-4000-8000-000000000004", 1, 0),
        ]);

        let text = drawn(&overlay);
        assert!(text.contains("platform · yours · can manage"), "{text}");
        assert!(
            text.contains("id 4f2a9c10-0000-4000-8000-000000000003 · 3 member(s) · 2 project(s)"),
            "the id is what the next command takes: {text}"
        );

        // The team the caller is not in must not borrow the markers.
        let design = text.find("design").expect("design row");
        assert!(
            !text[design..].contains("yours"),
            "markers belong to their own row: {text}"
        );
    }

    #[test]
    fn enter_opens_read_only_detail_for_the_highlighted_team() {
        let mut app = App::new("s", false);
        let mut overlay = ready(vec![
            team("platform", "id-platform", 1, 0),
            team("design", "id-design", 1, 0),
        ]);
        overlay.select_next();

        let action = overlay.handle_key(KeyEvent::from(KeyCode::Enter), &mut app);
        assert!(matches!(action, OverlayAction::None));
        assert_eq!(app.input, "");
        assert!(!app.pending_submit);
        assert!(matches!(
            overlay.detail,
            Some(DetailLoaded::Failed(ref error)) if error == "no workspace context"
        ));
    }

    #[test]
    fn detail_render_uses_returned_data_and_has_a_back_path() {
        let mut overlay = ready(vec![team("platform", "id-platform", 1, 1)]);
        let mut detail_team = team("platform", "id-platform", 1, 1);
        detail_team.is_member = true;
        overlay.detail = Some(DetailLoaded::Ready(TeamDetail {
            team: detail_team,
            members: vec![],
            projects: vec![],
            can_manage: false,
        }));
        let mut app = App::new("s", false);
        let action = overlay.handle_key(KeyEvent::from(KeyCode::Esc), &mut app);
        assert!(matches!(action, OverlayAction::None));
        assert!(overlay.detail.is_none());
        assert_eq!(overlay.total(), 1);
    }

    #[test]
    fn detail_navigation_is_isolated_from_team_selection_and_prompt() {
        let mut app = App::new("s", false);
        let mut overlay = ready(vec![
            team("platform", "id-platform", 1, 0),
            team("design", "id-design", 1, 0),
        ]);
        overlay.selected = 1;
        overlay.detail = Some(DetailLoaded::Ready(TeamDetail {
            team: team("design", "id-design", 1, 0),
            members: vec![],
            projects: vec![],
            can_manage: false,
        }));

        for key in [KeyCode::Down, KeyCode::PageDown, KeyCode::Char('x')] {
            overlay.handle_key(KeyEvent::from(key), &mut app);
        }

        assert_eq!(overlay.selected, 1, "detail keys must not move the list");
        assert_eq!(overlay.detail_scroll, 9);
        assert!(app.input.is_empty(), "detail keys must not reach chat");
        assert!(!app.pending_submit);

        overlay.handle_key(KeyEvent::from(KeyCode::Esc), &mut app);
        assert_eq!(overlay.selected, 1, "back restores the same team row");
        assert_eq!(overlay.detail_scroll, 0);
    }

    #[test]
    fn tiny_terminal_render_does_not_underflow() {
        let mut overlay = ready(vec![team("platform", "id-platform", 1, 1)]);
        overlay.detail = Some(DetailLoaded::Loading);
        let backend = TestBackend::new(24, 8);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| overlay.render(frame, frame.area(), &App::new("s", false)))
            .expect("small detail render");
    }

    #[test]
    fn typing_narrows_by_name_and_by_id_and_never_reaches_the_prompt() {
        let mut app = App::new("s", false);
        let mut overlay = ready(vec![
            team("platform", "id-platform", 1, 0),
            team("design", "id-design", 1, 0),
        ]);

        for ch in "design".chars() {
            overlay.handle_key(KeyEvent::from(KeyCode::Char(ch)), &mut app);
        }
        assert_eq!(overlay.total(), 1);
        assert!(app.input.is_empty(), "the prompt must stay untouched");

        // A uuid pasted into the filter finds the row even when the name does not.
        overlay.filter.clear();
        for ch in "platform".chars() {
            overlay.handle_key(KeyEvent::from(KeyCode::Char(ch)), &mut app);
        }
        assert_eq!(overlay.matches()[0].name, "platform");
    }

    #[test]
    fn an_empty_org_says_how_to_make_a_team_instead_of_drawing_nothing() {
        let overlay = ready(Vec::new());
        let text = overlay.state_text().expect("an empty org needs words");
        assert!(text.contains("No teams in this org yet"), "{text}");
        assert!(text.contains("/teams create <name>"), "{text}");
        assert!(text.contains("/app/teams"), "{text}");
    }

    #[test]
    fn a_failed_read_shows_the_reason_rather_than_an_empty_table() {
        let mut overlay = TeamsOverlay::new();
        overlay.show();
        overlay.loaded = Loaded::Failed(
            "not signed in. Run `/login` in the session, `mn auth login` in a shell, or set \
             MEMNOTH_API_KEY"
                .to_string(),
        );
        let text = overlay.state_text().expect("a failure needs words");
        assert!(text.contains("not signed in"), "{text}");
        assert!(text.contains("/login"), "the fix is named: {text}");
        assert!(text.contains("ctrl+r"), "{text}");
    }

    #[test]
    fn the_highlight_stops_at_both_ends() {
        let mut overlay = ready(vec![
            team("a", "id-a", 0, 0),
            team("b", "id-b", 0, 0),
            team("c", "id-c", 0, 0),
        ]);
        for _ in 0..10 {
            overlay.select_next();
        }
        assert_eq!(overlay.selected, 2);
        for _ in 0..10 {
            overlay.select_previous();
        }
        assert_eq!(overlay.selected, 0);
    }

    #[test]
    fn a_filter_that_matches_nothing_leaves_no_row_to_press_enter_on() {
        let mut app = App::new("s", false);
        let mut overlay = ready(vec![team("platform", "id-platform", 1, 0)]);
        for ch in "zzz".chars() {
            overlay.handle_key(KeyEvent::from(KeyCode::Char(ch)), &mut app);
        }
        overlay.handle_key(KeyEvent::from(KeyCode::Enter), &mut app);
        assert_eq!(overlay.total(), 0);
        assert!(
            app.input.is_empty(),
            "there is no team to take, so nothing runs"
        );
    }
}
