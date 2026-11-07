//! Diff pane: inline per-hunk accept/reject for file diffs in the transcript.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::tui::app::App;

/// A diff hunk for inline display
#[derive(Debug, Clone)]
pub struct InlineDiffHunk {
    pub file_path: String,
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<DiffLine>,
    pub accepted: Option<bool>, // None = pending, Some(true) = accepted, Some(false) = rejected
}

/// A line within a diff hunk
#[derive(Debug, Clone)]
pub enum DiffLine {
    Context(String),
    Added(String),
    Removed(String),
}

/// Diff pane showing inline diffs with accept/reject actions
#[derive(Debug, Default)]
pub struct DiffPane {
    pub visible: bool,
    pub hunks: Vec<InlineDiffHunk>,
    pub current_hunk: usize,
    pub list_state: ListState,
    pub show_line_numbers: bool,
    pub side_by_side: bool,
    pub show_whitespace: bool,
    pub detail_view: bool,
    pub detail_content: String,
}

impl DiffPane {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            visible: false,
            hunks: Vec::new(),
            current_hunk: 0,
            list_state: state,
            show_line_numbers: true,
            side_by_side: false,
            show_whitespace: false,
            detail_view: false,
            detail_content: String::new(),
        }
    }

    /// Load a diff from unified diff text
    pub fn load_diff(&mut self, diff: &str, file_path: &str) {
        self.hunks = parse_unified_diff(diff, file_path.to_string());
        self.current_hunk = 0;
        self.list_state
            .select(if self.hunks.is_empty() { None } else { Some(0) });
        self.visible = true;
    }

    /// Set hunks directly (for pre-parsed diffs)
    pub fn set_hunks(&mut self, hunks: Vec<InlineDiffHunk>) {
        self.hunks = hunks;
        self.current_hunk = 0;
        self.list_state
            .select(if self.hunks.is_empty() { None } else { Some(0) });
        self.visible = true;
    }

    /// Handle key events for the diff pane
    pub fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> bool {
        if !self.visible || self.hunks.is_empty() {
            return false;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Up => {
                if self.current_hunk > 0 {
                    self.current_hunk -= 1;
                    self.list_state.select(Some(self.current_hunk));
                }
                true
            }
            KeyCode::Down => {
                if self.current_hunk < self.hunks.len() - 1 {
                    self.current_hunk += 1;
                    self.list_state.select(Some(self.current_hunk));
                }
                true
            }
            KeyCode::PageUp => {
                self.current_hunk = self.current_hunk.saturating_sub(10);
                self.list_state.select(Some(self.current_hunk));
                true
            }
            KeyCode::PageDown => {
                self.current_hunk = (self.current_hunk + 10).min(self.hunks.len() - 1);
                self.list_state.select(Some(self.current_hunk));
                true
            }
            KeyCode::Home => {
                self.current_hunk = 0;
                self.list_state.select(Some(0));
                true
            }
            KeyCode::End => {
                self.current_hunk = self.hunks.len() - 1;
                self.list_state.select(Some(self.current_hunk));
                true
            }
            KeyCode::Char('a') if !ctrl && !shift => {
                // Accept current hunk
                if let Some(hunk) = self.hunks.get_mut(self.current_hunk) {
                    hunk.accepted = Some(true);
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        format!("Accepted hunk in {}", hunk.file_path),
                    );
                }
                true
            }
            KeyCode::Char('r') if !ctrl && !shift => {
                // Reject current hunk
                if let Some(hunk) = self.hunks.get_mut(self.current_hunk) {
                    hunk.accepted = Some(false);
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        format!("Rejected hunk in {}", hunk.file_path),
                    );
                }
                true
            }
            KeyCode::Char('A') => {
                // Accept all hunks
                for hunk in &mut self.hunks {
                    if hunk.accepted.is_none() {
                        hunk.accepted = Some(true);
                    }
                }
                app.push(
                    crate::tui::app::EntryRole::Info,
                    "Accepted all hunks".to_string(),
                );
                true
            }
            KeyCode::Char('R') => {
                // Reject all hunks
                for hunk in &mut self.hunks {
                    if hunk.accepted.is_none() {
                        hunk.accepted = Some(false);
                    }
                }
                app.push(
                    crate::tui::app::EntryRole::Info,
                    "Rejected all hunks".to_string(),
                );
                true
            }
            KeyCode::Char('n') => {
                // Next pending hunk
                for i in (self.current_hunk + 1)..self.hunks.len() {
                    if self.hunks[i].accepted.is_none() {
                        self.current_hunk = i;
                        self.list_state.select(Some(i));
                        break;
                    }
                }
                true
            }
            KeyCode::Char('N') => {
                // Previous pending hunk
                for i in (0..self.current_hunk).rev() {
                    if self.hunks[i].accepted.is_none() {
                        self.current_hunk = i;
                        self.list_state.select(Some(i));
                        break;
                    }
                }
                true
            }
            KeyCode::Char('l') => {
                self.show_line_numbers = !self.show_line_numbers;
                true
            }
            KeyCode::Char('s') => {
                self.side_by_side = !self.side_by_side;
                true
            }
            KeyCode::Char('w') => {
                self.show_whitespace = !self.show_whitespace;
                true
            }
            KeyCode::Enter => {
                self.detail_view = !self.detail_view;
                if self.detail_view {
                    self.update_detail_content();
                }
                true
            }
            KeyCode::Esc if self.detail_view => {
                self.detail_view = false;
                true
            }
            _ => false,
        }
    }

    fn update_detail_content(&mut self) {
        if let Some(hunk) = self.hunks.get(self.current_hunk) {
            let status = match hunk.accepted {
                Some(true) => "ACCEPTED",
                Some(false) => "REJECTED",
                None => "PENDING",
            };
            self.detail_content = format!(
                "File: {}\nHunk: {}/{}\nStatus: {}\n\nOld: {} lines from {}\nNew: {} lines from {}\n\nLines:\n{}",
                hunk.file_path,
                self.current_hunk + 1,
                self.hunks.len(),
                status,
                hunk.old_lines,
                hunk.old_start,
                hunk.new_lines,
                hunk.new_start,
                hunk.lines
                    .iter()
                    .map(|l| match l {
                        DiffLine::Context(s) => format!("  {}", s),
                        DiffLine::Added(s) => format!("+ {}", s),
                        DiffLine::Removed(s) => format!("- {}", s),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        } else {
            self.detail_content = String::new();
        }
    }

    /// Render the diff pane
    pub fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }

        if self.hunks.is_empty() {
            let empty = Paragraph::new("No diff loaded")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL).title(" Diff "))
                .alignment(ratatui::layout::Alignment::Center);
            frame.render_widget(empty, area);
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Header
                Constraint::Min(0),    // Diff content
                Constraint::Length(2), // Help footer
            ])
            .split(area);

        // Header
        self.render_header(frame, chunks[0]);

        // Diff content
        if self.detail_view {
            self.render_detail(frame, chunks[1]);
        } else {
            self.render_hunk_list(frame, chunks[1]);
        }

        // Footer
        self.render_footer(frame, chunks[2]);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        if let Some(hunk) = self.hunks.get(self.current_hunk) {
            let status = match hunk.accepted {
                Some(true) => "✓ ACCEPTED",
                Some(false) => "✗ REJECTED",
                None => "⏳ PENDING",
            };
            let status_color = match hunk.accepted {
                Some(true) => Color::Green,
                Some(false) => Color::Red,
                None => Color::Yellow,
            };

            let header = Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("Diff: ", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        &hunk.file_path,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::styled(
                        format!("Hunk {}/{}  ", self.current_hunk + 1, self.hunks.len()),
                        Style::default().fg(Color::Gray),
                    ),
                    Span::styled(
                        status,
                        Style::default()
                            .fg(status_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  (-{} +{})", hunk.old_lines, hunk.new_lines),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Inline Diff "),
            )
            .style(Style::default().fg(Color::White));
            frame.render_widget(header, area);
        }
    }

    fn render_hunk_list(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(hunk) = self.hunks.get(self.current_hunk) {
            let mut items = Vec::new();

            // Show context lines before
            for line in &hunk.lines {
                let (prefix, color) = match line {
                    DiffLine::Context(_) => ("  ", Color::DarkGray),
                    DiffLine::Added(_) => ("+ ", Color::Green),
                    DiffLine::Removed(_) => ("- ", Color::Red),
                };

                let line_text = match line {
                    DiffLine::Context(s) => s,
                    DiffLine::Added(s) => s,
                    DiffLine::Removed(s) => s,
                };

                let line_content = if self.show_whitespace {
                    line_text.replace(' ', "·").replace('\t', "→")
                } else {
                    line_text.to_string()
                };

                items.push(ListItem::new(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(color)),
                    Span::styled(line_content, Style::default().fg(color)),
                ])));
            }

            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Hunk Content "),
                )
                .style(Style::default().fg(Color::White));
            frame.render_widget(list, area);
        }
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect) {
        let detail = Paragraph::new(self.detail_content.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Hunk Detail "),
            )
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(Color::White));
        frame.render_widget(detail, area);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let footer_text = "↑↓: hunk · PgUp/PgDn: jump · a: accept · r: reject · A/R: all · n/N: next/prev pending · L: line nums · S: side-by-side · W: whitespace · Enter: detail · Esc: close";
        let footer = Paragraph::new(footer_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(footer, area);
    }
}

/// Parse a unified diff into inline diff hunks
fn parse_unified_diff(diff: &str, file_path: String) -> Vec<InlineDiffHunk> {
    let mut hunks = Vec::new();
    let lines: Vec<&str> = diff.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Look for hunk header: @@ -old_start,old_lines +new_start,new_lines @@
        if line.starts_with("@@ ") && line.ends_with(" @@") {
            // Parse hunk header
            let header = &line[3..line.len() - 3]; // Remove "@@ " and " @@"
            let parts: Vec<&str> = header.split(' ').collect();

            let mut old_start = 0;
            let mut old_lines = 1;
            let mut new_start = 0;
            let mut new_lines = 1;

            if parts.len() >= 2 {
                // Parse old range
                let old_part = parts[0].trim_start_matches('-');
                if let Some(comma) = old_part.find(',') {
                    old_start = old_part[..comma].parse().unwrap_or(0);
                    old_lines = old_part[comma + 1..].parse().unwrap_or(1);
                } else {
                    old_start = old_part.parse().unwrap_or(0);
                }

                // Parse new range
                let new_part = parts[1].trim_start_matches('+');
                if let Some(comma) = new_part.find(',') {
                    new_start = new_part[..comma].parse().unwrap_or(0);
                    new_lines = new_part[comma + 1..].parse().unwrap_or(1);
                } else {
                    new_start = new_part.parse().unwrap_or(0);
                }
            }

            // Collect hunk lines
            let mut hunk_lines = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].starts_with("@@ ") {
                let l = lines[i];
                if let Some(rest) = l.strip_prefix('+') {
                    hunk_lines.push(DiffLine::Added(rest.to_string()));
                } else if let Some(rest) = l.strip_prefix('-') {
                    hunk_lines.push(DiffLine::Removed(rest.to_string()));
                } else if let Some(rest) = l.strip_prefix(' ') {
                    hunk_lines.push(DiffLine::Context(rest.to_string()));
                } else if l.starts_with('\\') {
                    // No newline at end of file marker
                } else {
                    hunk_lines.push(DiffLine::Context(l.to_string()));
                }
                i += 1;
            }

            hunks.push(InlineDiffHunk {
                file_path: file_path.clone(),
                old_start,
                old_lines,
                new_start,
                new_lines,
                lines: hunk_lines,
                accepted: None,
            });
        } else {
            i += 1;
        }
    }

    hunks
}
