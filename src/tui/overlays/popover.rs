//! The answer window: a verb's rows in a window you can walk, with the buttons
//! that act on them.
//!
//! A table printed into the transcript is a dead end. Reading `/keys list` meant
//! scrolling prose, retyping an id, and hoping the id was the one you meant; the
//! palette had already solved that shape of problem — a filterable list whose
//! selected row does something — so the answers that are *lists of things* use
//! it too.
//!
//! Two rules keep the window honest:
//!
//! * It renders data the verb already returned (`Outcome::data`), never a second
//!   fetch. One implementation, two renderings: the text a pipe gets and the
//!   window a person gets cannot drift, because they are the same answer.
//! * A button exists only when there is a command behind it. Nothing here is a
//!   decorative chip: each key either runs a `/verb` line, opens the console, or
//!   closes the window.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use serde_json::Value;

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// What a key in the window does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopoverAction {
    /// Put this line in the prompt and run it, exactly as if it were typed.
    Run(String),
    /// Open a URL in the browser.
    Open(String),
}

impl PopoverAction {
    /// The line, for the footer and for the tests.
    pub fn describe(&self) -> String {
        match self {
            PopoverAction::Run(line) => line.clone(),
            PopoverAction::Open(url) => url.clone(),
        }
    }
}

/// One line of the window.
#[derive(Debug, Clone)]
pub struct PopoverRow {
    /// The thing itself: a team, a key, a reference.
    pub title: String,
    /// The facts beside it, already formatted by the caller.
    pub detail: String,
    /// A label drawn as a chip — `call`, `mention`, `revoked`.
    pub tag: Option<String>,
    /// What Enter does on this row. `None` draws the footer button as disabled.
    pub action: Option<PopoverAction>,
    /// A section divider: drawn, never selected, never acted on.
    pub header: bool,
}

impl PopoverRow {
    /// A row that can be selected.
    pub fn new(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
            tag: None,
            action: None,
            header: false,
        }
    }

    /// A section divider.
    pub fn section(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: String::new(),
            tag: None,
            action: None,
            header: true,
        }
    }

    /// Add the chip.
    pub fn tagged(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    /// Make Enter run this line.
    pub fn running(mut self, line: impl Into<String>) -> Self {
        self.action = Some(PopoverAction::Run(line.into()));
        self
    }
}

/// A button in the footer: a key, a label, and the thing it does.
#[derive(Debug, Clone)]
pub struct PopoverButton {
    pub key: char,
    pub label: String,
    pub action: PopoverAction,
}

impl PopoverButton {
    /// A button that runs a `/verb` line.
    pub fn run(key: char, label: impl Into<String>, line: impl Into<String>) -> Self {
        Self {
            key,
            label: label.into(),
            action: PopoverAction::Run(line.into()),
        }
    }

    /// A button that opens a URL.
    pub fn open(key: char, label: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            key,
            label: label.into(),
            action: PopoverAction::Open(url.into()),
        }
    }
}

/// Everything the window draws, built from one verb's answer.
#[derive(Debug, Clone, Default)]
pub struct PopoverSpec {
    /// The heading: what this is.
    pub title: String,
    /// The line under it: where it came from.
    pub subtitle: String,
    pub rows: Vec<PopoverRow>,
    pub buttons: Vec<PopoverButton>,
    /// Shown under the list — a count, or why a list is short.
    pub note: Option<String>,
}

impl PopoverSpec {
    pub fn new(title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            ..Self::default()
        }
    }

    pub fn rows(mut self, rows: Vec<PopoverRow>) -> Self {
        self.rows = rows;
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn button(mut self, button: PopoverButton) -> Self {
        self.buttons.push(button);
        self
    }

    /// One selectable row worth opening a window for?
    ///
    /// A window over nothing is worse than a sentence: `no keys yet` inside a
    /// modal is a box that has to be closed before work can continue.
    pub fn has_content(&self) -> bool {
        self.rows.iter().any(|row| !row.header)
    }

    /// The transcript line that stands in for the full table.
    ///
    /// The window is the answer; this says what it is showing, so the session
    /// file still reads as a conversation.
    pub fn transcript_line(&self) -> String {
        let body = match self.note.as_deref() {
            Some(note) => note.to_string(),
            None => format!(
                "{} rows",
                self.rows.iter().filter(|row| !row.header).count()
            ),
        };
        format!("{} · {body}", self.title.to_lowercase())
    }
}

/// Where the selected row is, in a list where headers cannot be selected.
fn step(rows: &[PopoverRow], from: usize, forward: bool) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let mut index = from;
    for _ in 0..rows.len() {
        index = if forward {
            (index + 1) % rows.len()
        } else if index == 0 {
            rows.len() - 1
        } else {
            index - 1
        };
        if !rows[index].header {
            return index;
        }
    }
    from
}

/// The first selectable row, or 0.
fn first_row(rows: &[PopoverRow]) -> usize {
    rows.iter().position(|row| !row.header).unwrap_or(0)
}

/// The last selectable row, or 0.
fn last_row(rows: &[PopoverRow]) -> usize {
    rows.iter().rposition(|row| !row.header).unwrap_or(0)
}

/// The window.
#[derive(Debug)]
pub struct PopoverOverlay {
    visible: bool,
    spec: PopoverSpec,
    selected: usize,
    scroll: usize,
    /// How many rows the window last drew. A key can only keep the selection
    /// inside the rows it can see if it knows how many there were.
    viewport: usize,
    /// What the last key did, so a key that cannot act says so instead of
    /// doing nothing at all.
    status: Option<String>,
}

impl PopoverOverlay {
    pub fn new(spec: PopoverSpec) -> Self {
        let selected = first_row(&spec.rows);
        Self {
            visible: false,
            spec,
            selected,
            scroll: 0,
            viewport: 0,
            status: None,
        }
    }

    /// The window a verb's answer opens, or `None` when a sentence is the honest
    /// rendering.
    ///
    /// Dispatched on the dotted command name — the same string a script branches
    /// on — so a verb that gains rows here cannot silently disagree with the verb
    /// that produced them.
    pub fn for_answer(command: &str, data: &Value) -> Option<PopoverSpec> {
        spec_for(command, data).filter(PopoverSpec::has_content)
    }

    fn move_to(&mut self, index: usize, height: usize) {
        self.selected = index;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if height > 0 && self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
        self.status = None;
    }

    fn act(&mut self, action: PopoverAction, app: &mut App) -> OverlayAction {
        match action {
            PopoverAction::Run(line) => {
                app.input = line;
                app.cursor = app.input.chars().count();
                app.pending_submit = true;
                OverlayAction::Close
            }
            PopoverAction::Open(url) => match crate::auth::open_browser(&url) {
                Ok(()) => {
                    self.status = Some(format!("opened {url}"));
                    OverlayAction::None
                }
                Err(error) => {
                    // The browser is not guaranteed: no `xdg-open`, no display.
                    // The URL is still the useful part, so it stays on screen.
                    self.status = Some(format!("could not open a browser ({error}) — {url}"));
                    OverlayAction::None
                }
            },
        }
    }

    /// The line the footer button for the selected row describes.
    fn selected_action(&self) -> Option<&PopoverAction> {
        self.spec
            .rows
            .get(self.selected)
            .filter(|row| !row.header)
            .and_then(|row| row.action.as_ref())
    }

    /// The footer, as chips.
    fn footer(&self) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let chip = |text: String, enabled: bool| {
            Span::styled(
                format!(" {text} "),
                if enabled {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM)
                },
            )
        };

        match self.spec.rows.get(self.selected).filter(|row| !row.header) {
            Some(row) => {
                let label = match &row.action {
                    Some(PopoverAction::Run(_)) => "run",
                    Some(PopoverAction::Open(_)) => "open",
                    None => "nothing to run",
                };
                spans.push(chip(format!("⏎ {label}"), row.action.is_some()));
            }
            None => spans.push(chip("⏎ —".to_string(), false)),
        }
        for button in &self.spec.buttons {
            spans.push(Span::raw(" "));
            spans.push(chip(format!("{} {}", button.key, button.label), true));
        }
        spans.push(Span::raw(" "));
        spans.push(chip("esc close".to_string(), true));
        Line::from(spans)
    }
}

impl Overlay for PopoverOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Popover
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.selected = first_row(&self.spec.rows);
        self.scroll = 0;
        self.status = None;
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let rows = self.spec.rows.len();
        match key.code {
            KeyCode::Esc => OverlayAction::Close,
            // This window has no filter field, so every letter that is not a
            // button is free to be navigation.
            KeyCode::Up | KeyCode::Char('k') if !ctrl => {
                let next = step(&self.spec.rows, self.selected, false);
                self.move_to(next, self.viewport);
                OverlayAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if rows == 0 {
                    self.status = Some("nothing in this window".to_string());
                    return OverlayAction::None;
                }
                let next = step(&self.spec.rows, self.selected, true);
                self.move_to(next, self.viewport);
                OverlayAction::None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                let next = first_row(&self.spec.rows);
                self.selected = next;
                self.scroll = 0;
                OverlayAction::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.selected = last_row(&self.spec.rows);
                OverlayAction::None
            }
            KeyCode::PageDown => {
                let mut next = self.selected;
                for _ in 0..5 {
                    next = step(&self.spec.rows, next, true);
                }
                self.move_to(next, self.viewport);
                OverlayAction::None
            }
            KeyCode::PageUp => {
                let mut next = self.selected;
                for _ in 0..5 {
                    next = step(&self.spec.rows, next, false);
                }
                self.move_to(next, self.viewport);
                OverlayAction::None
            }
            KeyCode::Enter => match self.selected_action().cloned() {
                Some(action) => self.act(action, app),
                None => {
                    // A button that looks dead but says nothing is the thing this
                    // window exists to avoid: say which row has no action.
                    self.status =
                        Some("this row has nothing to run — j/k moves, esc closes".to_string());
                    OverlayAction::None
                }
            },
            KeyCode::Char(ch) if !ctrl => {
                let button = self
                    .spec
                    .buttons
                    .iter()
                    .find(|button| button.key == ch)
                    .cloned();
                match button {
                    Some(button) => self.act(button.action, app),
                    None => {
                        self.status = Some(if self.spec.buttons.is_empty() {
                            format!("'{ch}' is not a key here — enter, j/k, esc")
                        } else {
                            let keys: Vec<String> = self
                                .spec
                                .buttons
                                .iter()
                                .map(|button| button.key.to_string())
                                .collect();
                            format!("'{ch}' is not a key here — enter, {}, esc", keys.join("/"))
                        });
                        OverlayAction::None
                    }
                }
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        // A window, not a full screen: the session behind it stays readable, so
        // closing it is a choice rather than an escape.
        let width = area.width.saturating_sub(6).clamp(24, 96);
        let height = (self.spec.rows.len() as u16 + 6)
            .min(area.height.saturating_sub(4))
            .max(7);
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let window = Rect::new(x, y, width, height);
        frame.render_widget(Clear, window);

        let title = format!(" {} ", self.spec.title);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Left);
        let inner = block.inner(window);
        frame.render_widget(block, window);
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // The footer is measured first — a note, what the last key said, and the
        // button chips — so the rows get the rest of the window and nothing is
        // ever drawn over anything.
        let mut footer: Vec<Line> = Vec::new();
        if let Some(note) = &self.spec.note {
            footer.push(Line::from(Span::styled(
                note.clone(),
                Style::default().fg(Color::DarkGray),
            )));
        }
        if let Some(status) = &self.status {
            footer.push(Line::from(Span::styled(
                status.clone(),
                Style::default().fg(Color::Yellow),
            )));
        }
        footer.push(self.footer());
        let footer_height = (footer.len() as u16).min(inner.height);

        let rows_area = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(footer_height),
        );
        let visible = rows_area.height as usize;
        self.viewport = visible;

        // Keep the cursor inside the window: scrolling follows selection, and the
        // selection never leaves the rows it can see. `handle_key` already does
        // this with the last known viewport; this is the same rule applied to the
        // height the window actually got this frame.
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if visible > 0 && self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }

        let mut lines: Vec<Line> = Vec::new();
        if !self.spec.subtitle.is_empty() {
            lines.push(Line::from(Span::styled(
                self.spec.subtitle.clone(),
                Style::default().fg(Color::DarkGray),
            )));
        }

        let width = inner.width as usize;
        for (index, row) in self
            .spec
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(visible.saturating_sub(lines.len()))
        {
            if row.header {
                lines.push(Line::from(Span::styled(
                    format!(" {}", row.title.to_uppercase()),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                continue;
            }

            let selected = index == self.selected;
            let marker = if selected { "› " } else { "  " };
            let mut spans = vec![Span::styled(
                marker.to_string(),
                if selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                },
            )];
            spans.push(Span::styled(
                row.title.clone(),
                if selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                },
            ));
            if let Some(tag) = &row.tag {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!(" {tag} "),
                    Style::default().fg(Color::Black).bg(if tag == "call" {
                        Color::Green
                    } else {
                        Color::DarkGray
                    }),
                ));
            }
            // One row is one row: a long path is cut, never wrapped into a second
            // line that looks like another entry.
            let used = marker.len() + row.title.chars().count() + 2;
            let room = width.saturating_sub(used + 1);
            if !row.detail.is_empty() && room > 8 {
                let detail: String = row.detail.chars().take(room).collect();
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    detail,
                    Style::default().fg(if selected {
                        Color::Gray
                    } else {
                        Color::DarkGray
                    }),
                ));
            }
            if selected {
                let fill = width.saturating_sub(
                    spans
                        .iter()
                        .map(|span| span.content.chars().count())
                        .sum::<usize>(),
                );
                spans.push(Span::raw(" ".repeat(fill)));
                lines.push(Line::from(spans).style(Style::default().bg(Color::Indexed(236))));
            } else {
                lines.push(Line::from(spans));
            }
        }

        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "nothing to show",
                Style::default().fg(Color::DarkGray),
            )));
        }

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rows_area);

        let footer_area = Rect::new(
            inner.x,
            inner.bottom().saturating_sub(footer_height),
            inner.width,
            footer_height,
        );
        if footer_area.height > 0 {
            frame.render_widget(
                Paragraph::new(footer).wrap(Wrap { trim: false }),
                footer_area,
            );
        }
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("up/down", "move the selection"),
            ("enter", "run what the selected row points at"),
            ("buttons", "the chips on the window's last line"),
            ("esc", "close the window"),
        ]
    }
}

/* --------------------------------- specs ---------------------------------- */

/// One verb's answer as a window, or `None` when prose is the right rendering.
///
/// Only the verbs whose answer is *a list of things* are here. A status line, a
/// diff, a token minted once: those stay text, because a window around one line
/// is a modal for no reason.
pub fn spec_for(command: &str, data: &Value) -> Option<PopoverSpec> {
    match command {
        "teams.list" => Some(teams_list(data)),
        "teams.members" => Some(teams_members(data)),
        "keys.list" => Some(keys_list(data)),
        "org.members" => Some(org_members(data)),
        "org.invites" => Some(org_invites(data)),
        "billing.plan" => Some(billing_plan(data)),
        "billing.limits" => Some(billing_limits(data)),
        "billing.invoices" => Some(billing_invoices(data)),
        "kg.refs" => Some(refs(data)),
        "kg.impact" => Some(impact(data)),
        "kg.callers" => Some(hits_window(data, "kg.callers", "Callers", "callers")),
        "kg.callees" => Some(hits_window(data, "kg.callees", "Callees", "callees")),
        "kg.search" => Some(search(data)),
        "kg.dead_code" => Some(dead_code(data)),
        // The families that live in their own module, then the shape every other
        // verb already answers with. A verb with a bespoke spec keeps it; a verb
        // that only has rows gets the generic window, which draws its own fields
        // and offers its own follow-ups instead of dumping a table into chat.
        other => {
            crate::tui::overlays::popover_specs::spec(other, data).or_else(|| generic(other, data))
        }
    }
}

/// The first string-ish field among `keys`.
fn first_field(item: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = present(item, key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Prefix a heading with the verb it came from, so a window is never anonymous.
pub(crate) fn heading(command: &str) -> String {
    let word = command
        .rsplit('.')
        .next()
        .unwrap_or(command)
        .replace('_', " ");
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => command.to_string(),
    }
}

/// The follow-ups a verb named, as footer buttons.
///
/// `next_commands` is the verb's own list of what to do next, so these chips are
/// the CLI's advice rather than the window's guess. Digits are used as keys
/// because `j`/`k`/`g`/`G` are navigation here.
fn follows(data: &Value) -> Vec<PopoverButton> {
    let mut buttons = Vec::new();
    for (index, line) in crate::output::next_commands(data).into_iter().enumerate() {
        if index >= 6 {
            break;
        }
        let key = char::from_digit((index + 1) as u32, 10).unwrap_or('0');
        if buttons
            .iter()
            .any(|button: &PopoverButton| button.key == key)
        {
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let label = match line.strip_prefix("memnoth ") {
            Some(rest) => format!("/{rest}"),
            None => line.to_string(),
        };
        let label = if label.chars().count() > 22 {
            let cut: String = label.chars().take(21).collect();
            format!("{cut}…")
        } else {
            label
        };
        buttons.push(PopoverButton::run(key, label, line));
    }
    buttons
}

/// Whether a string is worth offering as a symbol to explore.
fn symbol_like(text: &str) -> bool {
    !text.is_empty()
        && text.chars().count() <= 80
        && !text.contains('/')
        && !text.contains(' ')
        && !text.ends_with('.')
}

/// Any verb's list, drawn from the machine shape the verb already answers with.
///
/// This is a fallback, not a second implementation: the rows are the verb's own
/// fields, and the buttons are the follow-ups the verb itself names. A verb whose
/// answer has no list of objects stays text — a window around one line is a modal
/// for no reason.
fn generic(command: &str, data: &Value) -> Option<PopoverSpec> {
    let object = data.as_object()?;
    let (_, items) = object
        .iter()
        .filter_map(|(key, value)| value.as_array().map(|items| (key, items)))
        .filter(|(_, items)| items.iter().any(Value::is_object))
        .max_by_key(|(_, items)| items.len())?;

    let rows: Vec<PopoverRow> = items
        .iter()
        .filter_map(|item| {
            if !item.is_object() {
                return None;
            }
            let title = first_field(
                item,
                &[
                    "name", "symbol", "title", "path", "file", "command", "id", "email", "branch",
                ],
            )?;
            let mut parts: Vec<String> = Vec::new();
            if let Some(kind) = first_field(
                item,
                &[
                    "kind",
                    "change_type",
                    "status",
                    "source",
                    "scope",
                    "coverage_type",
                ],
            ) {
                parts.push(kind);
            }
            if let Some(where_) = first_field(item, &["path", "file", "branch", "dir"])
                .filter(|value| *value != title)
            {
                let line = item.get("line").and_then(Value::as_i64);
                parts.push(match line {
                    Some(line) if line > 0 => format!("{where_}:{line}"),
                    _ => where_,
                });
            }
            if let Some(count) = item
                .get("count")
                .or_else(|| item.get("total"))
                .and_then(Value::as_i64)
            {
                parts.push(count.to_string());
            }
            let mut row = PopoverRow::new(title.clone(), parts.join(" · "));
            if command.starts_with("kg.") && symbol_like(&title) {
                row = row.running(format!("/kg explore {title}"));
            }
            Some(row)
        })
        .collect();

    if rows.is_empty() {
        return None;
    }

    let mut spec = PopoverSpec::new(
        format!("{} ({})", heading(command), rows.len()),
        format!("from {command}"),
    )
    .rows(rows);

    if data
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        spec = spec.note("truncated — narrow the query for the rest");
    }
    for button in follows(data) {
        spec = spec.button(button);
    }
    Some(spec)
}

pub(crate) fn string(data: &Value, key: &str) -> String {
    match data.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

/// A field that is only worth drawing when it is present.
pub(crate) fn present(data: &Value, key: &str) -> Option<String> {
    match data.get(key) {
        Some(Value::Null) | None => None,
        Some(Value::String(text)) if text.trim().is_empty() => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => Some(other.to_string()),
    }
}

pub(crate) fn list<'a>(data: &'a Value, key: &str) -> &'a [Value] {
    data.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// The follow-ups a verb named, for a bespoke spec's footer.
pub(crate) fn follow_buttons(data: &Value) -> Vec<PopoverButton> {
    follows(data)
}

/// `3 members · 2 projects`, skipping the zeroes.
fn counts(pairs: &[(i64, &str)]) -> String {
    pairs
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, noun)| format!("{count} {noun}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn teams_list(data: &Value) -> PopoverSpec {
    let teams = list(data, "teams");
    let rows = teams
        .iter()
        .map(|team| {
            let mut detail = counts(&[
                (
                    team.get("member_count")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    "members",
                ),
                (
                    team.get("project_count")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    "projects",
                ),
            ]);
            if detail.is_empty() {
                detail = "empty".to_string();
            }
            if team
                .get("is_member")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                detail.push_str(" · yours");
            }
            if team
                .get("can_manage")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                detail.push_str(" · can manage");
            }
            let id = string(team, "id");
            let mut row = PopoverRow::new(string(team, "name"), detail);
            if !id.is_empty() {
                row = row.running(format!("/teams members {id}"));
                row.detail.push_str(&format!(" · {id}"));
            }
            row
        })
        .collect::<Vec<_>>();

    PopoverSpec::new(
        format!("Teams ({})", rows.len()),
        "enter opens a team's people",
    )
    .rows(rows)
    .button(PopoverButton::open(
        'u',
        "console",
        crate::context::console_url("teams"),
    ))
}

fn teams_members(data: &Value) -> PopoverSpec {
    let team = data.get("team").cloned().unwrap_or(Value::Null);
    let name = string(&team, "name");
    let mut rows = Vec::new();

    let members = list(data, "members");
    rows.push(PopoverRow::section(format!("people ({})", members.len())));
    rows.extend(members.iter().map(|member| {
        let role = string(member, "org_role");
        let mut detail = present(member, "email").unwrap_or_default();
        if !role.is_empty() {
            detail.push_str(&format!(" · {role}"));
        }
        PopoverRow::new(
            present(member, "name").unwrap_or_else(|| string(member, "user_id")),
            detail,
        )
    }));

    let projects = list(data, "projects");
    rows.push(PopoverRow::section(format!(
        "projects ({})",
        projects.len()
    )));
    rows.extend(projects.iter().map(|project| {
        PopoverRow::new(
            string(project, "name"),
            format!("access {}", string(project, "level")),
        )
    }));

    let note = if data
        .get("can_manage")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "you can manage this team".to_string()
    } else {
        "read-only: managing this team needs an org admin".to_string()
    };
    PopoverSpec::new(
        format!("Team: {}", if name.is_empty() { "?" } else { &name }),
        "everyone on it, and what it reaches",
    )
    .rows(rows)
    .note(note)
    .button(PopoverButton::open(
        'u',
        "console",
        crate::context::console_url("teams"),
    ))
}

fn keys_list(data: &Value) -> PopoverSpec {
    let keys = list(data, "keys");
    let live: Vec<&Value> = keys.iter().filter(|key| !is_revoked(key)).collect();
    let revoked: Vec<&Value> = keys.iter().filter(|key| is_revoked(key)).collect();

    let mut rows = varchar(&live, "live", false);
    rows.extend(varchar(&revoked, "revoked", true));

    PopoverSpec::new(
        format!("API keys ({})", keys.len()),
        "a key's secret is shown once, when it is minted",
    )
    .rows(rows)
    .button(PopoverButton::open(
        'u',
        "console",
        crate::context::console_url("api-keys"),
    ))
    .button(PopoverButton::run('c', "create", "/keys create"))
}

fn is_revoked(key: &Value) -> bool {
    match key.get("revoked_at") {
        Some(Value::Null) | None => false,
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(_) => true,
    }
}

/// Key rows: the name, its prefix, and when it was last used.
fn varchar(keys: &[&Value], section: &str, revoked: bool) -> Vec<PopoverRow> {
    let mut rows = vec![PopoverRow::section(format!("{section} ({})", keys.len()))];
    if keys.is_empty() {
        return rows;
    }
    for key in keys {
        let prefix = string(key, "prefix");
        let mut detail = format!("{prefix}… · {}", string(key, "scope"));
        if let Some(last) = present(key, "last_used_at") {
            detail.push_str(&format!(" · used {last}"));
        } else if !revoked {
            detail.push_str(" · never used");
        }
        if revoked && let Some(at) = present(key, "revoked_at") {
            detail.push_str(&format!(" · revoked {at}"));
        }
        let mut row = PopoverRow::new(string(key, "name"), detail);
        if revoked {
            row = row.tagged("revoked");
        }
        rows.push(row);
    }
    rows
}

fn org_members(data: &Value) -> PopoverSpec {
    let members = list(data, "members");
    let rows = members
        .iter()
        .map(|member| {
            let mut detail = present(member, "email").unwrap_or_else(|| string(member, "user_id"));
            detail.push_str(&format!(" · {}", string(member, "role")));
            if let Some(joined) = present(member, "joined_at") {
                detail.push_str(&format!(" · since {joined}"));
            }
            let mut row = PopoverRow::new(
                present(member, "name").unwrap_or_else(|| "unnamed".to_string()),
                detail,
            );
            if member
                .get("primary")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                row = row.tagged("owner");
            }
            row
        })
        .collect::<Vec<_>>();

    PopoverSpec::new(format!("Members ({})", rows.len()), "the org's roster")
        .rows(rows)
        .button(PopoverButton::open(
            'u',
            "console",
            crate::context::console_url("org"),
        ))
}

fn org_invites(data: &Value) -> PopoverSpec {
    let invites = list(data, "invites");
    let rows = invites
        .iter()
        .map(|invite| {
            let mut detail = format!(
                "{} · expires {}",
                string(invite, "role"),
                string(invite, "expires_at")
            );
            if let Some(by) = present(invite, "invited_by_email") {
                detail.push_str(&format!(" · by {by}"));
            }
            PopoverRow::new(string(invite, "email"), detail).tagged("pending")
        })
        .collect::<Vec<_>>();

    PopoverSpec::new(
        format!("Pending invites ({})", rows.len()),
        "nobody has accepted these yet",
    )
    .rows(rows)
    .button(PopoverButton::run('i', "invite", "/org invite"))
    .button(PopoverButton::open(
        'u',
        "console",
        crate::context::console_url("org"),
    ))
}

/// Money in this file is micro-units, as the control plane reports it.
fn money(micros: i64) -> String {
    format!("${:.2}", micros as f64 / 1_000_000.0)
}

fn billing_plan(data: &Value) -> PopoverSpec {
    let org = data.get("org").cloned().unwrap_or(Value::Null);
    let mut rows = Vec::new();
    if let Some(plan) = present(&org, "plan") {
        rows.push(PopoverRow::new("plan", plan));
    }
    if let Some(role) = present(&org, "role") {
        rows.push(PopoverRow::new("your role", role));
    }
    rows.push(PopoverRow::new(
        "seats",
        format!(
            "{} of {}",
            present(&org, "seats_used").unwrap_or_else(|| "0".into()),
            present(&org, "seats").unwrap_or_else(|| "unlimited".into())
        ),
    ));
    rows.push(PopoverRow::new(
        "people",
        format!(
            "{} members",
            present(&org, "member_count").unwrap_or_else(|| "0".into())
        ),
    ));

    if let Some(credits) = data.get("credits").filter(|value| !value.is_null()) {
        let balance = credits
            .get("balance_micros")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        rows.push(PopoverRow::new("credit balance", money(balance)));
        let burned = credits
            .get("burned_micros")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let window = credits
            .get("window_days")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if window > 0 {
            rows.push(PopoverRow::new(
                format!("spent (last {window} days)"),
                money(burned),
            ));
        }
    }

    PopoverSpec::new(
        format!(
            "Plan: {}",
            present(&org, "name").unwrap_or_else(|| "this org".into())
        ),
        "as the control plane reports it",
    )
    .rows(rows)
    .button(PopoverButton::open(
        'u',
        "usage",
        crate::context::console_url("usage"),
    ))
}

fn billing_limits(data: &Value) -> PopoverSpec {
    let quotas = data.get("quotas").cloned().unwrap_or(Value::Null);
    let mut rows = vec![
        PopoverRow::new(
            "seats",
            format!(
                "{} of {}",
                present(&quotas, "seats_used").unwrap_or_else(|| "0".into()),
                present(&quotas, "seats").unwrap_or_else(|| "unlimited".into())
            ),
        ),
        PopoverRow::new(
            "members",
            present(&quotas, "member_count").unwrap_or_else(|| "0".into()),
        ),
        PopoverRow::new(
            "teams",
            present(&quotas, "team_count").unwrap_or_else(|| "0".into()),
        ),
        PopoverRow::new(
            "projects",
            present(&quotas, "project_count").unwrap_or_else(|| "0".into()),
        ),
    ];
    if let Some(credits) = data.get("credits").filter(|value| !value.is_null()) {
        rows.push(PopoverRow::new(
            "credit balance",
            money(
                credits
                    .get("balance_micros")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            ),
        ));
        let expiring = credits
            .get("expiring_micros")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if expiring > 0 {
            rows.push(PopoverRow::new("expiring soon", money(expiring)));
        }
    }

    PopoverSpec::new("Quotas", "what this deployment counts")
        .rows(rows)
        .note("this deployment reports no request or token rate limits")
        .button(PopoverButton::open(
            'u',
            "usage",
            crate::context::console_url("usage"),
        ))
}

fn billing_invoices(data: &Value) -> PopoverSpec {
    let mut rows = Vec::new();
    if let Some(credits) = data.get("credits").filter(|value| !value.is_null()) {
        rows.push(PopoverRow::new(
            "balance",
            money(
                credits
                    .get("balance_micros")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            ),
        ));
        let burned = credits
            .get("burned_micros")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let window = credits
            .get("window_days")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if window > 0 {
            rows.push(PopoverRow::new(
                format!("spent (last {window} days)"),
                money(burned),
            ));
        }
        let expiring = credits
            .get("expiring_micros")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if expiring > 0 {
            let mut detail = money(expiring);
            if let Some(at) = present(credits, "next_expiry_at") {
                detail.push_str(&format!(" · {at}"));
            }
            rows.push(PopoverRow::new("expiring", detail));
        }
        if credits
            .get("demo")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            rows.push(PopoverRow::new("demo", "credits are simulated"));
        }
    }
    let url = present(data, "billing_url").unwrap_or_else(|| crate::context::console_url("usage"));

    PopoverSpec::new("Billing", "invoices live in the console")
        .rows(rows)
        .button(PopoverButton::open('u', "console", url))
}

/// One reference row: where it is, and whether the line calls it or mentions it.
fn hit_row(hit: &Value) -> PopoverRow {
    let path = string(hit, "path");
    let line = hit.get("line").and_then(Value::as_i64).unwrap_or(0);
    let call_shaped = hit
        .get("call_shaped")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let context = string(hit, "context");
    let row = PopoverRow::new(
        format!("{path}:{line}"),
        if context.is_empty() {
            string(hit, "kind")
        } else {
            context
        },
    );
    if call_shaped {
        row.tagged("call")
    } else {
        row.tagged("mention")
    }
}

fn refs(data: &Value) -> PopoverSpec {
    let symbol = string(data, "symbol");
    let references = list(data, "references");
    let calls = references.iter().filter(|hit| is_call(hit)).count();
    let rows: Vec<PopoverRow> = references.iter().map(hit_row).collect();

    let note = if data
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        format!(
            "{calls} call-shaped of {} shown · list truncated",
            references.len()
        )
    } else {
        format!("{calls} call-shaped of {}", references.len())
    };
    PopoverSpec::new(
        format!("Refs: {symbol}"),
        "call = the line calls it · mention = a type, a re-export, prose",
    )
    .rows(rows)
    .note(note)
    .button(PopoverButton::run('d', "def", format!("/kg def {symbol}")))
    .button(PopoverButton::run(
        'i',
        "impact",
        format!("/kg impact {symbol}"),
    ))
}

fn is_call(hit: &Value) -> bool {
    hit.get("call_shaped")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn impact(data: &Value) -> PopoverSpec {
    let symbol = string(data, "symbol");
    let mut rows = Vec::new();

    let definitions = list(data, "definitions");
    if !definitions.is_empty() {
        rows.push(PopoverRow::section(format!(
            "definitions ({})",
            definitions.len()
        )));
        rows.extend(definitions.iter().map(hit_row));
    }

    let references = list(data, "references");
    rows.push(PopoverRow::section(format!(
        "references ({} direct · {} second-hop)",
        data.get("references_direct")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        data.get("references_transitive")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    )));
    rows.extend(references.iter().map(hit_row));

    let mut note = format!(
        "{} files · {} call-shaped",
        data.get("files").and_then(Value::as_i64).unwrap_or(0),
        references.iter().filter(|hit| is_call(hit)).count()
    );
    let tests = list(data, "tests");
    if !tests.is_empty() {
        note.push_str(&format!(" · {} test files", tests.len()));
    }
    if data
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        note.push_str(" · truncated");
    }

    PopoverSpec::new(format!("Impact: {symbol}"), "what a change here reaches")
        .rows(rows)
        .note(note)
        .button(PopoverButton::run('d', "def", format!("/kg def {symbol}")))
        .button(PopoverButton::run(
            'r',
            "refs",
            format!("/kg refs {symbol}"),
        ))
}

/// Callers, callees, refs: the same list under three names.
fn hits_window(data: &Value, _command: &str, title: &str, key: &str) -> PopoverSpec {
    let symbol = string(data, "symbol");
    let hits = list(data, key);
    let rows: Vec<PopoverRow> = hits.iter().map(hit_row).collect();
    let calls = hits.iter().filter(|hit| is_call(hit)).count();
    PopoverSpec::new(
        format!("{title}: {symbol}"),
        "call = a call site · mention = an incidental reference",
    )
    .rows(rows)
    .note(format!("{calls} call-shaped of {}", hits.len()))
    .button(PopoverButton::run('d', "def", format!("/kg def {symbol}")))
}

fn search(data: &Value) -> PopoverSpec {
    let query = string(data, "query");
    // `kg.search` emits plain `SymbolHit`s: no call/mention split, because a
    // search match is a declaration, not an edge.
    let hits = list(data, "hits");
    let rows = hits
        .iter()
        .map(|hit| {
            let name = string(hit, "name");
            let location = format!("{}:{}", string(hit, "path"), string(hit, "line"));
            PopoverRow::new(name.clone(), location)
                .tagged(string(hit, "kind"))
                .running(format!("/kg def {name}"))
        })
        .collect::<Vec<_>>();

    let count = rows.len();
    PopoverSpec::new(
        if query.is_empty() {
            format!("Search ({count})")
        } else {
            format!("Search: {query}")
        },
        "enter shows the declaration",
    )
    .rows(rows)
    .note(format!("{count} symbols"))
}

fn dead_code(data: &Value) -> PopoverSpec {
    let exports = list(data, "exports");
    let rows = exports
        .iter()
        .map(|export| {
            let name = string(export, "name");
            let mut detail = format!("{}:{}", string(export, "file"), string(export, "line"));
            if export
                .get("is_public")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                detail.push_str(" · public");
            }
            PopoverRow::new(name.clone(), detail)
                .tagged(string(export, "kind"))
                .running(format!("/kg def {name}"))
        })
        .collect::<Vec<_>>();

    PopoverSpec::new(
        format!("Dead code ({})", rows.len()),
        "nothing outside the declaring file reaches these",
    )
    .rows(rows)
    .note("a `pub mod` used only inside its own file still counts as an export")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent};

    fn app() -> App {
        App::new("s", false)
    }

    /// The teams list as `mn teams list --json` emits it.
    fn teams_answer() -> Value {
        serde_json::json!({
            "teams": [
                { "id": "tm-1", "name": "platform", "member_count": 3, "project_count": 2,
                  "is_member": true, "can_manage": false },
                { "id": "tm-2", "name": "design", "member_count": 1, "project_count": 0,
                  "is_member": false, "can_manage": false }
            ]
        })
    }

    #[test]
    fn the_teams_list_opens_a_window_whose_rows_run_the_real_verb() {
        let spec = PopoverOverlay::for_answer("teams.list", &teams_answer()).expect("a spec");
        assert_eq!(spec.title, "Teams (2)");
        assert!(spec.rows[0].detail.contains("3 members"));
        assert_eq!(
            spec.rows[0].action,
            Some(PopoverAction::Run("/teams members tm-1".into()))
        );
        assert_eq!(spec.transcript_line(), "teams (2) · 2 rows");
    }

    #[test]
    fn an_empty_list_is_a_sentence_not_a_modal() {
        assert!(
            PopoverOverlay::for_answer("teams.list", &serde_json::json!({ "teams": [] })).is_none()
        );
        assert!(PopoverOverlay::for_answer("kg.status", &serde_json::json!({})).is_none());
    }

    #[test]
    fn enter_on_a_row_handed_the_line_to_the_prompt_and_closed() {
        let mut app = app();
        let mut overlay =
            PopoverOverlay::new(PopoverOverlay::for_answer("teams.list", &teams_answer()).unwrap());
        overlay.show();

        let action = overlay.handle_key(KeyEvent::from(KeyCode::Enter), &mut app);
        assert_eq!(action, OverlayAction::Close);
        assert_eq!(app.input, "/teams members tm-1");
        assert!(
            app.pending_submit,
            "the row runs the verb, not just fills it"
        );
    }

    #[test]
    fn a_section_divider_is_drawn_and_never_selected() {
        let data = serde_json::json!({
            "team": { "name": "platform" },
            "members": [{ "email": "a@example.com", "name": "A", "org_role": "owner" }],
            "projects": [{ "name": "site", "level": "write" }],
            "can_manage": true
        });
        let mut app = app();
        let mut overlay =
            PopoverOverlay::new(PopoverOverlay::for_answer("teams.members", &data).unwrap());
        overlay.show();
        assert!(overlay.spec.rows[0].header);

        overlay.handle_key(KeyEvent::from(KeyCode::Down), &mut app);
        assert!(
            !overlay.spec.rows[overlay.selected].header,
            "down skips the divider"
        );
        overlay.handle_key(KeyEvent::from(KeyCode::Enter), &mut app);
        // A member row has nothing to run, and says so instead of doing nothing.
        assert!(
            overlay
                .status
                .as_deref()
                .unwrap()
                .contains("nothing to run")
        );
    }

    #[test]
    fn refs_separate_a_call_site_from_a_mention() {
        let data = serde_json::json!({
            "symbol": "sync",
            "resolved": true,
            "truncated": false,
            "references": [
                { "path": "src/a.rs", "line": 4, "kind": "call", "call_shaped": true,
                  "context": "sync(ctx)?" },
                { "path": "src/b.rs", "line": 9, "kind": "reference", "call_shaped": false,
                  "context": "let f: fn() = sync;" }
            ]
        });
        let spec = PopoverOverlay::for_answer("kg.refs", &data).unwrap();
        assert_eq!(spec.rows[0].tag.as_deref(), Some("call"));
        assert_eq!(spec.rows[1].tag.as_deref(), Some("mention"));
        assert_eq!(spec.note.as_deref(), Some("1 call-shaped of 2"));
    }

    #[test]
    fn a_key_that_is_not_bound_says_which_ones_are() {
        let mut app = app();
        let mut overlay =
            PopoverOverlay::new(PopoverOverlay::for_answer("teams.list", &teams_answer()).unwrap());
        overlay.show();
        overlay.handle_key(KeyEvent::from(KeyCode::Char('z')), &mut app);
        let status = overlay.status.as_deref().unwrap();
        assert!(status.contains('u'), "{status}");
        assert!(status.contains("esc"), "{status}");
    }

    #[test]
    fn scrolling_keeps_the_selection_inside_the_window() {
        let teams: Vec<Value> = (0..40)
            .map(|index| serde_json::json!({ "id": format!("tm-{index}"), "name": format!("t{index}") }))
            .collect();
        let data = serde_json::json!({ "teams": teams });
        let mut overlay =
            PopoverOverlay::new(PopoverOverlay::for_answer("teams.list", &data).unwrap());
        overlay.show();
        // A window that shows ten rows: the count `handle_key` works from.
        overlay.viewport = 10;
        let mut app = app();
        for _ in 0..30 {
            overlay.handle_key(KeyEvent::from(KeyCode::Down), &mut app);
        }
        assert!(overlay.scroll > 0, "the window followed the selection");
    }

    #[test]
    fn keys_are_split_into_live_and_revoked_with_the_secret_absent() {
        let data = serde_json::json!({
            "keys": [
                { "id": "k1", "name": "deploy", "prefix": "mn_live_abc", "scope": "write",
                  "revoked_at": null, "last_used_at": "2026-11-02T18:30:00Z" },
                { "id": "k2", "name": "old", "prefix": "mn_live_def", "scope": "read",
                  "revoked_at": "2026-10-01T00:00:00Z", "last_used_at": null }
            ]
        });
        let spec = PopoverOverlay::for_answer("keys.list", &data).unwrap();
        // Stored as written; the window upper-cases a divider when it draws it.
        assert_eq!(spec.rows[0].title, "live (1)");
        assert!(spec.rows[0].header, "the divider is not a key");
        // live divider, the live key, revoked divider, the revoked key.
        assert_eq!(spec.rows[2].title, "revoked (1)");
        assert_eq!(spec.rows[3].tag.as_deref(), Some("revoked"));
        let drawn = spec
            .rows
            .iter()
            .map(|row| format!("{} {}", row.title, row.detail))
            .collect::<String>();
        assert!(!drawn.contains("token"), "{drawn}");
    }
}
