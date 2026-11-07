//! Feedback command: send feedback to memnoth cloud.

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::cli::{FeedbackArgs, FeedbackCommand, FeedbackSendArgs};
use crate::context::AppContext;
use crate::output::Outcome;

/// Where `mn feedback send` posts. The typed cloud client exposes no feedback
/// endpoint, so this verb posts to the documented URL directly.
const FEEDBACK_URL: &str = "https://memnoth.astrivya.ai/api/v1/feedback";

/// Run the feedback command.
pub fn run(ctx: &AppContext, args: &FeedbackArgs) -> Result<Outcome> {
    match &args.action {
        FeedbackCommand::Send(args) => send_feedback(ctx, args),
    }
}

/// The token a feedback post authenticates with: environment first, then the
/// credential store. Feedback is useful without an account, so a missing token
/// is not an error here — the server decides.
fn auth_token() -> Option<String> {
    std::env::var("MEMNOTH_API_KEY")
        .ok()
        .or_else(|| std::env::var("MEMNOTH_MCP_TOKEN").ok())
        .filter(|token| !token.trim().is_empty())
        .or_else(|| {
            memnoth_auth::get_credential_sync("memnoth")
                .ok()
                .flatten()
                .filter(|token| !token.trim().is_empty())
        })
}

/// `mn feedback send <message>` - POST /api/v1/feedback
pub fn send_feedback(_ctx: &AppContext, args: &FeedbackSendArgs) -> Result<Outcome> {
    let data = post_feedback(&args.message, &args.category, args.include_context)?;

    let text = format!(
        "Feedback sent successfully (ID: {})",
        data.get("id").and_then(|v| v.as_str()).unwrap_or("unknown")
    );
    Ok(Outcome::new("feedback.send", text, data))
}

/// POST /api/v1/feedback and return the decoded reply.
///
/// Feedback is a single small POST, so it goes straight to the endpoint the
/// console serves rather than through the typed client.
fn post_feedback(message: &str, category: &str, include_context: bool) -> Result<Value> {
    let body = json!({
        "message": message,
        "category": category,
        "include_context": include_context,
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .context("cannot create an HTTP client")?;

    let mut request = client.post(FEEDBACK_URL).json(&body);
    if let Some(token) = auth_token() {
        request = request.bearer_auth(token);
    }

    let response = request
        .send()
        .context("cannot reach the feedback endpoint")?;
    let status = response.status();
    let text = response.text().unwrap_or_default();

    if status.is_success() {
        // A non-JSON reply still means the server accepted the feedback.
        Ok(serde_json::from_str(&text).unwrap_or_else(|_| json!({ "text": text })))
    } else {
        Err(anyhow::anyhow!(match status.as_u16() {
            401 => "Authentication failed. Please run `mn auth login`".to_string(),
            402 => "Credits exhausted".to_string(),
            404 => "Feedback endpoint not found".to_string(),
            429 => "Rate limited. Please try again later".to_string(),
            code @ 500..=599 => format!("Server error ({code})"),
            code => format!("Error ({code}): {text}"),
        }))
    }
}

/// Feedback overlay state for TUI
#[derive(Debug, Default)]
pub struct FeedbackOverlay {
    visible: bool,
    message: String,
    cursor: usize,
    category: FeedbackCategory,
    include_context: bool,
    status: FeedbackStatus,
    /// Whether one `Esc` has already asked to throw the draft away.
    ///
    /// A draft is the only thing this screen holds, so the key that leaves it
    /// is the one key that has to ask twice.
    discard_armed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeedbackCategory {
    #[default]
    General,
    Bug,
    Feature,
    Performance,
    UI,
}

impl FeedbackCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            FeedbackCategory::General => "general",
            FeedbackCategory::Bug => "bug",
            FeedbackCategory::Feature => "feature",
            FeedbackCategory::Performance => "performance",
            FeedbackCategory::UI => "ui",
        }
    }

    pub fn all() -> [FeedbackCategory; 5] {
        [
            FeedbackCategory::General,
            FeedbackCategory::Bug,
            FeedbackCategory::Feature,
            FeedbackCategory::Performance,
            FeedbackCategory::UI,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum FeedbackStatus {
    #[default]
    Editing,
    Submitting,
    Success(String),
    Error(String),
}

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

impl FeedbackOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    fn submit_feedback(&mut self, _app: &mut App) {
        if self.message.trim().is_empty() {
            self.status = FeedbackStatus::Error("Message cannot be empty".to_string());
            return;
        }

        self.status = FeedbackStatus::Submitting;

        let message = self.message.clone();
        let category = self.category;

        // One small blocking POST. The overlay is modal, so nothing else is
        // waiting on the runtime while it runs.
        match post_feedback(&message, category.as_str(), self.include_context) {
            Ok(data) => {
                let id = data.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
                self.status = FeedbackStatus::Success(format!("Feedback sent (ID: {id})"));
                self.message.clear();
                self.cursor = 0;
            }
            Err(error) => {
                self.status = FeedbackStatus::Error(format!("Failed to send: {error}"));
            }
        }
    }
}

impl FeedbackOverlay {
    /// Nothing has been written yet, so there is nothing to lose: `Esc` may close
    /// without asking.
    fn draft_is_untouched(&self) -> bool {
        self.message.is_empty()
            && self.category == FeedbackCategory::default()
            && self.include_context
    }
}

impl Overlay for FeedbackOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Feedback
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.message.clear();
        self.cursor = 0;
        self.category = FeedbackCategory::default();
        self.include_context = true;
        self.status = FeedbackStatus::Editing;
        self.discard_armed = false;
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Arming a discard survives exactly one keypress: the second `Esc` is the
        // confirmation, anything else withdraws the question.
        if key.code != KeyCode::Esc {
            self.discard_armed = false;
        }

        match (&self.status, key.code) {
            (FeedbackStatus::Editing, KeyCode::Esc) => {
                if self.draft_is_untouched() {
                    return OverlayAction::Close;
                }
                if self.discard_armed {
                    return OverlayAction::Close;
                }
                self.discard_armed = true;
                return OverlayAction::None;
            }
            (FeedbackStatus::Editing, KeyCode::Enter) if ctrl => {
                // Ctrl+Enter submits
                self.submit_feedback(app);
                return OverlayAction::None;
            }
            (FeedbackStatus::Editing, KeyCode::Char(ch))
                if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.message.insert(self.cursor, ch);
                self.cursor += 1;
                return OverlayAction::None;
            }
            (FeedbackStatus::Editing, KeyCode::Backspace) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.message.remove(self.cursor);
                }
                return OverlayAction::None;
            }
            (FeedbackStatus::Editing, KeyCode::Delete) => {
                if self.cursor < self.message.len() {
                    self.message.remove(self.cursor);
                }
                return OverlayAction::None;
            }
            (FeedbackStatus::Editing, KeyCode::Left) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                return OverlayAction::None;
            }
            (FeedbackStatus::Editing, KeyCode::Right) => {
                if self.cursor < self.message.len() {
                    self.cursor += 1;
                }
                return OverlayAction::None;
            }
            (FeedbackStatus::Editing, KeyCode::Home) => {
                self.cursor = 0;
                return OverlayAction::None;
            }
            (FeedbackStatus::Editing, KeyCode::End) => {
                self.cursor = self.message.len();
                return OverlayAction::None;
            }
            (FeedbackStatus::Editing, KeyCode::Tab) => {
                // Cycle category
                let categories = FeedbackCategory::all();
                let current_idx = categories
                    .iter()
                    .position(|&c| c == self.category)
                    .unwrap_or(0);
                self.category = categories[(current_idx + 1) % categories.len()];
                return OverlayAction::None;
            }
            (FeedbackStatus::Editing, KeyCode::Char('c')) if ctrl => {
                // Ctrl+C toggles include_context
                self.include_context = !self.include_context;
                return OverlayAction::None;
            }
            (FeedbackStatus::Success(_), KeyCode::Esc) => {
                return OverlayAction::Close;
            }
            (FeedbackStatus::Error(_), KeyCode::Esc) => {
                self.status = FeedbackStatus::Editing;
                return OverlayAction::None;
            }
            _ => {}
        }

        OverlayAction::None
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        let block = Block::default()
            .title(" Feedback ")
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Cyan));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Message input
                Constraint::Length(1), // Category
                Constraint::Length(1), // Include context toggle
                Constraint::Min(0),    // Status
            ])
            .split(inner);

        // Message input
        let input_style = Style::default().fg(Color::White);
        let cursor_pos = self.cursor.min(self.message.len());
        let (before_cursor, after_cursor) = self.message.split_at(cursor_pos);
        let input_text = vec![Line::from(vec![
            Span::styled(before_cursor, input_style),
            Span::styled(
                "█",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
            ),
            Span::styled(after_cursor, input_style),
        ])];
        let input = Paragraph::new(input_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Message (Ctrl+Enter to send) "),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(input, chunks[0]);

        // Category
        let category_text = format!(" Category: {} (Tab to cycle) ", self.category.as_str());
        let category_widget = Paragraph::new(category_text)
            .block(Block::default().borders(Borders::ALL))
            .style(Style::default().fg(Color::Gray));
        frame.render_widget(category_widget, chunks[1]);

        // Include context toggle
        let context_text = format!(
            " Include context: {} (Ctrl+C to toggle) ",
            if self.include_context { "ON" } else { "OFF" }
        );
        let context_widget = Paragraph::new(context_text)
            .block(Block::default().borders(Borders::ALL))
            .style(Style::default().fg(Color::Gray));
        frame.render_widget(context_widget, chunks[2]);

        // Status
        let status_text = match &self.status {
            FeedbackStatus::Editing if self.discard_armed => {
                " Esc again discards this draft ".to_string()
            }
            FeedbackStatus::Editing => " Editing... ".to_string(),
            FeedbackStatus::Submitting => " Submitting... ".to_string(),
            FeedbackStatus::Success(msg) => format!(" ✓ {msg} (Esc to close) "),
            FeedbackStatus::Error(msg) => format!(" ✗ {msg} (Esc to retry) "),
        };
        let status_style = match &self.status {
            FeedbackStatus::Editing if self.discard_armed => Style::default().fg(Color::Yellow),
            FeedbackStatus::Error(_) => Style::default().fg(Color::Red),
            FeedbackStatus::Success(_) => Style::default().fg(Color::Green),
            FeedbackStatus::Submitting => Style::default().fg(Color::Yellow),
            _ => Style::default().fg(Color::Gray),
        };
        let status_widget = Paragraph::new(status_text)
            .block(Block::default().borders(Borders::ALL).title(" Status "))
            .style(status_style);
        frame.render_widget(status_widget, chunks[3]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        match self.status {
            FeedbackStatus::Editing => vec![
                ("Ctrl+Enter", "Send feedback"),
                ("Tab", "Cycle category"),
                ("Ctrl+C", "Toggle context"),
                ("Esc", "Close"),
            ],
            FeedbackStatus::Success(_) => vec![("Esc", "Close")],
            FeedbackStatus::Error(_) => vec![("Esc", "Retry")],
            FeedbackStatus::Submitting => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new("s", false)
    }

    fn opened() -> FeedbackOverlay {
        let mut overlay = FeedbackOverlay::new();
        overlay.show();
        overlay
    }

    #[test]
    fn one_escape_keeps_a_typed_draft_and_the_second_discards_it() {
        let mut app = app();
        let mut overlay = opened();
        overlay.message = "the graph forgets my macros".to_string();
        overlay.cursor = overlay.message.len();

        let first = overlay.handle_key(KeyEvent::from(KeyCode::Esc), &mut app);
        assert_eq!(
            first,
            OverlayAction::None,
            "one esc must not destroy a draft"
        );
        assert!(overlay.discard_armed);
        assert_eq!(overlay.message, "the graph forgets my macros");

        let second = overlay.handle_key(KeyEvent::from(KeyCode::Esc), &mut app);
        assert_eq!(
            second,
            OverlayAction::Close,
            "the second esc is the confirmation"
        );
    }

    #[test]
    fn an_untouched_draft_closes_on_the_first_escape() {
        let mut app = app();
        let mut overlay = opened();

        assert_eq!(
            overlay.handle_key(KeyEvent::from(KeyCode::Esc), &mut app),
            OverlayAction::Close
        );
    }

    #[test]
    fn typing_after_an_escape_withdraws_the_confirmation() {
        let mut app = app();
        let mut overlay = opened();
        overlay.message = "hello".to_string();
        overlay.cursor = overlay.message.len();

        overlay.handle_key(KeyEvent::from(KeyCode::Esc), &mut app);
        assert!(overlay.discard_armed);

        overlay.handle_key(KeyEvent::from(KeyCode::Char('!')), &mut app);
        assert!(!overlay.discard_armed, "typing is not a confirmation");
        assert_eq!(
            overlay.handle_key(KeyEvent::from(KeyCode::Esc), &mut app),
            OverlayAction::None,
            "the arming starts over instead of firing on the next esc"
        );
    }
}
