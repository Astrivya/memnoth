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
use crate::tui::app::{App, EntryRole, Fact, Home, HomeState};
use crate::tui::slash::Row;

/// Width of the label column in the brief's facts.
const FACT_LABEL: usize = 12;

/// Width of the command column in `next`.
const STEP_COLUMN: usize = 20;

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
    if app.approval.is_some() || app.pending_shell.is_some() {
        "y approve · n / esc reject · the box names exactly what changes".to_string()
    } else if let Some(needs) = app.slash.needs() {
        // The argument list says what the position is for as well as what is
        // there: a list of files with no word about why is a riddle.
        format!("needs {needs} · tab completes · enter runs")
    } else if app.slash.is_open() {
        if app.slash.total() == 0 {
            "no command matches · enter runs the line as typed".to_string()
        } else {
            "↑↓ move · tab complete · enter run · esc close".to_string()
        }
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
        "↑↓ choose · enter run · tab edit · / all commands · ? keys".to_string()
    } else if app.busy {
        "esc cancel turn · ctrl-c twice quit · / commands · ctrl-k palette".to_string()
    } else if let Some(running) = &app.running_verb {
        // A verb on its own thread: the screen stays live, so this line is what
        // says the silence is work and not a hang.
        format!("running {running} · ctrl-c quits · the answer lands here")
    } else if app.active_pane != 0 {
        "tab next pane · ctrl-t tasks · esc transcript · / commands".to_string()
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
        // It takes the fifth slot rather than adding a sixth: a footer is five
        // keys wide, and the one that belongs is the one this state enabled.
        "enter send · ctrl-g fix · ? help · ctrl-k palette · / commands".to_string()
    } else {
        "enter send · / commands · ? help · ctrl-k palette · ctrl-t tasks".to_string()
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
    // saying the same thing twice.
    let splash = app.home_showing() && app.entries.is_empty();
    let rows = Layout::vertical([
        Constraint::Length(if splash { 0 } else { 1 }),
        Constraint::Min(3),
        Constraint::Length(3),
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

    let label = Style::default().fg(Color::DarkGray);
    let lines = vec![
        Line::from(vec![
            Span::styled("  code ", label),
            Span::styled(
                signin.user_code.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  open ", label),
            Span::styled(
                ellipsize(&signin.url, (area.width as usize).saturating_sub(7)),
                Style::default().fg(Color::Cyan),
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
    let width = area.width as usize;
    let mut spans = vec![Span::styled(
        format!(" {}", app.name),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];

    // A state with no action is a riddle, so the only graph fact that earns this
    // line names the verb that resolves it (`mod.rs::graph_state` decides both
    // the wording and whether there is anything to say).
    if let Some(attention) = &app.graph_attention {
        spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format!("{} · {}", attention.label, attention.fix),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
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
        spans.push(Span::styled(path, Style::default().fg(Color::DarkGray)));
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
        Some(home) => draw_home(frame, columns[0], &home, app.home_cursor),
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
        .border_style(Style::default().fg(Color::DarkGray))
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
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    ))
}

fn draw_context_rail(frame: &mut Frame, area: Rect, app: &App) {
    let (block, inner) = rail_frame(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let mut lines = vec![rail_title("context")];
    for line in app.context.lines() {
        lines.push(Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(Color::Gray),
        )));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The task rail: what the session is doing in the background. Empty is a
/// state, not a blank box — it says what will appear here.
fn draw_tasks_rail(frame: &mut Frame, area: Rect, app: &App) {
    let (block, inner) = rail_frame(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let width = inner.width as usize;
    let mut lines: Vec<Line> = vec![rail_title("tasks")];
    if app.tasks_pane.tasks.is_empty() {
        lines.push(Line::from(Span::styled(
            ellipsize("no background tasks", width),
            Style::default().fg(Color::Gray),
        )));
        lines.push(Line::from(Span::styled(
            ellipsize("start one: /tasks run <cmd>", width),
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for task in app.tasks_pane.tasks.iter().take(inner.height as usize) {
            let (glyph, colour) = match task.status {
                crate::tui::panes::tasks::TaskStatus::Running => ("·", Color::Cyan),
                crate::tui::panes::tasks::TaskStatus::Completed => ("✓", Color::Green),
                crate::tui::panes::tasks::TaskStatus::Failed => ("!", Color::Red),
                crate::tui::panes::tasks::TaskStatus::Cancelled => ("×", Color::DarkGray),
                crate::tui::panes::tasks::TaskStatus::Pending => ("·", Color::DarkGray),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{glyph} "), Style::default().fg(colour)),
                Span::styled(
                    ellipsize(&task.name, width.saturating_sub(2)),
                    Style::default().fg(Color::White),
                ),
            ]));
        }
    }
    lines.push(Line::from(Span::styled(
        ellipsize("enter logs · ctrl-x cancel", width),
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The diff rail: a count of what this turn changed, or an honest empty state.
fn draw_diff_rail(frame: &mut Frame, area: Rect, app: &App) {
    let (block, inner) = rail_frame(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let width = inner.width as usize;
    let mut lines: Vec<Line> = vec![rail_title("diff")];
    if app.diff_pane.hunks.is_empty() {
        lines.push(Line::from(Span::styled(
            ellipsize("no file changes this turn", width),
            Style::default().fg(Color::Gray),
        )));
    } else {
        for hunk in app.diff_pane.hunks.iter().take(inner.height as usize) {
            let state = match hunk.accepted {
                Some(true) => ("accepted", Color::Green),
                Some(false) => ("rejected", Color::Red),
                None => ("pending", Color::Yellow),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    ellipsize(&hunk.file_path, width.saturating_sub(11)),
                    Style::default().fg(Color::White),
                ),
                Span::styled(format!("  {}", state.0), Style::default().fg(state.1)),
            ]));
        }
    }
    lines.push(Line::from(Span::styled(
        ellipsize("/diff opens the full review", width),
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
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
fn draw_completions(
    frame: &mut Frame,
    area: Rect,
    prompt: Rect,
    app: &App,
) -> Option<(usize, usize)> {
    if !app.slash.is_open() || area.width < 24 || area.height < 6 {
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

    // A single row the prompt already spells out is not a list: the ghost on
    // the line is the whole suggestion, and a one-row box under it is noise.
    if rows.len() == 1 && (app.slash.ghost().is_some() || app.slash.exact()) {
        return None;
    }

    let room = room as usize;
    let start = app.slash.window_start(rows.len().min(MAX_ROWS));
    let selected = app.slash.selected_index();
    let column = app.slash.name_column();

    // Rows are drawn from the window start until the row cap or the screen runs
    // out. A description takes as many lines as it needs, so the budget is in
    // lines, not rows: a long description costs the row below it, never its own
    // tail.
    let mut lines: Vec<Line> = Vec::new();
    for (offset, row) in rows.iter().skip(start).enumerate() {
        if offset >= MAX_ROWS || lines.len() >= room {
            break;
        }
        let mut block = row_lines(row, column, start + offset == selected, width);
        block.truncate(room - lines.len());
        lines.extend(block);
    }
    if lines.is_empty() {
        return None;
    }

    let height = lines.len() as u16;
    let rect = Rect::new(area.x, prompt.y - height, area.width, height);
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines), rect);
    // One row is not a list with a position in it.
    (rows.len() > 1).then(|| (selected + 1, rows.len()))
}

/// One row: the marker, the command with its matched letters picked out, and
/// the description beside it — wrapped, not cut, so the row says what the
/// command is actually for instead of trailing off in the middle of it.
///
/// The command column is one fixed width so the names line up in a single column
/// the eye runs down; the description wraps under the column it started in.
fn row_lines(row: &Row, column: usize, selected: bool, width: usize) -> Vec<Line<'static>> {
    let plain = if selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    // Cyan always means "this is what you typed": the prompt glyph, the marker
    // and the letters the query landed on.
    let matched = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan)
    };
    let mut spans = vec![Span::styled(
        if selected { "› " } else { "  " },
        if selected { matched } else { Style::default() },
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

    let description_style = Style::default().fg(if selected {
        Color::Gray
    } else {
        Color::DarkGray
    });

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
/// There is no wordmark rule and no tagline. On a 24-row terminal they cost a
/// sixth of the frame to restate the fact rows underneath them, and the version
/// reads just as well on the line it already shares.
fn draw_home(frame: &mut Frame, area: Rect, home: &Home, cursor: usize) {
    // No box and no `home` title. The screen is already the home; framing it
    // only to label it costs two rows and two columns of a small frame.
    let inner = Rect {
        x: area.x + 2.min(area.width),
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let height = inner.height as usize;

    let mut lines: Vec<Line> = Vec::new();

    // Product name left, version right: the only ornament on the screen, and
    // it fits on a line that would otherwise be blank.
    let version = env!("CARGO_PKG_VERSION");
    let pad = width
        .saturating_sub("memnoth".len() + version.chars().count() + 1)
        .max(1);
    lines.push(Line::from(vec![
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
        ellipsize(&home.identity, width),
        Style::default().fg(Color::Gray),
    )));

    // The blocks are built separately and then fitted, because on a short
    // window something has to give and it must not be the action: a brief that
    // lists five problems and no way to fix one is a complaint. Facts are the
    // first thing trimmed, then the timeline; `next` keeps at least one row.
    let mut facts: Vec<Line> = Vec::new();
    if !home.facts.is_empty() {
        facts.push(Line::from(""));
        for fact in &home.facts {
            facts.push(fact_row(fact, width));
        }
    }

    let mut next_block: Vec<Line> = Vec::new();
    if !home.steps.is_empty() {
        next_block.push(Line::from(""));
        next_block.push(Line::from(Span::styled(
            "next",
            Style::default().fg(Color::White),
        )));
        for (index, step) in home.steps.iter().enumerate() {
            let selected = index == cursor && step.runnable;
            let room = width.saturating_sub(2 + STEP_COLUMN);
            let marker = if selected { "▸ " } else { "  " };
            let marker_style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let command_style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            let description_style = if selected {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            next_block.push(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(format!("{:<STEP_COLUMN$}", step.command), command_style),
                Span::styled(ellipsize_words(&step.description, room), description_style),
            ]));
        }
        // The one row that is not a verb: it says what the prompt below it
        // accepts. Never selectable, so it never looks like a dead button.
        if height >= 20
            && let Some(invitation) = &home.invitation
        {
            let room = width.saturating_sub(2 + STEP_COLUMN);
            next_block.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{:<STEP_COLUMN$}", "ask anything"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    ellipsize_words(invitation, room),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }

    // 17 inner rows is a 24-row terminal: the footer and the header are
    // already gone, so a higher gate would hide the timeline on the baseline
    // screen the design is drawn at.
    let mut recent: Vec<Line> = Vec::new();
    if height >= 17 && !home.recent.is_empty() {
        recent.push(Line::from(""));
        recent.push(Line::from(Span::styled(
            "recent",
            Style::default().fg(Color::White),
        )));
        for row in home.recent.iter().take(3) {
            recent.push(Line::from(Span::styled(
                ellipsize(row, width),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    // Fit, in the order the brief is worth reading. On a short window `next`
    // drops to one row and the timeline goes first, because a fact with no way
    // to act on it is a report, not a brief.
    if lines.len() + facts.len() + next_block.len() + recent.len() > height {
        recent.clear();
        next_block.truncate(3); // blank, `next`, one command
    }
    let facts_room = height.saturating_sub(lines.len() + next_block.len() + recent.len());
    if facts.len() > facts_room && facts.first().is_some_and(|line| line.spans.is_empty()) {
        // The separator goes before a fact does.
        facts.remove(0);
    }
    facts.truncate(facts_room);
    lines.extend(facts);
    lines.extend(next_block);
    lines.extend(recent);
    // The key hint lives in the footer, one line below, so it is stated once.
    lines.truncate(height);
    while lines.len() < height {
        lines.push(Line::from(""));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// One label/value row: the label is ink, the value carries the state.
///
/// `missing` and `stale` are yellow, `ready` is white. The value says what is
/// true; the fix for a problem is a row in `next`, never a suffix here.
fn fact_row(fact: &Fact, width: usize) -> Line<'static> {
    let value_style = match fact.state {
        HomeState::Ready => Style::default().fg(Color::White),
        HomeState::Missing | HomeState::Stale => Style::default().fg(Color::Yellow),
    };
    Line::from(vec![
        Span::styled(
            format!("{:<FACT_LABEL$}", fact.label),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            ellipsize(&fact.value, width.saturating_sub(FACT_LABEL)),
            value_style,
        ),
    ])
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

    app.set_viewport(inner.width as usize, inner.height as usize);

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

fn draw_footer(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    position: Option<(usize, usize)>,
    caret: bool,
) {
    if area.width == 0 || area.height <= 2 {
        return;
    }

    // The position in the list rides on this row, right-aligned: it answers
    // "how many are left" on a row that already exists, instead of in a header
    // or a scrollbar that would cost their own lines.
    let counter = position
        .map(|(at, of)| format!("{at}/{of}"))
        .unwrap_or_default();
    let typed = 2 + app.input.chars().count();
    let room = (area.width as usize)
        .saturating_sub(typed)
        .saturating_sub(counter.chars().count() + 3);

    // The rest of the highlighted command, dimmed after what is typed. The
    // suggestion belongs on the line, where it is read, not in a box below it —
    // and when it is the whole answer there is no box at all.
    let ghost = app
        .slash
        .is_open()
        .then(|| app.slash.ghost())
        .flatten()
        .map(|rest| ellipsize(&rest, room))
        .unwrap_or_default();

    // A blank row is the separator. A rule across the width is the heaviest
    // line a terminal can draw, and this one separated things that were already
    // one row apart.
    let input_line = Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.input.clone(), Style::default().fg(Color::White)),
        Span::styled(ghost, Style::default().fg(Color::DarkGray)),
    ]);
    let hint = Line::from(Span::styled(
        ellipsize(&footer_hint(app), area.width as usize),
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(vec![Line::from(""), input_line, hint]), area);

    if !counter.is_empty() && (area.width as usize) > counter.chars().count() + 4 {
        let width = counter.chars().count() as u16;
        frame.render_widget(
            Paragraph::new(Span::styled(counter, Style::default().fg(Color::DarkGray))),
            Rect::new(
                area.x + area.width - width - 1,
                area.y + area.height - 2,
                width,
                1,
            ),
        );
    }

    // The cursor belongs after the prompt and the characters already typed —
    // but only when this line is the one taking keys.
    if caret {
        let prefix = 2u16;
        let typed = app.cursor.min(app.input.chars().count()) as u16;
        let x = (area.x + prefix + typed).min(area.x + area.width.saturating_sub(1));
        let y = area.y + area.height.saturating_sub(2);
        frame.set_cursor_position(Position::new(x, y));
    }
}

/// The glyph is the state: `▌` running, `✓` done. A tool row never says
/// "thinking", and it is the only row that changes after it is drawn.
fn style_for(role: EntryRole) -> (Style, &'static str) {
    match role {
        EntryRole::User => (Style::default().fg(Color::Yellow), "› "),
        EntryRole::Answer => (Style::default().fg(Color::White), "  "),
        EntryRole::Error => (Style::default().fg(Color::Red), "! "),
        EntryRole::Info => (Style::default().fg(Color::Gray), "· "),
        EntryRole::Tool => (Style::default().fg(Color::Cyan), "▌ "),
        EntryRole::ToolDone => (Style::default().fg(Color::Green), "✓ "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

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
            recent: Vec::new(),
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
        assert!(text.contains("enter send"), "{text}");
        // No box around the transcript and no rule above the prompt: no frame
        // characters at all, and the title that labelled the box is gone.
        assert!(!text.contains("transcript"), "{text}");
        for frame_char in ['─', '│', '┌', '┐', '└', '┘'] {
            assert!(!text.contains(frame_char), "{frame_char} in\n{text}");
        }
    }

    #[test]
    fn a_panel_is_a_left_rule_and_a_title_not_a_box() {
        let mut app = App::new("payments", false);
        app.push(EntryRole::Answer, "an answer");
        app.context = "vault\n  412 beliefs".into();
        let text = screen(&mut app, 100, 24);
        assert!(text.contains("context"), "{text}");
        assert!(!text.contains('─'), "no horizontal rules: {text}");
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
            invitation: Some("a turn with the vault and the graph as tools".into()),
            recent: vec!["2h ago   payments   retry cap raised".into()],
        }
    }

    #[test]
    fn the_brief_names_the_workspace_the_gaps_and_the_next_step() {
        let mut app = App::new("s", false);
        app.home = Some(example_home());
        let text = screen(&mut app, 110, 32);
        assert!(text.contains("memnoth"), "{text}");
        assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
        assert!(text.contains("E:/ws"), "{text}");
        assert!(text.contains("missing"), "{text}");
        assert!(text.contains("next"), "{text}");
        assert!(text.contains("/vault init"), "{text}");
        assert!(text.contains("beliefs with dates"), "{text}");
        assert!(text.contains("session \"s\" (new)"), "{text}");
        assert!(text.contains("↑↓ choose"), "{text}");
        // The invitation is a row, and says so: no marker, no slash.
        assert!(text.contains("ask anything"), "{text}");
        assert!(text.contains("recent"), "{text}");
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
        assert!(text.contains("workspace"), "{text}");
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
        assert!(!text.contains("↑↓ choose"), "{text}");
        assert!(text.contains("what changed?"), "{text}");

        app.open_home();
        let again = screen(&mut app, 110, 32);
        assert!(again.contains("↑↓ choose"), "{again}");
        assert!(!again.contains("what changed?"), "{again}");

        app.close_home();
        let transcript = screen(&mut app, 110, 32);
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
        assert!(text.contains("/vault"), "{text}");
        assert!(text.contains("esc close"), "{text}");
    }

    /// Type a line as if it had been typed, cursor at the end.
    fn typed(app: &mut App, line: &str) {
        app.input = line.to_string();
        app.cursor = line.chars().count();
        app.sync_slash();
    }

    #[test]
    fn the_completion_list_is_text_not_a_box() {
        let mut app = App::new("s", false);
        typed(&mut app, "/vault");
        let text = screen(&mut app, 80, 30);
        assert!(text.contains("/vault recall"), "{text}");
        // No frame, no title, and no key hint of its own: the footer one row
        // below already names the keys.
        for frame_char in ['─', '┌', '┐', '└', '┘', '│'] {
            assert!(!text.contains(frame_char), "{frame_char} in\n{text}");
        }
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
        assert!(text.contains("Search vault"), "{text}");
        assert!(
            text.contains("temporal)"),
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
    fn the_prompt_carries_the_rest_of_the_command() {
        let mut app = App::new("s", false);
        typed(&mut app, "/vau");
        assert_eq!(app.input, "/vau", "the ghost is never typed for you");
        let text = screen(&mut app, 80, 24);
        assert!(text.contains("/vault remember"), "{text}");

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

        let wrapped = crate::tui::app::wrap_text(long, 60 - crate::tui::app::GUTTER);
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
    fn a_tool_row_resolves_on_the_line_it_started_on() {
        let mut app = App::new("s", false);
        app.push(EntryRole::Answer, "an answer");
        let from = app.entries.len();
        app.push_tool("kg refs   RETRY_CAP");

        let running = screen(&mut app, 100, 24);
        assert!(running.contains("▌ kg refs"), "{running}");

        app.resolve_tools(from);
        let done = screen(&mut app, 100, 24);
        assert!(done.contains("✓ kg refs"), "{done}");
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
}
