//! Diff overlay: view and interact with unified diffs (accept/reject hunks).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use crate::tui::theme::{Tokens, tokens};
use memnoth_ui::diff::{DiffAction, DiffConfig, DiffLine, DiffViewer, parse_unified_diff};

// Declared here (not in overlays/mod.rs) so the pure model stays owned by this
// file. Do not also add `mod staged_diff;` to the parent module.
// The staged multi-file review model and its overlay are a pure, test-only
// prototype: no caller prepares a `StagedDiff` in production and no apply path
// consumes `EditRequest`s yet. Keeping them behind `cfg(test)` avoids shipping
// a surface that cannot be reached (AGENTS.md: no dead surface, no
// `#[allow(dead_code)]` to hide one).
#[cfg(test)]
#[path = "staged_diff.rs"]
mod staged_diff;

#[cfg(test)]
use self::staged_diff::{HunkDecision, StagedDiff};

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

        let t = tokens();
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(t.text))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(t.border))
                    .title(" Help "),
            )
            .wrap(Wrap { trim: true });
        frame.render_widget(help, area);
    }
}

/// The `+N -M` counts keep their signs. Colour reinforces them but never
/// replaces them: under NO_COLOR every token is Reset and the signs are still
/// the only thing that says which way the hunk goes.
fn hunk_count_spans(added: usize, removed: usize, t: Tokens) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("+{added}"), Style::default().fg(t.success)),
        Span::styled(format!(" -{removed}"), Style::default().fg(t.error)),
    ]
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
        let t = tokens();
        let hunk_info = if hunk_count > 0 {
            let mut spans = vec![Span::styled(
                format!("Hunk {hunk_idx}/{hunk_count}  ("),
                Style::default().fg(t.muted),
            )];
            spans.extend(hunk_count_spans(added, removed, t));
            spans.push(Span::styled(")", Style::default().fg(t.muted)));
            spans
        } else {
            vec![Span::styled("No hunks", Style::default().fg(t.muted))]
        };
        let header = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("Diff: ", Style::default().fg(t.text)),
                Span::styled(
                    header_text,
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(hunk_info),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border))
                .title(" Diff Viewer "),
        )
        .style(Style::default().fg(t.text));
        frame.render_widget(header, chunks[0]);

        // Diff viewer
        self.viewer.set_config(DiffConfig::default());
        self.viewer.render(frame, chunks[1]);

        // Help footer
        let help_text = "↑↓: hunk · ←→: scroll · a: accept · r: reject · A: accept all · R: reject all · n/N: conflict · s: side-by-side · w: whitespace · l: line nums · ?: help · esc: close";
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(t.muted))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(t.border)),
            );
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

/// Review overlay for a staged, pre-write, multi-file patch.
///
/// Additive to [`DiffOverlay`]: the single-file preview above is unchanged.
/// Nothing here writes to disk. [`StagedDiff::to_edits`] is the only output, and
/// the header says plainly that the staged changes have NOT been applied.
#[derive(Debug, Default)]
#[cfg(test)]
pub struct StagedDiffOverlay {
    visible: bool,
    staged: StagedDiff,
    cursor: usize,
    help_visible: bool,
}

#[cfg(test)]
impl StagedDiffOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a patch for review. Every hunk stays pending; nothing is applied.
    pub fn load(&mut self, staged: StagedDiff) {
        self.staged = staged;
        self.cursor = 0;
        self.help_visible = false;
    }

    /// Read-only access to the staged model (used by tests and future callers).
    pub fn staged(&self) -> &StagedDiff {
        &self.staged
    }

    fn move_cursor(&mut self, forward: bool) {
        let count = self.staged.hunk_count();
        if count == 0 {
            self.cursor = 0;
            return;
        }
        if forward {
            self.cursor = (self.cursor + 1).min(count - 1);
        } else {
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    fn decide(&mut self, decision: HunkDecision, app: &mut App) {
        if self.staged.decide_flat(self.cursor, decision) {
            let word = match decision {
                HunkDecision::Accept => "accepted",
                HunkDecision::Reject => "rejected",
                HunkDecision::Pending => "reset to pending",
            };
            app.push(
                crate::tui::app::EntryRole::Info,
                format!("Staged hunk {} {word} (not applied)", self.cursor + 1),
            );
        }
    }

    fn decide_all_pending(&mut self, decision: HunkDecision, app: &mut App) {
        let changed = self.staged.decide_all_pending(decision);
        let word = match decision {
            HunkDecision::Accept => "accepted",
            HunkDecision::Reject => "rejected",
            HunkDecision::Pending => "reset to pending",
        };
        app.push(
            crate::tui::app::EntryRole::Info,
            format!("{changed} pending hunk(s) {word} (not applied)"),
        );
    }

    fn next_pending(&mut self, forward: bool) {
        let count = self.staged.hunk_count();
        if count == 0 {
            return;
        }
        for step in 1..=count {
            let candidate = if forward {
                (self.cursor + step) % count
            } else {
                (self.cursor + count - (step % count)) % count
            };
            if let Some((_, hunk)) = self.staged.flat_hunk(candidate)
                && hunk.is_pending()
            {
                self.cursor = candidate;
                return;
            }
        }
    }

    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let t = tokens();
        let help = Paragraph::new(vec![
            Line::from("Staged Diff Review Help"),
            Line::from(""),
            Line::from("  ↑/↓          - Previous/next hunk"),
            Line::from("  a            - Accept current hunk"),
            Line::from("  r            - Reject current hunk"),
            Line::from("  p            - Reset current hunk to pending"),
            Line::from("  A            - Accept all pending hunks"),
            Line::from("  R            - Reject all pending hunks"),
            Line::from("  n/N          - Next/previous pending hunk"),
            Line::from("  ?            - Toggle this help"),
            Line::from("  Esc/q        - Close overlay"),
            Line::from(""),
            Line::from("Nothing in this overlay writes to disk. Accepted hunks become"),
            Line::from("edit requests only; reject leaves workspace bytes untouched."),
        ])
        .style(Style::default().fg(t.text))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border))
                .title(" Staged Diff Help "),
        )
        .wrap(Wrap { trim: true });
        frame.render_widget(help, area);
    }
}

/// The decision is stated by the literal mark as well as the colour. A NO_COLOR
/// frame still reads `[ACCEPT]`, `[REJECT]` or `[PENDING]`.
#[cfg(test)]
fn staged_mark(decision: HunkDecision, t: Tokens) -> (&'static str, Style) {
    match decision {
        HunkDecision::Accept => (
            "[ACCEPT]",
            Style::default().fg(t.success).add_modifier(Modifier::BOLD),
        ),
        HunkDecision::Reject => (
            "[REJECT]",
            Style::default().fg(t.error).add_modifier(Modifier::BOLD),
        ),
        HunkDecision::Pending => (
            "[PENDING]",
            Style::default().fg(t.warning).add_modifier(Modifier::BOLD),
        ),
    }
}

/// The sign is part of the meaning: `+` and `-` stay even when the tokens
/// collapse to Reset, so a removed line is never distinguishable by hue alone.
#[cfg(test)]
fn staged_line_style(line: &staged_diff::StagedLine, t: Tokens) -> (&'static str, Style) {
    match line {
        staged_diff::StagedLine::Context(_) => ("  ", Style::default().fg(t.muted)),
        staged_diff::StagedLine::Removed(_) => ("- ", Style::default().fg(t.error)),
        staged_diff::StagedLine::Added(_) => ("+ ", Style::default().fg(t.success)),
    }
}

#[cfg(test)]
impl Overlay for StagedDiffOverlay {
    fn kind(&self) -> OverlayKind {
        // No dedicated OverlayKind exists yet; this overlay is presented by a
        // caller with a prepared StagedDiff rather than opened from the stack.
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

        if key.code == KeyCode::Char('?') || (ctrl && key.code == KeyCode::Char('h')) {
            self.help_visible = !self.help_visible;
            return OverlayAction::None;
        }
        if self.help_visible {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('h')
            ) {
                self.help_visible = false;
            }
            return OverlayAction::None;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => OverlayAction::Close,
            KeyCode::Up => {
                self.move_cursor(false);
                OverlayAction::None
            }
            KeyCode::Down => {
                self.move_cursor(true);
                OverlayAction::None
            }
            KeyCode::Home => {
                self.cursor = 0;
                OverlayAction::None
            }
            KeyCode::End => {
                self.cursor = self.staged.hunk_count().saturating_sub(1);
                OverlayAction::None
            }
            KeyCode::Char('a') if !ctrl => {
                self.decide(HunkDecision::Accept, app);
                OverlayAction::None
            }
            KeyCode::Char('r') if !ctrl => {
                self.decide(HunkDecision::Reject, app);
                OverlayAction::None
            }
            KeyCode::Char('p') if !ctrl => {
                self.decide(HunkDecision::Pending, app);
                OverlayAction::None
            }
            KeyCode::Char('A') => {
                self.decide_all_pending(HunkDecision::Accept, app);
                OverlayAction::None
            }
            KeyCode::Char('R') => {
                self.decide_all_pending(HunkDecision::Reject, app);
                OverlayAction::None
            }
            KeyCode::Char('n') => {
                self.next_pending(true);
                OverlayAction::None
            }
            KeyCode::Char('N') => {
                self.next_pending(false);
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
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
                Constraint::Length(4), // Header + not-applied marker
                Constraint::Min(0),    // Staged hunks
                Constraint::Length(2), // Help footer
            ])
            .split(area);

        let t = tokens();
        let accepted = self.staged.count_decided(HunkDecision::Accept);
        let rejected = self.staged.count_decided(HunkDecision::Reject);
        let pending = self.staged.count_decided(HunkDecision::Pending);
        // The words are the state; the colours only reinforce them.
        let summary = vec![
            Span::styled(
                format!(
                    "{} file(s) · {} hunk(s) · ",
                    self.staged.files.len(),
                    self.staged.hunk_count()
                ),
                Style::default().fg(t.muted),
            ),
            Span::styled(
                format!("accepted {accepted}"),
                Style::default().fg(t.success),
            ),
            Span::styled(" · ", Style::default().fg(t.muted)),
            Span::styled(format!("rejected {rejected}"), Style::default().fg(t.error)),
            Span::styled(" · ", Style::default().fg(t.muted)),
            Span::styled(format!("pending {pending}"), Style::default().fg(t.warning)),
        ];
        let header = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("Staged Diff ", Style::default().fg(t.text)),
                Span::styled(
                    "NOT APPLIED",
                    Style::default().fg(t.error).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(summary),
            Line::from(Span::styled(
                "Reject leaves bytes untouched. No write path; a real apply must revalidate the base first.",
                Style::default().fg(t.muted),
            )),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border))
                .title(" Staged Diff (pre-write) "),
        )
        .style(Style::default().fg(t.text));
        frame.render_widget(header, chunks[0]);

        let mut body: Vec<Line> = Vec::new();
        let refs = self.staged.hunk_refs();
        if refs.is_empty() {
            body.push(Line::from(Span::styled(
                "No staged hunks",
                Style::default().fg(t.muted),
            )));
        }
        for (flat, &(fi, hi)) in refs.iter().enumerate() {
            let file = &self.staged.files[fi];
            let hunk = &file.hunks[hi];
            let (mark, mark_style) = staged_mark(hunk.decision, t);
            let focused = flat == self.cursor;
            let pointer = if focused { "▸ " } else { "  " };
            // The caret glyph marks the hunk the keys act on; the accent is only
            // ever spent on that focused actionable row.
            let pointer_style = if focused {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.muted)
            };
            body.push(Line::from(vec![
                Span::styled(pointer, pointer_style),
                Span::styled(format!("{}. ", flat + 1), Style::default().fg(t.muted)),
                Span::styled(mark, mark_style),
                Span::styled(
                    format!(" {} · {}", file.path, hunk.header),
                    Style::default().fg(t.text),
                ),
            ]));
            body.push(Line::from(vec![
                Span::styled("    keys: ", Style::default().fg(t.muted)),
                Span::styled("a", Style::default().fg(t.success)),
                Span::styled(" accept  ", Style::default().fg(t.muted)),
                Span::styled("r", Style::default().fg(t.error)),
                Span::styled(" reject  ", Style::default().fg(t.muted)),
                Span::styled("p", Style::default().fg(t.warning)),
                Span::styled(" pending", Style::default().fg(t.muted)),
            ]));
            for line in &hunk.lines {
                let (prefix, line_style) = staged_line_style(line, t);
                body.push(Line::from(vec![
                    Span::styled(format!("    {prefix}"), Style::default().fg(t.muted)),
                    Span::styled(line.text().to_string(), line_style),
                ]));
            }
        }
        let list = Paragraph::new(body)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(t.border))
                    .title(" Hunks "),
            )
            .style(Style::default().fg(t.text));
        frame.render_widget(list, chunks[1]);

        let footer = Paragraph::new(
            "↑↓: hunk · a: accept · r: reject · p: pending · A/R: all pending · n/N: next/prev · ?: help · esc: close — NOT APPLIED",
        )
        .style(Style::default().fg(t.muted))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border)),
        );
        frame.render_widget(footer, chunks[2]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑/↓", "Navigate staged hunks"),
            ("a", "Accept current hunk"),
            ("r", "Reject current hunk"),
            ("p", "Reset hunk to pending"),
            ("A/R", "Accept/Reject all pending"),
            ("n/N", "Next/Previous pending"),
            ("?", "Show help"),
            ("Esc", "Close overlay"),
        ]
    }
}

#[cfg(test)]
mod staged_tests {
    use super::staged_diff::{Span, StagedLine};
    use super::*;

    fn two_hunk_patch() -> StagedDiff {
        let mut staged = StagedDiff::new();
        let fi = staged.stage_file("src/lib.rs", "a\nb\n");
        staged.files[fi].hunks = vec![
            staged_diff::StagedHunk::new(
                "@@ -1,1 +1,1 @@".to_string(),
                Span::new(1, 1),
                Span::new(1, 1),
                vec![
                    StagedLine::Removed("a".into()),
                    StagedLine::Added("A".into()),
                ],
            ),
            staged_diff::StagedHunk::new(
                "@@ -2,1 +2,1 @@".to_string(),
                Span::new(2, 1),
                Span::new(2, 1),
                vec![
                    StagedLine::Removed("b".into()),
                    StagedLine::Added("B".into()),
                ],
            ),
        ];
        staged
    }

    #[test]
    fn staged_overlay_accepts_and_rejects_per_hunk_without_applying() {
        let mut overlay = StagedDiffOverlay::new();
        overlay.load(two_hunk_patch());
        overlay.show();
        let mut app = App::new("s", false);

        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            overlay.handle_key(key(KeyCode::Char('a')), &mut app),
            OverlayAction::None
        );
        assert_eq!(
            overlay.staged().files[0].hunks[0].decision,
            HunkDecision::Accept
        );
        overlay.handle_key(key(KeyCode::Down), &mut app);
        overlay.handle_key(key(KeyCode::Char('r')), &mut app);
        assert_eq!(
            overlay.staged().files[0].hunks[1].decision,
            HunkDecision::Reject
        );

        // Only the accepted hunk becomes an edit request.
        let edits = overlay.staged().to_edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].hunks.len(), 1);
        assert_eq!(edits[0].hunks[0].replacement, vec!["A"]);
    }

    #[test]
    fn staged_overlay_navigates_toggles_help_and_closes() {
        let mut overlay = StagedDiffOverlay::new();
        overlay.load(two_hunk_patch());
        overlay.show();
        let mut app = App::new("s", false);

        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        overlay.handle_key(key(KeyCode::Char('?')), &mut app);
        overlay.handle_key(key(KeyCode::Char('?')), &mut app);
        overlay.handle_key(key(KeyCode::End), &mut app);
        overlay.handle_key(key(KeyCode::Home), &mut app);
        overlay.handle_key(key(KeyCode::Char('n')), &mut app);
        overlay.handle_key(key(KeyCode::Char('N')), &mut app);
        overlay.handle_key(key(KeyCode::Char('A')), &mut app);
        assert_eq!(overlay.staged().count_decided(HunkDecision::Accept), 2);
        overlay.handle_key(key(KeyCode::Char('R')), &mut app);
        // Already decided hunks are not flipped by "all pending".
        assert_eq!(overlay.staged().count_decided(HunkDecision::Reject), 0);

        assert_eq!(
            overlay.handle_key(key(KeyCode::Esc), &mut app),
            OverlayAction::Close
        );
    }

    #[test]
    fn staged_overlay_empty_and_hidden_are_safe() {
        let mut overlay = StagedDiffOverlay::new();
        let mut app = App::new("s", false);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        // Hidden overlay consumes keys but does nothing.
        assert_eq!(
            overlay.handle_key(key(KeyCode::Char('a')), &mut app),
            OverlayAction::None
        );
        overlay.show();
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Char('a'),
            KeyCode::Char('r'),
            KeyCode::Char('A'),
            KeyCode::Char('R'),
            KeyCode::Char('n'),
        ] {
            let _ = overlay.handle_key(key(code), &mut app);
        }
        assert!(overlay.staged().to_edits().is_empty());
    }

    #[test]
    fn state_signs_and_marks_survive_a_no_color_palette() {
        use crate::tui::theme::{Palette, Tokens};
        use ratatui::style::Color;

        let colored = Tokens::for_palette(Palette::Dark);
        let plain = Tokens::for_palette(Palette::NoColor);

        // The header counts keep + and - in the text, with success/error only
        // reinforcing what the sign already says.
        let counts = hunk_count_spans(3, 1, colored);
        let text: String = counts.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, "+3 -1");
        assert_eq!(counts[0].style.fg, Some(colored.success));
        assert_eq!(counts[1].style.fg, Some(colored.error));
        let plain_counts = hunk_count_spans(3, 1, plain);
        let plain_text: String = plain_counts
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(plain_text, "+3 -1");
        assert!(
            plain_counts
                .iter()
                .all(|span| span.style.fg == Some(Color::Reset))
        );

        for (decision, mark, role) in [
            (HunkDecision::Accept, "[ACCEPT]", colored.success),
            (HunkDecision::Reject, "[REJECT]", colored.error),
            (HunkDecision::Pending, "[PENDING]", colored.warning),
        ] {
            let (shown, style) = staged_mark(decision, colored);
            assert_eq!(shown, mark);
            assert_eq!(style.fg, Some(role));
            let (shown, style) = staged_mark(decision, plain);
            assert_eq!(shown, mark, "the state word is not themed away");
            assert_eq!(style.fg, Some(Color::Reset));
        }

        for (line, prefix, role) in [
            (StagedLine::Added("A".into()), "+ ", colored.success),
            (StagedLine::Removed("a".into()), "- ", colored.error),
            (StagedLine::Context("c".into()), "  ", colored.muted),
        ] {
            let (shown, style) = staged_line_style(&line, colored);
            assert_eq!(shown, prefix);
            assert_eq!(style.fg, Some(role));
            let (shown, style) = staged_line_style(&line, plain);
            assert_eq!(shown, prefix, "the diff sign is not themed away");
            assert_eq!(style.fg, Some(Color::Reset));
        }
    }
}
