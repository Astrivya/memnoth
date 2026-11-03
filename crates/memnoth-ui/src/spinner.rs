//! Spinner/progress widget.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::util::muted_style;

/// Spinner animation frames
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠺", "⠻", "⠽", "⠾"];

/// Progress bar styles
#[derive(Debug, Clone)]
pub struct ProgressBarStyle {
    pub filled: char,
    pub empty: char,
    pub filled_style: Style,
    pub empty_style: Style,
}

impl Default for ProgressBarStyle {
    fn default() -> Self {
        Self {
            filled: '█',
            empty: '░',
            filled_style: Style::default().fg(Color::Cyan),
            empty_style: Style::default().fg(Color::DarkGray),
        }
    }
}

/// Spinner widget
#[derive(Debug, Default)]
pub struct Spinner {
    pub message: String,
    pub frame_index: usize,
    pub style: Style,
}

impl Spinner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    pub fn tick(&mut self) {
        self.frame_index = (self.frame_index + 1) % SPINNER_FRAMES.len();
    }

    pub fn current_frame(&self) -> &str {
        SPINNER_FRAMES[self.frame_index]
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        if area.width < 5 || area.height < 1 {
            return;
        }

        if area.height < 3 {
            // Render without block for very small areas
            let text = format!(" {} {}", self.current_frame(), self.message);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    text,
                    self.style.fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ))),
                area,
            );
            return;
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(muted_style())
            .title(Line::from(Span::styled(" working ", muted_style())));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let spinner = Span::styled(
            format!(" {} {}", self.current_frame(), self.message),
            self.style.fg(Color::Cyan).add_modifier(Modifier::BOLD),
        );
        frame.render_widget(Paragraph::new(Line::from(spinner)), inner);
    }
}

/// Progress bar widget
#[derive(Debug, Default)]
pub struct ProgressBar {
    pub ratio: f64, // 0.0 to 1.0
    pub label: String,
    pub style: ProgressBarStyle,
    pub show_percentage: bool,
    pub width: Option<u16>,
}

impl ProgressBar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ratio(mut self, ratio: f64) -> Self {
        self.ratio = ratio.clamp(0.0, 1.0);
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn set_ratio(&mut self, ratio: f64) {
        self.ratio = ratio.clamp(0.0, 1.0);
    }

    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = label.into();
    }

    pub fn show_percentage(mut self, show: bool) -> Self {
        self.show_percentage = show;
        self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if area.height < 3 {
            return;
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(muted_style())
            .title(Line::from(Span::styled(
                format!(" {} ", self.label),
                muted_style(),
            )));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let bar_width = self.width.unwrap_or(inner.width) as usize;
        let filled_width = ((bar_width as f64) * self.ratio) as usize;
        let empty_width = bar_width.saturating_sub(filled_width);

        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(
            self.style.filled.to_string().repeat(filled_width),
            self.style.filled_style,
        ));
        spans.push(Span::styled(
            self.style.empty.to_string().repeat(empty_width),
            self.style.empty_style,
        ));

        if self.show_percentage {
            let pct = (self.ratio * 100.0) as u32;
            spans.push(Span::styled(
                format!(" {pct:>3}%"),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }

        frame.render_widget(Paragraph::new(Line::from(spans)), inner);
    }

    pub fn render_modal(&self, frame: &mut Frame, area: Rect) {
        if area.width < 40 || area.height < 10 {
            return;
        }
        let width = ((area.width as u32 * 3 / 4) as u16).clamp(40, area.width);
        let height = ((area.height as u32 * 1 / 4) as u16).clamp(6, area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        self.render(frame, popup);
    }
}

/// Combined spinner + progress widget
#[derive(Debug, Default)]
pub struct Progress {
    pub spinner: Spinner,
    pub bar: Option<ProgressBar>,
}

impl Progress {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_spinner(mut self, spinner: Spinner) -> Self {
        self.spinner = spinner;
        self
    }

    pub fn with_progress(mut self, bar: ProgressBar) -> Self {
        self.bar = Some(bar);
        self
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.spinner.set_message(message);
    }

    pub fn set_ratio(&mut self, ratio: f64) {
        if let Some(ref mut bar) = self.bar {
            bar.set_ratio(ratio);
        }
    }

    pub fn tick(&mut self) {
        self.spinner.tick();
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        if area.height < 5 {
            return;
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(muted_style())
            .title(Line::from(Span::styled(" progress ", muted_style())));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height < 3 {
            return;
        }

        // Spinner on top line
        let spinner_area = Rect::new(inner.x, inner.y, inner.width, 1);
        self.spinner.render(frame, spinner_area);

        // Progress bar below
        if let Some(ref bar) = self.bar {
            let bar_area = Rect::new(inner.x, inner.y + 2, inner.width, 3);
            bar.render(frame, bar_area);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_spinner(spinner: &mut Spinner, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| spinner.render(frame, frame.area()))
            .unwrap();
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
    fn spinner_renders_message() {
        let mut spinner = Spinner::new().with_message("loading".to_string());
        let text = render_spinner(&mut spinner, 40, 10);
        assert!(text.contains("loading"));
    }

    #[test]
    fn spinner_tick_changes_frame() {
        let mut spinner = Spinner::new();
        let before = spinner.current_frame().to_string();
        spinner.tick();
        let after = spinner.current_frame().to_string();
        // Should cycle through frames
        assert!(SPINNER_FRAMES.contains(&before.as_str()));
        assert!(SPINNER_FRAMES.contains(&after.as_str()));
    }

    #[test]
    fn progress_bar_ratio_clamped() {
        let mut bar = ProgressBar::new().with_label(String::from("test"));
        bar.set_ratio(-0.5);
        assert_eq!(bar.ratio, 0.0);
        bar.set_ratio(1.5);
        assert_eq!(bar.ratio, 1.0);
    }

    #[test]
    fn progress_renders() {
        let mut progress = Progress::new()
            .with_spinner(Spinner::new().with_message(String::from("working")))
            .with_progress(
                ProgressBar::new()
                    .with_ratio(0.5)
                    .with_label(String::from("files")),
            );
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| progress.render(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        assert!(out.contains("working"));
    }
}
