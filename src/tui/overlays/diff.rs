//! Diff overlay: view and interact with unified diffs (accept/reject hunks).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use memnoth_ui::diff::{DiffAction, DiffConfig, DiffLine, DiffViewer, parse_unified_diff};

#[derive(Debug, Default)]
pub struct DiffOverlay {
    visible: bool,
    viewer: DiffViewer,
    diff_text: String,
    file_path: String,
    help_visible: bool,
}

impl DiffOverlay {
    pub fn new() -> Self {
        Self {
            viewer: DiffViewer::new(),
            ..Default::default()
        }
    }

    /// Load a diff from unified diff text.
    pub fn load_diff(&mut self, diff: String, file_path: String) {
        self.diff_text = diff;
        self.file_path = file_path.clone();
        self.viewer = parse_unified_diff(&self.diff_text, file_path);
    }

    /// Set diff directly from a DiffViewer (for testing or pre-parsed diffs).
    pub fn set_viewer(&mut self, viewer: DiffViewer) {
        self.viewer = viewer;
    }

    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let help_text = vec![
            Line::from("Diff Viewer Help"),
            Line::from(""),
            Line::from("Navigation:"),
            Line::from("  ↑/↓          - Previous/next hunk"),
            Line::from("  ←/→          - Scroll left/right"),
            Line::from("  PgUp/PgDn    - Jump 10 hunks"),
            Line::from("  Home/End     - First/last hunk"),
            Line::from("  n/N          - Next/previous conflict"),
            Line::from(""),
            Line::from("Actions:"),
            Line::from("  a            - Accept current hunk"),
            Line::from("  r            - Reject current hunk"),
            Line::from("  A            - Accept all hunks"),
            Line::from("  R            - Reject all hunks"),
            Line::from(""),
            Line::from("View Options:"),
            Line::from("  s            - Toggle side-by-side view"),
            Line::from("  w            - Toggle whitespace visibility"),
            Line::from("  l            - Toggle line numbers"),
            Line::from(""),
            Line::from("Other:"),
            Line::from("  Ctrl+C       - Copy current hunk"),
            Line::from("  P            - Pin current hunk"),
            Line::from("  ?            - Toggle this help"),
            Line::from("  Esc/q        - Close overlay"),
        ];

        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::White))
            .block(Block::default().borders(Borders::ALL).title(" Help "))
            .wrap(Wrap { trim: true });
        frame.render_widget(help, area);
    }
}

impl Overlay for DiffOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Diff
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.help_visible = false;
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Handle help toggle
        if key.code == KeyCode::Char('?') || (key.code == KeyCode::Char('h') && ctrl) {
            self.help_visible = !self.help_visible;
            return OverlayAction::None;
        }

        if self.help_visible {
            if key.code == KeyCode::Esc
                || key.code == KeyCode::Char('?')
                || key.code == KeyCode::Char('h')
            {
                self.help_visible = false;
            }
            return OverlayAction::None;
        }

        match key.code {
            KeyCode::Esc => return OverlayAction::Close,
            KeyCode::Char('q') => return OverlayAction::Close,
            KeyCode::Up => {
                self.viewer.previous_hunk();
            }
            KeyCode::Down => {
                self.viewer.next_hunk();
            }
            KeyCode::Left => {
                self.viewer.scroll_left();
            }
            KeyCode::Right => {
                self.viewer.scroll_right();
            }
            KeyCode::PageUp => {
                for _ in 0..10 {
                    self.viewer.previous_hunk();
                }
            }
            KeyCode::PageDown => {
                for _ in 0..10 {
                    self.viewer.next_hunk();
                }
            }
            KeyCode::Home => {
                self.viewer.select_first_hunk();
            }
            KeyCode::End => {
                self.viewer.select_last_hunk();
            }
            KeyCode::Char('a') => {
                // Accept current hunk
                if let Some(action) = self.viewer.handle_action(DiffAction::Accept) {
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        format!("Accepted hunk: {action:?}"),
                    );
                }
            }
            KeyCode::Char('r') => {
                // Reject current hunk
                if let Some(action) = self.viewer.handle_action(DiffAction::Reject) {
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        format!("Rejected hunk: {action:?}"),
                    );
                }
            }
            KeyCode::Char('A') => {
                // Accept all hunks
                if let Some(action) = self.viewer.handle_action(DiffAction::AcceptAll) {
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        format!("Accepted all: {action:?}"),
                    );
                }
            }
            KeyCode::Char('R') => {
                // Reject all hunks
                if let Some(action) = self.viewer.handle_action(DiffAction::RejectAll) {
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        format!("Rejected all: {action:?}"),
                    );
                }
            }
            KeyCode::Char('c') if ctrl => {
                // Copy current hunk
                if self.viewer.current_hunk().is_some() {
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        format!("Copied hunk from {}", self.file_path),
                    );
                }
            }
            KeyCode::Char('p') => {
                // Pin current hunk
                if self.viewer.current_hunk().is_some() {
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        format!("Pinned hunk from {}", self.file_path),
                    );
                }
            }
            KeyCode::Char('n') => {
                // Next conflict
                self.viewer.next_conflict();
            }
            KeyCode::Char('N') => {
                // Previous conflict
                self.viewer.previous_conflict();
            }
            KeyCode::Char('s') => {
                // Toggle side-by-side view
                self.viewer.toggle_side_by_side();
            }
            KeyCode::Char('w') => {
                // Toggle whitespace
                self.viewer.toggle_whitespace();
            }
            KeyCode::Char('l') => {
                // Toggle line numbers
                self.viewer.toggle_line_numbers();
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }

        frame.render_widget(Clear, area);

        if self.help_visible {
            self.render_help(frame, area);
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Header
                Constraint::Min(0),    // Diff viewer
                Constraint::Length(2), // Help footer
            ])
            .split(area);

        // Header
        let header_text = if self.file_path.is_empty() {
            "No diff loaded"
        } else {
            &self.file_path
        };
        // A hunk carries its lines rather than running counters, so the totals
        // shown in the header are counted from the hunk the cursor is on.
        let hunk_index = self.viewer.current_hunk_index();
        let hunk_count = self.viewer.hunk_count();
        let mut added = 0;
        let mut removed = 0;
        if let Some(hunk) = self.viewer.current_hunk() {
            for line in &hunk.lines {
                match line {
                    DiffLine::Added(_) => added += 1,
                    DiffLine::Removed(_) => removed += 1,
                    DiffLine::Context(_) => {}
                }
            }
        }
        let hunk_idx = hunk_index.map(|index| index + 1).unwrap_or(0);
        let hunk_info = if hunk_count > 0 {
            format!(
                "Hunk {}/{}  (+{} -{})",
                hunk_idx, hunk_count, added, removed
            )
        } else {
            "No hunks".to_string()
        };
        let header = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("Diff: ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    header_text,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![Span::styled(
                &hunk_info,
                Style::default().fg(Color::Gray),
            )]),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Diff Viewer "),
        )
        .style(Style::default().fg(Color::White));
        frame.render_widget(header, chunks[0]);

        // Diff viewer
        self.viewer.set_config(DiffConfig::default());
        self.viewer.render(frame, chunks[1]);

        // Help footer
        let help_text = "↑↓: hunk · ←→: scroll · a: accept · r: reject · A: accept all · R: reject all · n/N: conflict · s: side-by-side · w: whitespace · l: line nums · ?: help · esc: close";
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(help, chunks[2]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/diff", "Open diff overlay"),
            ("↑/↓", "Navigate hunks"),
            ("←/→", "Scroll horizontally"),
            ("A/R", "Accept/Reject current hunk"),
            ("Shift+A/R", "Accept/Reject all"),
            ("n/N", "Next/Previous conflict"),
            ("S", "Toggle side-by-side"),
            ("W", "Toggle whitespace"),
            ("L", "Toggle line numbers"),
            ("?", "Show help"),
            ("Esc", "Close overlay"),
        ]
    }
}
