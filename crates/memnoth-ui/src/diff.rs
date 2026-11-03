//! Diff viewer widget: per-hunk accept/reject, modal.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::util::{ellipsize, muted_style, normal_style, title_style};

/// A hunk in a diff
#[derive(Debug, Clone)]
pub struct DiffHunk {
    pub header: String,
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
    pub accepted: bool,
    pub rejected: bool,
}

impl DiffHunk {
    pub fn new(header: String) -> Self {
        Self {
            header,
            old_start: 0,
            old_count: 0,
            new_start: 0,
            new_count: 0,
            lines: Vec::new(),
            accepted: false,
            rejected: false,
        }
    }

    pub fn is_decided(&self) -> bool {
        self.accepted || self.rejected
    }
}

/// A line within a diff hunk
#[derive(Debug, Clone)]
pub enum DiffLine {
    Context(String),
    Removed(String),
    Added(String),
}

/// Diff viewer configuration
#[derive(Debug, Clone)]
pub struct DiffConfig {
    pub title: String,
    pub file_path: String,
    pub show_line_numbers: bool,
    pub accept_style: Style,
    pub reject_style: Style,
    pub context_style: Style,
    pub removed_style: Style,
    pub added_style: Style,
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            title: "diff".to_string(),
            file_path: String::new(),
            show_line_numbers: true,
            accept_style: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            reject_style: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            context_style: normal_style(),
            removed_style: Style::default().fg(Color::Red),
            added_style: Style::default().fg(Color::Green),
        }
    }
}

/// Diff viewer widget state
#[derive(Debug, Default)]
pub struct DiffViewer {
    config: DiffConfig,
    hunks: Vec<DiffHunk>,
    state: ListState,
    scroll_offset: u16,
    decided_count: usize,
}

impl DiffViewer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: DiffConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }

    pub fn set_hunks(&mut self, hunks: Vec<DiffHunk>) {
        self.hunks = hunks;
        self.decided_count = self.hunks.iter().filter(|h| h.is_decided()).count();
        if !self.hunks.is_empty() {
            self.state.select(Some(0));
        }
    }

    pub fn add_hunk(&mut self, hunk: DiffHunk) {
        self.hunks.push(hunk);
    }

    pub fn set_config(&mut self, config: DiffConfig) {
        self.config = config;
    }

    pub fn accept_current(&mut self) {
        if let Some(idx) = self.state.selected() {
            if let Some(hunk) = self.hunks.get_mut(idx) {
                if !hunk.is_decided() {
                    hunk.accepted = true;
                    hunk.rejected = false;
                    self.decided_count = self.hunks.iter().filter(|h| h.is_decided()).count();
                }
            }
        }
    }

    pub fn reject_current(&mut self) {
        if let Some(idx) = self.state.selected() {
            if let Some(hunk) = self.hunks.get_mut(idx) {
                if !hunk.is_decided() {
                    hunk.rejected = true;
                    hunk.accepted = false;
                    self.decided_count = self.hunks.iter().filter(|h| h.is_decided()).count();
                }
            }
        }
    }

    pub fn select_next(&mut self) {
        let len = self.hunks.len();
        if len == 0 {
            return;
        }
        let next = self
            .state
            .selected()
            .map(|s| (s + 1).min(len - 1))
            .unwrap_or(0);
        self.state.select(Some(next));
    }

    pub fn select_previous(&mut self) {
        let len = self.hunks.len();
        if len == 0 {
            return;
        }
        let prev = self
            .state
            .selected()
            .map(|s| s.saturating_sub(1))
            .unwrap_or(0);
        self.state.select(Some(prev));
    }

    pub fn selected_hunk(&self) -> Option<&DiffHunk> {
        self.state.selected().and_then(|i| self.hunks.get(i))
    }

    pub fn selected_hunk_mut(&mut self) -> Option<&mut DiffHunk> {
        self.state.selected().and_then(|i| self.hunks.get_mut(i))
    }

    pub fn hunk_count(&self) -> usize {
        self.hunks.len()
    }

    pub fn undecided_count(&self) -> usize {
        self.hunks.iter().filter(|h| !h.is_decided()).count()
    }

    pub fn decided_count(&self) -> usize {
        self.decided_count
    }

    pub fn all_decided(&self) -> bool {
        self.decided_count == self.hunks.len()
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        if area.width < 10 || area.height < 5 {
            return;
        }

        let mut lines: Vec<ListItem> = Vec::new();
        for (idx, hunk) in self.hunks.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();

            // Status marker
            let status = if hunk.accepted {
                "[A]".to_string()
            } else if hunk.rejected {
                "[R]".to_string()
            } else {
                "[ ]".to_string()
            };
            let status_style = if hunk.accepted {
                self.config.accept_style
            } else if hunk.rejected {
                self.config.reject_style
            } else {
                Style::default().fg(Color::Gray)
            };
            spans.push(Span::styled(status, status_style));
            spans.push(Span::raw(" "));

            // Hunk header
            spans.push(Span::styled(&hunk.header, title_style().fg(Color::Cyan)));

            if self.state.selected() == Some(idx) {
                spans.push(Span::raw(" ◀"));
            }

            lines.push(ListItem::new(Line::from(spans)));

            // Render diff lines
            for line in &hunk.lines {
                let (prefix, style, text) = match line {
                    DiffLine::Context(t) => ("  ", self.config.context_style, t.clone()),
                    DiffLine::Removed(t) => ("- ", self.config.removed_style, t.clone()),
                    DiffLine::Added(t) => ("+ ", self.config.added_style, t.clone()),
                };
                lines.push(ListItem::new(Line::from(vec![
                    Span::styled(prefix, style),
                    Span::styled(
                        ellipsize(&text, area.width.saturating_sub(2) as usize),
                        style,
                    ),
                ])));
            }
        }

        let list = List::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(muted_style())
                    .title(Line::from(Span::styled(
                        format!(" {} — {} file", self.config.title, self.config.file_path),
                        muted_style(),
                    ))),
            )
            .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan));

        frame.render_stateful_widget(list, area, &mut self.state);
    }

    pub fn render_modal(&mut self, frame: &mut Frame, area: Rect) {
        if area.width < 40 || area.height < 10 {
            return;
        }
        let width = ((area.width as u32 * 4 / 5) as u16).clamp(40, area.width);
        let height = ((area.height as u32 * 4 / 5) as u16).clamp(10, area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        self.render(frame, popup);
    }

    /// Keyboard event handling
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<DiffAction> {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::Down => {
                self.select_next();
                None
            }
            KeyCode::Up => {
                self.select_previous();
                None
            }
            KeyCode::Char('a') => {
                self.accept_current();
                None
            }
            KeyCode::Char('r') => {
                self.reject_current();
                None
            }
            KeyCode::Enter => {
                if let Some(hunk) = self.selected_hunk() {
                    if hunk.is_decided() {
                        Some(DiffAction::Accepted)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            KeyCode::Char('A') => {
                self.accept_current();
                Some(DiffAction::AcceptedCurrent)
            }
            KeyCode::Char('R') => {
                self.reject_current();
                Some(DiffAction::RejectedCurrent)
            }
            KeyCode::Esc => Some(DiffAction::Cancel),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(DiffAction::AcceptAll)
            }
            KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(DiffAction::RejectAll)
            }
            _ => None,
        }
    }
}

/// Actions the diff viewer can request
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffAction {
    Cancel,
    Accepted,
    AcceptedCurrent,
    RejectedCurrent,
    AcceptAll,
    RejectAll,
}

/// Helper to parse a unified diff into hunks
pub fn parse_unified_diff(diff: &str, file_path: String) -> DiffViewer {
    let mut viewer = DiffViewer::new();
    viewer.set_config(DiffConfig {
        title: "diff".to_string(),
        file_path,
        ..Default::default()
    });

    let mut current_hunk: Option<DiffHunk> = None;

    for line in diff.lines() {
        if line.starts_with("@@") {
            if let Some(hunk) = current_hunk.take() {
                viewer.add_hunk(hunk);
            }
            let mut hunk = DiffHunk::new(line.to_string());
            // Parse @@ -old_start,old_count +new_start,new_count @@
            if let Some(rest) = line.strip_prefix("@@ -") {
                let parts: Vec<&str> = rest.splitn(2, " +").collect();
                if parts.len() == 2 {
                    let old_part = parts[0];
                    let new_part = parts[1];
                    if let Some(comma_pos) = old_part.find(',') {
                        hunk.old_start = old_part[..comma_pos].parse().unwrap_or(0);
                        let rest_old = &old_part[comma_pos + 1..];
                        if let Some(space_pos) = rest_old.find(' ') {
                            hunk.old_count = rest_old[..space_pos].parse().unwrap_or(1);
                        } else {
                            hunk.old_count = rest_old.parse().unwrap_or(1);
                        }
                    } else {
                        hunk.old_start = old_part.parse().unwrap_or(0);
                        hunk.old_count = 1;
                    }
                    if let Some(space_pos) = new_part.find(' ') {
                        hunk.new_start = new_part[..space_pos].parse().unwrap_or(0);
                    } else {
                        hunk.new_start = new_part.parse().unwrap_or(0);
                    }
                }
            }
            current_hunk = Some(hunk);
        } else if let Some(hunk) = current_hunk.as_mut() {
            if line.starts_with('+') {
                hunk.lines.push(DiffLine::Added(line[1..].to_string()));
                hunk.new_count += 1;
            } else if line.starts_with('-') {
                hunk.lines.push(DiffLine::Removed(line[1..].to_string()));
                hunk.old_count += 1;
            } else if line.starts_with(' ') {
                hunk.lines.push(DiffLine::Context(line[1..].to_string()));
            }
        }
    }
    if let Some(hunk) = current_hunk {
        viewer.add_hunk(hunk);
    }

    viewer
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_diff(viewer: &mut DiffViewer, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| viewer.render(frame, frame.area()))
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
    fn diff_renders_hunks() {
        let mut viewer = DiffViewer::new();
        viewer.set_hunks(vec![DiffHunk::new("@@ -1,3 +1,3 @@".to_string())]);
        let text = render_diff(&mut viewer, 80, 20);
        assert!(text.contains("@@ -1,3 +1,3 @@"));
    }

    #[test]
    fn diff_accept_and_reject() {
        let mut viewer = DiffViewer::new();
        viewer.set_hunks(vec![DiffHunk::new("@@ -1,1 +1,1 @@".to_string())]);
        assert_eq!(viewer.undecided_count(), 1);

        viewer.accept_current();
        assert_eq!(viewer.decided_count(), 1);
        assert_eq!(viewer.undecided_count(), 0);
        assert!(viewer.selected_hunk().unwrap().accepted);

        viewer.set_hunks(vec![DiffHunk::new("@@ -1,1 +1,1 @@".to_string())]);
        viewer.reject_current();
        assert!(!viewer.selected_hunk().unwrap().accepted);
        assert!(viewer.selected_hunk().unwrap().rejected);
    }

    #[test]
    fn parse_unified_diff_basic() {
        let diff = "@@ -1,3 +1,3 @@\n line1\n-line2\n+line2 modified\n line3\n";
        let viewer = parse_unified_diff(diff, "test.rs".into());
        assert_eq!(viewer.hunk_count(), 1);
        assert_eq!(viewer.config.file_path, "test.rs");
    }
}
