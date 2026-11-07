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

    for raw in text.split('\n') {
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
            out.extend(block(&code, width, Some(2), code_style()));
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

/// One code line, or prose already split into runs, wrapped as a block.
fn block(text: &str, width: usize, indent: Option<usize>, style: Style) -> Vec<Line<'static>> {
    block_runs(vec![Run::plain(text)], width, indent, style)
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
