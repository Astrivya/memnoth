//! `/sessions` (alias `/resume`): every conversation on this machine, and the
//! way back into one.
//!
//! The list is `SessionStore::list` — the same rows `memnoth session list`
//! prints — so the picker cannot disagree with the command about what exists.
//! Picking a row parks a [`SessionSwitch`] on the app instead of loading the
//! file here: the run loop owns the one `Session` being written, and a second
//! reader that also opens it would be a second writer the moment a turn lands.
//!
//! What this does not do: fork a conversation (nothing copies a transcript into
//! a new one), sync to the cloud (sessions are per-machine, like the vault), or
//! delete on one keypress. Those are all features of other tools, and each one
//! would be a promise this screen cannot keep from here.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::output::{ellipsize, human_age};
use crate::session::{SessionStore, SessionSummary, now_ms};
use crate::tui::app::{App, SessionSwitch};
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// How many messages of the selected conversation the preview shows.
///
/// The tail, not the head: what you are deciding is whether to continue, and
/// continue means "where it left off".
const PREVIEW_TURNS: usize = 6;

/// Longest single message line the preview keeps before it cuts.
const PREVIEW_LINE: usize = 220;

/// Below this width a side-by-side preview is a column of hyphens, so the
/// preview moves under the list instead of disappearing: picking a conversation
/// without seeing any of it is a guess.
const SIDE_BY_SIDE: u16 = 90;

/// Below this body height there is no room for a stacked preview either, and a
/// three-line pane of half a sentence is worse than the list on its own.
const COMPACT_PREVIEW_MIN_HEIGHT: u16 = 10;

/// One conversation, as this screen holds it.
#[derive(Debug, Clone)]
struct SessionEntry {
    name: String,
    title: String,
    model: String,
    messages: usize,
    /// `2h ago`, computed when the list was read.
    age: String,
    is_current: bool,
}

impl SessionEntry {
    /// What the row says when it is not being read: the title a person
    /// recognises, else the name they can type, else the fact that there is
    /// nothing in it yet.
    fn label(&self) -> String {
        if !self.title.is_empty() {
            self.title.clone()
        } else if self.messages > 0 {
            self.name.clone()
        } else {
            format!("{} (no messages yet)", self.name)
        }
    }

    /// The right-hand tail: size, then age.
    fn tail(&self) -> String {
        if self.messages == 0 {
            return "new".to_string();
        }
        format!("{} msgs · {}", self.messages, self.age)
    }
}

/// The conversation picker.
#[derive(Debug, Default)]
pub struct SessionsOverlay {
    visible: bool,
    /// The directory the rows came from, kept so `r` and `d` can act without
    /// being handed the app again.
    store: Option<SessionStore>,
    rows: Vec<SessionEntry>,
    /// Indices into `rows` that survive the filter, in list order.
    shown: Vec<usize>,
    /// Position within `shown`, never within `rows`.
    selected: usize,
    list_state: ListState,
    filter: String,
    filtering: bool,
    /// The name being typed, when a rename is in progress.
    renaming: Option<String>,
    /// The row whose delete was armed by the first `d`.
    confirm_delete: Option<String>,
    /// Drawn preview, and which row it belongs to.
    preview: String,
    preview_for: Option<String>,
    /// Why the list is empty, or what just happened.
    note: Option<String>,
}

impl SessionsOverlay {
    /// A picker with nothing read yet; `on_open` fills it.
    pub fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            list_state,
            ..Default::default()
        }
    }

    /// The row under the cursor.
    fn current(&self) -> Option<&SessionEntry> {
        self.shown
            .get(self.selected)
            .and_then(|index| self.rows.get(*index))
    }

    /// Read the sessions on disk.
    ///
    /// `current` is the session the screen is recording, marked in the list so
    /// reopening it is a choice rather than an accident.
    fn read(&mut self, store: &SessionStore, current: &str) {
        self.note = None;
        self.rows = store
            .list()
            .into_iter()
            .map(|summary| entry(summary, current))
            .collect();
        if self.rows.is_empty() {
            self.note = Some(
                "no conversations yet — one is written when its first answer lands".to_string(),
            );
        }
        self.rebuild();
        self.preview.clear();
        self.preview_for = None;
    }

    /// Re-apply the filter and put the cursor back inside the list.
    fn rebuild(&mut self) {
        let needle = self.filter.to_lowercase();
        self.shown = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                needle.is_empty()
                    || fuzzy(&row.title, &needle)
                    || fuzzy(&row.name, &needle)
                    || fuzzy(&row.model, &needle)
            })
            .map(|(index, _)| index)
            .collect();
        self.clamp();
    }

    /// Keep the selection on a row that exists.
    fn clamp(&mut self) {
        if self.shown.is_empty() {
            self.selected = 0;
            self.list_state.select(None);
            return;
        }
        self.selected = self.selected.min(self.shown.len() - 1);
        self.list_state.select(Some(self.selected));
    }

    /// Move by `delta` rows, clamped at both ends rather than wrapping: the top
    /// row is the most recent conversation, and overshooting it should not put
    /// you at the bottom of a list of forty.
    fn move_by(&mut self, delta: i32) {
        if self.shown.is_empty() {
            return;
        }
        let last = self.shown.len() - 1;
        let next = self.selected as i64 + delta as i64;
        self.selected = next.clamp(0, last as i64) as usize;
        self.list_state.select(Some(self.selected));
    }

    /// Read the selected conversation into the preview pane, once per row.
    fn load_preview(&mut self) {
        let Some(row) = self.current() else {
            self.preview.clear();
            self.preview_for = None;
            return;
        };
        let name = row.name.clone();
        if self.preview_for.as_deref() == Some(name.as_str()) {
            return;
        }
        self.preview_for = Some(name.clone());
        self.preview = match &self.store {
            Some(store) => match store.load(&name) {
                Ok(session) => describe(&name, &session),
                Err(error) => format!("{name}\n\ncannot read this transcript: {error}"),
            },
            None => format!("{name}\n\nno sessions directory is open"),
        };
    }

    /// Start a fresh conversation from inside the picker.
    fn new_chat(&self, app: &mut App) -> OverlayAction {
        let name = self
            .store
            .as_ref()
            .map(SessionStore::next_name)
            .unwrap_or_else(|| "chat-1".to_string());
        app.pending_session = Some(SessionSwitch::New(name));
        OverlayAction::Close
    }

    /// Rename the selected conversation to `to`. Returns true when the rename
    /// was handed to the loop that owns the open session.
    ///
    /// The file name is `Session::id`, so renaming the conversation this
    /// process is recording has to move the writer with the file — otherwise the
    /// next save recreates the old path and the conversation exists twice. That
    /// one rename is parked for the run loop; every other row the picker renames
    /// itself and stays open.
    fn rename_to(&mut self, app: &mut App, to: &str) -> bool {
        let Some(from) = self.current().map(|row| row.name.clone()) else {
            return false;
        };
        let Some(store) = self.store.clone() else {
            return false;
        };
        if app.name == from {
            self.renaming = None;
            app.pending_session = Some(SessionSwitch::Rename {
                from,
                to: to.to_string(),
            });
            return true;
        }
        match store.rename(&from, to) {
            Ok(renamed) => {
                self.read(&store, &app.name);
                self.load_preview();
                self.note = Some(format!("renamed \"{from}\" to \"{renamed}\""));
            }
            Err(error) => {
                self.note = Some(format!("cannot rename: {error}"));
                self.renaming = Some(to.to_string());
            }
        }
        false
    }

    /// Delete the selected conversation, when the arming key was pressed twice.
    fn delete_current(&mut self, app: &App) {
        let Some(row) = self.current().cloned() else {
            return;
        };
        // Deleting the file this screen is writing would be a deletion the next
        // answer undoes: the loop still holds the transcript in memory. Refusing
        // is the only version of this that keeps its word.
        if row.is_current {
            self.confirm_delete = None;
            self.note = Some(format!(
                "\"{}\" is the conversation being recorded — open another one first",
                row.name
            ));
            return;
        }
        let Some(store) = self.store.clone() else {
            return;
        };
        match store.delete(&row.name) {
            Ok(()) => {
                self.confirm_delete = None;
                self.read(&store, &app.name);
                self.load_preview();
                self.note = Some(format!("deleted \"{}\"", row.name));
            }
            Err(error) => {
                self.confirm_delete = None;
                self.note = Some(format!("cannot delete: {error}"));
            }
        }
    }

    /// The footer, which is where the screen says what the next key does.
    fn keys(&self) -> String {
        if self.renaming.is_some() {
            return "type a name · enter renames · esc cancels".to_string();
        }
        if let Some(name) = &self.confirm_delete {
            return format!("press d again to delete \"{name}\" · any other key cancels");
        }
        if self.filtering {
            return "type to filter · enter opens · esc clears the filter".to_string();
        }
        "↑↓ move · enter open · n new · r rename · d delete · / filter · esc close".to_string()
    }
}

/// One summary as the picker holds it.
fn entry(summary: SessionSummary, current: &str) -> SessionEntry {
    SessionEntry {
        is_current: summary.name == current,
        age: human_age(summary.modified_ms, now_ms()),
        name: summary.name,
        title: summary.title,
        model: summary.model,
        messages: summary.messages,
    }
}

/// The preview text: what the session is, then how it ended.
///
/// The context block is the part a row cannot carry — which model, when it was
/// opened, what it has cost — because those are the reasons to pick one
/// conversation over another.
fn describe(name: &str, session: &memnoth_session::Session) -> String {
    let created = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(session.created_ms)
        .map(|when| when.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let tokens: i64 = session
        .receipts
        .iter()
        .map(|receipt| receipt.total_tokens as i64)
        .sum();
    let cost_micros: u64 = session
        .receipts
        .iter()
        .map(|receipt| receipt.cost_micros.unwrap_or(0))
        .sum();
    let model = if session.model.is_empty() {
        "model unknown"
    } else {
        session.model.as_str()
    };
    let mut out = format!(
        "{name}\n{model} · {} messages\nopened {created}",
        session.messages.len()
    );
    if tokens > 0 {
        out.push_str(&format!(" · {} tokens", short_count(tokens)));
    }
    if cost_micros > 0 {
        out.push_str(&format!(" · ${:.2}", cost_micros as f64 / 1_000_000.0));
    }
    out.push_str("\n\n");

    if session.messages.is_empty() {
        out.push_str("nothing said yet: this conversation is a name and an empty file");
        return out;
    }

    let skip = session.messages.len().saturating_sub(PREVIEW_TURNS);
    if skip > 0 {
        out.push_str(&format!("… {skip} earlier messages\n\n"));
    }
    for message in session.messages.iter().skip(skip) {
        let who = match message.role {
            memnoth_session::Role::User => "you",
            memnoth_session::Role::Assistant => "memnoth",
            memnoth_session::Role::System => "system",
            memnoth_session::Role::Tool => "tool",
        };
        let text = message
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!("{who}: {}\n\n", ellipsize(&text, PREVIEW_LINE)));
    }
    out
}

/// A token count as a person reads it: `82.6k`, not `82600`.
fn short_count(value: i64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.max(0).to_string()
    }
}

/// Subsequence match, case already folded by the caller.
///
/// The same rule the slash menu uses: every character of the needle appears in
/// the haystack, in order. Typing `lg` finds "fix the login guard".
fn fuzzy(haystack: &str, needle: &str) -> bool {
    let mut wanted = needle.chars();
    let Some(mut next) = wanted.next() else {
        return true;
    };
    for ch in haystack.to_lowercase().chars() {
        if ch == next {
            match wanted.next() {
                Some(following) => next = following,
                None => return true,
            }
        }
    }
    false
}

impl Overlay for SessionsOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Sessions
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.filter.clear();
        self.filtering = false;
        self.renaming = None;
        self.confirm_delete = None;
        self.note = None;
        self.selected = 0;
        self.list_state.select(Some(0));
    }

    /// Reading needs the app: the sessions directory and which name is current.
    fn on_open(&mut self, app: &mut App) {
        match app.ctx.as_ref() {
            Some(ctx) => {
                let store = SessionStore::new(&ctx.sessions_dir);
                self.read(&store, &app.name);
                self.store = Some(store);
                self.load_preview();
            }
            None => {
                self.store = None;
                self.rows.clear();
                self.shown.clear();
                self.preview.clear();
                self.preview_for = None;
                self.note = Some(
                    "no workspace context — conversations live under the workspace's sessions \
                     directory"
                        .to_string(),
                );
            }
        }
    }

    fn hide(&mut self) {
        self.visible = false;
        self.renaming = None;
        self.confirm_delete = None;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // A rename is a line being typed: it owns every key until it is
        // finished, exactly like the prompt does.
        if self.renaming.is_some() {
            match key.code {
                KeyCode::Esc => self.renaming = None,
                KeyCode::Enter => {
                    let typed = self.renaming.take().unwrap_or_default();
                    let typed = typed.trim().to_string();
                    if typed.is_empty() {
                        self.note = Some("a name cannot be empty".to_string());
                    } else if self.rename_to(app, &typed) {
                        // Parked for the loop: the picker has nothing left to
                        // show, and the header will carry the new name.
                        return OverlayAction::Close;
                    }
                }
                KeyCode::Backspace => {
                    if let Some(buffer) = self.renaming.as_mut() {
                        buffer.pop();
                    }
                }
                KeyCode::Char(ch) if !ctrl => {
                    if let Some(buffer) = self.renaming.as_mut() {
                        buffer.push(ch);
                    }
                }
                _ => {}
            }
            return OverlayAction::None;
        }

        // Arming a delete lasts one key: anything but the confirming `d` puts
        // the row back the way it was.
        if key.code != KeyCode::Char('d') {
            self.confirm_delete = None;
        }

        match key.code {
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    return OverlayAction::Close;
                }
                self.filter.clear();
                self.filtering = false;
                self.rebuild();
                self.load_preview();
            }
            KeyCode::Up => {
                self.move_by(-1);
                self.load_preview();
            }
            KeyCode::Down => {
                self.move_by(1);
                self.load_preview();
            }
            KeyCode::PageUp => {
                self.move_by(-10);
                self.load_preview();
            }
            KeyCode::PageDown => {
                self.move_by(10);
                self.load_preview();
            }
            KeyCode::Home => {
                self.move_by(i32::MIN / 2);
                self.load_preview();
            }
            KeyCode::End => {
                self.move_by(i32::MAX / 2);
                self.load_preview();
            }
            KeyCode::Enter => {
                if let Some(row) = self.current() {
                    app.pending_session = Some(SessionSwitch::Open(row.name.clone()));
                    return OverlayAction::Close;
                }
            }
            // `n` is the one key that does not need a row: a new conversation
            // is the thing you want when the list is not it.
            KeyCode::Char('n') if !ctrl => return self.new_chat(app),
            KeyCode::Char('r') if !ctrl => {
                if let Some(row) = self.current() {
                    self.renaming = Some(row.name.clone());
                }
            }
            KeyCode::Char('d') if !ctrl => match self.confirm_delete.clone() {
                Some(armed) if Some(&armed) == self.current().map(|row| &row.name) => {
                    self.delete_current(app);
                }
                _ => {
                    if let Some(row) = self.current() {
                        self.confirm_delete = Some(row.name.clone());
                    }
                }
            },
            KeyCode::Char('/') if !ctrl => self.filtering = true,
            KeyCode::Backspace => {
                if self.filtering {
                    self.filter.pop();
                    self.rebuild();
                    self.load_preview();
                }
            }
            KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.filtering = true;
                self.filter.push(ch);
                self.rebuild();
                self.load_preview();
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

        let total = self.rows.len();
        let title = if self.shown.len() == total {
            format!(" Sessions ({total}) ")
        } else {
            format!(" Sessions ({}/{total}) ", self.shown.len())
        };
        let outer = Block::default().borders(Borders::ALL).title(title);
        let inner = outer.inner(area);
        frame.render_widget(outer, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(inner);
        let (filter_area, body, footer) = (rows[0], rows[1], rows[2]);

        // The filter line is always there, so the list does not jump down a row
        // the moment the first character is typed.
        let (filter_text, filter_style) = if self.filter.is_empty() && !self.filtering {
            (
                "filter · type to search titles, names and models".to_string(),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            let caret = if self.filtering { "▌" } else { "" };
            (
                format!("filter: {}{caret}", self.filter),
                Style::default().fg(Color::White),
            )
        };
        frame.render_widget(
            Paragraph::new(ellipsize(&filter_text, filter_area.width as usize)).style(filter_style),
            filter_area,
        );

        if self.shown.is_empty() {
            let empty = Paragraph::new(self.note.clone().unwrap_or_else(|| {
                if self.rows.is_empty() {
                    "no conversations yet".to_string()
                } else {
                    format!("nothing matches \"{}\"", self.filter)
                }
            }))
            .style(Style::default().fg(Color::Gray))
            .wrap(Wrap { trim: true });
            frame.render_widget(empty, body);
        } else if body.width >= SIDE_BY_SIDE {
            let split = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                .split(body);
            self.render_list(frame, split[0]);
            self.render_preview(frame, split[1], false);
        } else if body.height >= COMPACT_PREVIEW_MIN_HEIGHT {
            // The split is even: the list keeps enough rows to choose from, and
            // the bottom half answers "which of these is it" without opening
            // one. A taller list would push the preview to three lines of
            // nothing; a taller preview would hide the rows it describes.
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(body);
            self.render_list(frame, split[0]);
            self.render_preview(frame, split[1], true);
        } else {
            self.render_list(frame, body);
        }

        frame.render_widget(
            Paragraph::new(ellipsize(&self.keys(), footer.width as usize))
                .style(Style::default().fg(Color::DarkGray)),
            footer,
        );
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/sessions", "Every conversation on disk, newest first"),
            ("↑/↓", "Move"),
            ("Enter", "Open the selected conversation"),
            ("N", "New conversation"),
            ("R", "Rename"),
            ("D", "Delete (press twice)"),
            ("/", "Filter by title, name or model"),
            ("Esc", "Clear the filter, then close"),
        ]
    }
}

impl SessionsOverlay {
    /// The list half: one line per conversation, title left and facts right.
    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let width = area.width as usize;
        let items: Vec<ListItem> = self
            .shown
            .iter()
            .enumerate()
            .filter_map(|(position, index)| self.rows.get(*index).map(|row| (position, row)))
            .map(|(position, row)| {
                let selected = position == self.selected;
                let style = if selected {
                    Style::default().bg(Color::Blue).fg(Color::White)
                } else {
                    Style::default()
                };
                let marker = if row.is_current { "· " } else { "  " };
                // The tail is the first thing to go in a narrow window: a title
                // cut to three characters so a timestamp can keep its column is
                // a list that has stopped being a list.
                let tail = if width >= 40 {
                    row.tail()
                } else {
                    String::new()
                };
                let room = width.saturating_sub(2 + tail.chars().count() + 1);
                let label = ellipsize(&row.label(), room.max(8));
                let pad = width
                    .saturating_sub(2 + label.chars().count() + tail.chars().count())
                    .max(1);
                let line = Line::from(vec![
                    Span::styled(
                        marker,
                        Style::default().fg(if row.is_current {
                            Color::Cyan
                        } else {
                            Color::DarkGray
                        }),
                    ),
                    Span::styled(
                        label,
                        if selected {
                            style.add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        },
                    ),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(
                        tail,
                        if selected {
                            style
                        } else {
                            Style::default().fg(Color::DarkGray)
                        },
                    ),
                ]);
                ListItem::new(line).style(style)
            })
            .collect();

        let list = List::new(items).highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    /// The preview: what the row is, then the last few messages.
    ///
    /// `stacked` is the narrow frame, where the pane sits under the list and a
    /// top border is the divider. The title names the conversation, so the
    /// preview cannot be read against the wrong row.
    fn render_preview(&self, frame: &mut Frame, area: Rect, stacked: bool) {
        let title = match self.current() {
            Some(row) => format!(
                " preview · {} ",
                ellipsize(&row.label(), area.width.saturating_sub(16) as usize)
            ),
            None => " preview ".to_string(),
        };
        let borders = if stacked { Borders::TOP } else { Borders::LEFT };
        let preview = Paragraph::new(self.preview.clone())
            .style(Style::default().fg(Color::Gray))
            .block(
                Block::default()
                    .borders(borders)
                    .title(title)
                    .style(Style::default().fg(Color::DarkGray)),
            )
            .wrap(Wrap { trim: true });
        frame.render_widget(preview, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Two conversations, the second written after the first so its mtime is
    /// later. The gap is a few milliseconds: every filesystem this ships on
    /// timestamps finer than that.
    fn two_sessions(dir: &std::path::Path) -> SessionStore {
        let store = SessionStore::new(dir);
        let mut first = store.create("older").unwrap();
        store
            .record(&mut first, "the older question", "an answer")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut second = store.create("newer").unwrap();
        store
            .record(&mut second, "the newer question", "another answer")
            .unwrap();
        store
    }

    #[test]
    fn rows_are_newest_activity_first_and_carry_the_first_prompt_as_a_title() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_sessions(dir.path());
        let mut overlay = SessionsOverlay::new();
        overlay.read(&store, "newer");

        assert_eq!(overlay.rows.len(), 2);
        assert_eq!(overlay.rows[0].name, "newer", "most recent activity leads");
        assert_eq!(overlay.rows[0].title, "the newer question");
        assert_eq!(overlay.rows[1].title, "the older question");
        assert!(overlay.rows[0].is_current);
        assert!(!overlay.rows[1].is_current);
    }

    #[test]
    fn filtering_by_a_subsequence_keeps_matching_rows_and_the_selection_in_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_sessions(dir.path());
        let mut overlay = SessionsOverlay::new();
        overlay.read(&store, "newer");
        overlay.selected = 1;

        // Subsequence, not prefix: `oqn` is inside "the older question".
        overlay.filter = "oqn".to_string();
        overlay.rebuild();
        assert_eq!(overlay.shown.len(), 1);
        assert_eq!(
            overlay.current().map(|row| row.name.as_str()),
            Some("older")
        );
        assert_eq!(overlay.selected, 0, "the cursor cannot point past the end");

        overlay.filter = "zzz".to_string();
        overlay.rebuild();
        assert!(overlay.shown.is_empty());
        assert_eq!(overlay.selected, 0);
    }

    #[test]
    fn delete_takes_two_presses_and_never_deletes_the_open_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_sessions(dir.path());
        let mut overlay = SessionsOverlay::new();
        overlay.visible = true;
        overlay.store = Some(store.clone());
        overlay.read(&store, "newer");
        let mut app = App::new("newer", false);

        // The open conversation is refused even by a second press: its file is
        // the one the next answer rewrites.
        let open_path = store.path("newer").unwrap();
        overlay.handle_key(key(KeyCode::Char('d')), &mut app);
        overlay.handle_key(key(KeyCode::Char('d')), &mut app);
        assert!(open_path.exists(), "the recorded conversation stays");
        assert!(
            overlay
                .note
                .as_deref()
                .unwrap_or_default()
                .contains("being recorded")
        );

        // The other row takes two presses: the first only arms.
        overlay.selected = 1;
        overlay.clamp();
        let doomed = overlay.current().unwrap().name.clone();
        let path = store.path(&doomed).unwrap();
        overlay.handle_key(key(KeyCode::Char('d')), &mut app);
        assert!(path.exists(), "one press must not delete anything");
        assert_eq!(overlay.confirm_delete.as_deref(), Some(doomed.as_str()));

        overlay.handle_key(key(KeyCode::Char('d')), &mut app);
        assert!(!path.exists(), "the second press is the one that deletes");
        assert!(overlay.confirm_delete.is_none());
    }

    /// Draw the overlay the way the screen does and read the frame back.
    fn draw(overlay: &mut SessionsOverlay, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let app = App::new("newer", false);
        terminal
            .draw(|frame| overlay.render(frame, frame.area(), &app))
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
    fn a_narrow_frame_puts_the_preview_under_the_list_with_its_context() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_sessions(dir.path());
        let mut overlay = SessionsOverlay::new();
        overlay.visible = true;
        overlay.store = Some(store.clone());
        overlay.read(&store, "newer");
        overlay.load_preview();

        let text = draw(&mut overlay, 80, 30);
        let mut lines = text.lines();
        let top = lines.by_ref().take(15).collect::<Vec<_>>().join("\n");
        let bottom = lines.collect::<Vec<_>>().join("\n");

        assert!(top.contains("the newer question"), "{top}");
        assert!(
            !top.contains("preview"),
            "a narrow frame must not spend a column on the preview: {top}"
        );
        assert!(bottom.contains("preview"), "{bottom}");
        assert!(bottom.contains("messages"), "{bottom}");
        assert!(bottom.contains("opened"), "{bottom}");
        assert!(bottom.contains("another answer"), "{bottom}");
    }

    #[test]
    fn a_wide_frame_keeps_the_preview_beside_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_sessions(dir.path());
        let mut overlay = SessionsOverlay::new();
        overlay.visible = true;
        overlay.store = Some(store.clone());
        overlay.read(&store, "newer");
        overlay.load_preview();

        let text = draw(&mut overlay, 120, 30);
        assert!(
            text.lines()
                .any(|line| line.contains("preview") && line.contains("the newer question")),
            "the list and the preview share a row in a wide frame: {text}"
        );
    }

    #[test]
    fn a_fuzzy_query_matches_characters_in_order_only() {
        assert!(fuzzy("fix the login guard", "lg"));
        assert!(fuzzy("fix the login guard", ""));
        assert!(!fuzzy("fix the login guard", "gl"));
        assert!(!fuzzy("", "a"));
    }
}
