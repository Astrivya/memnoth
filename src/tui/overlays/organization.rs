//! `/org`: a read-only overview of the authenticated organization.
//!
//! Opening this page only reads the existing typed `GET /api/v1/org` response.
//! It does not alter the chat scope, transcript, or server-side organization.

use std::sync::mpsc::{self, Receiver};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use memnoth_cloud_client::{CloudError, OrgInfo};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use crate::context::AppContext;
use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

type OrgReply = Result<OrgInfo, OrgFailure>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct OrgFailure {
    title: &'static str,
    detail: String,
}

impl OrgFailure {
    fn from_cloud(error: CloudError) -> Self {
        let (title, detail) = match error {
            CloudError::AuthError(detail) => (
                "Sign-in required",
                format!("{detail}\nRun /login to reconnect."),
            ),
            CloudError::ClientError {
                status: 403,
                message,
            } => ("Access denied", message),
            CloudError::ClientError { status, message } => {
                ("Request denied", format!("HTTP {status}: {message}"))
            }
            CloudError::NetworkError(detail) => ("Offline", detail),
            CloudError::Transient { status, message } => {
                ("Cloud unavailable", format!("HTTP {status}: {message}"))
            }
            CloudError::NotFound(detail) => ("Organization unavailable", detail),
            other => ("Could not load organization", other.to_string()),
        };
        Self { title, detail }
    }

    fn from_setup(detail: String) -> Self {
        Self {
            title: "Cloud unavailable",
            detail,
        }
    }
}

#[derive(Debug)]
enum OrgState {
    Loading,
    Ready(OrgInfo),
    Failed(OrgFailure),
}

/// Full-screen organization facts, independent of the active chat assignment.
#[derive(Debug)]
pub(crate) struct OrganizationOverlay {
    visible: bool,
    ctx: Option<AppContext>,
    state: OrgState,
    rx: Option<Receiver<OrgReply>>,
}

impl OrganizationOverlay {
    pub(crate) fn new() -> Self {
        Self {
            visible: false,
            ctx: None,
            state: OrgState::Loading,
            rx: None,
        }
    }

    fn reload(&mut self) {
        let Some(ctx) = self.ctx.clone() else {
            self.state = OrgState::Failed(OrgFailure::from_setup(
                "No session cloud context is available.".to_string(),
            ));
            self.rx = None;
            return;
        };

        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.state = OrgState::Loading;
        std::thread::spawn(move || {
            let result = match crate::cloud::signed_in_client(&ctx) {
                Ok(client) => client.org().map_err(OrgFailure::from_cloud),
                Err(error) => Err(OrgFailure::from_setup(format!("{error:#}"))),
            };
            let _ = tx.send(result);
        });
    }

    fn fact(label: &'static str, value: impl Into<String>) -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("{label:<18}"), Style::default().fg(Color::DarkGray)),
            Span::styled(value.into(), Style::default().fg(Color::White)),
        ])
    }

    fn overview_lines(org: &OrgInfo) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::from(Span::styled(
                org.name.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Self::fact("organization", org.slug.clone()),
            Self::fact("your role", org.role.clone()),
            Self::fact("plan", org.plan.clone()),
            Self::fact(
                "seats",
                format!("{} used / {} available", org.seats_used, org.seats),
            ),
            Self::fact("members", org.member_count.to_string()),
            Self::fact("teams", org.team_count.to_string()),
            Self::fact("projects", org.project_count.to_string()),
        ];
        if org.unassigned_sessions > 0 {
            lines.push(Self::fact(
                "unassigned chats",
                org.unassigned_sessions.to_string(),
            ));
        }

        if org.member_count == 0 && org.team_count == 0 && org.project_count == 0 {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "This organization has no members, teams, or projects reported yet.",
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines
    }
}

impl Default for OrganizationOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl Overlay for OrganizationOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Organization
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
    }

    fn on_open(&mut self, app: &mut App) {
        self.ctx = app.ctx.clone();
        self.reload();
    }

    fn on_tick(&mut self, _app: &mut App) {
        let Some(rx) = self.rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(org)) => self.state = OrgState::Ready(org),
            Ok(Err(error)) => self.state = OrgState::Failed(error),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.state = OrgState::Failed(OrgFailure::from_setup(
                    "The organization request ended without a response.".to_string(),
                ));
            }
            Err(mpsc::TryRecvError::Empty) => self.rx = Some(rx),
        }
    }

    fn hide(&mut self) {
        self.visible = false;
        self.rx = None;
    }

    fn handle_key(&mut self, key: KeyEvent, _app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Backspace => OverlayAction::Close,
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.reload();
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

        let title = match &self.state {
            OrgState::Loading => {
                "memnoth / Organization                                      loading"
            }
            OrgState::Ready(_) => {
                "memnoth / Organization                                      read only"
            }
            OrgState::Failed(_) => {
                "memnoth / Organization                                      unavailable"
            }
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                title,
                Style::default().fg(Color::DarkGray),
            ))),
            Rect::new(area.x, area.y, area.width, 1),
        );

        let body = match &self.state {
            OrgState::Loading => vec![Line::from(Span::styled(
                "Loading organization details…",
                Style::default().fg(Color::Gray),
            ))],
            OrgState::Ready(org) => Self::overview_lines(org),
            OrgState::Failed(failure) => vec![
                Line::from(Span::styled(
                    failure.title,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(failure.detail.clone()),
                Line::from(""),
                Line::from("Ctrl+R retries this read."),
            ],
        };
        let body_area = Rect::new(
            area.x.saturating_add(2),
            area.y.saturating_add(2),
            area.width.saturating_sub(4),
            area.height.saturating_sub(5),
        );
        frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), body_area);

        let footer = " /org members   /teams   /projects    ·    Ctrl+R refresh    ·    Esc chat";
        frame.render_widget(
            Paragraph::new(Span::styled(footer, Style::default().fg(Color::DarkGray))),
            Rect::new(
                area.x,
                area.y + area.height.saturating_sub(1),
                area.width,
                1,
            ),
        );
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("ctrl+r", "refresh organization facts"),
            ("esc", "return to chat"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn org() -> OrgInfo {
        OrgInfo {
            slug: "astrivya".into(),
            name: "Astrivya".into(),
            plan: "team".into(),
            role: "owner".into(),
            member_count: 4,
            seats_used: 4,
            seats: 10,
            team_count: 2,
            project_count: 3,
            unassigned_sessions: 0,
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn draw(state: OrgState) -> String {
        let mut overlay = OrganizationOverlay::new();
        overlay.show();
        overlay.state = state;
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
        terminal
            .draw(|frame| overlay.render(frame, frame.area(), &App::new("s", false)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn overview_uses_only_returned_facts_and_is_unframed() {
        let text = draw(OrgState::Ready(org()));
        for fact in [
            "Astrivya",
            "astrivya",
            "owner",
            "team",
            "4 used / 10 available",
            "members",
            "projects",
        ] {
            assert!(text.contains(fact), "missing {fact:?} in {text}");
        }
        assert!(
            !text.contains('┌'),
            "the page should not be a dialog: {text}"
        );
        assert!(text.contains("read only"));
        assert!(text.contains("Esc chat"));
    }

    #[test]
    fn zero_counts_are_presented_as_an_empty_organization() {
        let mut empty = org();
        empty.member_count = 0;
        empty.team_count = 0;
        empty.project_count = 0;
        empty.seats_used = 0;
        let text = draw(OrgState::Ready(empty));
        assert!(
            text.contains("no members, teams, or projects reported"),
            "{text}"
        );
        assert!(text.contains("members"));
        assert!(text.contains("teams"));
        assert!(text.contains("projects"));
    }

    #[test]
    fn loading_offline_and_denied_states_are_truthful_and_retryable() {
        assert!(draw(OrgState::Loading).contains("Loading organization details"));
        let offline = OrgFailure::from_cloud(CloudError::NetworkError("connection refused".into()));
        let text = draw(OrgState::Failed(offline));
        assert!(text.contains("Offline"), "{text}");
        assert!(text.contains("Ctrl+R retries"), "{text}");

        let denied = OrgFailure::from_cloud(CloudError::ClientError {
            status: 403,
            message: "membership required".into(),
        });
        assert_eq!(denied.title, "Access denied");
    }

    #[test]
    fn escape_closes_without_touching_chat_draft_or_transcript() {
        let mut overlay = OrganizationOverlay::new();
        overlay.show();
        overlay.state = OrgState::Ready(org());
        let mut app = App::new("session-1", false);
        app.input = "unfinished question".into();
        app.cursor = app.input.chars().count();
        let entries = app.entries.clone();
        app.name = "session-1".into();
        let action = overlay.handle_key(KeyEvent::from(KeyCode::Esc), &mut app);
        assert_eq!(action, OverlayAction::Close);
        assert_eq!(app.input, "unfinished question");
        assert_eq!(
            app.entries
                .iter()
                .map(|entry| (entry.role, entry.text.as_str()))
                .collect::<Vec<_>>(),
            entries
                .iter()
                .map(|entry| (entry.role, entry.text.as_str()))
                .collect::<Vec<_>>()
        );
        assert_eq!(app.name, "session-1");
    }
}
