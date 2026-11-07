//! Minting a key and inviting someone: the two answers that must not become a
//! file.
//!
//! Every other verb prints into the transcript, and the transcript is written to
//! the session on disk. That is the right default — a record of the work — and it
//! is exactly wrong for the two responses that carry a credential once: `POST
//! /api/v1/api-keys` returns the only copy of a key's secret, and
//! `POST /api/v1/orgs/invites` returns the only copy of an invite's link and
//! code. `/keys create` and `/org invite` were therefore refused inside a
//! session and sent to a shell.
//!
//! This is the screen that replaces the refusal: a form, a submit on a worker
//! thread, and a result that lives in this overlay and nowhere else. Closing it
//! with `esc` is what forgets the secret — `hide` drops it — and the only line
//! that reaches the transcript names the key's prefix, which is not a credential.
//!
//! The request shapes and defaults are the server's, not this screen's: an empty
//! field is sent as `None` so the control plane applies its own default, because
//! a form that quietly substitutes `write` mints authority nobody asked for.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use memnoth_cloud_client::CreateKeyRequest;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::context::AppContext;
use crate::output::ellipsize;
use crate::tui::app::{App, EntryRole};
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// Where a form is: being filled in, waiting on the server, or holding an answer
/// that must not be written down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Form,
    Working,
    Done,
}

/// The labels and the visual fields, shared by both screens.
///
/// Two forms with the same focus, caret and paste behaviour are one form; only
/// the labels, the validation and the call behind `enter` differ.
#[derive(Debug)]
struct Form {
    labels: &'static [&'static str],
    placeholders: &'static [&'static str],
    values: Vec<String>,
    focus: usize,
}

impl Form {
    fn new(labels: &'static [&'static str], placeholders: &'static [&'static str]) -> Self {
        Self {
            labels,
            placeholders,
            values: vec![String::new(); labels.len()],
            focus: 0,
        }
    }

    /// A form with what the person already typed on the command line, so
    /// `/keys create --name laptop` does not ask for the laptop again.
    fn seeded(
        labels: &'static [&'static str],
        placeholders: &'static [&'static str],
        seeds: &[Option<String>],
    ) -> Self {
        let mut form = Self::new(labels, placeholders);
        for (index, seed) in seeds.iter().enumerate() {
            if let Some(value) = seed {
                form.set(index, value.trim());
            }
        }
        form
    }

    fn value(&self, index: usize) -> &str {
        self.values.get(index).map(String::as_str).unwrap_or("")
    }

    /// The value without the spaces someone typed around it: `name` is a label,
    /// and a label that starts with a space is one nobody can match later.
    fn trimmed(&self, index: usize) -> &str {
        self.value(index).trim()
    }

    /// An empty field is absent, never a default of this screen's invention.
    fn optional(&self, index: usize) -> Option<String> {
        let value = self.trimmed(index);
        (!value.is_empty()).then(|| value.to_string())
    }

    fn insert(&mut self, ch: char) {
        if let Some(slot) = self.values.get_mut(self.focus) {
            slot.push(ch);
        }
    }

    fn backspace(&mut self) {
        if let Some(slot) = self.values.get_mut(self.focus) {
            slot.pop();
        }
    }

    fn next(&mut self) {
        if !self.values.is_empty() {
            self.focus = (self.focus + 1) % self.values.len();
        }
    }

    fn previous(&mut self) {
        if !self.values.is_empty() {
            self.focus = (self.focus + self.values.len() - 1) % self.values.len();
        }
    }

    fn set(&mut self, index: usize, value: &str) {
        if let Some(slot) = self.values.get_mut(index) {
            *slot = value.to_string();
        }
    }

    fn label_width(&self) -> usize {
        self.labels
            .iter()
            .map(|label| label.chars().count())
            .max()
            .unwrap_or(0)
    }

    /// The fields, with the focused label in cyan and an empty one showing what
    /// belongs there instead of an empty gap.
    fn lines(&self) -> Vec<Line<'static>> {
        let width = self.label_width();
        self.labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let focused = index == self.focus;
                let value = self.value(index);
                let (shown, value_style) = if value.is_empty() {
                    (
                        self.placeholders
                            .get(index)
                            .copied()
                            .unwrap_or("")
                            .to_string(),
                        Style::default().fg(Color::DarkGray),
                    )
                } else {
                    (value.to_string(), Style::default().fg(Color::White))
                };
                let label_style = if focused {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Line::from(vec![
                    Span::styled(format!("{label:<width$}  "), label_style),
                    Span::styled(shown, value_style),
                ])
            })
            .collect()
    }
}

/// A posted key, in the form this screen displays it.
#[derive(Debug, Clone)]
struct KeyDone {
    id: String,
    name: String,
    prefix: String,
    scope: String,
    project_id: Option<String>,
    expires_at: Option<String>,
    /// The secret. Held here and nowhere else, and dropped by `hide`.
    token: String,
}

/// A created invite, in the form this screen displays it.
#[derive(Debug, Clone)]
struct InviteDone {
    email: String,
    role: String,
    expires_at: String,
    link: Option<String>,
    code: String,
    email_sent: bool,
    email_error: Option<String>,
}

/// `/keys create` — the form, and the one box the secret ever appears in.
#[derive(Debug)]
pub struct KeyCreateOverlay {
    visible: bool,
    form: Form,
    stage: Stage,
    /// A refusal to show under the fields: validation, or the server's answer.
    note: Option<String>,
    /// The invocation's paths and credentials, copied from the app on open so
    /// the submit thread owns what it needs. `None` in a test.
    ctx: Option<AppContext>,
    /// The answer in flight. Drained by `on_tick`, never by `render`.
    pending: Option<Receiver<Result<KeyDone, String>>>,
    done: Option<KeyDone>,
}

const KEY_LABELS: &[&str] = &["name", "scope", "project", "expires"];
const KEY_PLACEHOLDERS: &[&str] = &[
    "optional label, e.g. laptop",
    "optional: read, write or admin",
    "optional project id",
    "optional days, e.g. 30",
];

impl KeyCreateOverlay {
    pub fn new() -> Self {
        Self::seeded(None, None, None, None)
    }

    /// Opened from `/keys create …`, with whatever flags were already typed.
    pub fn seeded(
        name: Option<String>,
        scope: Option<String>,
        project: Option<String>,
        expires_in_days: Option<i64>,
    ) -> Self {
        Self {
            visible: false,
            form: Form::seeded(
                KEY_LABELS,
                KEY_PLACEHOLDERS,
                &[
                    name,
                    scope,
                    project,
                    expires_in_days.map(|days| days.to_string()),
                ],
            ),
            stage: Stage::Form,
            note: None,
            ctx: None,
            pending: None,
            done: None,
        }
    }

    /// True while the secret is on screen. The transcript must never be able to
    /// say the same, which is why this is a state and not a string elsewhere.
    #[cfg(test)]
    fn has_secret(&self) -> bool {
        self.done.is_some()
    }

    /// The request the form describes, or the one thing wrong with it.
    ///
    /// Only what was typed is sent: every empty field stays `None` so the server
    /// picks its own default, which is the difference between a key with the
    /// authority the person asked for and one with the authority this screen
    /// guessed.
    fn request(&self) -> Result<CreateKeyRequest, String> {
        let scope = self.form.trimmed(1);
        if !scope.is_empty() && !matches!(scope, "read" | "write" | "admin") {
            return Err(
                "scope must be read, write or admin — leave it empty for the server's default"
                    .to_string(),
            );
        }

        let expires_in_days = match self.form.trimmed(3) {
            "" => None,
            typed => Some(
                typed
                    .parse::<i64>()
                    .ok()
                    .filter(|days| *days > 0)
                    .ok_or_else(|| {
                        format!("expires must be a positive whole number of days, not {typed:?}")
                    })?,
            ),
        };

        Ok(CreateKeyRequest {
            name: self.form.optional(0),
            scope: self.form.optional(1),
            project_id: self.form.optional(2),
            expires_in_days,
        })
    }

    fn submit(&mut self, _app: &mut App) {
        if self.stage == Stage::Working {
            return;
        }
        let request = match self.request() {
            Ok(request) => request,
            Err(message) => {
                self.note = Some(message);
                return;
            }
        };
        let Some(ctx) = self.ctx.clone() else {
            self.note = Some(
                "this screen has no workspace context, so it cannot reach the cloud".to_string(),
            );
            return;
        };

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<KeyDone> {
                let client = crate::cloud::signed_in_client(&ctx)?;
                let response = client.create_key(request)?;
                Ok(KeyDone {
                    id: response.key.id,
                    name: response.key.name,
                    prefix: response.key.prefix,
                    scope: response.key.scope,
                    project_id: response.key.project_id,
                    expires_at: response.key.expires_at,
                    token: response.token,
                })
            })();
            let _ = tx.send(result.map_err(|error| format!("{error:#}")));
        });

        self.pending = Some(rx);
        self.stage = Stage::Working;
        self.note = None;
    }

    /// The result: what was minted, and the secret in its own box.
    fn done_lines(&self) -> Vec<Line<'static>> {
        let Some(done) = &self.done else {
            return Vec::new();
        };
        let name = if done.name.trim().is_empty() {
            done.id.clone()
        } else {
            done.name.clone()
        };
        let project = done
            .project_id
            .clone()
            .unwrap_or_else(|| "org-wide".to_string());
        let expires = done
            .expires_at
            .clone()
            .unwrap_or_else(|| "never".to_string());
        vec![
            Line::from(vec![
                Span::styled("minted   ", Style::default().fg(Color::DarkGray)),
                Span::styled(name, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("prefix   ", Style::default().fg(Color::DarkGray)),
                Span::styled(done.prefix.clone(), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("scope    ", Style::default().fg(Color::DarkGray)),
                Span::styled(done.scope.clone(), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("project  ", Style::default().fg(Color::DarkGray)),
                Span::styled(project, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("expires  ", Style::default().fg(Color::DarkGray)),
                Span::styled(expires, Style::default().fg(Color::White)),
            ]),
        ]
    }

    fn secret_lines(&self) -> Vec<Line<'static>> {
        match &self.done {
            Some(done) => vec![Line::from(Span::styled(
                done.token.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ))],
            None => Vec::new(),
        }
    }

    fn outcome_note(&self) -> String {
        "Shown once: the server keeps only a hash. This screen is not written to the transcript; \
         esc closes it and the secret is gone."
            .to_string()
    }

    fn apply_pending(&mut self, app: &mut App) {
        let Some(rx) = &self.pending else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(done)) => {
                // The transcript names what exists, never the credential.
                let label = if done.name.trim().is_empty() {
                    done.prefix.clone()
                } else {
                    format!("{} ({})", done.name, done.prefix)
                };
                app.push(
                    EntryRole::Info,
                    format!("minted a key: {label} — the secret is in the overlay only"),
                );
                self.done = Some(done);
                self.pending = None;
                self.stage = Stage::Done;
            }
            Ok(Err(message)) => {
                self.note = Some(message);
                self.pending = None;
                self.stage = Stage::Form;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                // A thread that ended without sending is a bug, and reporting it
                // as a failed mint is better than a form stuck on "working".
                self.note = Some("the mint stopped without an answer".to_string());
                self.pending = None;
                self.stage = Stage::Form;
            }
        }
    }
}

impl Default for KeyCreateOverlay {
    fn default() -> Self {
        Self::new()
    }
}

/// `/org invite` — the same screen for the link and the code.
#[derive(Debug)]
pub struct InviteOverlay {
    visible: bool,
    form: Form,
    stage: Stage,
    note: Option<String>,
    ctx: Option<AppContext>,
    pending: Option<Receiver<Result<InviteDone, String>>>,
    done: Option<InviteDone>,
}

const INVITE_LABELS: &[&str] = &["email", "role"];
const INVITE_PLACEHOLDERS: &[&str] = &["their address", "optional: admin or member"];

impl InviteOverlay {
    pub fn new() -> Self {
        Self::seeded(None, None)
    }

    /// Opened from `/org invite someone@example.com`, with the address typed.
    pub fn seeded(email: Option<String>, role: Option<String>) -> Self {
        Self {
            visible: false,
            form: Form::seeded(INVITE_LABELS, INVITE_PLACEHOLDERS, &[email, role]),
            stage: Stage::Form,
            note: None,
            ctx: None,
            pending: None,
            done: None,
        }
    }

    /// True while the invite is on screen; the transcript must not echo it.
    #[cfg(test)]
    fn has_secret(&self) -> bool {
        self.done.is_some()
    }

    /// The address and role, or the one thing wrong with them.
    fn request(&self) -> Result<(String, Option<String>), String> {
        let email = self.form.trimmed(0);
        if email.is_empty() {
            return Err("an email address is required".to_string());
        }
        if !email.contains('@') {
            // The same test the server applies. Catching it here keeps the form
            // open with the typo in front of the person instead of a 400.
            return Err(format!("{email:?} is not an address — it needs an @"));
        }

        let role = self.form.trimmed(1);
        if !role.is_empty() && !matches!(role, "owner" | "admin" | "member") {
            return Err(
                "role must be owner, admin or member — leave it empty for the server's default"
                    .to_string(),
            );
        }

        Ok((email.to_string(), self.form.optional(1)))
    }

    fn submit(&mut self, _app: &mut App) {
        if self.stage == Stage::Working {
            return;
        }
        let (email, role) = match self.request() {
            Ok(parsed) => parsed,
            Err(message) => {
                self.note = Some(message);
                return;
            }
        };
        let Some(ctx) = self.ctx.clone() else {
            self.note = Some(
                "this screen has no workspace context, so it cannot reach the cloud".to_string(),
            );
            return;
        };

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<InviteDone> {
                let client = crate::cloud::signed_in_client(&ctx)?;
                let created = client.create_invite(&email, role.as_deref())?;
                Ok(InviteDone {
                    email: created.email,
                    role: created.role,
                    expires_at: created.expires_at,
                    link: created.link,
                    code: created.code,
                    email_sent: created.email_sent,
                    email_error: created.email_error,
                })
            })();
            let _ = tx.send(result.map_err(|error| format!("{error:#}")));
        });

        self.pending = Some(rx);
        self.stage = Stage::Working;
        self.note = None;
    }

    fn done_lines(&self) -> Vec<Line<'static>> {
        let Some(done) = &self.done else {
            return Vec::new();
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled("invited  ", Style::default().fg(Color::DarkGray)),
                Span::styled(done.email.clone(), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("role     ", Style::default().fg(Color::DarkGray)),
                Span::styled(done.role.clone(), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("expires  ", Style::default().fg(Color::DarkGray)),
                Span::styled(done.expires_at.clone(), Style::default().fg(Color::White)),
            ]),
        ];

        // Delivery is reported, never assumed: "sent" when it was not is the one
        // failure that leaves someone waiting for a mail that never comes.
        if done.email_sent {
            lines.push(Line::from(Span::styled(
                "the invite email was sent",
                Style::default().fg(Color::Green),
            )));
        } else {
            let reason = match done.email_error.as_deref() {
                Some(reason) => format!("the invite email was NOT sent: {reason}"),
                None => "the invite email was NOT sent".to_string(),
            };
            lines.push(Line::from(Span::styled(
                reason,
                Style::default().fg(Color::Yellow),
            )));
        }

        lines
    }

    /// The link and the code, which are the credentials here.
    fn secret_lines(&self) -> Vec<Line<'static>> {
        let Some(done) = &self.done else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        match &done.link {
            Some(link) => lines.push(Line::from(Span::styled(
                link.clone(),
                Style::default().fg(Color::White),
            ))),
            // A deployment with no public URL cannot build a join link. Saying so
            // in words points at the code, which still works.
            None => lines.push(Line::from(Span::styled(
                "no link — this deployment has no public URL; give them the code",
                Style::default().fg(Color::Yellow),
            ))),
        }
        lines.push(Line::from(vec![
            Span::styled("code  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                done.code.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines
    }

    fn outcome_note(&self) -> String {
        "Shown once: only hashes are stored. This screen is not written to the transcript; esc \
         closes it and the link and code are gone."
            .to_string()
    }

    fn apply_pending(&mut self, app: &mut App) {
        let Some(rx) = &self.pending else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(done)) => {
                app.push(
                    EntryRole::Info,
                    format!(
                        "invited {} as {} — the link and code are in the overlay only",
                        done.email, done.role
                    ),
                );
                self.done = Some(done);
                self.pending = None;
                self.stage = Stage::Done;
            }
            Ok(Err(message)) => {
                self.note = Some(message);
                self.pending = None;
                self.stage = Stage::Form;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.note = Some("the invite stopped without an answer".to_string());
                self.pending = None;
                self.stage = Stage::Form;
            }
        }
    }
}

impl Default for InviteOverlay {
    fn default() -> Self {
        Self::new()
    }
}

/* --------------------------------- drawing --------------------------------- */

/// The geometry both screens share: a centred popup, an explanation band, the
/// body, a note band that is always reserved, and the footer.
///
/// The bands are fixed so a validation message does not shove the fields up under
/// the cursor — the same reason `/connect` keeps its status line when it is empty.
fn frame_for(area: Rect, lines: u16, done: bool) -> (Rect, Vec<Rect>) {
    let width = area
        .width
        .saturating_sub(4)
        .min(84)
        .max(area.width.min(44))
        .max(1);
    let height = (if done {
        22.min(area.height)
    } else {
        (lines + 7).min(area.height)
    })
    .max(7.min(area.height));

    let popup = Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    );

    let inner = Block::default().borders(Borders::ALL).inner(popup);
    let rows = if done {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(5),
                Constraint::Min(4),
                Constraint::Length(2),
                Constraint::Length(2),
            ])
            .split(inner)
            .to_vec()
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(2),
                Constraint::Length(1),
            ])
            .split(inner)
            .to_vec()
    };
    (popup, rows)
}

fn draw_shell(
    frame: &mut Frame,
    popup: Rect,
    title: &str,
    explanation: &str,
    rows: &[Rect],
    note: Option<&str>,
    footer: &str,
) {
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} "))
            .border_style(Style::default().fg(Color::Cyan)),
        popup,
    );

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            ellipsize(explanation, rows[0].width as usize),
            Style::default().fg(Color::DarkGray),
        ))),
        rows[0],
    );

    if let Some(note) = note {
        // `!` and the words carry the meaning; the colour only reinforces it.
        let lines = vec![
            Line::from(Span::styled(
                ellipsize(&format!("! {note}"), rows[rows.len() - 2].width as usize),
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
        ];
        frame.render_widget(Paragraph::new(lines), rows[rows.len() - 2]);
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            ellipsize(footer, rows[rows.len() - 1].width as usize),
            Style::default().fg(Color::DarkGray),
        ))),
        rows[rows.len() - 1],
    );
}

/// Where the caret goes: inside the value of the focused field, not on the prompt
/// behind the overlay.
fn caret_for(fields: Rect, form: &Form) -> Option<Position> {
    let y = fields.y + form.focus as u16;
    if y >= fields.y + fields.height {
        return None;
    }
    // `fields` is already inside the popup's border: label, then the two spaces
    // the line puts between the label and the value.
    let lead = form.label_width() as u16 + 2;
    let typed = form.value(form.focus).chars().count() as u16;
    let x = (fields.x + lead + typed).min(fields.x + fields.width.saturating_sub(1));
    Some(Position::new(x, y))
}

/* ---------------------------------- key ----------------------------------- */

impl Overlay for KeyCreateOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::KeyCreate
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.stage = Stage::Form;
        self.note = None;
        // Reopening starts from the fields again: a secret from a previous open
        // must not survive into this one.
        self.done = None;
    }

    fn on_open(&mut self, app: &mut App) {
        // Copied here, not read per frame: the submit thread needs to own it.
        self.ctx = app.ctx.clone();
    }

    fn on_tick(&mut self, app: &mut App) {
        self.apply_pending(app);
    }

    fn hide(&mut self) {
        self.visible = false;
        // Closing the screen is what forgets the secret.
        self.done = None;
        self.pending = None;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }
        if self.stage == Stage::Working {
            // One key has a meaning while a mint is in flight: leave.
            return match key.code {
                KeyCode::Esc => OverlayAction::Close,
                _ => OverlayAction::None,
            };
        }

        match key.code {
            KeyCode::Esc => return OverlayAction::Close,
            KeyCode::Enter => {
                if self.stage == Stage::Done {
                    return OverlayAction::Close;
                }
                self.submit(app);
            }
            KeyCode::Tab | KeyCode::Down => self.form.next(),
            KeyCode::BackTab | KeyCode::Up => self.form.previous(),
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if self.stage == Stage::Done {
                    // The answer is read-only: a keystroke must not quietly edit
                    // the form behind it while the secret is on screen.
                    return OverlayAction::None;
                }
                self.form.insert(ch);
                self.note = None;
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn handle_paste(&mut self, text: &str) -> bool {
        if self.stage != Stage::Form {
            return false;
        }
        // A pasted project id usually carries a newline; a value with one in it is
        // a 404 that looks like a wrong id.
        for ch in text.chars().filter(|ch| !ch.is_control()) {
            self.form.insert(ch);
        }
        self.note = None;
        true
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }
        let done = self.stage == Stage::Done;
        let (popup, rows) = frame_for(area, self.form.labels.len() as u16, done);
        let explanation = if done {
            "The response that carries the secret is in this box, and nowhere else."
        } else {
            "Only what you type is sent; an empty field means the server's default."
        };
        let footer = match self.stage {
            Stage::Form => "tab next · shift-tab back · enter mint · esc cancel",
            Stage::Working => "minting… esc closes this screen",
            Stage::Done => "copy the secret · esc closes and forgets it",
        };
        draw_shell(
            frame,
            popup,
            "Mint a key",
            explanation,
            &rows,
            self.note.as_deref(),
            footer,
        );

        if done {
            frame.render_widget(Paragraph::new(self.done_lines()), rows[1]);
            frame.render_widget(
                Paragraph::new(self.secret_lines())
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" the secret — shown once ")
                            .border_style(Style::default().fg(Color::Yellow)),
                    ),
                rows[2],
            );
            frame.render_widget(
                Paragraph::new(self.outcome_note())
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(Color::DarkGray)),
                rows[3],
            );
            return;
        }

        let fields = rows[1];
        frame.render_widget(Paragraph::new(self.form.lines()), fields);
        if self.stage == Stage::Form
            && let Some(position) = caret_for(fields, &self.form)
        {
            frame.set_cursor_position(position);
        }
        if self.stage == Stage::Working {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "asking the control plane…",
                    Style::default().fg(Color::Cyan),
                ))),
                Rect::new(rows[2].x, rows[2].y, rows[2].width, rows[2].height.min(1)),
            );
        }
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "/keys create",
                "Mint an API key; the secret appears here, not in the transcript",
            ),
            ("tab", "Next field"),
            ("enter", "Mint the key"),
            ("esc", "Close — and forget a secret that was shown"),
        ]
    }
}

/* --------------------------------- invite --------------------------------- */

impl Overlay for InviteOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Invite
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.stage = Stage::Form;
        self.note = None;
        self.done = None;
    }

    fn on_open(&mut self, app: &mut App) {
        self.ctx = app.ctx.clone();
    }

    fn on_tick(&mut self, app: &mut App) {
        self.apply_pending(app);
    }

    fn hide(&mut self) {
        self.visible = false;
        self.done = None;
        self.pending = None;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }
        if self.stage == Stage::Working {
            return match key.code {
                KeyCode::Esc => OverlayAction::Close,
                _ => OverlayAction::None,
            };
        }

        match key.code {
            KeyCode::Esc => return OverlayAction::Close,
            KeyCode::Enter => {
                if self.stage == Stage::Done {
                    return OverlayAction::Close;
                }
                self.submit(app);
            }
            KeyCode::Tab | KeyCode::Down => self.form.next(),
            KeyCode::BackTab | KeyCode::Up => self.form.previous(),
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if self.stage == Stage::Done {
                    return OverlayAction::None;
                }
                self.form.insert(ch);
                self.note = None;
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn handle_paste(&mut self, text: &str) -> bool {
        if self.stage != Stage::Form {
            return false;
        }
        for ch in text.chars().filter(|ch| !ch.is_control()) {
            self.form.insert(ch);
        }
        self.note = None;
        true
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }
        let done = self.stage == Stage::Done;
        let (popup, rows) = frame_for(area, self.form.labels.len() as u16, done);
        let explanation = if done {
            "The link and the code are in this box, and nowhere else."
        } else {
            "One invite per address; the role is the org role it carries."
        };
        let footer = match self.stage {
            Stage::Form => "tab next · shift-tab back · enter invite · esc cancel",
            Stage::Working => "inviting… esc closes this screen",
            Stage::Done => "copy the link and code · esc closes and forgets them",
        };
        draw_shell(
            frame,
            popup,
            "Invite someone",
            explanation,
            &rows,
            self.note.as_deref(),
            footer,
        );

        if done {
            frame.render_widget(Paragraph::new(self.done_lines()), rows[1]);
            frame.render_widget(
                Paragraph::new(self.secret_lines())
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" the link and code — shown once ")
                            .border_style(Style::default().fg(Color::Yellow)),
                    ),
                rows[2],
            );
            frame.render_widget(
                Paragraph::new(self.outcome_note())
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(Color::DarkGray)),
                rows[3],
            );
            return;
        }

        let fields = rows[1];
        frame.render_widget(Paragraph::new(self.form.lines()), fields);
        if self.stage == Stage::Form
            && let Some(position) = caret_for(fields, &self.form)
        {
            frame.set_cursor_position(position);
        }
        if self.stage == Stage::Working {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "asking the control plane…",
                    Style::default().fg(Color::Cyan),
                ))),
                Rect::new(rows[2].x, rows[2].y, rows[2].width, rows[2].height.min(1)),
            );
        }
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "/org invite",
                "Invite by email; the link and code appear here, not in the transcript",
            ),
            ("tab", "Next field"),
            ("enter", "Create the invite"),
            ("esc", "Close — and forget a link or code that was shown"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opened without `on_open`: a test must never read the credential store, and
    /// `ctx` stays `None`, which is what a submit then reports.
    fn key_form() -> KeyCreateOverlay {
        let mut overlay = KeyCreateOverlay::new();
        overlay.show();
        overlay
    }

    fn invite_form() -> InviteOverlay {
        let mut overlay = InviteOverlay::new();
        overlay.show();
        overlay
    }

    #[test]
    fn what_the_command_line_said_is_already_in_the_fields() {
        let overlay = KeyCreateOverlay::seeded(
            Some("  laptop ".to_string()),
            Some("read".to_string()),
            None,
            Some(30),
        );
        assert_eq!(overlay.form.value(0), "laptop", "trimmed, not re-asked");
        assert_eq!(overlay.form.value(1), "read");
        assert_eq!(overlay.form.value(3), "30");
        assert_eq!(
            overlay.form.value(2),
            "",
            "a flag that was not given leaves its field empty"
        );
    }

    #[test]
    fn tab_and_shift_tab_walk_the_fields_and_stop_on_the_edges() {
        let mut app = App::new("s", false);
        let mut overlay = key_form();

        for _ in 0..overlay.form.labels.len() {
            overlay.handle_key(KeyEvent::from(KeyCode::Tab), &mut app);
        }
        assert_eq!(overlay.form.focus, 0, "tab wraps back to the first field");

        overlay.handle_key(KeyEvent::from(KeyCode::BackTab), &mut app);
        assert_eq!(
            overlay.form.focus,
            overlay.form.labels.len() - 1,
            "shift-tab wraps the other way"
        );
    }

    #[test]
    fn an_invite_without_an_at_sign_is_refused_before_any_request() {
        let mut app = App::new("s", false);
        let mut overlay = InviteOverlay::seeded(Some("bob".to_string()), None);
        overlay.show();

        overlay.handle_key(KeyEvent::from(KeyCode::Enter), &mut app);

        assert_eq!(overlay.stage, Stage::Form, "the form stays open");
        assert!(
            overlay.note.as_deref().unwrap_or("").contains('@'),
            "the refusal names what is missing: {:?}",
            overlay.note
        );
        assert!(
            overlay.pending.is_none(),
            "nothing was sent: no address reached the network"
        );
        assert!(!overlay.has_secret());
    }

    #[test]
    fn an_expiry_that_is_not_a_whole_number_of_days_is_refused() {
        let mut app = App::new("s", false);
        let mut overlay = key_form();
        overlay.form.set(3, "soon");

        overlay.handle_key(KeyEvent::from(KeyCode::Enter), &mut app);

        assert_eq!(overlay.stage, Stage::Form);
        assert!(
            overlay
                .note
                .as_deref()
                .unwrap_or("")
                .contains("whole number"),
            "{:?}",
            overlay.note
        );
        assert!(overlay.pending.is_none(), "no request was made");
    }

    #[test]
    fn only_what_was_typed_is_sent_so_the_server_keeps_its_defaults() {
        let overlay = KeyCreateOverlay::seeded(Some("laptop".to_string()), None, None, None);
        let request = overlay.request().expect("an empty optional set is valid");

        assert_eq!(request.name.as_deref(), Some("laptop"));
        assert_eq!(request.scope, None, "no scope was invented");
        assert_eq!(request.project_id, None);
        assert_eq!(request.expires_in_days, None, "no deadline was invented");
    }

    #[test]
    fn an_unknown_scope_is_refused_rather_than_minted_as_written() {
        let overlay = KeyCreateOverlay::seeded(None, Some("superuser".to_string()), None, None);
        let error = overlay
            .request()
            .expect_err("the scope is not one the server has");
        assert!(error.contains("read, write or admin"), "{error}");
    }

    /// The one that matters: a finished result is held by the overlay, and closing
    /// the overlay is what forgets it. Nothing here writes the secret anywhere the
    /// session persists.
    #[test]
    fn a_shown_secret_lives_in_the_overlay_and_dies_with_it() {
        let mut overlay = key_form();
        overlay.stage = Stage::Done;
        overlay.done = Some(KeyDone {
            id: "key_01".to_string(),
            name: "laptop".to_string(),
            prefix: "mn_live_1a2b".to_string(),
            scope: "write".to_string(),
            project_id: None,
            expires_at: None,
            token: "mn_live_1a2b_the_secret".to_string(),
        });

        assert!(overlay.has_secret(), "the secret is on screen");
        let rendered: String = overlay
            .secret_lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(rendered.contains("the_secret"), "{rendered}");
        assert!(
            !overlay.done_lines().iter().any(|line| line
                .spans
                .iter()
                .any(|span| span.content.contains("the_secret"))),
            "the secret belongs in its own box, not among the details"
        );

        overlay.hide();
        assert!(
            !overlay.has_secret(),
            "closing the screen forgets the secret"
        );
        assert!(overlay.secret_lines().is_empty());
    }

    #[test]
    fn an_invite_result_says_when_the_mail_was_not_sent_and_still_shows_the_code() {
        let mut overlay = invite_form();
        overlay.done = Some(InviteDone {
            email: "bob@example.com".to_string(),
            role: "member".to_string(),
            expires_at: "2026-11-20T00:00:00Z".to_string(),
            link: None,
            code: "ABCD-1234".to_string(),
            email_sent: false,
            email_error: Some("no public URL is configured".to_string()),
        });

        let details: String = overlay
            .done_lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(details.contains("NOT sent"), "{details}");
        assert!(details.contains("no public URL"), "{details}");

        let secret: String = overlay
            .secret_lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(secret.contains("ABCD-1234"), "{secret}");
        assert!(
            secret.contains("no link"),
            "a missing link is stated, not left blank: {secret}"
        );
    }
}
