//! Drawing. No state decisions live here beyond the scroll clamp.
//!
//! The transcript is windowed by visual line, not by widget scroll: [`crate::tui::app`]
//! already wraps, so the first visible line is exact and the last page is not an
//! estimate. The only mutation this module performs is writing the measured
//! viewport back into the app, which is what makes `PgUp`/`PgDn` move a screen.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};

use crate::output::ellipsize;
use crate::tui::app::{App, EntryRole, Home, MentionRow};
use crate::tui::slash::Row;
use crate::tui::theme::tokens;

/// Commands a completion list may show at once. Five is the most that can be
/// read at a glance; past that the eye stops scanning and starts reading, which
/// is what the filter is for.
const MAX_ROWS: usize = 5;

/// The narrowest window that can carry the context pane. Below this its share
/// of the width is a sliver that stacks one word per line, which reads worse
/// than not having it: the transcript takes the whole frame instead.
const CONTEXT_MIN_WIDTH: u16 = 88;

/// The footer is state, not decoration: at most five keys, and only ones that
/// do something in the state the screen is in.
fn footer_hint(app: &App) -> String {
    // The evidence overlay owns every key while it is up, so its hint comes
    // first: a key that works on the screen behind it is not a key the person
    // can use now.
    if app
        .overlay_stack
        .top_is(crate::tui::overlays::OverlayKind::Evidence)
    {
        return "tab/] page · [ back · ↑↓ row · v detail · l/c/i jump · esc close".to_string();
    }
    // The memory surface (Ctrl+4) is still a title until a chain is composed.
    // Naming the command is honest; drawing a fake chain would not be.
    if app.surface == crate::tui::surface::Surface::Memory && !app.overlay_stack.has_overlay() {
        return "/evidence <subject> composes the five-page chain on this surface".to_string();
    }
    if app.approval.is_some() || app.pending_shell.is_some() {
        "y approve · n / esc reject · the box names exactly what changes".to_string()
    } else if let Some(needs) = app.slash.needs() {
        // The argument list says what the position is for as well as what is
        // there: a list of files with no word about why is a riddle.
        format!("needs {needs} · tab completes · enter runs")
    } else if app.slash.is_open() {
        if app.slash.total() == 0 {
            "no command matches · enter runs the line as typed".to_string()
        } else if app.slash.filter_len().is_some() {
            "↑↓ move · enter open/run · tab complete · esc clears filter".to_string()
        } else {
            "↑↓ move · enter open/run · tab complete · esc close".to_string()
        }
    } else if app.mentions.is_open() {
        // The mention picker owns the same keys as the `/` list, but Enter
        // inserts a token rather than running a command — say so here.
        "↑↓/tab mention · enter insert · esc close".to_string()
    } else if app.login_busy {
        // A device grant can wait ten minutes on a browser. Counting the wait is
        // what separates "waiting" from "hung", and the key that ends it is not
        // on any row, so it is named beside the count.
        let waited = app
            .login_started
            .map(|started| format!("{} · ", crate::auth::human_duration(started.elapsed())))
            .unwrap_or_default();
        // The key named is the one that works here: esc only when the prompt has
        // nothing else for it, `/login` while a line or a turn owns it.
        let key = if app.esc_cancels_signin() {
            "esc cancels"
        } else {
            "/login cancels"
        };
        let open = if app.login_prompt.is_some() {
            "ctrl-o opens · "
        } else {
            ""
        };
        format!("sign in: waiting for approval · {waited}{open}{key}")
    } else if app.home_showing() && app.input.is_empty() {
        // On a session with history the brief is standing in front of it, so
        // the key that puts it back is named here rather than discovered.
        if app.entries.is_empty() {
            "1–3 choose · enter run · ↓ activity · / commands".to_string()
        } else {
            "1–3 choose · enter run · ↓ activity · esc chat".to_string()
        }
    } else if app.busy {
        let elapsed = ((crate::session::now_ms() - app.turn_started_ms).max(0) / 1000) as u64;
        let age = if elapsed < 60 {
            format!("{elapsed}s")
        } else {
            format!("{}m {:02}s", elapsed / 60, elapsed % 60)
        };
        format!("working {age} · ↓ activity · esc cancel")
    } else if let Some(running) = &app.running_verb {
        // A verb on its own thread: the screen stays live, so this line is what
        // says the silence is work and not a hang.
        format!("running {running}")
    } else if app.active_pane != 0 {
        "tab pane · ctrl-t tasks · esc transcript".to_string()
    } else if !app.follow {
        // Scrolled back: the position is said here, in the line that is already
        // on screen, rather than in a status strip that costs a row.
        let max = app.max_scroll().max(1);
        format!(
            "{}% scrolled · end newest · esc clears the line",
            (app.scroll * 100 / max).min(100)
        )
    } else if app.graph_attention.is_some() {
        // The chip in the header names its fix; this is the key that runs it.
        // Keep the graph repair action beside its warning while retaining the
        // correct send affordance for the current draft state.
        if app.input.is_empty() && app.attachments.is_empty() {
            "type message · ctrl-g fix · / commands · ? help".to_string()
        } else {
            "enter send · shift-enter newline · ctrl-g fix · shift-tab mode".to_string()
        }
    } else if !app.attachments.is_empty() {
        "enter send · ctrl-x remove attachment · shift-tab mode".to_string()
    } else if !app.input.is_empty() {
        "enter send · shift-enter newline · shift-tab mode".to_string()
    } else {
        "type message · ↓ activity · shift-tab mode · / commands".to_string()
    }
}

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

    // The brief owns the whole frame. The header repeats its facts (root,
    // vault, graph), so showing both would spend the first thing a person sees
    // saying the same thing twice. The brief's masthead is the header then:
    // while it is up, the one-line transcript header is not drawn at all.
    let splash = app.home_showing();
    // The composer grows with what is typed into it, up to a cap. A long
    // question used to wrap into one clipped row — the text past the right edge
    // was simply not drawn — and a pasted page must not take the whole frame.
    let composer = composer_rows(&app.input, area.width);
    let rows = Layout::vertical([
        Constraint::Length(if splash { 0 } else { 1 }),
        Constraint::Min(3),
        // Top rule, breathing row, draft, bottom rule, then one hint row.
        Constraint::Length(composer + u16::from(!app.attachments.is_empty()) + 3),
    ])
    .split(area);

    if !splash {
        draw_header(frame, rows[0], app);
    }
    draw_body(frame, rows[1], app);

    // A pending sign-in keeps its code and link directly above the prompt, on
    // the same rows a hint list would use: the transcript's copy can be cleared
    // or scrolled past, and the one thing the wait needs must not be either.
    draw_signin(frame, area, rows[2], app);

    // The list sits directly above the prompt it belongs to and is drawn before
    // the prompt, so the prompt can carry the position in it. It covers
    // transcript rows, never the line being typed.
    let position = draw_completions(frame, area, rows[2], app);
    // An open overlay owns the frame, and that includes the caret. The prompt
    // line still exists underneath, so its cursor used to blink on top of the
    // overlay's footer — a stray block sitting on the status text, looking like
    // a broken pointer. An overlay that wants a caret sets its own position.
    let caret = !app.overlay_stack.has_overlay();
    draw_footer(frame, rows[2], app, position, caret);

    // Progress is a transient, dismissible layer over the finished frame —
    // never text written into the transcript. It is drawn last so no other
    // widget can overwrite a stage row.
    draw_progress(frame, area, app);

    // A write is waiting on the person. Drawn last, over everything, because a
    // question about changing a file must not be mistaken for transcript text.
    // A parked shell command asks in the same box, so "waiting on you" looks
    // the same whichever tool asked.
    if let Some(text) = &app.approval {
        draw_approval(frame, area, text);
    } else if let Some(pending) = &app.pending_shell {
        draw_approval(frame, area, &pending.prompt());
    }

    // Render overlays on top. The stack is moved out for the call because an
    // overlay renders against the whole app: holding `&mut app.overlay_stack`
    // while the overlay reads `app` is two borrows of one value.
    let mut overlays = std::mem::take(&mut app.overlay_stack);
    overlays.render(frame, area, app);
    app.overlay_stack = overlays;
}

/// Most rows the composer takes before the text scrolls inside it.
const MAX_COMPOSER_ROWS: u16 = 8;

/// Characters inside the frame after its inset and prompt gutter.
fn composer_width(width: u16) -> usize {
    width.saturating_sub(6).max(1) as usize
}

/// Rows the composer needs for `input`, capped so a paste cannot eat the frame.
fn composer_rows(input: &str, width: u16) -> u16 {
    let rows = composer_lines(input, composer_width(width)).len();
    rows.clamp(1, MAX_COMPOSER_ROWS as usize) as u16
}

/// `input` split into the rows it occupies at `usable` characters per row.
fn composer_lines(input: &str, usable: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut col = 0usize;
    for ch in input.chars() {
        if ch == '\n' {
            rows.push(String::new());
            col = 0;
            continue;
        }
        if col == usable {
            rows.push(String::new());
            col = 0;
        }
        rows.last_mut().expect("rows is never empty").push(ch);
        col += 1;
    }
    rows
}

/// Row and column of the caret once `input` is wrapped at `usable` characters.
fn caret_cell(input: &str, cursor: usize, usable: usize) -> (usize, usize) {
    let mut row = 0usize;
    let mut col = 0usize;
    for (index, ch) in input.chars().enumerate() {
        if index == cursor {
            break;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
            continue;
        }
        if col == usable {
            row += 1;
            col = 0;
        }
        col += 1;
    }
    (row, col)
}

/// The pending sign-in's code and link, above the prompt.
///
/// Two rows, drawn from the prompt upward, so the line being typed never moves.
/// An open hint list takes the rows instead: the person is typing a command,
/// and the code is still in the transcript and comes back with the list closed.
fn draw_signin(frame: &mut Frame, area: Rect, prompt: Rect, app: &App) {
    let Some(signin) = &app.login_prompt else {
        return;
    };
    if app.slash.is_open() || area.width < 24 {
        return;
    }
    if prompt.y.saturating_sub(area.y) < 2 {
        return;
    }

    let t = tokens();
    let label = Style::default().fg(t.muted);
    let lines = vec![
        Line::from(vec![
            Span::styled("  code ", label),
            Span::styled(
                signin.user_code.clone(),
                Style::default().fg(t.text).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  open ", label),
            Span::styled(
                ellipsize(&signin.url, (area.width as usize).saturating_sub(7)),
                Style::default().fg(t.accent),
            ),
        ]),
    ];
    let rect = Rect::new(area.x, prompt.y - 2, area.width, 2);
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines), rect);
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
    // Tall enough for the text that is actually there, capped so a long diff
    // cannot cover the screen. A one-line question used to be drawn in a box
    // three fifths of the terminal high, which looks like a broken layout.
    let wanted = text.lines().count() as u16 + 2; // + borders
    let height = wanted
        .min((area.height as u32 * 3 / 5) as u16)
        .clamp(6, area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    // A confirmation asks for a decision, so its frame and title carry the
    // warning role. The two keys are written in the text, not in a colour.
    let t = tokens();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.warning))
        .title(Line::from(Span::styled(
            " confirm ",
            Style::default().fg(t.warning).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(text.to_string())
            .style(Style::default().fg(t.text))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

/// One line: the session, anything that needs a person, and where it opened.
///
/// What is deliberately absent is the point. The version cannot change during a
/// run and the brief prints it; `new`/`resumed` is history, not state, and the
/// transcript already shows the entries; a healthy graph's counters are on the
/// brief and the context rail. Three places saying good news is what teaches the
/// eye to skip the line that carries bad news.
///
/// What is left is the three facts a person asks of a terminal: which
/// conversation is this, what is wrong, and where am I.
fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let t = tokens();
    let width = area.width as usize;
    let mut spans = vec![Span::styled(
        format!(" {}", app.name),
        Style::default().fg(t.text).add_modifier(Modifier::BOLD),
    )];

    // A state with no action is a riddle, so the only graph fact that earns this
    // line names the verb that resolves it (`mod.rs::graph_state` decides both
    // the wording and whether there is anything to say).
    if let Some(attention) = &app.graph_attention {
        spans.push(Span::styled(" · ", Style::default().fg(t.muted)));
        spans.push(Span::styled(
            format!("{} · {}", attention.label, attention.fix),
            Style::default().fg(t.warning).add_modifier(Modifier::BOLD),
        ));
    }

    // The path is right-aligned and loses its leading characters first: it is
    // read once per session, so it is the fact that can afford to be trimmed.
    let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
    let room = width.saturating_sub(used + 1);
    if room > 4 {
        let path = ellipsize(&short_path(&app.root), room);
        let pad = room.saturating_sub(path.chars().count());
        spans.push(Span::raw(" ".repeat(pad + 1)));
        spans.push(Span::styled(path, Style::default().fg(t.muted)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The workspace, cut down to the part that distinguishes it.
///
/// Home becomes `~`, so two sessions in different projects do not spend their
/// first twenty columns agreeing; anything else keeps its last two components,
/// because the leading ones are identical for every session of a project.
fn short_path(root: &str) -> String {
    if root.is_empty() {
        return String::new();
    }
    for key in ["USERPROFILE", "HOME"] {
        let Some(home) = std::env::var_os(key) else {
            continue;
        };
        let Some(home) = home.to_str() else {
            continue;
        };
        let home = home.trim_end_matches(['/', '\\']);
        if let Some(rest) = root.strip_prefix(home) {
            let rest = rest.trim_start_matches(['/', '\\']);
            return if rest.is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", rest.replace('\\', "/"))
            };
        }
    }
    let parts: Vec<&str> = root.rsplit(['/', '\\']).take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

fn draw_body(frame: &mut Frame, area: Rect, app: &mut App) {
    // No frame around the transcript: a border around the whole screen spends
    // four rows' worth of ink per column and tells you nothing the layout does
    // not already say. The gutter is what makes the text read as a block.
    // One column is earned by width: the context pane at 88. Below that the
    // transcript takes the frame, because a squeezed column costs more room
    // than the glance is worth. The old 26-column sidebar at 104 is gone: it
    // repeated `/login` and `ctrl-k palette`, and the footer already says the
    // second one.
    let context = area.width >= CONTEXT_MIN_WIDTH;
    let columns = if context {
        Layout::horizontal([Constraint::Percentage(72), Constraint::Percentage(28)]).split(area)
    } else {
        Layout::horizontal([Constraint::Percentage(100)]).split(area)
    };

    // The brief stands where the transcript will be: on an empty session, or
    // when `/home` asked for it back.
    let home = if app.home_showing() {
        app.home.clone()
    } else {
        None
    };
    match home {
        Some(home) => draw_home(frame, columns[0], &home, app),
        None => draw_transcript(frame, columns[0], app),
    }

    if context {
        // The rail follows focus: transcript keeps the context glance, and
        // ctrl-t / tab / ctrl-1..4 swap it for the work the person asked about.
        if app.pane_layout == 1 {
            match app.active_pane {
                2 => draw_tasks_rail(frame, columns[1], app),
                3 => draw_diff_rail(frame, columns[1], app),
                _ => draw_context_rail(frame, columns[1], app),
            }
        } else {
            draw_context_rail(frame, columns[1], app);
        }
    }
}

/// A panel is a single left rule and a title line, not a box.
///
/// Four borders around every panel doubled the chrome on a screen whose whole
/// point is text; one rule separates the panel from the transcript by exactly
/// the amount that needs separating, and the title costs one row instead of a
/// border row top and bottom.
fn rail_block() -> Block<'static> {
    Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(tokens().border))
}

/// The panel's rule, and the rectangle its content may use: one column in from
/// the rule, so text never sits against the line.
fn rail_frame(area: Rect) -> (Block<'static>, Rect) {
    let block = rail_block();
    let inner = block.inner(area);
    (
        block,
        Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width.saturating_sub(1),
            height: inner.height,
        },
    )
}

/// The panel's heading, in the first row of its content.
fn rail_title(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(tokens().muted)
            .add_modifier(Modifier::BOLD),
    ))
}

fn draw_context_rail(frame: &mut Frame, area: Rect, app: &mut App) {
    let (block, inner) = rail_frame(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let t = tokens();
    let mut lines = vec![Line::from(Span::styled(
        "context",
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
    ))];
    // Where this machine's sessions are filed, on the rail that already answers
    // "what is this screen". Without it the only way to know is the console.
    if let Some(project) = &app.project {
        lines.push(Line::from(vec![
            Span::styled("project ", Style::default().fg(t.muted)),
            Span::styled(project.clone(), Style::default().fg(t.text)),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("project ", Style::default().fg(t.muted)),
            Span::styled("unassigned", Style::default().fg(t.warning)),
        ]));
    }
    for line in app.context.lines() {
        let text = line.trim();
        if text.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        let style = if matches!(
            text,
            "vault" | "coordination" | "usage" | "decisions" | "lessons"
        ) {
            Style::default().fg(t.muted).add_modifier(Modifier::BOLD)
        } else if text.starts_with('/') {
            // A slash command is actionable, the one place accent is earned.
            Style::default().fg(t.accent)
        } else if text.contains("unavailable") || text == "missing" {
            Style::default().fg(t.warning)
        } else {
            Style::default().fg(t.text)
        };
        lines.push(Line::from(Span::styled(text.to_string(), style)));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    cache_rail_selection(frame, inner, app);
}

fn cache_rail_selection(frame: &mut Frame, inner: Rect, app: &mut App) {
    // Cache the actual wrapped rows from the terminal buffer so a drag copies
    // exactly what is visible, even when long values wrap in the rail.
    let rows = {
        let buffer = frame.buffer_mut();
        (inner.y..inner.y.saturating_add(inner.height))
            .map(|y| {
                (inner.x..inner.x.saturating_add(inner.width))
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    };
    app.set_rail_selection_rows(inner.x, inner.y, inner.width, rows);
    if let Some((first, last)) = app.rail_selection {
        let y_start = inner.y.saturating_add(first as u16);
        let y_end = inner
            .y
            .saturating_add(last as u16)
            .min(inner.y.saturating_add(inner.height.saturating_sub(1)));
        for y in y_start..=y_end {
            let row_end = (inner.x..inner.x.saturating_add(inner.width))
                .rev()
                .find(|x| !matches!(frame.buffer_mut().cell((*x, y)), Some(cell) if cell.symbol().trim().is_empty()));
            let Some(row_end) = row_end else {
                continue;
            };
            for x in inner.x..=row_end {
                if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                    cell.set_style(mouse_selection_style(cell.style(), tokens()));
                }
            }
        }
    }
}

/// The task rail: what the session is doing in the background. Empty is a
/// state, not a blank box — it says what will appear here.
fn draw_tasks_rail(frame: &mut Frame, area: Rect, app: &mut App) {
    let (block, inner) = rail_frame(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let t = tokens();
    let mut lines: Vec<Line> = vec![rail_title("tasks")];
    if app.tasks_pane.tasks.is_empty() {
        lines.push(Line::from(Span::styled(
            "no background tasks",
            Style::default().fg(t.muted),
        )));
        lines.push(Line::from(Span::styled(
            "start one: /tasks run <cmd>",
            Style::default().fg(t.muted),
        )));
    } else {
        for task in app.tasks_pane.tasks.iter().take(inner.height as usize) {
            // The glyph is the state; the colour only reinforces it. Running is
            // the one current/actionable task, so it alone earns the accent.
            let (glyph, style) = match task.status {
                crate::tui::panes::tasks::TaskStatus::Running => {
                    ("·", Style::default().fg(t.accent))
                }
                crate::tui::panes::tasks::TaskStatus::Completed => {
                    ("✓", Style::default().fg(t.success))
                }
                crate::tui::panes::tasks::TaskStatus::Failed => ("!", Style::default().fg(t.error)),
                crate::tui::panes::tasks::TaskStatus::Cancelled => {
                    ("×", Style::default().fg(t.muted))
                }
                crate::tui::panes::tasks::TaskStatus::Pending => {
                    ("·", Style::default().fg(t.muted))
                }
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{glyph} "), style),
                Span::styled(task.name.clone(), Style::default().fg(t.text)),
            ]));
        }
    }
    lines.push(Line::from(Span::styled(
        "enter logs · ctrl-x cancel",
        Style::default().fg(t.muted),
    )));
    // The rail wraps at the width it was given: a name that does not fit used
    // to be cut with `…`, which hid exactly which job it was.
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    cache_rail_selection(frame, inner, app);
}

/// The diff rail: a count of what this turn changed, or an honest empty state.
fn draw_diff_rail(frame: &mut Frame, area: Rect, app: &mut App) {
    let (block, inner) = rail_frame(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let t = tokens();
    let mut lines: Vec<Line> = vec![rail_title("diff")];
    if app.diff_pane.hunks.is_empty() {
        lines.push(Line::from(Span::styled(
            "no file changes this turn",
            Style::default().fg(t.muted),
        )));
    } else {
        for hunk in app.diff_pane.hunks.iter().take(inner.height as usize) {
            let state = match hunk.accepted {
                Some(true) => ("accepted", t.success),
                Some(false) => ("rejected", t.error),
                None => ("pending", t.warning),
            };
            lines.push(Line::from(vec![
                Span::styled(hunk.file_path.clone(), Style::default().fg(t.text)),
                Span::styled(format!("  {}", state.0), Style::default().fg(state.1)),
            ]));
        }
    }
    lines.push(Line::from(Span::styled(
        "/diff opens the full review",
        Style::default().fg(t.muted),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    cache_rail_selection(frame, inner, app);
}

/// The completion list: one line per command, drawn against the prompt.
///
/// No border, no title, no count header, no key hints and no scrollbar. A box
/// around eight rows costs four rules to say "this is a list", and the footer
/// directly below already names the keys. Every row is exactly one line: a
/// description that wraps turns one command into three, which is what makes a
/// dropdown unreadable.
///
/// Returns the position within the list, for the prompt line to carry.
/// A bounded, animated progress layer for an in-process verb. Raw progress
/// lines arrive on a channel and are folded into stages here; nothing is ever
/// written to stderr while the TUI owns the terminal.
fn draw_progress(frame: &mut Frame, area: Rect, app: &App) {
    if app.progress_hidden
        || app.running_verb.is_none()
        || app.progress_lines.is_empty()
        || area.height < 12
        || area.width < 46
    {
        return;
    }
    let t = tokens();
    let lines = &app.progress_lines;
    let last = lines.back().map(String::as_str).unwrap_or("");
    let started = app.turn_started_ms.max(1);
    let elapsed_ms = crate::session::now_ms().saturating_sub(started);
    let secs = elapsed_ms / 1000;

    // The known kg stages, in order. A line matching a stage marks it done when
    // a later stage has begun, active while it is the last one seen.
    const STAGES: [(&str, &str); 5] = [
        ("walk", "Walk files"),
        ("extract", "Extract symbols"),
        ("embed", "Embed"),
        ("index", "Index"),
        ("reference", "References"),
    ];
    let mut active = 0usize;
    for (i, (needle, _)) in STAGES.iter().enumerate() {
        if lines
            .iter()
            .any(|line| line.to_ascii_lowercase().contains(needle))
        {
            active = i;
        }
    }
    let stage_count = STAGES
        .iter()
        .filter(|(needle, _)| {
            lines
                .iter()
                .any(|line| line.to_ascii_lowercase().contains(needle))
        })
        .count()
        .max(1);
    let fraction = (active + 1) as f64 / (STAGES.len() + 1) as f64;
    let _ = stage_count;

    // Braille spinner advances on a wall-clock tick: ~80 ms per frame, no SGR
    // blink, no color-only state.
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let frame_index = ((elapsed_ms / 80) as usize) % FRAMES.len();
    let spinner = FRAMES[frame_index];

    let width = area.width.saturating_sub(8).min(66);
    let height = (STAGES.len() + 4).min(area.height.saturating_sub(4) as usize) as u16;
    let x = area.x + (area.width - width) / 2;
    let y = area.y + 2;
    let rect = Rect::new(x, y, width, height);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .style(Style::default().bg(t.surface)),
        rect,
    );
    let inner = Rect::new(
        rect.x + 2,
        rect.y + 1,
        rect.width.saturating_sub(4),
        rect.height.saturating_sub(2),
    );

    let title = Line::from(vec![
        Span::styled(
            "Indexing code graph",
            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {spinner} {}s", secs),
            Style::default().fg(t.accent),
        ),
    ]);
    let mut body = vec![title];
    for (i, (_, label)) in STAGES.iter().enumerate() {
        let (glyph, style) = if i < active {
            ("✓", Style::default().fg(t.success))
        } else if i == active {
            (spinner, Style::default().fg(t.accent))
        } else {
            ("○", Style::default().fg(t.muted))
        };
        body.push(Line::from(vec![
            Span::styled(format!(" {glyph} "), style),
            Span::styled(
                (*label).to_string(),
                if i == active {
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.muted)
                },
            ),
        ]));
    }
    // Gauge last, then the current item and the honest key hints.
    let gauge = ratatui::widgets::LineGauge::default()
        .filled_style(Style::default().fg(t.accent))
        .unfilled_style(Style::default().fg(t.border))
        .ratio(fraction.clamp(0.0, 1.0))
        .label(format!(" {:.0}% ", fraction * 100.0));
    let gauge_area = Rect::new(inner.x, inner.y + body.len() as u16, inner.width, 1);
    let detail = Rect::new(inner.x, gauge_area.y + 1, inner.width, 1);
    if detail.y < rect.y + rect.height {
        frame.render_widget(
            Paragraph::new(ellipsize(last, inner.width as usize)),
            detail,
        );
    }
    if gauge_area.y < rect.y + rect.height {
        frame.render_widget(gauge, gauge_area);
    }
    body.truncate((inner.height).saturating_sub(2) as usize);
    frame.render_widget(Paragraph::new(body), inner);
}

fn draw_completions(
    frame: &mut Frame,
    area: Rect,
    prompt: Rect,
    app: &App,
) -> Option<(usize, usize)> {
    if area.width < 24 || area.height < 6 {
        return None;
    }
    // The `@` picker and the `/` list are both prompt-local. Only one is open
    // at a time: a line starting with `/` cannot open mentions.
    if app.mentions.is_open() {
        return draw_mention_completions(frame, area, prompt, app);
    }
    if !app.slash.is_open() {
        return None;
    }
    // The list grows upward from the prompt, so the line being typed never
    // moves. It may not start above the frame.
    let room = prompt.y.saturating_sub(area.y);
    if room == 0 {
        return None;
    }
    let width = area.width as usize;
    let rows = app.slash.rows();

    // Nothing matches: the list simply closes. The footer, one row below, says
    // so — a second line here saying the same thing would be the noise this
    // list exists to avoid.
    if rows.is_empty() {
        return None;
    }

    // A single row the prompt already spells out is not a list: a one-row box
    // under it would only repeat what is on the prompt.
    if rows.len() == 1 && app.slash.exact() {
        return None;
    }

    let room = room as usize;
    let selected = app.slash.selected_index().min(rows.len() - 1);
    let column = app.slash.name_column();
    // The dropdown is themed, not hard-coded: every colour comes from the
    // semantic tokens, so a palette change reaches the list with the rest of
    // the screen and `NO_COLOR` leaves only glyphs and weight.
    let t = tokens();

    // Measure the rows before drawing them. A wrapped description costs more
    // than one line, so a window sized only in rows could leave the highlight
    // off the top of a short screen while the counter still named it as the
    // selected row. Anchor the window on the highlight and drop rows above it
    // until the block fits the space that actually exists.
    let heights: Vec<usize> = rows
        .iter()
        .map(|row| row_lines(row, column, false, width, t).len())
        .collect();
    let mut start = selected.saturating_sub(MAX_ROWS - 1);
    while rows.len().saturating_sub(start) > MAX_ROWS {
        start += 1;
    }
    let mut start = start.min(selected);
    while start < selected
        && (start..=selected)
            .map(|index| heights[index])
            .sum::<usize>()
            > room
    {
        start += 1;
    }

    // Rows are drawn from the window start until the row cap or the screen runs
    // out. A description takes as many lines as it needs, so the budget is in
    // lines, not rows: a long description costs the row below it, never its own
    // tail.
    let mut lines: Vec<Line> = Vec::new();
    let mut highlight_drawn = false;
    for (index, row) in rows.iter().enumerate().skip(start) {
        if index >= start + MAX_ROWS || lines.len() >= room {
            break;
        }
        let is_selected = index == selected;
        let mut block = row_lines(row, column, is_selected, width, t);
        block.truncate(room - lines.len());
        lines.extend(block);
        highlight_drawn |= is_selected;
    }
    if lines.is_empty() {
        return None;
    }

    let height = lines.len() as u16;
    let rect = Rect::new(area.x, prompt.y - height, area.width, height);
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines), rect);
    // One row is not a list with a position in it, and a counter that names a
    // row the terminal never drew would be a lie.
    (rows.len() > 1 && highlight_drawn).then(|| (selected + 1, rows.len()))
}

/// The unified `@` picker: skills, agents and files in one list above the
/// prompt. It uses the same geometry and the same ink-only selection as the
/// `/` list; the only extra is a literal category word on every row, which
/// keeps the groups distinguishable when `NO_COLOR` strips every palette.
fn draw_mention_completions(
    frame: &mut Frame,
    area: Rect,
    prompt: Rect,
    app: &App,
) -> Option<(usize, usize)> {
    let room = prompt.y.saturating_sub(area.y);
    if room == 0 {
        return None;
    }
    let width = area.width as usize;
    let rows = &app.mentions.rows;
    if rows.is_empty() {
        return None;
    }
    let selected = app.mentions.selected_index().min(rows.len() - 1);
    let t = tokens();

    let mut start = selected.saturating_sub(MAX_ROWS - 1);
    while rows.len().saturating_sub(start) > MAX_ROWS {
        start += 1;
    }
    let start = start.min(selected);
    let mut lines: Vec<Line> = Vec::new();
    for (index, row) in rows.iter().enumerate().skip(start) {
        if index >= start + MAX_ROWS || lines.len() >= room as usize {
            break;
        }
        lines.extend(mention_row_lines(row, index == selected, width, t));
    }
    if lines.is_empty() {
        return None;
    }

    let height = lines.len() as u16;
    let rect = Rect::new(area.x, prompt.y - height, area.width, height);
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines), rect);
    (rows.len() > 1).then_some((selected + 1, rows.len()))
}

/// Clip a row to the frame without ever spending a background fill.
fn truncate_spans(spans: &mut Vec<Span<'static>>, width: usize) {
    let mut used = 0usize;
    let mut kept = Vec::with_capacity(spans.len());
    for span in spans.drain(..) {
        let len = span.content.chars().count();
        if used + len <= width {
            used += len;
            kept.push(span);
            continue;
        }
        let room = width.saturating_sub(used);
        if room > 0 {
            let text: String = span.content.chars().take(room - 1).collect();
            let mut text = text;
            text.push('…');
            kept.push(Span::styled(text, span.style));
        }
        break;
    }
    *spans = kept;
}

/// One mention row: `›`, a literal `[skill]`/`[agent]`/`[file]` tag, the token
/// with its matched letters picked out, and the source as muted ink.
fn mention_row_lines(
    row: &MentionRow,
    selected: bool,
    width: usize,
    t: crate::tui::theme::Tokens,
) -> Vec<Line<'static>> {
    let plain = if selected {
        Style::default().fg(t.text).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(t.muted)
    };
    let matched = if selected {
        Style::default()
            .fg(t.accent)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(t.accent)
    };
    let mut spans = vec![Span::styled(
        if selected { "› " } else { "  " },
        if selected {
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    )];
    // A word, not a colour: this is what separates the three groups in a
    // terminal where every colour token resolves to Reset.
    spans.push(Span::styled(
        format!("[{}] ", row.kind.label()),
        Style::default().fg(t.muted).add_modifier(Modifier::BOLD),
    ));

    let mut run = String::new();
    let mut run_matched = false;
    for (index, ch) in row.name.chars().enumerate() {
        let hit = row.hits.contains(&index);
        if !run.is_empty() && hit != run_matched {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_matched { matched } else { plain },
            ));
        }
        run_matched = hit;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_matched { matched } else { plain }));
    }
    if !row.description.is_empty() {
        spans.push(Span::styled(
            format!("  {}", row.description),
            Style::default().fg(t.muted),
        ));
    }
    truncate_spans(&mut spans, width);
    vec![Line::from(spans)]
}

/// One row: the marker, the command with its matched letters picked out, and
/// the description beside it — wrapped, not cut, so the row says what the
/// command is actually for instead of trailing off in the middle of it.
///
/// The command column is one fixed width so the names line up in a single column
/// the eye runs down; the description wraps under the column it started in.
fn row_lines(
    row: &Row,
    column: usize,
    selected: bool,
    width: usize,
    t: crate::tui::theme::Tokens,
) -> Vec<Line<'static>> {
    // Selection is carried by ink and weight, never by a fill. The old
    // selected_bg block painted a bright orange rectangle across the name and
    // description — loud, and in NO_COLOR it flattened every row to the same
    // default ink. Here the selected name steps up to `t.text` + BOLD, matched
    // letters keep `t.accent` (+ UNDERLINED), and the `›` marker and description
    // stay put. Every foreground is a real contrast against the canvas:
    // `selected_fg` is near-black on dark and would vanish with no fill, so it
    // is deliberately not used. Under NO_COLOR all tokens are Reset, so the
    // marker glyph and the BOLD modifier — which survive colour stripping —
    // are what name the selection.
    let plain = if selected {
        Style::default().fg(t.text).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(t.muted)
    };
    let matched = if selected {
        Style::default()
            .fg(t.accent)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(t.accent)
    };
    let mut spans = vec![Span::styled(
        if selected { "› " } else { "  " },
        if selected {
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    )];

    // With a fuzzy filter a row can match for a reason that is not obvious, and
    // this is the reason.
    let mut run = String::new();
    let mut run_matched = false;
    for (index, ch) in row.name.chars().enumerate() {
        let hit = row.hits.contains(&index);
        if !run.is_empty() && hit != run_matched {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_matched { matched } else { plain },
            ));
        }
        run_matched = hit;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_matched { matched } else { plain }));
    }

    // The description keeps its muted ink in both states: selection is already
    // named by the marker and the bold, accented name, so repainting the whole
    // row would only shout.
    let description_style = Style::default().fg(t.muted);

    // Beside the name while there is a column's worth of room for it, and under
    // the name when there is not: a three-word-per-line column is not a
    // description, it is a vertical list of words.
    let name_width = row.name.chars().count();
    let beside = width.saturating_sub(column + 4);
    let side = name_width < column && beside >= 16;
    let room = if side {
        beside
    } else {
        width.saturating_sub(4)
    };
    let wrapped: Vec<String> = if row.description.is_empty() || room < 8 {
        Vec::new()
    } else {
        crate::tui::app::wrap_text(&row.description, room)
    };

    if side && !wrapped.is_empty() {
        spans.push(Span::raw(" ".repeat(column - name_width + 2)));
        spans.push(Span::styled(wrapped[0].clone(), description_style));
    }
    let mut lines = vec![Line::from(spans)];
    let skip = usize::from(side);
    for chunk in wrapped.iter().skip(skip) {
        lines.push(Line::from(Span::styled(
            format!("{}{chunk}", " ".repeat(if side { column + 4 } else { 4 })),
            description_style,
        )));
    }
    lines
}

/// The brief: what this session opened, what is missing, what to do next —
/// then out of the way.
///
/// The shape is [`crate::tui::home`]'s decision: width alone does not answer
/// it, because a wide short window wants a strip and a tall narrow one wants
/// stacked facts. That module also owns the rule that a value wraps instead of
/// being cut at the frame edge.
fn draw_home(frame: &mut Frame, area: Rect, home: &Home, app: &mut App) {
    crate::tui::home::draw(frame, area, home, app.home_cursor);
    let x = area.x + 2.min(area.width);
    let width = area.width.saturating_sub(2);
    let rows = {
        let buffer = frame.buffer_mut();
        (area.y..area.y.saturating_add(area.height))
            .map(|y| {
                (x..x.saturating_add(width))
                    .map(|column| buffer[(column, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
    };
    app.set_home_selection_rows(x, area.y, width, rows);

    if let Some((first, last)) = app.home_selection {
        let y_start = area.y.saturating_add(first as u16);
        let y_end = area.y.saturating_add(last as u16);
        for y in y_start..=y_end.min(area.y.saturating_add(area.height.saturating_sub(1))) {
            let row_end = (x..x.saturating_add(width))
                .rev()
                .find(|column| !matches!(frame.buffer_mut().cell((*column, y)), Some(cell) if cell.symbol().trim().is_empty()));
            let Some(row_end) = row_end else {
                continue;
            };
            for x in x..=row_end {
                if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                    cell.set_style(mouse_selection_style(cell.style(), tokens()));
                }
            }
        }
    }
}

/// One follow-up button as ink only: no background fill, ever.
///
/// Hover, keyboard focus and drag selection all ask for the same treatment —
/// accent plus BOLD — so a prompt never wears the orange band the selection
/// fill would paint. When tokens carry no colour (NO_COLOR), the `[N]` and
/// `›`/`[ ]` glyphs in `line` and the BOLD modifier are the whole cue.
fn followup_line(line: &Line<'_>, active: bool, t: crate::tui::theme::Tokens) -> Line<'static> {
    let ink = if active {
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(t.muted)
    };
    Line::from(
        line.spans
            .iter()
            .map(|span| Span::styled(span.content.as_ref().to_string(), ink))
            .collect::<Vec<_>>(),
    )
}

fn draw_transcript(frame: &mut Frame, area: Rect, app: &mut App) {
    // No border and no `transcript` title: the transcript is the screen, and a
    // box drawn around the whole of it is a rule per column that says only "the
    // terminal is here". Two columns of gutter do the same job for a tenth of
    // the ink and keep the text off the edge.
    let inner = Rect {
        x: area.x + 2.min(area.width),
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // One column is reserved for the scrollbar for as long as the pane exists,
    // not only while it overflows: text that re-wraps the moment a bar appears
    // is a screen that jumps every time a turn lands.
    let bar = inner.width > 4;
    let text_width = inner.width.saturating_sub(u16::from(bar));
    app.set_viewport(text_width as usize, inner.height as usize);

    let visual = app.visual_lines(text_width as usize);
    let max = visual.len().saturating_sub(inner.height as usize);
    #[cfg(test)]
    eprintln!(
        "DEBUG transcript area={area:?} inner={inner:?} text_width={text_width} visual={} scroll_before={} follow={}",
        visual.len(),
        app.scroll,
        app.follow
    );
    if app.follow {
        app.scroll = max;
    } else {
        app.scroll = app.scroll.min(max);
    }
    let start = app.scroll.min(visual.len());
    let end = (start + inner.height as usize).min(visual.len());

    // The pointer's row, translated into the visible window. The shortcut
    // operates on the newest reasoning/tool detail, so only that row advertises
    // it; hovering an older row must not imply it will be the one expanded.
    let hovered = app.mouse_row.and_then(|row| {
        if row >= inner.y {
            let offset = (row - inner.y) as usize;
            if offset < end - start {
                return Some(start + offset);
            }
        }
        None
    });
    let latest_detail =
        visual
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, (role, _, header, _))| {
                (*header
                    && matches!(
                        role,
                        EntryRole::Thinking
                            | EntryRole::Tool
                            | EntryRole::ToolDone
                            | EntryRole::ToolFailed
                            | EntryRole::ToolInterrupted
                    ))
                .then_some(index)
            });
    let t = tokens();
    let lines: Vec<Line> = visual[start..end]
        .iter()
        .enumerate()
        .map(|(offset, (role, line, header, follow_index))| {
            // A follow-up button is ink only: accent + BOLD when hovered,
            // keyboard-focused or drag-selected, muted otherwise. There is no
            // background fill at any point — an orange band under a prompt is
            // the exact thing this surface avoids. Under NO_COLOR the `›`/`[ ]`
            // glyphs and BOLD carry the cue, not ink.
            if let Some(button) = follow_index {
                let active = app.follow_up_focus == Some(*button)
                    || hovered == Some(start + offset)
                    || app
                        .selection
                        .is_some_and(|(first, last)| (first..=last).contains(&(start + offset)));
                return followup_line(line, active, t);
            }
            let (style, glyph) = style_for(*role);
            let selected = app
                .selection
                .is_some_and(|(first, last)| (first..=last).contains(&(start + offset)));
            // A blank separator row carries no glyph: the space is the whole
            // of its ink. A thinking header carries its own marker, and its
            // body runs down a rail under it.
            if line.width() == 0 {
                return Line::from("");
            }
            // The rail is what makes reasoning impossible to mistake for the
            // answer, in a dark theme or a light one. Three marks stack: the
            // header wears the accent in bold, the body is the muted token
            // in italic faint under an accent gutter, and the body indents
            // one level deeper than any answer text. Any one of the three is
            // subtle; together they are unmistakable.
            let style = if *role == EntryRole::Thinking {
                if *header {
                    Style::default()
                        .fg(t.accent)
                        .add_modifier(Modifier::BOLD | Modifier::ITALIC)
                } else {
                    Style::default()
                        .fg(t.muted)
                        .add_modifier(Modifier::ITALIC | Modifier::DIM)
                }
            } else {
                style
            };
            let (glyph, mut glyph_style) = match (*role, *header) {
                (EntryRole::Thinking, true) => ("", style),
                (EntryRole::User, _) => (
                    glyph,
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                ),
                (EntryRole::Thinking, false) => ("┃   ", Style::default().fg(t.accent)),
                _ => (glyph, style.add_modifier(Modifier::BOLD)),
            };
            if selected {
                glyph_style = mouse_selection_style(glyph_style, t);
            }
            let mut spans = vec![Span::styled(glyph, glyph_style)];
            for span in &line.spans {
                // The row's colour belongs to the row; a span that named one
                // (inline code, say) keeps it, and every span keeps its weight.
                let mut patched = style.add_modifier(span.style.add_modifier);
                if let Some(fg) = span.style.fg {
                    patched = patched.fg(fg);
                }
                if selected {
                    patched = mouse_selection_style(patched, t);
                }
                spans.push(Span::styled(span.content.clone(), patched));
            }
            let mut drawn = Line::from(spans);
            if hovered == Some(start + offset)
                && latest_detail == Some(start + offset)
                && *header
                && let Some(text) = drawn.spans.get_mut(1)
            {
                text.content.to_mut().push_str(" · ctrl-r");
            }
            drawn
        })
        .collect();
    // Record where each button landed so a click can be mapped back to its
    // prompt without guessing from text. The fourth tuple field is set only on
    // follow-up rows by `visual_lines`, so this can never capture a normal row.
    let mut follow_rows = Vec::new();
    let mut follow_line_indices = Vec::new();
    for (offset, (_, _, _, follow_index)) in visual[start..end].iter().enumerate() {
        if let Some(button) = follow_index {
            if let Some(prompt) = app
                .follow_ups
                .first()
                .and_then(|(_, items)| items.get(*button))
                .map(|item| item.prompt.clone())
            {
                follow_rows.push((inner.y + offset as u16, prompt));
            }
            follow_line_indices.push((start + offset, *button));
        }
    }
    app.follow_up_rows = follow_rows;
    app.follow_up_line_indices = follow_line_indices;
    // Lines are already wrapped to the pane, so they are drawn verbatim. The
    // wrapper used to guard against a measured/render width mismatch, but it
    // re-wraps to the render width: a line the widget measured one column wider
    // than the wrapper did took a second row, and those extra rows pushed the
    // newest line out of the view exactly when following it.
    frame.render_widget(Paragraph::new(lines), inner);

    // A quiet thumb marks position without drawing a permanent rail beside the
    // transcript. It appears only when content actually overflows.
    if bar && visual.len() > inner.height as usize {
        let mut state = ScrollbarState::new(max).position(app.scroll);
        let track = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y,
            width: 1,
            height: area.height,
        };
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some(" "))
                .thumb_symbol("▏")
                .thumb_style(Style::default().fg(t.accent).add_modifier(Modifier::DIM)),
            track,
            &mut state,
        );
    }
}

fn draw_footer(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    position: Option<(usize, usize)>,
    caret: bool,
) {
    if area.width == 0 || area.height <= 1 {
        return;
    }

    // The position in the list rides on this row, right-aligned: it answers
    // "how many are left" on a row that already exists, instead of in a header
    // or a scrollbar that would cost their own lines.
    let counter = position
        .map(|(at, of)| format!("{at}/{of}"))
        .unwrap_or_default();
    let t = tokens();
    // The rest of the highlighted command is deliberately not drawn: the
    // dropdown already marks the selection with `›` and picks out the letters
    // the query landed on, and `Tab` completes. Painting the remainder into
    // the line made typing `/` look like it had already typed `/search`.
    let notice = app
        .notification
        .as_ref()
        .filter(|(_, expires)| std::time::Instant::now() < *expires);
    let hint_text = notice
        .map(|(message, _)| ellipsize(message, area.width as usize))
        .unwrap_or_else(|| ellipsize(&footer_hint(app), area.width as usize));
    // One hint row, always muted: the keys are stable chrome, and a bright
    // colour here would pull the eye off the answer above it. A notice borrows
    // the row for a moment but does not change its weight.
    let hint = Line::from(Span::styled(hint_text, Style::default().fg(t.muted)));

    // The active draft is the only framed surface in the conversation.
    let chips_h = u16::from(!app.attachments.is_empty());
    let surface = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
    // PLAN is a caution about what a turn will do, so it borrows the warning
    // role; every other state uses the one accent.
    let accent = if app.plan_mode { t.warning } else { t.accent };
    let effort = app.reasoning_effort.as_deref().unwrap_or("auto");
    let mode = if app.plan_mode { "PLAN" } else { "BUILD" };
    let home_model = app
        .home
        .as_ref()
        .and_then(|home| home.facts.iter().find(|fact| fact.label == "model"))
        .map(|fact| fact.value.as_str())
        .unwrap_or("default model");
    let model_id = if app.model.trim().is_empty() {
        let mut parts = home_model.split(" · ");
        let first = parts.next().unwrap_or(home_model);
        if first.eq_ignore_ascii_case("not configured") {
            "default model"
        } else if first == "model"
            || parts
                .clone()
                .next()
                .is_some_and(|state| state.starts_with("no key"))
        {
            first
        } else {
            parts.next().unwrap_or(first)
        }
    } else {
        app.model.as_str()
    };
    let context = Some(match (app.context_window_used, app.context_window_size) {
        (Some(used), Some(window)) if window > 0 => {
            format!("ctx {}/{}", compact_tokens(used), compact_tokens(window))
        }
        (Some(used), _) => format!("ctx {}/?", compact_tokens(used)),
        (_, Some(window)) if window > 0 => format!("ctx —/{}", compact_tokens(window)),
        _ => "ctx ?".to_string(),
    });
    let suffix = format!(
        " {effort} · {mode}{} ",
        context
            .as_ref()
            .map(|value| format!(" · {value}"))
            .unwrap_or_default()
    );
    let model = ellipsize(
        model_id,
        (area.width as usize).saturating_sub(suffix.chars().count() + 4),
    );
    let mut title = vec![Span::styled(
        format!(" {model}"),
        Style::default().fg(t.muted),
    )];
    title.push(Span::styled(
        format!(" {effort} · "),
        Style::default().fg(t.muted),
    ));
    title.extend([
        Span::styled(
            mode,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        context
            .map(|value| Span::styled(format!(" · {value}"), Style::default().fg(t.muted)))
            .unwrap_or_else(|| Span::raw("")),
        Span::raw(" "),
    ]);
    let title = Line::from(title);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(accent))
            .title_bottom(title)
            .title_alignment(Alignment::Right),
        surface,
    );
    let editor = Rect::new(
        area.x + 2,
        area.y + 1 + chips_h,
        area.width.saturating_sub(4),
        area.height.saturating_sub(3).saturating_sub(chips_h).max(1),
    );
    if chips_h == 1 {
        let mut chips = String::new();
        for file in &app.attachments {
            let name = std::path::Path::new(&file.path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| file.path.clone());
            chips.push_str(&format!("▧ {name}  "));
        }
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                ellipsize(
                    &format!("    {}", chips.trim_end()),
                    area.width.saturating_sub(1) as usize,
                ),
                Style::default().fg(accent),
            ))),
            Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), 1),
        );
    }
    // The typed text is hard-wrapped to the editor so the caret math below and
    // what is on screen agree; word wrapping would move a character to a row
    // the caret does not know about.
    let usable = composer_width(area.width);
    let rows_all = composer_lines(&app.input, usable);
    let visible = editor.height.max(1) as usize;
    let cursor_chars = app.cursor.min(app.input.chars().count());
    let (caret_row, caret_col) = caret_cell(&app.input, cursor_chars, usable);
    // Keep the caret's row on screen: scroll only once the text passes the box.
    let first = caret_row
        .saturating_sub(visible.saturating_sub(1))
        .min(rows_all.len().saturating_sub(visible));
    let mut lines: Vec<Line> = Vec::with_capacity(visible);
    for (index, text) in rows_all.iter().enumerate().skip(first).take(visible) {
        let glyph = if index == 0 {
            Span::styled(
                "› ",
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("  ")
        };
        let content = if app.input.is_empty() && app.attachments.is_empty() && index == 0 {
            Span::styled(" Ask Memnoth anything…", Style::default().fg(t.muted))
        } else {
            Span::styled(text.clone(), Style::default().fg(t.text))
        };
        lines.push(Line::from(vec![glyph, content]));
    }
    frame.render_widget(Paragraph::new(lines), editor);
    frame.render_widget(
        Paragraph::new(hint),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );

    if !counter.is_empty() && (area.width as usize) > counter.chars().count() + 4 {
        let width = counter.chars().count() as u16;
        frame.render_widget(
            Paragraph::new(Span::styled(counter, Style::default().fg(t.muted))),
            Rect::new(area.x + area.width - width - 2, area.y + 1, width, 1),
        );
    }

    // The cursor belongs after the prompt and the characters already typed —
    // but only when this line is the one taking keys.
    if caret {
        let x = (editor.x + 2 + caret_col as u16).min(editor.x + editor.width.saturating_sub(1));
        let y = editor.y + caret_row.saturating_sub(first) as u16;
        frame.set_cursor_position(Position::new(x, y));
    }
}

/// Compact token counts for the composer status, without rounding small
/// windows up to a misleading whole-kilo value.
fn compact_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 10_000 {
        format!("{}k", tokens / 1_000)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// Highlight the cells a person dragged over. The fill and ink are the
/// selection pair, so they change with the palette instead of being painted
/// with raw RGB here. With no palette there is no colour to spend, so reversal
/// keeps the selection visible — the only case that needs a modifier to carry
/// the meaning.
fn mouse_selection_style(style: Style, t: crate::tui::theme::Tokens) -> Style {
    if t.colored() {
        style.fg(t.selected_fg).bg(t.selected_bg)
    } else {
        style.add_modifier(Modifier::REVERSED)
    }
}

fn style_for(role: EntryRole) -> (Style, &'static str) {
    let t = tokens();
    match role {
        EntryRole::User => (Style::default().fg(t.accent), "› "),
        EntryRole::Answer => (Style::default().fg(t.text), "  "),
        EntryRole::Error => (Style::default().fg(t.error), "! "),
        EntryRole::Info => (Style::default().fg(t.muted), "· "),
        EntryRole::Tool => (Style::default().fg(t.accent), "◌ "),
        EntryRole::ToolDone => (Style::default().fg(t.success), "✓ "),
        EntryRole::ToolFailed => (Style::default().fg(t.error), "✗ "),
        EntryRole::ToolInterrupted => (Style::default().fg(t.muted), "· "),
        // Thinking is the one role that is italic: dim italic is how every
        // coding agent separates reasoning from the answer it produced, and
        // a reader's eye learns the distinction without a legend.
        EntryRole::Thinking => (
            Style::default().fg(t.muted).add_modifier(Modifier::ITALIC),
            "",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, Fact, HomeState, MentionKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    #[test]
    fn a_verb_on_its_thread_is_named_in_the_footer() {
        // Without this the screen looks idle while `/stats` waits on two cloud
        // calls, which is what "it hangs" means from the outside.
        let mut app = App::new("s", false);
        app.running_verb = Some("/stats".to_string());
        assert!(footer_hint(&app).contains("running /stats"));
        app.running_verb = None;
        assert!(!footer_hint(&app).contains("running"));
    }

    #[test]
    fn a_pending_sign_in_keeps_its_code_on_screen_after_a_cleared_transcript() {
        // The code only in a transcript is a code to hunt for: ctrl-l, a long
        // answer, or a scroll past it leaves the wait with nothing to act on.
        let mut app = App::new("s", false);
        app.push(
            EntryRole::Info,
            "sign in: open this link\n  https://x.test/app?device=8XXA-W58R",
        );
        app.clear();
        app.login_busy = true;
        app.login_started = Some(std::time::Instant::now());
        app.login_prompt = Some(crate::tui::login::LoginPrompt {
            user_code: "8XXA-W58R".to_string(),
            url: "https://memnoth.astrivya.ai/app/api-keys?device=8XXA-W58R".to_string(),
        });

        let text = screen(&mut app, 100, 24);
        assert!(text.contains("code 8XXA-W58R"), "{text}");
        assert!(
            text.contains("https://memnoth.astrivya.ai/app/api-keys?device=8XXA-W58R"),
            "{text}"
        );
        // And the footer names the key that opens it.
        assert!(text.contains("ctrl-o opens"), "{text}");
    }

    #[test]
    fn a_pending_sign_in_counts_its_wait_in_the_footer() {
        // The grant thread is silent until someone approves, so the footer is
        // the only place the wait can report itself.
        let mut app = App::new("s", false);
        app.login_busy = true;
        app.login_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(65));
        let hint = footer_hint(&app);
        assert!(hint.contains("waiting for approval"), "{hint}");
        assert!(hint.contains("1m 05s"), "{hint}");
        // Nothing is in front of the wait, so esc is its key.
        assert!(hint.contains("esc cancels"), "{hint}");

        // A half-typed line owns esc, so the footer names the key that still
        // cancels instead of one that would clear the line.
        app.input = "fix the parser".to_string();
        let hint = footer_hint(&app);
        assert!(!hint.contains("esc cancels"), "{hint}");
        assert!(hint.contains("/login cancels"), "{hint}");
        app.input.clear();

        // And it is on the frame, not only in the string: the footer is the row
        // that survives an ellipsized hint.
        let frame = screen(&mut app, 80, 24);
        assert!(frame.contains("waiting for approval"), "{frame}");
        assert!(frame.contains("1m 05s"), "{frame}");

        // Once the attempt ends the footer goes back to the keys of whatever
        // the person is doing.
        app.login_busy = false;
        app.login_started = None;
        assert!(!footer_hint(&app).contains("waiting for approval"));
    }

    #[test]
    fn a_pending_sign_in_is_drawn_over_the_brief() {
        // ctrl-l puts the brief back over an empty transcript; the wait is
        // still running, so its code has to be on that frame too.
        let mut app = App::new("s", false);
        app.home = Some(crate::tui::app::Home {
            identity: "solo · not signed in".to_string(),
            facts: Vec::new(),
            steps: Vec::new(),
            invitation: None,
            hidden_turns: 0,
        });
        app.login_busy = true;
        app.login_started = Some(std::time::Instant::now());
        app.login_prompt = Some(crate::tui::login::LoginPrompt {
            user_code: "QWER-1234".to_string(),
            url: "https://x.test/app/api-keys?device=QWER-1234".to_string(),
        });

        let text = screen(&mut app, 80, 25);
        assert!(text.contains("QWER-1234"), "{text}");
        assert!(text.contains("device=QWER-1234"), "{text}");
    }

    #[test]
    fn the_model_and_session_controls_stay_visible_in_the_composer_status() {
        let mut app = App::new("s", false);
        app.model = "kilo/reflection/beam-free".to_string();
        app.reasoning_effort = Some("medium".to_string());
        let wide = screen(&mut app, 80, 24);
        assert!(wide.contains("kilo/reflection/beam-free"), "{wide}");
        assert!(wide.contains("medium"), "{wide}");

        // The model clips to fit on a narrow terminal, preserving effort and
        // mode in the status strip.
        app.plan_mode = true;
        let narrow = screen(&mut app, 44, 24);
        assert!(narrow.contains("kilo/"), "{narrow}");
        assert!(narrow.contains("medium"), "{narrow}");
        assert!(narrow.contains("PLAN"), "{narrow}");
        assert!(narrow.contains("type message"), "{narrow}");
    }

    #[test]
    fn the_composer_grows_with_the_line_and_stops_at_eight_rows() {
        // The defect this covers: a long question wrapped into one clipped row,
        // so the words past the right edge were typed but never drawn.
        let width = 80;
        assert_eq!(composer_rows("", width), 1);
        assert_eq!(composer_rows("short question", width), 1);
        let two_rows = "x".repeat(composer_width(width) + 1);
        assert_eq!(composer_rows(&two_rows, width), 2, "one char past the row");
        assert_eq!(composer_rows("one\ntwo", width), 2, "an explicit newline");
        assert_eq!(
            composer_rows(&"x".repeat(10_000), width),
            MAX_COMPOSER_ROWS,
            "a paste must not eat the frame"
        );
    }

    #[test]
    fn the_quiet_composer_keeps_attachments_and_status_visible() {
        let mut app = App::new("s", false);
        app.set_paths("E:/memnoth-prod".into(), "E:/vault.db".into());
        app.model = "provider/actual-model:free".into();
        let blank = screen(&mut app, 80, 24);
        assert!(blank.contains("Ask Memnoth anything…"), "{blank}");
        assert!(
            blank.contains("provider/actual-model:free auto · BUILD"),
            "{blank}"
        );
        assert!(blank.contains('╭') && blank.contains('╯'), "{blank}");

        app.input = "Explain this image".into();
        app.cursor = app.input.chars().count();
        app.plan_mode = true;
        let draft = screen(&mut app, 80, 24);
        assert!(draft.contains("Explain this image"), "{draft}");
        assert!(
            draft.contains("provider/actual-model:free auto · PLAN"),
            "{draft}"
        );
        assert!(!draft.contains("Ask Memnoth anything…"), "{draft}");

        app.attachments.push(crate::provider::Attachment {
            path: "E:/screenshots/login.png".into(),
            mime: "image/png",
            bytes: Vec::new(),
        });
        let attached = screen(&mut app, 80, 24);
        assert!(attached.contains("▧ login.png"), "{attached}");
        assert!(attached.contains("Explain this image"), "{attached}");
    }

    #[test]
    fn a_frame_stays_inside_the_perf_budget() {
        // W12-05: one frame of the live app must stay far under a 60 fps
        // budget. Debug builds are slower than release, so the budget is
        // generous; the point is catching a frame that grew by an order of
        // magnitude, not shaving microseconds.
        let mut app = App::new("perf", true);
        app.set_paths("D:/ws".into(), "D:/state/vault.db".into());
        for i in 0..50 {
            app.push(EntryRole::User, format!("question {i}"));
            app.push(EntryRole::Answer, format!("answer {i}"));
        }
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        // Warm the layout caches, then time 100 frames.
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("warm draw");
        let started = std::time::Instant::now();
        for _ in 0..100 {
            terminal
                .draw(|frame| draw(frame, &mut app))
                .expect("draw must not fail");
        }
        let per_frame_ms = started.elapsed().as_secs_f64() * 10.0;
        assert!(
            per_frame_ms < 25.0,
            "mean frame {per_frame_ms:.2} ms exceeds the 25 ms debug budget"
        );
    }

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
    fn a_full_frame_is_one_header_line_flat_text_and_the_prompt() {
        let mut app = App::new("payments", true);
        app.set_paths("D:/ws".into(), "D:/state/vault.db".into());
        app.push(EntryRole::User, "what changed?");
        app.push(EntryRole::Answer, "a fact from the vault");
        // 80 columns is the baseline: no rail, so every line on screen is the
        // screen's own text.
        let text = screen(&mut app, 80, 30);
        // The header carries the session and the place, and nothing that cannot
        // change your next action: no version, no `(resumed)`.
        assert!(text.contains(" payments"), "{text}");
        assert!(!text.contains("(resumed)"), "{text}");
        assert!(!text.contains("graph missing"), "{text}");
        assert!(text.contains("D:/ws"), "{text}");
        assert!(text.contains("what changed?"), "{text}");
        assert!(text.contains("a fact from the vault"), "{text}");
        assert!(text.contains("type message"), "{text}");
        // The transcript has no frame; the active composer alone is framed.
        assert!(!text.contains("transcript"), "{text}");
        for frame_char in ['┌', '┐', '└', '┘'] {
            assert!(!text.contains(frame_char), "{frame_char} in\n{text}");
        }
        // The active composer alone has a frame; the transcript remains flat.
        assert!(text.contains('╭'), "the composer has a frame:\n{text}");
    }

    #[test]
    fn home_and_chat_hints_never_exceed_four_actions() {
        let mut app = App::new("s", false);
        let chat = footer_hint(&app);
        assert!(chat.split(" · ").count() <= 4, "{chat}");
        assert!(!chat.contains("Ctrl+Shift"), "{chat}");

        app.home_visible = true;
        let home = footer_hint(&app);
        assert!(home.split(" · ").count() <= 4, "{home}");

        app.home_visible = false;
        app.input = "draft".into();
        let editing = footer_hint(&app);
        assert!(editing.split(" · ").count() <= 4, "{editing}");
        assert!(editing.contains("shift-enter newline"), "{editing}");
    }

    #[test]
    fn a_panel_is_a_left_rule_and_a_title_not_a_box() {
        let mut app = App::new("payments", false);
        app.push(EntryRole::Answer, "an answer");
        app.context = "vault\n  412 beliefs".into();
        let text = screen(&mut app, 100, 24);
        assert!(text.contains("context"), "{text}");
        // The panel itself is a left rule, not a box: no square corners.
        // (The composer's rounded box below has its own rules.)
        assert!(!text.contains('┌'), "{text}");
        assert!(text.contains('│'), "the left rule still separates: {text}");
    }

    use crate::tui::app::Step;

    fn example_home() -> Home {
        Home {
            identity: "solo · not signed in · session \"s\" (new)".into(),
            facts: vec![
                Fact {
                    label: "workspace",
                    value: "E:/ws".into(),
                    state: HomeState::Ready,
                },
                Fact {
                    label: "vault",
                    value: "missing".into(),
                    state: HomeState::Missing,
                },
                Fact {
                    label: "graph",
                    value: "missing".into(),
                    state: HomeState::Missing,
                },
                Fact {
                    label: "model",
                    value: "claude-sonnet-4-5 · ready".into(),
                    state: HomeState::Ready,
                },
                Fact {
                    label: "cloud",
                    value: "not connected · /login".into(),
                    state: HomeState::Ready,
                },
            ],
            steps: vec![
                Step {
                    command: "/vault init".into(),
                    description: "beliefs with dates, kept on this machine".into(),
                    runnable: true,
                },
                Step {
                    command: "/search".into(),
                    description: "answer from both stores, no model needed".into(),
                    runnable: true,
                },
            ],
            invitation: None,
            hidden_turns: 0,
        }
    }

    #[test]
    fn the_brief_names_the_workspace_the_gaps_and_the_next_step() {
        let mut app = App::new("s", false);
        app.home = Some(example_home());
        let text = screen(&mut app, 110, 32);
        assert!(text.contains("memnoth"), "{text}");
        // The masthead is the header while the brief is up: the build version
        // belongs there, as a muted span, not behind a `/version` to go find.
        assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
        assert!(text.contains("E:/ws"), "{text}");
        assert!(text.contains("missing"), "{text}");
        assert!(text.contains("next"), "{text}");
        assert!(text.contains("/vault init"), "{text}");
        assert!(text.contains("beliefs with dates"), "{text}");
        // The invitation is a row, and says so: no marker, no slash.
    }

    #[test]
    fn the_selected_row_is_the_one_enter_would_run() {
        let mut app = App::new("s", false);
        app.home = Some(example_home());
        let first = screen(&mut app, 90, 30);
        assert_eq!(app.home_command().as_deref(), Some("/vault init"));

        app.home_move(1);
        let second = screen(&mut app, 90, 30);
        assert_eq!(app.home_command().as_deref(), Some("/search"));
        assert_ne!(first, second);

        // Wrapping: three rows or two, one key each way is symmetric.
        app.home_move(1);
        assert_eq!(app.home_command().as_deref(), Some("/vault init"));
        app.home_move(-1);
        assert_eq!(app.home_command().as_deref(), Some("/search"));
    }

    #[test]
    fn a_short_window_keeps_the_facts_and_one_action() {
        let mut app = App::new("s", false);
        app.home = Some(example_home());
        let text = screen(&mut app, 80, 14);
        assert!(text.contains("E:/ws"), "{text}");
        assert!(text.contains("/vault init"), "{text}");
        assert!(
            !text.contains("recent"),
            "the timeline goes before an action"
        );
    }

    #[test]
    fn the_brief_gives_way_to_the_first_answer_and_comes_back_on_request() {
        let mut app = App::new("s", false);
        app.home = Some(example_home());
        app.push(EntryRole::User, "what changed?");
        let text = screen(&mut app, 110, 32);
        assert!(text.contains("what changed?"), "{text}");

        app.open_home();
        let again = screen(&mut app, 110, 32);
        assert!(!again.contains("what changed?"), "{again}");

        app.close_home();
        let transcript = screen(&mut app, 110, 32);
        assert!(transcript.contains("what changed?"), "{transcript}");
    }

    #[test]
    fn the_brief_says_how_much_conversation_is_behind_it() {
        // The shape a launch makes: entries loaded, brief in front, count of
        // what is behind it on screen.
        let mut app = App::new("payments", true);
        let mut home = example_home();
        home.hidden_turns = 3;
        app.home = Some(home);
        app.push(EntryRole::User, "what changed?");
        app.open_home();

        let text = screen(&mut app, 100, 30);
        assert!(text.contains("3 turns"), "{text}");
        assert!(!text.contains("what changed?"), "{text}");
        // The footer names the key the line above it just promised.
        assert!(text.contains("esc chat"), "{text}");

        app.close_home();
        let transcript = screen(&mut app, 100, 30);
        assert!(transcript.contains("what changed?"), "{transcript}");
    }

    #[test]
    fn the_sidebar_is_gone_and_the_transcript_keeps_the_width() {
        let mut app = App::new("payments", false);
        app.account = Some("me@example.com".into());
        app.push(EntryRole::Answer, "an answer");

        let wide = screen(&mut app, 120, 32);
        assert!(!wide.contains("sidebar"), "{wide}");
        assert!(wide.contains("context"), "{wide}");
    }

    #[test]
    fn the_context_pane_is_dropped_when_the_window_stops_affording_it() {
        let mut app = App::new("payments", false);
        app.push(EntryRole::Answer, "an answer");
        app.context = "vault\n  initialised".into();

        // Two columns: the context pane still has a readable share.
        let middling = screen(&mut app, 92, 30);
        assert!(middling.contains("context"), "{middling}");
        assert!(!middling.contains("sidebar"), "{middling}");

        // One column: the transcript takes the frame, and the text the context
        // pane carried is gone rather than stacked one word per line.
        let narrow = screen(&mut app, 70, 24);
        assert!(!narrow.contains("context"), "{narrow}");
        assert!(narrow.contains("an answer"), "{narrow}");
    }

    #[test]
    fn the_slash_menu_lists_commands_above_the_prompt() {
        let mut app = App::new("s", false);
        app.push(EntryRole::Answer, "an answer");
        app.slash.sync("/vault", 6);
        let text = screen(&mut app, 110, 32);
        assert!(text.contains("recall"), "{text}");
        assert!(text.contains("esc close"), "{text}");
    }

    /// Type a line as if it had been typed, cursor at the end.
    fn typed(app: &mut App, line: &str) {
        app.input = line.to_string();
        app.cursor = line.chars().count();
        app.sync_slash();
    }

    #[test]
    fn the_highlight_stays_on_screen_when_rows_wrap_and_space_is_short() {
        let mut app = App::new("s", false);
        typed(&mut app, "/vault");
        assert!(app.slash.total() > MAX_ROWS);
        for _ in 0..4 {
            app.slash.select_next();
        }
        let expected = app.slash.rows()[app.slash.selected_index()].name.clone();
        // At 60 columns the descriptions wrap to two lines each, so five rows
        // need more room than a short terminal has above the prompt. The old
        // row-sized window drew only the first four and hid the highlight.
        let text = screen(&mut app, 60, 12);
        let marked: Vec<&str> = text
            .lines()
            .filter(|line| line.trim_start().starts_with('›'))
            .collect();
        assert!(
            marked.iter().any(|line| line.contains(&expected)),
            "highlighted {expected:?} is missing: {marked:?}\n{text}"
        );
        assert!(text.contains(&format!("5/{}", app.slash.total())), "{text}");
    }

    #[test]
    fn the_completion_list_is_text_not_a_box() {
        let mut app = App::new("s", false);
        typed(&mut app, "/vault");
        let text = screen(&mut app, 80, 30);
        assert!(text.contains("recall"), "{text}");
        // The list itself is text, not a box: no frame, no title, and no key
        // hint of its own — the footer already names the keys. The box on
        // screen belongs to the composer, which is rounded by design.
        for frame_char in ['┌', '┐', '└', '┘'] {
            assert!(!text.contains(frame_char), "{frame_char} in\n{text}");
        }
        assert!(text.contains('╭'), "the composer has a frame:\n{text}");
        assert!(!text.contains("commands ·"), "{text}");
        assert_eq!(text.matches("↑↓ move").count(), 1, "{text}");
        // The position in the list rides on the prompt row instead.
        assert!(text.contains("1/"), "{text}");
    }

    #[test]
    fn a_description_wraps_so_the_whole_sentence_is_readable() {
        let mut app = App::new("s", false);
        typed(&mut app, "/vault");
        let text = screen(&mut app, 60, 24);
        assert!(text.contains("Search the vault"), "{text}");
        assert!(
            text.contains("windows understood)"),
            "the end of the sentence is on screen:\n{text}"
        );
    }

    #[test]
    fn the_list_offers_at_most_five_commands() {
        let mut app = App::new("s", false);
        typed(&mut app, "/");
        let text = screen(&mut app, 100, 40);
        let rows = text
            .lines()
            .filter(|line| line.trim_start().starts_with("/"))
            .count();
        assert!(
            rows <= 5,
            "{rows} commands is more than can be read:\n{text}"
        );
        assert!(
            text.contains("1/"),
            "the count says how many there are: {text}"
        );
    }

    #[test]
    fn nothing_is_painted_into_the_line() {
        let mut app = App::new("s", false);
        typed(&mut app, "/vau");
        assert_eq!(app.input, "/vau", "the suggestion is never typed for you");
        let text = screen(&mut app, 80, 24);
        assert!(text.contains("/vault"), "{text}");

        // Fully typed: nothing left to suggest, so nothing is drawn twice.
        typed(&mut app, "/models");
        let text = screen(&mut app, 80, 24);
        assert_eq!(text.matches("/models").count(), 1, "{text}");
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

    /// At the bottom of a long transcript the newest line is the one on screen.
    /// A view that follows but cuts the last rows is a view that hides the
    /// answer it just received.
    #[test]
    fn following_shows_the_newest_line() {
        let mut app = App::new("s", false);
        for index in 0..40 {
            app.push(EntryRole::Answer, format!("filler line {index:02}"));
        }
        app.push(EntryRole::Answer, "the newest line must be visible");
        let text = screen(&mut app, 120, 40);
        assert!(text.contains("the newest line must be visible"), "{text}");
    }

    /// The same, with the markdown answer the live check resumes: a wrapped
    /// bullet and a final paragraph are the shapes that were clipped live.
    #[test]
    fn following_shows_the_last_line_of_a_markdown_answer() {
        let mut app = App::new("s", false);
        for index in 0..40 {
            app.push(EntryRole::Answer, format!("filler line {index:02}"));
        }
        app.push(
            EntryRole::Answer,
            "**Temporal vault**\n\nA belief is a claim with a date it became true, never a \
             fact that floats free of one.\n\n- Impact analysis — what breaks if you change a \
             symbol, which is exactly the question this sentence is long enough to have to \
             answer across several wrapped lines before it finally ends here (`kg_impact`)\n\
             - Read files directly with line ranges\n\n`kg_impact_status` is one name, never italics.",
        );
        let text = screen(&mut app, 120, 40);
        assert!(text.contains("never italics"), "{text}");
    }

    #[test]
    fn a_wrapped_line_never_breaks_again_on_screen() {
        // The pane is 60 wide; the glyph spends two of those columns. Wrapping
        // to 60 made the terminal re-wrap each full line, which showed up as a
        // stray word at the left margin.
        let mut app = App::new("s", false);
        let long = "It moved 3 to 5 because upstream raised the gateway timeout \
                    and the reason is recorded on the closed version of the belief";
        app.push(EntryRole::Answer, long);
        let text = screen(&mut app, 60, 24);

        // Wrapping to the pane width instead of the text width is what made the
        // terminal re-wrap each full line. The draw records the text width it
        // used, and `visual_lines` takes the glyph off that.
        // The draw spends two columns on the gutter and one on the scrollbar
        // column it reserves; `visual_lines` then takes the glyph off that.
        let wrapped = crate::tui::app::wrap_text(long, 60 - 2 - 1 - crate::tui::app::GUTTER);
        let rows: Vec<&str> = text
            .lines()
            .skip(1)
            .take_while(|line| !line.trim().is_empty())
            .collect();
        assert_eq!(rows.len(), wrapped.len(), "{text}");
        // Four columns is the screen's gutter plus the row's glyph.
        for (row, chunk) in rows.iter().zip(&wrapped) {
            assert_eq!(row.trim_end(), format!("    {chunk}"), "{text}");
        }
    }

    #[test]
    fn an_unobserved_tool_note_is_never_rendered_as_success() {
        let mut app = App::new("s", false);
        app.push(EntryRole::Answer, "an answer");
        let from = app.entries.len();
        app.push_tool("kg refs   RETRY_CAP");

        let running = screen(&mut app, 100, 24);
        assert!(running.contains("searching code graph"), "{running}");

        app.resolve_tools(from);
        let done = screen(&mut app, 100, 24);
        assert!(done.contains("interrupted (result not observed)"), "{done}");
        assert!(!done.contains('▌'), "no row is left running: {done}");
    }

    #[test]
    fn a_turn_closes_with_what_it_cost_and_how_it_answered() {
        let mut app = App::new("s", false);
        app.push(EntryRole::Answer, "an answer");
        app.push(EntryRole::Info, "evidence · 3 tools · 1.2k tokens");
        let text = screen(&mut app, 100, 24);
        assert!(text.contains("evidence · 3 tools · 1.2k tokens"), "{text}");
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

    #[test]
    fn selection_uses_tokens_and_keeps_a_reversal_without_colour() {
        let dark = crate::tui::theme::Tokens::for_palette(crate::tui::theme::Palette::Dark);
        let styled = mouse_selection_style(Style::default(), dark);
        assert_eq!(styled.fg, Some(dark.selected_fg));
        assert_eq!(styled.bg, Some(dark.selected_bg));

        let none = crate::tui::theme::Tokens::for_palette(crate::tui::theme::Palette::NoColor);
        let plain = mouse_selection_style(Style::default(), none);
        assert_eq!(plain.fg, None, "NO_COLOR spends no ink on a foreground");
        assert_eq!(plain.bg, None, "NO_COLOR spends no ink on a background");
        assert!(plain.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn progress_overlay_derives_stages_from_real_lines_only() {
        // The overlay must reflect work the verb actually reported, never
        // invent a stage or a percentage that the operation did not measure.
        let lines = [
            "walking E:\\memnoth-prod",
            "walked 1362 file(s) in 0.09s",
            "extracting symbols from 560 changed file(s)",
        ];
        let lowered: Vec<String> = lines.iter().map(|l| l.to_ascii_lowercase()).collect();
        let seen = ["walk", "extract", "embed", "index", "reference"]
            .iter()
            .map(|needle| lowered.iter().any(|l| l.contains(needle)))
            .collect::<Vec<_>>();
        assert_eq!(seen, vec![true, true, false, false, false]);
        assert!(
            lines[1].contains("1362"),
            "counts come from the verb, not a guess"
        );
    }

    #[test]
    fn selected_completion_rows_change_ink_not_fill() {
        let t = tokens();
        let row = Row {
            name: "/vault".into(),
            description: "search the vault".into(),
            hits: vec![1],
        };
        let selected = row_lines(&row, 20, true, 80, t);
        let spans = &selected[0].spans;
        assert_eq!(spans[0].content.as_ref(), "› ");
        // No loud paint: every span of the selected row leaves the background
        // alone. The old selected_bg block covered the name and description
        // with a bright orange rectangle — the complaint this change fixes.
        for span in spans {
            assert_eq!(span.style.bg, None, "selected rows carry no fill: {span:?}");
            assert_ne!(
                span.style.bg,
                Some(t.selected_bg),
                "the bright selection fill came back: {span:?}"
            );
        }
        // The marker names the selection in ink that survives every palette.
        assert_eq!(spans[0].style.fg, Some(t.accent));
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));

        let prefix = spans
            .iter()
            .find(|span| span.content.as_ref() == "/")
            .expect("the unmatched prefix");
        assert_eq!(
            prefix.style.fg,
            Some(t.text),
            "the selected name must contrast with the canvas"
        );
        assert_ne!(prefix.style.fg, Some(t.selected_fg));
        assert!(prefix.style.add_modifier.contains(Modifier::BOLD));

        let hit = spans
            .iter()
            .find(|span| span.content.as_ref() == "v")
            .expect("the matched letter");
        assert_eq!(hit.style.fg, Some(t.accent));
        assert!(
            hit.style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::UNDERLINED)
        );

        // The description stays muted, not repainted with the loud fill.
        let desc = spans
            .iter()
            .find(|span| span.content.as_ref() == "search the vault")
            .expect("the description");
        assert_eq!(desc.style.fg, Some(t.muted));

        let plain = row_lines(&row, 20, false, 80, t);
        let prefix = plain[0]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "/")
            .expect("the unmatched prefix");
        assert_eq!(prefix.style.fg, Some(t.muted));
    }

    #[test]
    fn no_color_dropdown_keeps_the_marker_and_weight_as_selection() {
        // NO_COLOR makes every token Reset, so colour cannot mark the selected
        // row. The `›` glyph and the BOLD modifier survive colour stripping and
        // still carry the selection; no raw colour ever reaches a background.
        let t = crate::tui::theme::Tokens::for_palette(crate::tui::theme::Palette::NoColor);
        let row = Row {
            name: "/vault".into(),
            description: "search the vault".into(),
            hits: vec![1],
        };
        let selected = row_lines(&row, 20, true, 80, t);
        let spans = &selected[0].spans;
        assert_eq!(spans[0].content.as_ref(), "› ");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        let name = spans
            .iter()
            .find(|span| span.content.as_ref() == "/")
            .expect("the selected name");
        assert!(name.style.add_modifier.contains(Modifier::BOLD));
        for span in spans {
            assert_eq!(span.style.bg, None, "no fill spends raw colour: {span:?}");
        }
    }

    #[test]
    fn the_footer_hint_stays_muted() {
        let mut app = App::new("s", false);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("draw must not fail");
        let buffer = terminal.backend().buffer();
        let last = buffer.area.height - 1;
        let t = tokens();
        let cells: Vec<_> = (0..buffer.area.width)
            .map(|x| &buffer[(x, last)])
            .filter(|cell| !cell.symbol().trim().is_empty())
            .collect();
        assert!(!cells.is_empty(), "the hint row is drawn");
        for cell in cells {
            assert_eq!(cell.fg, t.muted, "a bright key hint: {cell:?}");
        }
    }

    #[test]
    fn every_colour_on_the_frame_comes_from_the_tokens() {
        let t = tokens();
        let allowed = [
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
            Color::Reset,
        ];
        let mut app = App::new("s", false);
        app.home = Some(example_home());
        app.push(EntryRole::User, "what changed?");
        app.open_home();
        app.plan_mode = true;
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("draw must not fail");
        for cell in terminal.backend().buffer().content() {
            assert!(
                allowed.contains(&cell.fg) && allowed.contains(&cell.bg),
                "a colour outside the token set reached the frame: {cell:?}"
            );
        }
    }

    fn sample_mentions() -> Vec<MentionRow> {
        vec![
            MentionRow {
                kind: MentionKind::Skill,
                name: "@skill:agent-reach".into(),
                token: "@skill:agent-reach".into(),
                description: String::new(),
                hits: vec![8],
                enabled: true,
            },
            MentionRow {
                kind: MentionKind::Agent,
                name: "@agent:explore".into(),
                token: "@agent:explore".into(),
                description: "Explore the codebase and report findings".into(),
                hits: vec![9],
                enabled: true,
            },
            MentionRow {
                kind: MentionKind::File,
                name: "@src/tui/app.rs".into(),
                token: "@src/tui/app.rs".into(),
                description: String::new(),
                hits: vec![1],
                enabled: true,
            },
        ]
    }

    #[test]
    fn mention_rows_name_their_category_without_a_background_fill() {
        let t = tokens();
        let selected = mention_row_lines(&sample_mentions()[0], true, 80, t);
        let spans = &selected[0].spans;
        assert_eq!(spans[0].content.as_ref(), "› ");
        assert_eq!(spans[0].style.fg, Some(t.accent));
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(
            spans.iter().any(|span| span.content.as_ref() == "[skill] "),
            "the category is a word, not a colour: {spans:?}"
        );
        for span in spans {
            assert_eq!(span.style.bg, None, "mention rows carry no fill: {span:?}");
            assert_ne!(span.style.bg, Some(t.selected_bg));
        }

        let plain = mention_row_lines(&sample_mentions()[2], false, 80, t);
        assert!(
            plain[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref() == "[file] ")
        );
    }

    #[test]
    fn no_color_mention_rows_keep_the_cue_and_the_marker() {
        let t = crate::tui::theme::Tokens::for_palette(crate::tui::theme::Palette::NoColor);
        let lines = mention_row_lines(&sample_mentions()[1], true, 80, t);
        let spans = &lines[0].spans;
        assert_eq!(spans[0].content.as_ref(), "› ");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(
            spans.iter().any(|span| span.content.as_ref() == "[agent] "),
            "NO_COLOR strips ink, not words: {spans:?}"
        );
        for span in spans {
            assert_eq!(span.style.bg, None, "no fill spends raw colour: {span:?}");
        }
    }

    #[test]
    fn the_picker_draws_all_three_groups_above_the_prompt() {
        let mut app = App::new("s", false);
        app.input = "@".into();
        app.cursor = 1;
        app.mentions.active = true;
        app.mentions.prefix = "@".into();
        app.mentions.rows = sample_mentions();

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("draw must not fail");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        for cue in [
            "[skill] @skill:agent-reach",
            "[agent] @agent:explore",
            "[file] @src/tui/app.rs",
        ] {
            assert!(text.contains(cue), "the picker did not draw {cue:?}");
        }
        assert!(footer_hint(&app).contains("enter insert"));
    }

    fn sample_followups() -> Vec<crate::tui::app::FollowUp> {
        vec![
            crate::tui::app::FollowUp {
                label: "verify: rerun cargo test".into(),
                prompt: "Rerun cargo test and report the failing cases.".into(),
            },
            crate::tui::app::FollowUp {
                label: "explain: why main.rs changed".into(),
                prompt: "Explain what changed in /ws/src/main.rs and why.".into(),
            },
        ]
    }

    #[test]
    fn followup_buttons_draw_their_exact_labels_in_the_transcript_flow() {
        let mut app = App::new("s", false);
        app.push(EntryRole::User, "fix it");
        app.push(EntryRole::Answer, "a substantive answer with real content");
        app.set_follow_ups(Some(1), sample_followups());
        let text = screen(&mut app, 100, 30);
        for label in [
            "[1] verify: rerun cargo test",
            "[2] explain: why main.rs changed",
        ] {
            assert!(
                text.contains(label),
                "button missing from transcript: {label}\n{text}"
            );
        }
        // They sit in the flow, below the answer, not in a floating modal.
        let answer = text.find("a substantive answer").unwrap();
        let first_button = text.find("[1] verify").unwrap();
        assert!(
            first_button > answer,
            "buttons must follow the answer: {text}"
        );
        assert!(!text.contains("follow-ups"), "no modal wrapper: {text}");
    }

    #[test]
    fn a_focused_followup_uses_accent_and_bold_with_no_fill() {
        let t = tokens();
        let base = Line::from("› [1] verify: rerun cargo test");
        let active = followup_line(&base, true, t);
        for span in &active.spans {
            assert_eq!(span.style.bg, None, "no background fill: {span:?}");
            assert_ne!(span.style.bg, Some(t.selected_bg));
            assert_eq!(span.style.fg, Some(t.accent));
            assert!(span.style.add_modifier.contains(Modifier::BOLD));
        }
        let plain = followup_line(&base, false, t);
        for span in &plain.spans {
            assert_eq!(span.style.fg, Some(t.muted));
            assert_eq!(span.style.bg, None);
        }
    }

    #[test]
    fn no_color_followups_keep_the_glyph_and_bold_as_the_cue() {
        let t = crate::tui::theme::Tokens::for_palette(crate::tui::theme::Palette::NoColor);
        let base = Line::from("› [1] verify: rerun cargo test");
        let active = followup_line(&base, true, t);
        // The marker and the number are literal text, so colour stripping cannot
        // take them away; BOLD survives the strip and carries the selection.
        assert_eq!(
            active.spans[0].content.as_ref(),
            "› [1] verify: rerun cargo test"
        );
        assert!(active.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(active.spans[0].style.bg, None);
        assert_eq!(
            active.spans[0].style.fg,
            Some(Color::Reset),
            "NO_COLOR spends no ink"
        );
    }
}
