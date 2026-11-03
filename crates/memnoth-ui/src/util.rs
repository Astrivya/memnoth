//! Shared utilities for memnoth-ui widgets.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Wrap;

/// Truncate to `width` characters, with an ellipsis when it had to cut.
pub fn ellipsize(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let cut: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Cut to the last whole word that fits, so a narrow pane ends on a word
/// instead of half of one. A single unbroken word still falls back to a hard
/// cut, because dropping the note entirely would hide state.
pub fn ellipsize_words(text: &str, width: usize) -> String {
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

/// Standard wrap configuration for paragraphs
pub fn wrap() -> Wrap {
    Wrap { trim: false }
}

/// Style for a highlighted title
pub fn title_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

/// Style for muted/dimmed text
pub fn muted_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Style for normal text
pub fn normal_style() -> Style {
    Style::default().fg(Color::White)
}

/// Style for error text
pub fn error_style() -> Style {
    Style::default().fg(Color::Red)
}

/// Style for warning/yellow text
pub fn warning_style() -> Style {
    Style::default().fg(Color::Yellow)
}

/// Style for gray text
pub fn gray_style() -> Style {
    Style::default().fg(Color::Gray)
}

/// Style for cyan text
pub fn cyan_style() -> Style {
    Style::default().fg(Color::Cyan)
}

/// Create a line with a label and value, optionally with a note
pub fn label_value_line(
    label: &str,
    value: &str,
    note: &str,
    width: usize,
    label_width: usize,
) -> Line<'static> {
    let room = width.saturating_sub(1 + label_width);
    let mut spans = vec![
        ratatui::text::Span::raw(" "),
        ratatui::text::Span::styled(format!("{label:<label_width$}"), muted_style()),
        ratatui::text::Span::styled(ellipsize(value, room), normal_style()),
    ];
    let used = 1 + label_width + value.chars().count();
    if !note.is_empty() {
        let separator = "  ·  ";
        let remaining = width.saturating_sub(used + separator.len());
        if remaining >= 8 {
            spans.push(ratatui::text::Span::styled(separator, muted_style()));
            spans.push(ratatui::text::Span::styled(
                ellipsize_words(note, remaining),
                muted_style(),
            ));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipsize_short() {
        assert_eq!(ellipsize("hello", 10), "hello");
    }

    #[test]
    fn ellipsize_long() {
        assert_eq!(ellipsize("hello world", 8), "hello w…");
    }

    #[test]
    fn ellipsize_zero() {
        assert_eq!(ellipsize("hello", 0), "");
    }

    #[test]
    fn ellipsize_words_short() {
        assert_eq!(ellipsize_words("hello world", 20), "hello world");
    }

    #[test]
    fn ellipsize_words_long() {
        assert_eq!(ellipsize_words("hello world", 8), "hello…");
    }

    #[test]
    fn ellipsize_words_at_space() {
        // "hello world" - space at index 5, width 10: width/2=5, so 5>5 is false
        // Falls back to hard cut at 9 chars = "hello wor…"
        assert_eq!(ellipsize_words("hello world", 10), "hello wor…");
    }
}
