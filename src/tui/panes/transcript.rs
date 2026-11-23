//! Transcript pane: the main conversation view with scroll and follow modes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use crate::tui::app::{App, Entry, EntryRole};
use crate::tui::theme::tokens;

/// One entry's recorded time as `HH:MM`.
///
/// Local time, because this is read next to a wall clock rather than compared
/// across machines. A negative or zero `at_ms` (a session header with no time)
/// clamps to the epoch rather than panicking on a `Duration`.
fn clock(at_ms: i64) -> String {
    chrono::DateTime::<chrono::Local>::from(
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(at_ms.max(0) as u64),
    )
    .format("%H:%M")
    .to_string()
}

/// Whether a role is one of the four tool-call states.
fn is_tool_role(role: EntryRole) -> bool {
    matches!(
        role,
        EntryRole::Tool | EntryRole::ToolDone | EntryRole::ToolFailed | EntryRole::ToolInterrupted
    )
}

/// The state word a tool row carries in plain text.
///
/// It is part of the grammar, not decoration: under `NO_COLOR` every token
/// resolves to `Reset`, so the word is the only thing that says what happened.
fn state_word(role: EntryRole) -> &'static str {
    match role {
        EntryRole::Tool => "running",
        EntryRole::ToolDone => "completed",
        EntryRole::ToolFailed => "failed",
        EntryRole::ToolInterrupted => "interrupted",
        _ => "tool",
    }
}

/// The call portion of a tool entry, with any recorded state suffix removed.
///
/// `finish_tool_call` appends ` · completed · N chars` (or the failed variant)
/// to the call text; `interrupt_tool_calls` appends ` · interrupted …`. Those
/// words are *state*, not arguments, so they must not survive into the dedupe
/// key or the one-line summary.
fn tool_call_identity(text: &str) -> String {
    for marker in [" · completed · ", " · failed · ", " · interrupted"] {
        if let Some((call, _)) = text.split_once(marker) {
            return call.trim().to_string();
        }
    }
    text.trim().to_string()
}

/// Split a call into its tool name and the arguments that follow it.
fn split_call(call: &str) -> (&str, &str) {
    let cut = call
        .find(|ch: char| ch.is_whitespace() || ch == '{')
        .unwrap_or(call.len());
    (&call[..cut], call[cut..].trim())
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A short, honest summary of a tool call's arguments.
///
/// Nothing is inferred: a path argument becomes `reading <path>`, a query
/// becomes `query: <text>`, and anything else is the argument text itself.
/// A bare tool name earns a fixed descriptor so the row always has two halves.
fn tool_summary(name: &str, args: &str) -> String {
    if let Some((_, rest)) = args.split_once("{\"path\":\"")
        && let Some(path) = rest.split('"').next()
        && !path.is_empty()
    {
        return format!("reading {path}");
    }
    if let Some((_, rest)) = args.split_once("{\"query\":\"")
        && let Some(query) = rest.split('"').next()
        && !query.is_empty()
    {
        return format!("query: {query}");
    }
    let plain = args.trim_matches(|ch| ch == '{' || ch == '}').trim();
    if !plain.is_empty() {
        return clip(plain, 40);
    }
    if name.starts_with("kg") {
        "code graph".to_string()
    } else if name.starts_with("vault") {
        "memory".to_string()
    } else {
        "tool call".to_string()
    }
}

/// Elapsed wall-clock time, rounded down, with no invented precision.
fn tool_age(elapsed_ms: i64) -> String {
    let secs = elapsed_ms.max(0) / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// One deduplicated tool row: a call, its summary, a literal state word and,
/// only while running, the honest elapsed time. The status glyph is added by
/// `style_for` when the row is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolRow {
    role: EntryRole,
    call: String,
    summary: String,
    state: &'static str,
    age: Option<String>,
}

impl ToolRow {
    fn text(&self) -> String {
        let mut text = format!("{} — {} · {}", self.call, self.summary, self.state);
        if let Some(age) = &self.age {
            text.push_str(" · ");
            text.push_str(age);
        }
        text
    }
}

/// Collapse a consecutive run of the same call into one calm row.
///
/// The aggregate role keeps the most live state: a still-running call wins over
/// a settled one, and a failure wins over a plain success. Elapsed time is
/// computed only for a running row, so it is frozen the moment the call lands.
fn tool_row(run: &[&Entry], now_ms: i64) -> ToolRow {
    let role = if run.iter().any(|entry| entry.role == EntryRole::Tool) {
        EntryRole::Tool
    } else if run.iter().any(|entry| entry.role == EntryRole::ToolFailed) {
        EntryRole::ToolFailed
    } else if run
        .iter()
        .any(|entry| entry.role == EntryRole::ToolInterrupted)
    {
        EntryRole::ToolInterrupted
    } else {
        EntryRole::ToolDone
    };
    let call_text = tool_call_identity(&run[0].text);
    let (name, args) = split_call(&call_text);
    let call = if run.len() > 1 {
        format!("{name} ×{}", run.len())
    } else {
        name.to_string()
    };
    let age = (role == EntryRole::Tool).then(|| {
        let started = run
            .iter()
            .find(|entry| entry.role == EntryRole::Tool)
            .map_or(run[0].at_ms, |entry| entry.at_ms);
        tool_age(now_ms.saturating_sub(started))
    });
    ToolRow {
        role,
        call,
        summary: tool_summary(name, args),
        state: state_word(role),
        age,
    }
}

/// The transcript as rows: `(entry index, role, text)`.
///
/// Consecutive identical tool calls become one row with a `×N` count. Every
/// other entry keeps its own row and its own index, which is where the
/// timestamp and the selection anchor come from.
fn transcript_rows(entries: &[Entry], now_ms: i64) -> Vec<(usize, EntryRole, String)> {
    let mut rows = Vec::new();
    let mut index = 0;
    while index < entries.len() {
        if is_tool_role(entries[index].role) {
            let identity = tool_call_identity(&entries[index].text);
            let mut end = index + 1;
            while end < entries.len()
                && is_tool_role(entries[end].role)
                && tool_call_identity(&entries[end].text) == identity
            {
                end += 1;
            }
            let run: Vec<&Entry> = entries[index..end].iter().collect();
            let row = tool_row(&run, now_ms);
            rows.push((index, row.role, row.text()));
            index = end;
        } else {
            rows.push((index, entries[index].role, entries[index].text.clone()));
            index += 1;
        }
    }
    rows
}

/// Transcript pane showing the conversation history
#[derive(Debug, Default)]
pub struct TranscriptPane {
    pub visible: bool,
    pub list_state: ListState,
    pub scroll: usize,
    pub follow: bool,
    pub show_timestamps: bool,
    pub max_width: usize,
}

impl TranscriptPane {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            visible: true,
            list_state: state,
            scroll: 0,
            follow: true,
            show_timestamps: true,
            max_width: 120,
        }
    }

    /// Handle key events for the transcript pane
    pub fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> bool {
        if !self.visible {
            return false;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let _shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Up => {
                self.follow = false;
                if self.scroll > 0 {
                    self.scroll -= 1;
                    self.update_selection(app);
                }
                true
            }
            KeyCode::Down => {
                if self.scroll + 1 < app.entries.len().saturating_sub(1) {
                    self.scroll += 1;
                    self.update_selection(app);
                } else {
                    self.follow = true;
                    self.scroll = app.entries.len().saturating_sub(1);
                    self.update_selection(app);
                }
                true
            }
            KeyCode::PageUp => {
                self.follow = false;
                self.scroll = self.scroll.saturating_sub(20);
                self.update_selection(app);
                true
            }
            KeyCode::PageDown => {
                self.scroll = (self.scroll + 20).min(app.entries.len().saturating_sub(1));
                if self.scroll >= app.entries.len().saturating_sub(1) {
                    self.follow = true;
                }
                self.update_selection(app);
                true
            }
            KeyCode::Home => {
                self.follow = false;
                self.scroll = 0;
                self.update_selection(app);
                true
            }
            KeyCode::End => {
                self.follow = true;
                self.scroll = app.entries.len().saturating_sub(1);
                self.update_selection(app);
                true
            }
            KeyCode::Char('t') if ctrl => {
                self.show_timestamps = !self.show_timestamps;
                true
            }
            KeyCode::Char('f') if ctrl => {
                self.follow = !self.follow;
                if self.follow {
                    self.scroll = app.entries.len().saturating_sub(1);
                    self.update_selection(app);
                }
                true
            }
            _ => false,
        }
    }

    fn update_selection(&mut self, app: &App) {
        if !app.entries.is_empty() {
            self.list_state
                .select(Some(self.scroll.min(app.entries.len() - 1)));
        }
    }

    /// Render the transcript pane
    pub fn render(&mut self, frame: &mut Frame, area: Rect, app: &App) {
        if !self.visible {
            return;
        }
        let t = tokens();
        let rows = transcript_rows(&app.entries, crate::session::now_ms());
        // The deduped row list can be shorter than the entry list. Clamp here
        // so the highlight always lands on a row that was actually drawn.
        let selected = self.scroll.min(rows.len().saturating_sub(1));
        self.list_state
            .select((!rows.is_empty()).then_some(selected));

        let items: Vec<ListItem> = rows
            .iter()
            .enumerate()
            .map(|(i, (entry_index, role, text))| {
                let entry = &app.entries[*entry_index];
                let style = if i == selected {
                    selected_style()
                } else {
                    Style::default()
                };

                let (role_style, role_glyph) = style_for(*role);
                let mut lines = Vec::new();

                // The row's own recorded time, in local time to the minute.
                // A stamp reading differently from the same turn in the session
                // file would be a second truth, so this formats the entry's
                // `at_ms` and nothing else.
                if self.show_timestamps {
                    lines.push(Line::from(Span::styled(
                        clock(entry.at_ms),
                        Style::default().fg(t.muted),
                    )));
                }

                // Role and content
                let content_lines = wrap_text(text, self.max_width);
                for (line_idx, line) in content_lines.iter().enumerate() {
                    if line_idx == 0 {
                        lines.push(Line::from(vec![
                            Span::styled(role_glyph, role_style),
                            Span::styled(" ", Style::default()),
                            Span::styled(line.to_string(), style),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::styled("  ", Style::default()),
                            Span::styled(line.to_string(), style),
                        ]));
                    }
                }

                // Selection is carried by each span's ink above. No item-level
                // fill is applied, so no row paints a band behind its words.
                ListItem::new(lines)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(format!(
                " Transcript {} ",
                if self.follow { "●" } else { "○" }
            )))
            .highlight_style(selected_style());

        // Calculate scrollbar state
        let list_height = area.height.saturating_sub(2) as usize; // Account for borders
        let mut scrollbar_state = ScrollbarState::default()
            .content_length(rows.len())
            .viewport_content_length(list_height)
            .position(self.scroll);

        frame.render_stateful_widget(list, area, &mut self.list_state);

        // Render scrollbar
        let scrollbar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        };
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some(" "))
            .thumb_symbol("▏")
            .thumb_style(Style::default().fg(t.accent).add_modifier(Modifier::DIM));
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}

/// Selection changes ink, not fill: bold accent text survives NO_COLOR and
/// theme switches, and no row paints a band behind its words.
fn selected_style() -> Style {
    let t = tokens();
    Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
}

/// Style for entry role. The tokens mirror the canonical mapping in `ui.rs`;
/// only the glyphs differ between the two transcript renderers.
fn style_for(role: EntryRole) -> (Style, &'static str) {
    let t = tokens();
    match role {
        EntryRole::User => (
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            "▶",
        ),
        EntryRole::Answer => (
            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
            "▶",
        ),
        EntryRole::Info => (
            Style::default().fg(t.muted).add_modifier(Modifier::BOLD),
            "ℹ",
        ),
        EntryRole::Error => (
            Style::default().fg(t.error).add_modifier(Modifier::BOLD),
            "✗",
        ),
        // The glyph is the state: running, then done in place.
        EntryRole::Tool => (Style::default().fg(t.accent), "▌"),
        EntryRole::ToolDone => (Style::default().fg(t.success), "✓"),
        EntryRole::ToolFailed => (Style::default().fg(t.error), "✗"),
        EntryRole::ToolInterrupted => (Style::default().fg(t.muted), "·"),
        EntryRole::Thinking => (
            Style::default().fg(t.muted).add_modifier(Modifier::ITALIC),
            "▸",
        ),
    }
}

/// Break text into lines of at most `width` characters.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        if current.len() + word.len() + 1 > width && !current.is_empty() {
            lines.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    #[test]
    fn no_raw_colours_in_production_render_code() {
        let source = include_str!("transcript.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            !production.contains("Color::"),
            "production render code must use semantic tokens, not raw Color::"
        );
    }

    #[test]
    fn role_styles_use_tokens_and_keep_their_glyphs() {
        let t = tokens();
        let expected = [
            (EntryRole::User, t.accent, "▶"),
            (EntryRole::Answer, t.text, "▶"),
            (EntryRole::Info, t.muted, "ℹ"),
            (EntryRole::Error, t.error, "✗"),
            (EntryRole::Tool, t.accent, "▌"),
            (EntryRole::ToolDone, t.success, "✓"),
            (EntryRole::ToolFailed, t.error, "✗"),
            (EntryRole::ToolInterrupted, t.muted, "·"),
            (EntryRole::Thinking, t.muted, "▸"),
        ];
        for (role, color, glyph) in expected {
            let (style, got_glyph) = style_for(role);
            assert_eq!(got_glyph, glyph, "the glyph is the meaning: {role:?}");
            assert_eq!(style.fg, Some(color), "role ink: {role:?}");
        }
        // Weight and italics are the colour-independent half of the meaning.
        for role in [
            EntryRole::User,
            EntryRole::Answer,
            EntryRole::Info,
            EntryRole::Error,
        ] {
            assert!(
                style_for(role).0.add_modifier.contains(Modifier::BOLD),
                "spoken roles stay bold: {role:?}"
            );
        }
        assert!(
            style_for(EntryRole::Thinking)
                .0
                .add_modifier
                .contains(Modifier::ITALIC)
        );
    }

    #[test]
    fn selection_changes_ink_not_fill() {
        let t = tokens();
        let selected = selected_style();
        assert_eq!(selected.fg, Some(t.accent));
        assert_eq!(selected.bg, None, "no selected-bg band");
        assert!(selected.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn every_colour_in_the_rendered_pane_comes_from_the_tokens() {
        // A different test may switch the global palette while this one runs,
        // so allow the token set of *every* palette, not just the one sampled
        // before the draw. The claim is "only tokens ever reach the pane", not
        // "one particular palette did".
        let mut allowed = vec![Color::Reset];
        for palette in crate::tui::theme::Palette::all() {
            let t = crate::tui::theme::Tokens::for_palette(*palette);
            allowed.extend([
                t.canvas,
                t.surface,
                t.text,
                t.muted,
                t.border,
                t.accent,
                t.success,
                t.warning,
                t.error,
                t.selected_bg,
                t.selected_fg,
            ]);
        }
        let mut app = App::new("s", false);
        app.push(EntryRole::User, "a question".to_string());
        app.push(EntryRole::ToolDone, "a finished tool".to_string());
        let mut pane = TranscriptPane::new();
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 60, 10), &app))
            .expect("draw must not fail");
        for cell in terminal.backend().buffer().content() {
            assert!(
                allowed.contains(&cell.fg) && allowed.contains(&cell.bg),
                "a colour outside the token set reached the pane: {cell:?}"
            );
        }
    }

    const NOW: i64 = 1_800_000_000_000;

    fn entry(role: EntryRole, text: &str, at_ms: i64) -> Entry {
        Entry {
            role,
            text: text.to_string(),
            at_ms,
            live: false,
            expanded: false,
            stats: None,
            tool_call_id: None,
            tool_detail: None,
            follow_ups: Vec::new(),
        }
    }

    #[test]
    fn tool_rows_read_as_glyph_name_summary_and_literal_state() {
        let call = entry(
            EntryRole::Tool,
            "kg_search {\"query\":\"auth\"}",
            NOW - 4_000,
        );
        let row = tool_row(&[&call], NOW);
        assert_eq!(row.role, EntryRole::Tool);
        assert_eq!(row.text(), "kg_search — query: auth · running · 4s");

        let (_, glyph) = style_for(EntryRole::Tool);
        assert_eq!(glyph, "▌", "the running glyph still leads the row");

        let done = entry(
            EntryRole::ToolDone,
            "kg_search {\"query\":\"auth\"} · completed · 12 chars",
            NOW - 4_000,
        );
        assert_eq!(
            tool_row(&[&done], NOW).text(),
            "kg_search — query: auth · completed",
            "the recorded state word is not duplicated"
        );
        let failed = entry(
            EntryRole::ToolFailed,
            "bash git push · failed · 3 chars",
            NOW,
        );
        assert_eq!(tool_row(&[&failed], NOW).text(), "bash — git push · failed");
        let interrupted = entry(
            EntryRole::ToolInterrupted,
            "bash git status · interrupted (result not observed)",
            NOW,
        );
        assert_eq!(
            tool_row(&[&interrupted], NOW).text(),
            "bash — git status · interrupted"
        );
    }

    #[test]
    fn consecutive_identical_calls_collapse_to_one_counted_row() {
        let run = vec![
            entry(
                EntryRole::ToolDone,
                "bash git status · completed · 4 chars",
                NOW,
            ),
            entry(
                EntryRole::ToolDone,
                "bash git status · completed · 9 chars",
                NOW,
            ),
            entry(
                EntryRole::ToolDone,
                "bash git status · completed · 2 chars",
                NOW,
            ),
        ];
        let refs: Vec<&Entry> = run.iter().collect();
        let row = tool_row(&refs, NOW);
        assert_eq!(
            row.call, "bash ×3",
            "different result sizes are the same call"
        );
        assert_eq!(row.text(), "bash ×3 — git status · completed");

        let mut entries = vec![entry(EntryRole::User, "check status", NOW)];
        entries.extend(run);
        entries.push(entry(EntryRole::Answer, "clean", NOW));
        let rows = transcript_rows(&entries, NOW);
        assert_eq!(rows.len(), 3, "three tool calls take one row");
        assert_eq!(rows[1].2, "bash ×3 — git status · completed");
    }

    #[test]
    fn elapsed_comes_from_at_ms_and_is_frozen_on_completion() {
        let running = entry(
            EntryRole::Tool,
            "vault recall {\"path\":\"x\"}",
            NOW - 9_000,
        );
        let row = tool_row(&[&running], NOW);
        assert_eq!(row.age.as_deref(), Some("9s"));
        assert_eq!(row.text(), "vault — reading x · running · 9s");

        let settled = entry(
            EntryRole::ToolDone,
            "vault recall {\"path\":\"x\"} · completed · 1 chars",
            NOW - 9_000,
        );
        let settled_row = tool_row(&[&settled], NOW);
        assert_eq!(
            settled_row.age, None,
            "no clock keeps ticking after the result"
        );
        assert_eq!(settled_row.text(), "vault — reading x · completed");

        assert_eq!(tool_age(125_000), "2m 05s");
        assert_eq!(tool_age(-5), "0s");
    }

    #[test]
    fn selected_tool_row_has_ink_but_no_background_band() {
        let mut app = App::new("s", false);
        app.push(
            EntryRole::Tool,
            "kg_search {\"query\":\"auth\"}".to_string(),
        );
        app.push(EntryRole::User, "next question".to_string());
        let mut pane = TranscriptPane::new();
        pane.scroll = 0;
        let backend = TestBackend::new(70, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 70, 12), &app))
            .expect("draw must not fail");
        let cells = terminal.backend().buffer().content();
        for cell in cells {
            assert_eq!(cell.bg, Color::Reset, "no row paints a fill: {cell:?}");
        }
        let text: String = cells.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("kg_search — query: auth"), "{text}");
        assert!(text.contains("running"), "{text}");
        // The palette can be switched by another test while this one runs, so
        // assert the invariant (ink, never a fill) rather than a sampled colour.
        assert!(selected_style().fg.is_some());
        assert_eq!(selected_style().bg, None);
    }

    #[test]
    fn no_color_keeps_the_glyph_and_the_state_word() {
        let t = crate::tui::theme::Tokens::for_palette(crate::tui::theme::Palette::NoColor);
        assert_eq!(t.accent, Color::Reset);
        assert_eq!(t.success, Color::Reset);
        assert_eq!(t.error, Color::Reset);

        let call = entry(
            EntryRole::ToolFailed,
            "bash git push · failed · 3 chars",
            NOW,
        );
        let row = tool_row(&[&call], NOW);
        // Colour is gone; the glyph and the literal word are not.
        assert_eq!(style_for(row.role).1, "✗");
        assert!(row.text().contains("failed"), "{:?}", row.text());
    }
}
