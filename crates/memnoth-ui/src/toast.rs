//! Toast/notification system: ephemeral messages that appear and fade.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::util::{ellipsize, normal_style};

/// Severity level of a toast
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastLevel {
    pub fn style(&self) -> Style {
        match self {
            ToastLevel::Info => Style::default().fg(Color::Cyan),
            ToastLevel::Success => Style::default().fg(Color::Green),
            ToastLevel::Warning => Style::default().fg(Color::Yellow),
            ToastLevel::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ToastLevel::Info => "info",
            ToastLevel::Success => "ok",
            ToastLevel::Warning => "warn",
            ToastLevel::Error => "error",
        }
    }
}

/// A single toast notification
#[derive(Debug, Clone)]
pub struct Toast {
    pub level: ToastLevel,
    pub title: String,
    pub body: String,
    pub ttl_ms: u64,
    pub created_ms: u64,
}

impl Toast {
    pub fn info(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            level: ToastLevel::Info,
            title: title.into(),
            body: body.into(),
            ttl_ms: 5000,
            created_ms: 0,
        }
    }

    pub fn success(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            level: ToastLevel::Success,
            title: title.into(),
            body: body.into(),
            ttl_ms: 3000,
            created_ms: 0,
        }
    }

    pub fn warning(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            level: ToastLevel::Warning,
            title: title.into(),
            body: body.into(),
            ttl_ms: 7000,
            created_ms: 0,
        }
    }

    pub fn error(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            level: ToastLevel::Error,
            title: title.into(),
            body: body.into(),
            ttl_ms: 10000,
            created_ms: 0,
        }
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.created_ms) >= self.ttl_ms
    }

    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.created_ms)
    }
}

/// Toast manager: holds active toasts and handles expiration
#[derive(Debug)]
pub struct ToastManager {
    toasts: Vec<Toast>,
    position: ToastPosition,
    max_visible: usize,
}

impl Default for ToastManager {
    fn default() -> Self {
        Self {
            toasts: Vec::new(),
            position: ToastPosition::default(),
            max_visible: 5,
        }
    }
}

/// Where toasts appear on screen
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToastPosition {
    TopRight,
    TopLeft,
    #[default]
    BottomRight,
    BottomLeft,
}

impl ToastManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_position(mut self, position: ToastPosition) -> Self {
        self.position = position;
        self
    }

    pub fn max_visible(mut self, max: usize) -> Self {
        self.max_visible = max;
        self
    }

    pub fn add(&mut self, toast: Toast) {
        self.toasts.push(toast);
        self.prune();
    }

    pub fn info(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.add(Toast::info(title, body));
    }

    pub fn success(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.add(Toast::success(title, body));
    }

    pub fn warning(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.add(Toast::warning(title, body));
    }

    pub fn error(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.add(Toast::error(title, body));
    }

    pub fn tick(&mut self, now_ms: u64) {
        self.toasts.retain(|t| !t.is_expired(now_ms));
    }

    pub fn dismiss(&mut self, index: usize) {
        if index < self.toasts.len() {
            self.toasts.remove(index);
        }
    }

    pub fn clear(&mut self) {
        self.toasts.clear();
    }

    pub fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    pub fn count(&self) -> usize {
        self.toasts.len()
    }

    fn prune(&mut self) {
        if self.toasts.len() > self.max_visible {
            self.toasts.drain(..self.toasts.len() - self.max_visible);
        }
    }

    /// Render all toasts
    pub fn render(&self, frame: &mut Frame, area: Rect, now_ms: u64) {
        let visible: Vec<&Toast> = self
            .toasts
            .iter()
            .filter(|t| !t.is_expired(now_ms))
            .take(self.max_visible)
            .collect();

        if visible.is_empty() {
            return;
        }

        let toast_height = 3;
        let gap = 1;
        let total_height = visible.len() * (toast_height + gap) - gap;
        let width = area.width.min(60);

        let (x, y) = match self.position {
            ToastPosition::TopRight => (area.x + area.width.saturating_sub(width), area.y),
            ToastPosition::TopLeft => (area.x, area.y),
            ToastPosition::BottomRight => (
                area.x + area.width.saturating_sub(width),
                area.y.saturating_sub(total_height as u16),
            ),
            ToastPosition::BottomLeft => (area.x, area.y.saturating_sub(total_height as u16)),
        };

        for (idx, toast) in visible.iter().enumerate() {
            let toast_y = y + (idx * (toast_height + gap)) as u16;
            let toast_area = Rect::new(x, toast_y, width, toast_height as u16);

            let mut spans: Vec<Span> = Vec::new();
            spans.push(Span::styled(
                format!(" [{}] ", toast.level.label()),
                toast.level.style(),
            ));
            spans.push(Span::styled(
                format!(" {} ", toast.title),
                Style::default().add_modifier(Modifier::BOLD),
            ));

            let title_line = Line::from(spans);
            let body_line = Line::from(Span::styled(
                ellipsize(&toast.body, width.saturating_sub(4) as usize),
                normal_style(),
            ));

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(toast.level.style())
                .title(title_line);
            let inner_area = block.inner(toast_area);
            frame.render_widget(Clear, toast_area);
            frame.render_widget(block, toast_area);
            frame.render_widget(
                Paragraph::new(vec![body_line]).wrap(Wrap { trim: false }),
                inner_area,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_expiration() {
        let mut toast = Toast::info("test", "body");
        toast.created_ms = 100;
        assert!(!toast.is_expired(200));
        assert!(toast.is_expired(6000));
    }

    #[test]
    fn toast_manager_add_and_tick() {
        let mut manager = ToastManager::new();
        manager.add(Toast::info("t1", "b1"));
        manager.add(Toast::success("t2", "b2"));
        assert_eq!(manager.count(), 2);

        // Set creation times manually for expiry testing
        // Note: in real usage created_ms is set when added; here we simulate
        manager.toasts[0].created_ms = 0;
        manager.toasts[1].created_ms = 0;
        manager.tick(100);
        assert_eq!(manager.count(), 2);
        manager.tick(10000);
        assert_eq!(manager.count(), 0);
    }

    #[test]
    fn toast_level_styles() {
        assert!(matches!(ToastLevel::Info.style().fg, Some(Color::Cyan)));
        assert!(matches!(ToastLevel::Error.style().fg, Some(Color::Red)));
        assert_eq!(ToastLevel::Success.label(), "ok");
    }
}
