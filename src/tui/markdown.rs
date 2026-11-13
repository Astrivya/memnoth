//! Inline markdown, rendered for the transcript.
//!
//! The model answers in markdown and the transcript used to print it raw: a
//! `**Temporal vault**` heading arrived with its asterisks, every tool name wore
//! a pair of backticks, and a wrapped bullet lost its indent and started again at
//! column zero — which is what made a list read as prose that happened to have
//! dashes in it.
//!
//! The renderer is deliberately small: paragraphs, headings, bullets, numbered
//! steps, blockquotes, fenced code and the four inline emphases (`**bold**`,
//! `*italic*`, `_italic_`, `` `code` ``). It is a terminal, not a browser: links
//! keep their text and lose their URL, tables stay as they were typed, and
//! nothing is ever hidden. Wrapping happens here rather than in the widget
//! because a style run can straddle a line break, and a paragraph that had its
//! spans split after the fact would lose the emphasis on the second line.
//!
//! Styles are *emphasis only*: fg and modifiers, never a background for normal
//! text. The transcript colours a row by who said it, and a span that carried a
//! colour of its own would fight that; the caller patches the role's colour onto
//! every span that does not name one.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// One run of text with the emphasis it was written in.
#[derive(Debug, Clone)]
struct Run {
    text: String,
    style: Style,
}

impl Run {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: Style::default(),
        }
    }
}

/// Render one entry's text at `width` columns.
///
/// `width` is the text column, not the pane: the caller has already taken off
/// the gutter a row spends on its glyph. Returns at least one line for any
/// input, so a caller can index it without a guard.
pub fn lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(8);
    let mut out = Vec::new();
    let mut fence: Option<()> = None;
    let mut blank = false;
    let raws: Vec<&str> = text.split('\n').collect();
    let mut index = 0;

    while index < raws.len() {
        let raw = raws[index];
        index += 1;
        let trimmed = raw.trim_end();

        // A fence opens and closes; between them the text is code however it
        // looks. Rendering the body as prose would eat the indentation that is
        // the whole point of a snippet.
        if trimmed.trim_start().starts_with("```") {
            fence = if fence.is_some() { None } else { Some(()) };
            blank = false;
            continue;
        }
        if fence.is_some() {
            let code = trimmed.replace('\t', "    ");
            // Verbatim, never reflowed: a bar chart is columns of characters
            // whose meaning is their alignment, and wrapping it word by word
            // shuffles the bars into noise — which is exactly how a perfectly
            // good graph arrived on screen as overlapping blocks. A code line
            // longer than the pane is cut at a character boundary instead.
            let room = width.saturating_sub(2).max(1);
            if code.is_empty() {
                out.push(Line::from(""));
            } else {
                let chars: Vec<char> = code.chars().collect();
                for chunk in chars.chunks(room) {
                    let text: String = chunk.iter().collect();
                    out.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(text, code_style()),
                    ]));
                }
            }
            blank = false;
            continue;
        }

        if trimmed.trim().is_empty() {
            // One blank row separates two blocks; three in a row is a gap the
            // reader scrolls through for nothing.
            if !blank && !out.is_empty() {
                out.push(Line::from(""));
            }
            blank = true;
            continue;
        }
        blank = false;

        // A GFM table: a pipe row followed by a separator row. Detected
        // before bullets and headings because a `| - - |` row would
        // otherwise be eaten as prose, pipes and all.
        if let Some(header) = table_row(trimmed)
            && index < raws.len()
            && is_separator(raws[index])
        {
            index += 1; // the separator itself is structure, never a row
            let mut rows = Vec::new();
            while index < raws.len() {
                match table_row(raws[index].trim_end()) {
                    Some(row) => {
                        rows.push(row);
                        index += 1;
                    }
                    None => break,
                }
            }
            out.extend(table_lines(&header, &rows, width));
            continue;
        }

        let indent = leading_spaces(raw);
        let body = &trimmed[indent.min(trimmed.len())..];

        // Headings: the marker is structure, not text, so it is dropped and the
        // line is carried by weight alone. `###` is the same heading here as `#`.
        if let Some(rest) = heading(body) {
            let mut runs = inline(rest);
            for run in &mut runs {
                run.style = run.style.add_modifier(Modifier::BOLD);
            }
            out.extend(block_runs(runs, width, Some(0), Style::default()));
            continue;
        }

        if let Some(rest) = bullet(body) {
            let marker = Span::styled("• ", Style::default().fg(Color::Cyan));
            out.extend(wrap_runs(
                inline(rest),
                width,
                2,
                Some(marker),
                Style::default(),
            ));
            continue;
        }

        if let Some((marker, rest)) = numbered(body) {
            let marker_span = Span::styled(marker.clone(), Style::default().fg(Color::Cyan));
            let indent = marker.chars().count();
            out.extend(wrap_runs(
                inline(rest),
                width,
                indent,
                Some(marker_span),
                Style::default(),
            ));
            continue;
        }

        if let Some(rest) = body.strip_prefix("> ") {
            let marker = Span::styled("│ ", Style::default().fg(Color::DarkGray));
            let style = Style::default().add_modifier(Modifier::ITALIC | Modifier::DIM);
            out.extend(wrap_runs(inline(rest), width, 2, Some(marker), style));
            continue;
        }

        // A horizontal rule is a line, not three dashes the reader has to
        // interpret.
        if body.trim_matches('-').is_empty() && body.trim().len() >= 3 {
            out.push(Line::from(Span::styled(
                "─".repeat(width),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }

        let pad = if indent > 0 {
            Some(Span::raw(" ".repeat(indent.min(width.saturating_sub(4)))))
        } else {
            None
        };
        out.extend(wrap_runs(
            inline(body),
            width,
            indent,
            pad,
            Style::default(),
        ));
    }

    if out.is_empty() {
        out.push(Line::from(""));
    }
    // A trailing blank line under the last paragraph is a gap between entries
    // that the entry itself should not own.
    while out.last().is_some_and(|line| line.spans.is_empty()) {
        out.pop();
    }
    if out.is_empty() {
        out.push(Line::from(""));
    }
    out
}

/// `| a | b |` → `["a", "b"]`. The leading pipe is what makes a row a row;
/// prose that merely contains pipes is not a table.
fn table_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') {
        return None;
    }
    let mut cells: Vec<String> = trimmed
        .strip_prefix('|')?
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect();
    // The closing pipe leaves one empty cell behind it; that is the row's
    // frame, not a column.
    if cells.last().is_some_and(|cell| cell.is_empty()) {
        cells.pop();
    }
    if cells.is_empty() {
        return None;
    }
    Some(cells)
}

/// `|---|---|`, `|:--|--:|` — every cell is dashes and optional colons.
fn is_separator(line: &str) -> bool {
    let Some(cells) = table_row(line) else {
        return false;
    };
    cells.iter().all(|cell| {
        let bare = cell.trim_matches(':');
        !bare.is_empty() && bare.chars().all(|ch| ch == '-')
    })
}

/// A GFM table as box-drawn rows: real columns, a header rule, and borders.
///
/// Column widths come from the widest cell, then shrink from the widest
/// column down until the table fits the pane — a wide answer must not push
/// the table off the right edge where the terminal clips it unreadably.
/// Cells wrap instead of overflowing, so every cell's words survive.
fn table_lines(header: &[String], rows: &[Vec<String>], width: usize) -> Vec<Line<'static>> {
    let cols = header
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0))
        .max(1);
    let mut col_w = vec![3usize; cols];
    for (index, cell) in header.iter().enumerate() {
        col_w[index] = col_w[index].max(cell.chars().count());
    }
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            col_w[index] = col_w[index].max(cell.chars().count());
        }
    }
    // `│ ` + cells + ` │` per column: sum + 3 per column + the frame.
    let total = |widths: &[usize]| widths.iter().sum::<usize>() + 3 * cols + 1;
    while total(&col_w) > width {
        let Some((index, &widest)) = col_w.iter().enumerate().max_by_key(|(_, value)| *value)
        else {
            break;
        };
        if widest <= 3 {
            break;
        }
        col_w[index] -= 1;
    }

    let rule = Style::default().fg(Color::DarkGray);
    let border = |left: &str, mid: &str, right: &str, widths: &[usize]| -> Line<'static> {
        let mut text = String::from(left);
        for (index, &w) in widths.iter().enumerate() {
            if index > 0 {
                text.push_str(mid);
            }
            text.push_str(&"─".repeat(w + 2));
        }
        text.push_str(right);
        Line::from(Span::styled(text, rule))
    };

    let render_row =
        |cells: &[String], widths: &[usize], bold: bool, out: &mut Vec<Line<'static>>| {
            // Every cell wraps on its own, so a long cell grows the row rather
            // than pushing its neighbours off the line.
            let mut rendered: Vec<Vec<Line<'static>>> = Vec::with_capacity(cells.len());
            for (index, &col_width) in widths.iter().enumerate() {
                let text = cells.get(index).map(String::as_str).unwrap_or("");
                let mut runs = inline(text);
                if bold {
                    for run in &mut runs {
                        run.style = run.style.add_modifier(Modifier::BOLD);
                    }
                }
                rendered.push(wrap_runs(runs, col_width, 0, None, Style::default()));
            }
            let height = rendered.iter().map(Vec::len).max().unwrap_or(1);
            for row in 0..height {
                let mut spans = vec![Span::styled("│ ", rule)];
                for (column, &w) in rendered.iter().zip(widths.iter()) {
                    let line = column.get(row);
                    let (line_spans, used) = match line {
                        Some(line) => {
                            let used: usize = line
                                .spans
                                .iter()
                                .map(|span| span.content.chars().count())
                                .sum();
                            (line.spans.clone(), used)
                        }
                        None => (Vec::new(), 0),
                    };
                    spans.extend(line_spans);
                    if used < w {
                        spans.push(Span::raw(" ".repeat(w - used)));
                    }
                    spans.push(Span::styled(" │ ", rule));
                }
                out.push(Line::from(spans));
            }
        };

    let mut out = Vec::new();
    out.push(border("┌", "┬", "┐", &col_w));
    render_row(header, &col_w, true, &mut out);
    out.push(border("├", "┼", "┤", &col_w));
    for row in rows {
        render_row(row, &col_w, false, &mut out);
    }
    out.push(border("└", "┴", "┘", &col_w));
    out
}

/// The columns a line spends before its first word.
fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

/// `## heading` → `heading`.
fn heading(body: &str) -> Option<&str> {
    let hashes = body.chars().take_while(|ch| *ch == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    body[hashes..].strip_prefix(' ').map(str::trim_start)
}

/// `- item`, `* item`, `+ item` → `item`.
fn bullet(body: &str) -> Option<&str> {
    body.strip_prefix("- ")
        .or_else(|| body.strip_prefix("* "))
        .or_else(|| body.strip_prefix("+ "))
}

/// `12. step` → (`12. `, `step`).
fn numbered(body: &str) -> Option<(String, &str)> {
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = body[digits..].strip_prefix(". ")?;
    Some((format!("{}. ", &body[..digits]), rest))
}

/// Whether `ch` is part of a word, which is what an underscore may not sit in.
fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn code_style() -> Style {
    Style::default().fg(Color::Yellow)
}

fn block_runs(
    runs: Vec<Run>,
    width: usize,
    indent: Option<usize>,
    style: Style,
) -> Vec<Line<'static>> {
    let indent = indent.unwrap_or(0);
    let pad = if indent > 0 {
        Some(Span::raw(" ".repeat(indent)))
    } else {
        None
    };
    let runs = runs
        .into_iter()
        .map(|mut run| {
            run.style = style.patch(run.style);
            run
        })
        .collect();
    wrap_runs(runs, width, indent, pad, style)
}

/// Parse `**bold**`, `*italic*`, `_italic_` and `` `code` `` into runs.
///
/// Delimiters are only recognised when they are not adjacent to more of the same
/// character, so a lone `*` in prose stays a `*` and `2 * 3 * 4` is arithmetic
/// rather than an italic `3`.
fn inline(text: &str) -> Vec<Run> {
    let chars: Vec<char> = text.chars().collect();
    let mut runs: Vec<Run> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;

    let flush = |plain: &mut String, runs: &mut Vec<Run>| {
        if !plain.is_empty() {
            runs.push(Run::plain(std::mem::take(plain)));
        }
    };

    while i < chars.len() {
        // Inline code: everything up to the closing backtick is literal.
        if chars[i] == '`'
            && let Some(end) = chars[i + 1..].iter().position(|ch| *ch == '`')
        {
            flush(&mut plain, &mut runs);
            let code: String = chars[i + 1..i + 1 + end].iter().collect();
            runs.push(Run {
                text: code,
                style: Style::default().fg(Color::Yellow),
            });
            i += end + 2;
            continue;
        }

        // `**bold**`, before the single-character case so `***x***` does not
        // become a stray asterisk plus italics.
        if chars[i] == '*'
            && chars.get(i + 1) == Some(&'*')
            && let Some(end) = chars[i + 2..]
                .windows(2)
                .position(|w| w[0] == '*' && w[1] == '*')
        {
            flush(&mut plain, &mut runs);
            let body: String = chars[i + 2..i + 2 + end].iter().collect();
            runs.push(Run {
                text: body,
                style: Style::default().add_modifier(Modifier::BOLD),
            });
            i += end + 4;
            continue;
        }

        // `*italic*` / `_italic_`: the delimiter may not touch a word character
        // on its inside, which is what keeps snake_case names intact. An
        // underscore may not sit inside a word at all: `kg_impact_status` is one
        // name, never `kg` + *impact* + `status`.
        let delimiter = match chars[i] {
            '*' | '_' => chars[i],
            _ => {
                plain.push(chars[i]);
                i += 1;
                continue;
            }
        };
        if delimiter == '_' && i > 0 && is_word(chars[i - 1]) {
            plain.push(chars[i]);
            i += 1;
            continue;
        }
        let inner_start = i + 1;
        let closes = chars[inner_start..]
            .iter()
            .enumerate()
            .position(|(offset, ch)| {
                *ch == delimiter
                    && (delimiter != '_'
                        || chars
                            .get(inner_start + offset + 1)
                            .is_none_or(|next| !is_word(*next)))
            })
            .map(|offset| inner_start + offset);
        match closes {
            Some(end)
                if end > inner_start
                    && !chars[end - 1].is_whitespace()
                    && chars.get(inner_start).is_some_and(|ch| !ch.is_whitespace()) =>
            {
                flush(&mut plain, &mut runs);
                let body: String = chars[inner_start..end].iter().collect();
                runs.push(Run {
                    text: body,
                    style: Style::default().add_modifier(Modifier::ITALIC),
                });
                i = end + 1;
            }
            _ => {
                plain.push(chars[i]);
                i += 1;
            }
        }
    }
    flush(&mut plain, &mut runs);
    runs
}

/// Greedy wrap over styled words.
///
/// The first line starts with `first` when there is one; every later line is
/// indented by `indent`, which is what keeps a wrapped bullet inside the bullet
/// instead of resetting to the left margin. A word longer than the line is split
/// hard rather than allowed to overflow.
fn wrap_runs(
    runs: Vec<Run>,
    width: usize,
    indent: usize,
    first: Option<Span<'static>>,
    _base: Style,
) -> Vec<Line<'static>> {
    let width = width.max(4);
    // Adjacency is part of the text. Splitting runs into words drops the spaces
    // themselves, and putting one back between every pair turned ``(`kg_impact`)``
    // into `( kg_impact )` — a code span glued to punctuation must stay glued.
    // Each word therefore remembers whether a space really separated it from the
    // one before, and a run boundary is not a space: the space, when there is
    // one, is at the end of the run that precedes it.
    let mut words: Vec<(String, Style, bool)> = Vec::new();
    let mut separated = false;
    for run in runs {
        let mut first = true;
        for piece in run.text.split(' ') {
            if piece.is_empty() {
                separated = true;
                continue;
            }
            words.push((piece.to_string(), run.style, separated || !first));
            separated = false;
            first = false;
        }
    }

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = first
        .as_ref()
        .map(|span| span.content.chars().count())
        .unwrap_or(indent);
    if let Some(span) = first.clone() {
        current.push(span);
    }
    // Whether the current line already holds a word (the indent does not count).
    let mut has_word = false;

    for (text, style, space_before) in words {
        let word_len = text.chars().count();
        let gap = usize::from(space_before);
        if has_word && used + gap + word_len <= width {
            if gap == 1 {
                current.push(Span::styled(" ".to_string(), style));
            }
            current.push(Span::styled(text, style));
            used += gap + word_len;
            continue;
        }
        if has_word {
            out.push(Line::from(std::mem::take(&mut current)));
            current = Vec::new();
            if indent > 0 {
                current.push(Span::raw(" ".repeat(indent)));
            }
            used = indent;
        }
        // A word that cannot fit on a line of its own is cut into the space
        // available, so it can never hide the rest of the line.
        let room = width.saturating_sub(used).max(1);
        let mut rest: Vec<char> = text.chars().collect();
        while rest.len() > room {
            let cut: String = rest.drain(..room).collect();
            current.push(Span::styled(cut, style));
            out.push(Line::from(std::mem::take(&mut current)));
            if indent > 0 {
                current.push(Span::raw(" ".repeat(indent)));
            }
            used = indent;
        }
        let tail: String = rest.into_iter().collect();
        current.push(Span::styled(tail.clone(), style));
        used += tail.chars().count();
        has_word = true;
    }

    if has_word || !current.is_empty() {
        // A line holding only the indent is not a line.
        if current.iter().any(|span| !span.content.trim().is_empty()) {
            out.push(Line::from(current));
        }
    }
    if out.is_empty() {
        out.push(Line::from(""));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn styles(lines: &[Line<'_>]) -> Vec<(String, Style)> {
        lines
            .iter()
            .flat_map(|line| {
                line.spans
                    .iter()
                    .map(|span| (span.content.to_string(), span.style))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// A streamed answer that is still open — an unclosed fence, a half-bold
    /// run — renders as what it is so far, never as an error and never as
    /// raw markers.
    #[test]
    fn a_streamed_answer_renders_mid_form() {
        let rendered = lines("```\nlet x = ", 60);
        let text = plain(&rendered).join("\n");
        assert!(!text.contains('`'), "{text}");
        assert!(text.contains("let x"), "{text}");
    }

    /// The complaint this module exists for: the markers are structure, never
    /// text. A reader must not see `**` or a backtick.
    #[test]
    fn markers_are_rendered_not_printed() {
        let rendered = lines("**Temporal vault** and `kg_impact`", 60);
        let text = plain(&rendered).join("");
        assert!(!text.contains('*'), "{text}");
        assert!(!text.contains('`'), "{text}");
        assert!(text.contains("Temporal vault"));
        assert!(text.contains("kg_impact"));

        // The wrapper splits a run into words, so the bold text arrives as
        // several spans: what must survive is the weight on all of them.
        let spans = styles(&rendered);
        let bold: String = spans
            .iter()
            .filter(|(_, style)| style.add_modifier == Modifier::BOLD)
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(bold, "Temporal vault", "a bold run stays bold");
        assert!(
            spans
                .iter()
                .any(|(text, style)| text == "kg_impact" && style.fg == Some(Color::Yellow)),
            "code is picked out"
        );
    }

    /// A wrapped bullet stays inside its bullet.
    #[test]
    fn a_wrapped_bullet_keeps_its_hanging_indent() {
        let rendered = lines(
            "- Impact analysis — what breaks if you change a symbol (`kg_impact`)",
            28,
        );
        let text = plain(&rendered);
        assert!(text[0].starts_with("• "), "{text:?}");
        for line in text.iter().skip(1) {
            assert!(line.starts_with("  "), "continuation indented: {line:?}");
        }
        assert!(
            text.iter().all(|line| line.chars().count() <= 28),
            "no line overflows: {text:?}"
        );
    }

    /// Headings lose their hashes and keep their weight; fences keep their
    /// indentation and are not parsed as prose.
    #[test]
    fn headings_and_fences_render_as_structure() {
        let rendered = lines("### Notes\n```\nlet x = 1 * 2;\n```", 40);
        let text = plain(&rendered);
        assert_eq!(text[0], "Notes");
        assert!(text.iter().any(|line| line.contains("let x = 1 * 2;")));
        assert!(!text.join(" ").contains("```"));
    }

    /// Three blank lines in a row are one gap.
    #[test]
    fn runs_of_blank_lines_collapse() {
        let rendered = lines("one\n\n\n\ntwo", 40);
        let text = plain(&rendered);
        assert_eq!(text, vec!["one", "", "two"]);
    }

    /// Arithmetic is not emphasis and a snake_case name is not italics.
    #[test]
    fn stray_delimiters_stay_text() {
        let text = plain(&lines("2 * 3 * 4 and kg_impact_status here", 60)).join("");
        assert!(text.contains("2 * 3 * 4"), "{text}");
        assert!(text.contains("kg_impact_status"), "{text}");
    }

    /// A code span keeps the punctuation it was written against.
    #[test]
    fn inline_code_keeps_the_spacing_it_was_written_with() {
        let text = plain(&lines("impact (`kg_impact`) and `x` y", 60)).join("\n");
        assert!(text.contains("(kg_impact)"), "{text}");
        assert!(!text.contains("( kg_impact )"), "{text}");
        assert!(text.contains("and x y"), "{text}");
    }

    /// A GFM table renders as a box table: columns aligned, header ruled,
    /// and no raw pipes or separator dashes left for the reader to decode.
    #[test]
    fn a_table_renders_as_boxed_columns() {
        let rendered = lines(
            "| Dimension | memnoth | Codex |\n|---|---|---|\n| Scope | This workspace | Any repo |\n| Shell | No | Yes |",
            60,
        );
        let text = plain(&rendered);
        let joined = text.join("\n");
        assert!(joined.contains('┌') && joined.contains('│'), "{joined}");
        assert!(!joined.contains('|'), "no pipes survive: {joined}");
        assert!(!joined.contains("---"), "the separator is a rule: {joined}");
        assert!(
            text.iter().any(|line| line.contains("Dimension")),
            "{joined}"
        );
        assert!(
            text.iter().all(|line| line.chars().count() <= 60),
            "the table fits the pane: {text:?}"
        );
    }

    /// A bar chart in a fence keeps every character exactly where it was
    /// written: alignment is the data, and word-wrapping it shuffled the
    /// bars into noise.
    #[test]
    fn a_code_block_keeps_its_columns() {
        let chart =
            "Autonomy  ██████░░░░  Codex ████████████\nTeam      ███░░░░░░░  Codex ████████";
        let rendered = lines(&format!("```\n{chart}\n```"), 60);
        let text = plain(&rendered);
        let joined = text.join("\n");
        for line in chart.lines() {
            assert!(joined.contains(line), "chart line verbatim: {joined}");
        }
    }

    /// A word longer than the pane is cut, never allowed to overflow.
    #[test]
    fn a_long_word_is_split_not_overflowing() {
        let rendered = lines("supercalifragilisticexpialidocious", 10);
        assert!(rendered.iter().all(|line| {
            line.spans
                .iter()
                .map(|span| span.content.chars().count())
                .sum::<usize>()
                <= 10
        }));
    }
}
