//! Drawing. No state decisions live here beyond the scroll clamp.
//!
//! The transcript is windowed by visual line, not by widget scroll: [`crate::tui::app`]
//! already wraps, so the first visible line is exact and the last page is not an
//! estimate. The only mutation this module performs is writing the measured
//! viewport back into the app, which is what makes `PgUp`/`PgDn` move a screen.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::output::ellipsize;
use crate::tui::app::{App, EntryRole, Home};

/// Width of the command column on the startup screen.
const COMMAND_COLUMN: usize = 16;

const FOOTER_HINT: &str =
    "enter send · ↑↓ history · ←→ edit · pgup/pgdn scroll · end newest · ctrl-c quit · /help";

/// Draw one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }
    if area.width < 40 || area.height < 10 {
        let note = "terminal too small — resize to at least 40×10";
        frame.render_widget(Paragraph::new(ellipsize(note, area.width as usize)), area);
        return;
    }

    // The startup screen owns the whole frame. The header repeats its three
    // facts (root, vault, graph), so showing both would waste the first thing
    // the person sees on saying the same thing twice.
    let splash = app.entries.is_empty() && app.home.is_some();
    let rows = Layout::vertical([
        Constraint::Length(if splash { 0 } else { 4 }),
        Constraint::Min(3),
        Constraint::Length(3),
    ])
    .split(area);

    if !splash {
        draw_header(frame, rows[0], app);
    }
    draw_body(frame, rows[1], app);
    draw_footer(frame, rows[2], app);

    // A write is waiting on the person. Drawn last, over everything, because a
    // question about changing a file must not be mistaken for transcript text.
    if let Some(text) = &app.approval {
        draw_approval(frame, area, text);
    }

    // Render overlays on top
    app.overlay_stack.render(frame, area, app);
}

/// The confirmation box: what would change, and the two keys that answer.
///
/// It covers the transcript rather than sitting in a status line: the whole
/// point is that the change cannot happen while the person is reading
/// something else.
fn draw_approval(frame: &mut Frame, area: Rect, text: &str) {
    if area.width < 20 || area.height < 6 {
        return;
    }
    let width = ((area.width as u32 * 3 / 4) as u16).clamp(20, area.width);
    let height = ((area.height as u32 * 3 / 5) as u16).clamp(6, area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Line::from(Span::styled(
            " confirm ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(text.to_string())
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    let state = if app.resumed { "resumed" } else { "new" };
    let title = format!(
        "memnoth {} · session \"{}\" ({state}){}",
        env!("CARGO_PKG_VERSION"),
        app.name,
        if app.busy { " · thinking" } else { "" }
    );
    let paths = format!("root {}   vault {}", app.root, app.vault);

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = vec![
        Line::from(Span::styled(
            ellipsize(&title, width),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            ellipsize(&paths, width),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            ellipsize(&format!("graph {}", app.graph), width),
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_body(frame: &mut Frame, area: Rect, app: &mut App) {
    let columns =
        Layout::horizontal([Constraint::Percentage(72), Constraint::Percentage(28)]).split(area);

    // The startup screen stands where the transcript will be, and gets out of
    // the way the moment there is something to read: a welcome that stayed at
    // the top of a long conversation would only push the answer off screen.
    let home = if app.entries.is_empty() {
        app.home.clone()
    } else {
        None
    };
    match home {
        Some(home) => draw_home(frame, columns[0], &home),
        None => draw_transcript(frame, columns[0], app),
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(Span::styled(
            " context ",
            Style::default().fg(Color::DarkGray),
        )));
    let inner = block.inner(columns[1]);
    frame.render_widget(block, columns[1]);
    frame.render_widget(
        Paragraph::new(app.context.clone())
            .style(Style::default().fg(Color::Gray))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

/// The startup screen: what this session opened, and where to start.
///
/// One rule under the wordmark is the whole ornament. Everything below it is a
/// label and a value, so a long Windows path and a short model name still line
/// up.
fn draw_home(frame: &mut Frame, area: Rect, home: &Home) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(Span::styled(
            " home ",
            Style::default().fg(Color::DarkGray),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    // Wordmark on the left, version on the right.
    let version = home.version.trim();
    let pad = width
        .saturating_sub(1 + "memnoth".len() + version.chars().count() + 1)
        .max(1);
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "memnoth",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(pad)),
        Span::styled(format!("{version} "), Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(Span::styled(
        format!(" {}", "─".repeat(width.saturating_sub(1))),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        ellipsize(" terminal agent · temporal vault · code graph", width),
        Style::default().fg(Color::Gray),
    )));
    lines.push(Line::from(""));

    lines.push(home_row("workspace", &home.workspace, "", width));
    lines.push(home_row("vault", &home.vault, &home.vault_note, width));
    lines.push(home_row("graph", &home.graph, "", width));
    lines.push(home_row("model", &home.model, &home.model_note, width));

    if !home.steps.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " start here",
            Style::default().fg(Color::White),
        )));
        for (command, description) in &home.steps {
            let room = width.saturating_sub(2 + COMMAND_COLUMN);
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{command:<COMMAND_COLUMN$}"),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    ellipsize_words(description, room),
                    Style::default().fg(Color::Gray),
                ),
            ]));
        }
    }

    // Pinned to the bottom when there is room, directly after the steps when
    // there is not: on a short terminal the commands matter more than air.
    let hint = Line::from(vec![
        Span::raw(" "),
        Span::styled(home.session.clone(), Style::default().fg(Color::Gray)),
        Span::styled(
            "  ·  /help every command  ·  /clear empties the screen",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let height = inner.height as usize;
    let gap = height.saturating_sub(lines.len() + 1);
    for _ in 0..gap {
        lines.push(Line::from(""));
    }
    lines.push(hint);

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Cut to the last whole word that fits, so a narrow pane ends on a word
/// instead of half of one. A single unbroken word still falls back to a hard
/// cut, because dropping the note entirely would hide state.
fn ellipsize_words(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let cut: String = text.chars().take(width.saturating_sub(1)).collect();
    match cut.rfind(' ') {
        Some(index) if index > width / 2 => format!("{}…", &cut[..index]),
        _ => format!("{cut}…"),
    }
}

/// One label/value row of the startup screen. The note is dropped rather than
/// wrapped when it would not fit beside the value.
fn home_row(label: &str, value: &str, note: &str, width: usize) -> Line<'static> {
    const LABEL: usize = 11;
    let room = width.saturating_sub(1 + LABEL);
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(
            format!("{label:<LABEL$}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(ellipsize(value, room), Style::default().fg(Color::White)),
    ];
    let used = 1 + LABEL + value.chars().count();
    if !note.is_empty() {
        let separator = "  ·  ";
        let remaining = width.saturating_sub(used + separator.len());
        if remaining >= 8 {
            spans.push(Span::styled(
                separator,
                Style::default().fg(Color::DarkGray),
            ));
            spans.push(Span::styled(
                ellipsize_words(note, remaining),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    Line::from(spans)
}

fn draw_transcript(frame: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    if inner.width == 0 || inner.height == 0 {
        frame.render_widget(block, area);
        return;
    }

    // The viewport is measured before the title, because the percentage in it
    // is relative to the page that was just measured.
    app.set_viewport(inner.width as usize, inner.height as usize);
    let title = if app.follow {
        " transcript ".to_string()
    } else {
        let max = app.max_scroll().max(1);
        format!(" transcript · {}% ", (app.scroll * 100 / max).min(100))
    };
    let block = block.title(Line::from(Span::styled(
        title,
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(block, area);

    let visual = app.visual_lines(inner.width as usize);
    let max = visual.len().saturating_sub(inner.height as usize);
    if app.follow {
        app.scroll = max;
    } else {
        app.scroll = app.scroll.min(max);
    }
    let start = app.scroll.min(visual.len());
    let end = (start + inner.height as usize).min(visual.len());

    let lines: Vec<Line> = visual[start..end]
        .iter()
        .map(|(role, text)| {
            let (style, glyph) = style_for(*role);
            Line::from(vec![
                Span::styled(glyph, style.add_modifier(Modifier::BOLD)),
                Span::styled(text.clone(), style),
            ])
        })
        .collect();
    // Lines are already wrapped to the pane, so this is a no-op guard: if the
    // measured width ever disagrees with the render width, text wraps instead of
    // being silently clipped.
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let input_line = Line::from(vec![
        Span::styled(
            "> ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.input.clone(), Style::default().fg(Color::White)),
    ]);
    let hint = Line::from(Span::styled(
        ellipsize(FOOTER_HINT, inner.width as usize),
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(vec![input_line, hint]), inner);

    // The cursor belongs after the prompt and the characters already typed.
    let prefix = 2u16;
    let typed = app.cursor.min(app.input.chars().count()) as u16;
    let x = (inner.x + prefix + typed).min(inner.x + inner.width.saturating_sub(1));
    frame.set_cursor_position(Position::new(x, inner.y));
}

fn style_for(role: EntryRole) -> (Style, &'static str) {
    match role {
        EntryRole::User => (Style::default().fg(Color::Yellow), "› "),
        EntryRole::Answer => (Style::default().fg(Color::White), "  "),
        EntryRole::Error => (Style::default().fg(Color::Red), "! "),
        EntryRole::Info => (Style::default().fg(Color::Gray), "· "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, app))
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
    fn a_full_frame_shows_the_session_the_paths_and_the_prompt() {
        let mut app = App::new("payments", true);
        app.set_paths("D:/ws".into(), "D:/state/vault.db".into());
        app.push(EntryRole::User, "what changed?");
        app.push(EntryRole::Answer, "a fact from the vault");
        let text = screen(&mut app, 100, 30);
        assert!(text.contains("session \"payments\" (resumed)"), "{text}");
        assert!(text.contains("D:/state/vault.db"), "{text}");
        assert!(text.contains("what changed?"), "{text}");
        assert!(text.contains("a fact from the vault"), "{text}");
        assert!(text.contains("enter send"), "{text}");
    }

    fn example_home() -> Home {
        Home {
            version: "0.1.0-beta.1".into(),
            session: "session \"s\" (new)".into(),
            workspace: "E:/ws".into(),
            vault: "E:/state/vault.db".into(),
            vault_note: "not initialised".into(),
            graph: "not indexed".into(),
            model: "claude-sonnet-4-5".into(),
            model_note: "no key — export MEMNOTH_API_KEY".into(),
            steps: vec![
                (
                    "/vault init".into(),
                    "create the temporal vault for this machine".into(),
                ),
                (
                    "ask anything".into(),
                    "a turn with the vault and the graph as tools".into(),
                ),
            ],
        }
    }

    #[test]
    fn the_startup_screen_names_the_workspace_and_the_next_step() {
        let mut app = App::new("s", false);
        app.home = Some(example_home());
        let text = screen(&mut app, 110, 32);
        assert!(text.contains("memnoth"), "{text}");
        assert!(text.contains("0.1.0-beta.1"), "{text}");
        assert!(text.contains("E:/ws"), "{text}");
        assert!(text.contains("not initialised"), "{text}");
        assert!(text.contains("start here"), "{text}");
        assert!(text.contains("/vault init"), "{text}");
        assert!(text.contains("create the temporal vault"), "{text}");
        assert!(text.contains("session \"s\" (new)"), "{text}");
        assert!(text.contains("/help every command"), "{text}");
    }

    #[test]
    fn the_startup_screen_gives_way_to_the_first_answer() {
        let mut app = App::new("s", false);
        app.home = Some(example_home());
        app.push(EntryRole::User, "what changed?");
        let text = screen(&mut app, 110, 32);
        assert!(!text.contains("start here"), "{text}");
        assert!(text.contains("what changed?"), "{text}");
    }

    #[test]
    fn a_narrow_terminal_says_so_instead_of_panicking() {
        let mut app = App::new("s", false);
        let text = screen(&mut app, 20, 5);
        assert!(text.contains("terminal too small"), "{text}");
        // A zero-sized area is legal during a resize and must not index past it.
        let _ = screen(&mut app, 0, 0);
    }

    #[test]
    fn a_pending_approval_is_drawn_over_the_transcript() {
        let mut app = App::new("s", false);
        app.push(EntryRole::Answer, "an answer already on screen");
        app.approval = Some(
            "edit src/a.rs\n\nedit src/a.rs (1 line(s) → 1)\n\ny approve · n reject".to_string(),
        );
        let text = screen(&mut app, 100, 30);
        assert!(text.contains("confirm"), "{text}");
        assert!(text.contains("edit src/a.rs"), "{text}");
        assert!(text.contains("y approve"), "{text}");
    }

    #[test]
    fn long_entries_wrap_instead_of_being_cut_off() {
        let mut app = App::new("s", false);
        let long = "lorem ipsum dolor sit amet ".repeat(12);
        app.push(EntryRole::Answer, long.clone());
        let text = screen(&mut app, 60, 30);
        let tail = long.split_whitespace().last().unwrap();
        assert!(text.contains(tail), "the end of the entry must be visible");
    }
}
