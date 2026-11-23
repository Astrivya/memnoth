//! The brief — the home screen — and its answer to the window it was given.
//!
//! Width alone does not decide the shape. A 140×16 landscape split wants one
//! status row and the actions; a 60×44 portrait terminal has rows to spend on
//! stacked facts and a longer timeline. `HomeLayout::for_size` therefore reads
//! width first and height second, and each variant is a named decision rather
//! than a pair of numbers repeated at a call site.
//!
//! Two rules hold in every layout:
//!
//! * **A value wraps; it is never cut at the frame edge.** The old brief
//!   ellipsized `graph  765 files · 12,624 symbols · 0 stal…` at 80 columns,
//!   throwing away the number the row existed to report.
//! * **`next` never loses its last action.** Height trims the timeline first,
//!   then whole facts — never half of one — because an action is the reason the
//!   screen exists.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::output::ellipsize;
use crate::tui::app::{Fact, Home, HomeState, Step, wrap_text};
use crate::tui::theme::tokens;

/// Width of the label column in the brief's facts.
const FACT_LABEL: usize = 12;

/// Width of the command column in `next` when the description sits beside it.
const STEP_COLUMN: usize = 20;

/// Keep labels and values on the same row down to a genuinely narrow window.
/// Values wrap beside their label above this threshold; stacking is reserved
/// for widths where the label and a useful value fragment cannot share a row.
const COMPACT_MIN_WIDTH: u16 = 48;

/// A wide window at or below this height is a strip: a status row and the
/// actions, with nothing that needs a paragraph.
const STRIP_MAX_HEIGHT: u16 = 17;

/// Columns the state glyph is right-aligned into, plus the space before it.
const BADGE_WIDTH: usize = 2;

/// How many rows `next` may keep when the window is short: blank, header, one
/// action.
const NEXT_MIN_ROWS: usize = 3;

/// The narrowest window where a description sits beside its command. Below
/// this the command column leaves too little for a clause, and two short lines
/// read better than one cut one.
const INLINE_MIN_WIDTH: usize = 72;

/// How the brief answers the window it was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HomeLayout {
    /// One column, inline label and value, the state glyph right-aligned.
    Compact,
    /// One column, the fact label on its own line above the value.
    Portrait,
    /// Wide but short: a status row and the actions.
    Strip,
}

impl HomeLayout {
    fn for_size(width: u16, height: u16) -> Self {
        if width >= COMPACT_MIN_WIDTH && height <= STRIP_MAX_HEIGHT {
            Self::Strip
        } else if width < COMPACT_MIN_WIDTH {
            Self::Portrait
        } else {
            Self::Compact
        }
    }
}

/// Draw the brief inside `area`.
pub(crate) fn draw(frame: &mut Frame, area: Rect, home: &Home, cursor: usize) {
    // Two columns of left inset, no border and no title: the screen already is
    // the home, and a box drawn around it costs two rows to say so.
    let inner = Rect {
        x: area.x + 2.min(area.width),
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    match HomeLayout::for_size(inner.width, inner.height) {
        HomeLayout::Strip => strip(frame, inner, home, cursor),
        HomeLayout::Compact => column(frame, inner, home, cursor, false),
        HomeLayout::Portrait => column(frame, inner, home, cursor, true),
    }
}

// ── one column: compact and portrait ──────────────────────────────────────

fn column(frame: &mut Frame, area: Rect, home: &Home, cursor: usize, stacked: bool) {
    let width = area.width as usize;
    let height = area.height as usize;

    let mut header = masthead(width);
    header.extend(identity_rows(home, width));

    let groups = if stacked {
        stacked_groups(&home.facts, width)
    } else {
        inline_groups(&home.facts, width)
    };

    let mut steps = step_rows(&home.steps, width, cursor, None);

    // The ladder: the timeline gives way first, then whole facts, and the last
    // action row is the one thing that never goes.
    if header.len() + group_rows(&groups) + steps.len() > height {
        steps.truncate(NEXT_MIN_ROWS);
    }
    let room = height.saturating_sub(header.len() + steps.len());
    let facts = fit_groups(groups, room);

    let mut lines = header;
    lines.extend(facts);
    lines.extend(steps);
    finish(frame, area, lines, height);
}

// ── two columns ──────────────────────────────────────────────────────────

fn strip(frame: &mut Frame, area: Rect, home: &Home, cursor: usize) {
    let width = area.width as usize;
    let mut lines = masthead(width);
    lines.push(status_row(home, width));
    lines.push(Line::from(""));

    // Two actions, inline: the strip exists for the person who already knows
    // the workspace and wants the next step without a wall of text.
    let mut steps = step_rows(&home.steps, width, cursor, None);
    steps.truncate(NEXT_MIN_ROWS + 2);
    lines.extend(steps);
    finish(frame, area, lines, area.height as usize);
}

/// One row: the workspace, then the two states that need a person.
fn status_row(home: &Home, width: usize) -> Line<'static> {
    let t = tokens();
    let share = (width / 4).max(12);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for label in ["workspace", "graph", "vault", "sandbox"] {
        let Some(fact) = home.facts.iter().find(|fact| fact.label == label) else {
            continue;
        };
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", Style::default().fg(t.muted)));
        }
        if let Some((mark, style)) = badge(fact) {
            spans.push(Span::styled(format!("{mark} "), style));
        }
        spans.push(Span::styled(
            ellipsize(&fact.value, share),
            value_style(fact),
        ));
    }
    Line::from(spans)
}

// ── the pieces ───────────────────────────────────────────────────────────

fn masthead(_width: usize) -> Vec<Line<'static>> {
    // The release is a fact about this build, so it belongs next to the name
    // rather than behind a `/version` the person has to know to ask for.
    // The name is a heading; the version is a caption about that build, so it
    // stays muted and never competes with the facts below it.
    let t = tokens();
    vec![Line::from(vec![
        Span::styled(
            "memnoth",
            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" v{}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(t.muted),
        ),
    ])]
}

fn identity_rows(home: &Home, width: usize) -> Vec<Line<'static>> {
    // The identity is the one string this screen may cut: it is a caption, and
    // its full form is one `/whoami` away. A fact is a measurement and a step is
    // an instruction, so neither of those is allowed to end in `…`.
    let t = tokens();
    let mut rows = vec![Line::from(Span::styled(
        ellipsize(&home.identity, width),
        Style::default().fg(t.muted),
    ))];
    if home.hidden_turns > 0 {
        let turns = format!("{} turns · esc history", home.hidden_turns);
        rows.push(Line::from(Span::styled(
            ellipsize(&turns, width),
            Style::default().fg(t.muted),
        )));
    }
    rows
}

fn value_style(fact: &Fact) -> Style {
    let t = tokens();
    match fact.state {
        HomeState::Ready => Style::default().fg(t.text),
        HomeState::Missing | HomeState::Stale => Style::default().fg(t.warning),
    }
}

/// The glyph a fact's health earns, when health is what the row is about.
///
/// `workspace`, `sessions` and `spend` are facts, not states: a green dot beside
/// `no turns yet` would congratulate the person for a session that has not run.
/// The glyph carries the meaning; colour only emphasises it, so `NO_COLOR`
/// loses nothing a reader needs.
fn badge(fact: &Fact) -> Option<(char, Style)> {
    let t = tokens();
    match fact.label {
        "graph" | "vault" | "model" | "sandbox" => Some(match fact.state {
            HomeState::Ready => ('●', Style::default().fg(t.success)),
            HomeState::Stale => ('!', Style::default().fg(t.warning)),
            HomeState::Missing => ('○', Style::default().fg(t.warning)),
        }),
        _ => None,
    }
}

/// Facts as groups, so a short window drops a whole fact instead of half of one.
fn inline_groups(facts: &[Fact], width: usize) -> Vec<Vec<Line<'static>>> {
    let mut groups: Vec<Vec<Line<'static>>> = facts
        .iter()
        .filter(|fact| visible_fact(fact))
        .map(|fact| inline_fact(fact, width))
        .collect();
    // The blank row before the grid is part of it: fitting then cannot strand a
    // fact under a header with no breathing room.
    if let Some(first) = groups.first_mut() {
        first.insert(0, Line::from(""));
    }
    groups
}

fn stacked_groups(facts: &[Fact], width: usize) -> Vec<Vec<Line<'static>>> {
    facts
        .iter()
        .filter(|fact| visible_fact(fact))
        .map(|fact| stacked_fact(fact, width))
        .collect()
}

fn visible_fact(fact: &Fact) -> bool {
    matches!(fact.label, "workspace" | "vault" | "graph" | "sandbox")
}

fn inline_fact(fact: &Fact, width: usize) -> Vec<Line<'static>> {
    let t = tokens();
    let glyph = badge(fact);
    let reserved = if glyph.is_some() { BADGE_WIDTH + 1 } else { 0 };
    let room = width.saturating_sub(FACT_LABEL + reserved).max(8);
    let mut rows = Vec::new();
    for (index, chunk) in wrap_text(&fact.value, room).iter().enumerate() {
        let mut spans = Vec::new();
        if index == 0 {
            spans.push(Span::styled(
                format!("{:<FACT_LABEL$}", fact.label),
                Style::default().fg(t.muted),
            ));
        } else {
            spans.push(Span::raw(" ".repeat(FACT_LABEL)));
        }
        spans.push(Span::styled(chunk.clone(), value_style(fact)));
        if index == 0
            && let Some((mark, style)) = glyph
        {
            let used = FACT_LABEL + chunk.chars().count();
            spans.push(Span::raw(" ".repeat(width.saturating_sub(used + 1))));
            spans.push(Span::styled(mark.to_string(), style));
        }
        rows.push(Line::from(spans));
    }
    rows
}

fn stacked_fact(fact: &Fact, width: usize) -> Vec<Line<'static>> {
    let t = tokens();
    let glyph = badge(fact);
    let room = width.saturating_sub(6).max(8);
    let mut rows = vec![Line::from(Span::styled(
        fact.label.to_string(),
        Style::default().fg(t.muted),
    ))];
    for (index, chunk) in wrap_text(&fact.value, room).iter().enumerate() {
        let mut spans = vec![Span::raw("  ")];
        match (index, glyph) {
            (0, Some((mark, style))) => spans.push(Span::styled(format!("{mark} "), style)),
            (_, Some(_)) => spans.push(Span::raw("  ")),
            _ => {}
        }
        spans.push(Span::styled(chunk.clone(), value_style(fact)));
        rows.push(Line::from(spans));
    }
    rows
}

/// `next`, with the description beside the command when there is room for a
/// clause and under it when there is not.
fn step_rows(
    steps: &[Step],
    width: usize,
    cursor: usize,
    invitation: Option<&str>,
) -> Vec<Line<'static>> {
    if steps.is_empty() && invitation.is_none() {
        return Vec::new();
    }
    let t = tokens();
    let mut rows = vec![
        Line::from(""),
        Line::from(Span::styled("next", Style::default().fg(t.text))),
    ];
    let inline = width >= INLINE_MIN_WIDTH;
    for (index, step) in steps.iter().enumerate() {
        let selected = index == cursor && step.runnable;
        // The row the cursor is on is a selection, not an accent: the `▸`
        // marker says which one Enter runs even when every colour is stripped.
        // The whole row shares one style so the highlight is continuous.
        let (marker_style, command_style, description_style) = if selected {
            let row = Style::default()
                .fg(t.selected_fg)
                .bg(t.selected_bg)
                .add_modifier(Modifier::BOLD);
            (row, row, row)
        } else {
            (
                Style::default().fg(t.muted),
                Style::default().fg(t.text),
                Style::default().fg(t.muted),
            )
        };
        let marker = if selected { "▸ " } else { "  " };

        if inline {
            let room = width.saturating_sub(2 + STEP_COLUMN).max(8);
            let mut wrapped = wrap_text(&step.description, room);
            let first = wrapped.first().cloned().unwrap_or_default();
            rows.push(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(format!("{:<STEP_COLUMN$}", step.command), command_style),
                Span::styled(first, description_style),
            ]));
            // A description that does not fit beside the command continues
            // under it, aligned with the column it started in. It is never cut.
            for chunk in wrapped.drain(1..) {
                rows.push(Line::from(Span::styled(
                    format!("{}{chunk}", " ".repeat(2 + STEP_COLUMN)),
                    description_style,
                )));
            }
        } else {
            rows.push(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(step.command.clone(), command_style),
            ]));
            let room = width.saturating_sub(4).max(8);
            for chunk in wrap_text(&step.description, room) {
                rows.push(Line::from(Span::styled(
                    format!("    {chunk}"),
                    description_style,
                )));
            }
        }
    }

    // The row that is not a verb: it says what the prompt accepts, and the
    // cursor skips it, so nothing looks clickable and does nothing.
    if let Some(invitation) = invitation {
        if inline {
            let room = width.saturating_sub(2 + STEP_COLUMN).max(8);
            let mut wrapped = wrap_text(invitation, room);
            let first = wrapped.first().cloned().unwrap_or_default();
            rows.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{:<STEP_COLUMN$}", "ask anything"),
                    Style::default().fg(t.muted),
                ),
                Span::styled(first, Style::default().fg(t.muted)),
            ]));
            for chunk in wrapped.drain(1..) {
                rows.push(Line::from(Span::styled(
                    format!("{}{chunk}", " ".repeat(2 + STEP_COLUMN)),
                    Style::default().fg(t.muted),
                )));
            }
        } else {
            rows.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("ask anything", Style::default().fg(t.muted)),
            ]));
            for chunk in wrap_text(invitation, width.saturating_sub(4).max(8)) {
                rows.push(Line::from(Span::styled(
                    format!("    {chunk}"),
                    Style::default().fg(t.muted),
                )));
            }
        }
    }
    rows
}

fn group_rows(groups: &[Vec<Line<'static>>]) -> usize {
    groups.iter().map(Vec::len).sum()
}

/// Keep whole facts while they fit, and always keep at least one: a brief with
/// no facts is a header, and a half-drawn fact is worse than an absent one.
fn fit_groups(groups: Vec<Vec<Line<'static>>>, room: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut used = 0;
    for group in groups {
        if used + group.len() > room && !out.is_empty() {
            break;
        }
        used += group.len();
        out.extend(group);
    }
    out
}

fn finish(frame: &mut Frame, area: Rect, mut lines: Vec<Line<'static>>, height: usize) {
    lines.truncate(height);
    while lines.len() < height {
        lines.push(Line::from(""));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    fn home() -> Home {
        Home {
            identity: "Default (team) · amruthjakku@gmail.com · owner · session \"hi\" (new)"
                .into(),
            facts: vec![
                Fact {
                    label: "workspace",
                    value: "E:/memnoth-prod".into(),
                    state: HomeState::Ready,
                },
                Fact {
                    label: "vault",
                    value: "empty — no beliefs yet".into(),
                    state: HomeState::Missing,
                },
                Fact {
                    label: "graph",
                    value: "765 files · 12,624 symbols · 0 stale".into(),
                    state: HomeState::Ready,
                },
                Fact {
                    label: "model",
                    value: "kilo · reflection/beam-free".into(),
                    state: HomeState::Ready,
                },
            ],
            steps: vec![
                Step {
                    command: "/vault init".into(),
                    description: "create the vault — beliefs with dates".into(),
                    runnable: true,
                },
                Step {
                    command: "/search".into(),
                    description: "answer from both stores, no model needed".into(),
                    runnable: true,
                },
            ],
            invitation: Some("a turn with the vault and the graph as tools".into()),
            hidden_turns: 0,
        }
    }

    fn text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_layout_answers_the_window_it_gets() {
        assert_eq!(HomeLayout::for_size(120, 32), HomeLayout::Compact);
        assert_eq!(HomeLayout::for_size(140, 16), HomeLayout::Strip);
        assert_eq!(HomeLayout::for_size(80, 24), HomeLayout::Compact);
        assert_eq!(HomeLayout::for_size(60, 40), HomeLayout::Compact);
        assert_eq!(HomeLayout::for_size(44, 40), HomeLayout::Portrait);
    }

    /// The defect this module was written for: at 80 columns the value ended in
    /// `…` because it was cut at the frame edge instead of wrapped.
    #[test]
    fn a_long_value_wraps_instead_of_ending_in_an_ellipsis() {
        let fact = Fact {
            label: "graph",
            value: "765 files · 12,624 symbols · 0 stale · built 2m ago · local only".into(),
            state: HomeState::Ready,
        };
        let rows = inline_fact(&fact, 60);
        let rendered = text(&rows);
        assert!(!rendered.contains('…'), "{rendered}");
        for row in &rows {
            assert!(row.width() <= 60, "{row:?} is wider than the pane");
        }
        assert!(rows.len() > 1, "the value has to continue somewhere");
    }

    #[test]
    fn a_narrow_pane_puts_the_description_under_its_command() {
        let rows = step_rows(&home().steps, 60, 0, None);
        let rendered = text(&rows);
        assert!(rendered.contains("/vault init"), "{rendered}");
        assert!(
            rendered.contains("    create the vault"),
            "the description is indented under the command: {rendered}"
        );
        assert!(!rendered.contains('…'), "{rendered}");
    }

    #[test]
    fn the_inline_row_keeps_the_state_glyph_in_the_last_column() {
        let groups = inline_groups(&home().facts, 80);
        let graph = groups
            .iter()
            .find(|group| group.iter().any(|line| line.to_string().contains("graph")))
            .expect("the graph fact is on the brief");
        let row = graph[0].to_string();
        assert!(row.ends_with('●'), "{row:?}");
    }

    #[test]
    fn only_the_stateful_facts_get_a_glyph() {
        let facts = &home().facts;
        assert!(
            badge(&facts[0]).is_none(),
            "workspace is not a health check"
        );
        assert!(badge(&facts[1]).is_some(), "vault state is");
        assert!(badge(&facts[2]).is_some(), "graph state is");
    }

    #[test]
    fn the_strip_row_names_the_workspace_the_graph_and_the_vault() {
        let row = status_row(&home(), 140).to_string();
        assert!(row.contains("E:/memnoth-prod"), "{row}");
        assert!(row.contains("765 files"), "{row}");
        assert!(row.contains("empty"), "{row}");
    }

    /// A short window drops a fact whole. Half a value is a sentence that stops
    /// mid-word, which is what the old renderer did with `…`.
    #[test]
    fn fitting_drops_whole_facts_and_never_all_of_them() {
        let groups = inline_groups(&home().facts, 80);
        let first = groups[0].len();
        let kept = fit_groups(groups, first);
        assert_eq!(kept.len(), first, "{kept:?}");
        assert!(!kept.is_empty());
    }

    #[test]
    fn no_layout_draws_a_value_that_ends_in_an_ellipsis() {
        for width in [60usize, 80, 110, 140] {
            let groups = inline_groups(&home().facts, width);
            let rendered = text(&groups.concat());
            assert!(!rendered.contains('…'), "at {width}: {rendered}");
        }
    }

    #[test]
    fn masthead_keeps_the_version_as_a_muted_span() {
        let t = tokens();
        let line = &masthead(80)[0];
        assert_eq!(line.spans.len(), 2, "{line:?}");
        assert_eq!(line.spans[0].content.as_ref(), "memnoth");
        assert_eq!(line.spans[0].style.fg, Some(t.text));
        assert_eq!(
            line.spans[1].content.as_ref(),
            format!(" v{}", env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(line.spans[1].style.fg, Some(t.muted));
    }

    #[test]
    fn fact_roles_use_the_semantic_tokens() {
        let t = tokens();
        let facts = &home().facts;
        // A healthy value is primary text; a gap is a warning, not an error.
        assert_eq!(value_style(&facts[0]).fg, Some(t.text));
        assert_eq!(value_style(&facts[1]).fg, Some(t.warning));
        // `workspace` is a fact, not a health check; `graph` and `vault` are.
        assert!(badge(&facts[0]).is_none());
        assert_eq!(badge(&facts[2]).unwrap().1.fg, Some(t.success));
        assert_eq!(badge(&facts[1]).unwrap().1.fg, Some(t.warning));
        // The glyph is the meaning: it survives every colour being stripped.
        assert_eq!(badge(&facts[2]).unwrap().0, '●');
        assert_eq!(badge(&facts[1]).unwrap().0, '○');
    }

    #[test]
    fn the_selected_step_uses_the_selection_tokens() {
        let t = tokens();
        let steps = &home().steps;
        let selected = step_rows(steps, 80, 0, None)
            .into_iter()
            .find(|line| line.to_string().contains("/vault init"))
            .expect("the first action is on a row");
        assert_eq!(selected.spans[0].content.as_ref(), "▸ ");
        assert_eq!(selected.spans[0].style.fg, Some(t.selected_fg));
        assert_eq!(selected.spans[0].style.bg, Some(t.selected_bg));

        let other = step_rows(steps, 80, 1, None)
            .into_iter()
            .find(|line| line.to_string().contains("/vault init"))
            .expect("the other action is on a row");
        assert_eq!(other.spans[0].content.as_ref(), "  ");
        assert_eq!(other.spans[0].style.fg, Some(t.muted));
    }

    #[test]
    fn every_colour_on_the_brief_comes_from_the_tokens() {
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
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let brief = home();
        terminal
            .draw(|frame| draw(frame, Rect::new(0, 0, 100, 30), &brief, 0))
            .expect("draw must not fail");
        for cell in terminal.backend().buffer().content() {
            assert!(
                allowed.contains(&cell.fg) && allowed.contains(&cell.bg),
                "a colour outside the token set reached the brief: {cell:?}"
            );
        }
    }
}
