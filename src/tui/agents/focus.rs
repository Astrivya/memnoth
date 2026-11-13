//! Focus overlay: one child owns the composer.
//!
//! Child transcript, `composer → <agent>` target chip, per-child draft and
//! scroll, `Esc` back to the exact parent state, `Tab`/`Shift+Tab` sibling
//! cycling, `Ctrl+↑` parent.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::tui::agents::ChildInfo;
use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// Focus: one child agent with its transcript and composer target chip.
#[derive(Debug)]
pub struct AgentFocus {
    visible: bool,
    child: Option<ChildInfo>,
    transcript: Vec<String>,
    scroll: usize,
    /// Per-child draft text, kept when the view switches siblings.
    drafts: HashMap<String, String>,
    /// Per-child scroll offset, kept beside the draft.
    scrolls: HashMap<String, usize>,
    /// The parent composer state as it was when Focus opened, so `Esc`
    /// returns to exactly what was there — input, cursor, and scroll.
    saved_parent: Option<(String, usize, usize)>,
}

impl AgentFocus {
    pub fn new() -> Self {
        Self {
            visible: false,
            child: None,
            transcript: Vec::new(),
            scroll: 0,
            drafts: HashMap::new(),
            scrolls: HashMap::new(),
            saved_parent: None,
        }
    }

    pub fn transcript(&self) -> &[String] {
        &self.transcript
    }

    pub fn with_child(mut self, child: ChildInfo) -> Self {
        self.child = Some(child);
        self
    }

    /// The draft for the child in view; empty when none was typed.
    fn draft(&self) -> String {
        self.child
            .as_ref()
            .and_then(|child| self.drafts.get(&child.agent_id).cloned())
            .unwrap_or_default()
    }

    /// Keep the current draft and scroll before the view moves.
    fn stash(&mut self) {
        if let Some(child) = &self.child {
            let draft = self.draft();
            self.drafts.insert(child.agent_id.clone(), draft);
            self.scrolls.insert(child.agent_id.clone(), self.scroll);
        }
    }

    /// Load one child: its roster row, its transcript from the JSONL, and the
    /// draft and scroll that child had before.
    fn load(&mut self, app: &mut App, id: &str) {
        self.transcript.clear();
        let rows = crate::tui::agents::roster(app);
        self.child = rows.into_iter().find(|child| child.agent_id == id);
        let stored = self.scrolls.get(id).copied();
        self.scroll = stored.unwrap_or(usize::MAX);
        let Some(delegator) = app.delegator.as_ref() else {
            self.transcript.push("no delegator".to_string());
            return;
        };
        match delegator.read_log(id) {
            Ok(events) => {
                for event in &events {
                    self.transcript
                        .push(format!("{:<14} {}", event.event, event.detail));
                }
            }
            Err(error) => self.transcript.push(format!("cannot read log: {error}")),
        }
        if self.transcript.is_empty() {
            self.transcript
                .push("no events yet — the run is pending, no executor has driven it".to_string());
        }
        if self.scroll >= self.transcript.len() {
            self.scroll = self.transcript.len().saturating_sub(1);
        }
    }

    /// Siblings: children with the same parent, in spawn order.
    fn siblings(&self, app: &App) -> Vec<ChildInfo> {
        let Some(current) = &self.child else {
            return Vec::new();
        };
        let parent = current.parent_id.as_deref();
        let mut rows: Vec<ChildInfo> = crate::tui::agents::roster(app)
            .into_iter()
            .filter(|row| row.parent_id.as_deref() == parent)
            .collect();
        rows.sort_by_key(|row| row.spawn_seq);
        rows
    }

    fn move_sibling(&mut self, app: &mut App, forward: bool) {
        let Some(current) = self.child.clone() else {
            return;
        };
        let siblings = self.siblings(app);
        if siblings.len() < 2 {
            return;
        }
        let Some(index) = siblings
            .iter()
            .position(|row| row.agent_id == current.agent_id)
        else {
            return;
        };
        let next = if forward {
            (index + 1) % siblings.len()
        } else if index == 0 {
            siblings.len() - 1
        } else {
            index - 1
        };
        self.stash();
        let id = siblings[next].agent_id.clone();
        self.load(app, &id);
    }

    /// Restore the parent composer exactly as it was when Focus opened.
    fn restore_parent(&self, app: &mut App) {
        if let Some((input, cursor, scroll)) = &self.saved_parent {
            app.input = input.clone();
            app.cursor = *cursor;
            app.scroll = *scroll;
        }
    }
}

impl Default for AgentFocus {
    fn default() -> Self {
        Self::new()
    }
}

impl Overlay for AgentFocus {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Focus
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
    }

    fn on_open(&mut self, app: &mut App) {
        if self.saved_parent.is_none() {
            self.saved_parent = Some((app.input.clone(), app.cursor, app.scroll));
        }

        let rows = crate::tui::agents::roster(app);
        let chosen = app
            .focus_agent
            .clone()
            .or_else(|| rows.first().map(|child| child.agent_id.clone()));
        let Some(id) = chosen else {
            self.transcript
                .push("no child runs — /delegate <agent> <task> spawns one".to_string());
            return;
        };
        self.load(app, &id);
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.restore_parent(app);
                OverlayAction::Close
            }
            KeyCode::Enter => {
                // Sending the draft steers the child: the note lands in its
                // log, and the draft clears for the next one.
                let draft = self.draft();
                let trimmed = draft.trim().to_string();
                if !trimmed.is_empty()
                    && let Some(child) = self.child.clone()
                    && let Some(delegator) = app.delegator.as_mut()
                {
                    match delegator.steer(&child.agent_id, &trimmed) {
                        Ok(()) => {
                            self.stash_draft_clear(app);
                        }
                        Err(error) => {
                            self.transcript.push(format!("steer failed: {error}"));
                        }
                    }
                }
                OverlayAction::None
            }
            KeyCode::Backspace => {
                let mut draft = self.draft();
                draft.pop();
                self.set_draft(draft);
                OverlayAction::None
            }
            KeyCode::Tab => {
                self.move_sibling(app, true);
                OverlayAction::None
            }
            KeyCode::BackTab => {
                self.move_sibling(app, false);
                OverlayAction::None
            }
            KeyCode::Up if ctrl => {
                // Parent: the run that spawned this one, or the root.
                if let Some(parent_id) = self
                    .child
                    .as_ref()
                    .and_then(|child| child.parent_id.clone())
                {
                    self.stash();
                    self.load(app, &parent_id);
                }
                OverlayAction::None
            }
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                OverlayAction::None
            }
            KeyCode::Down => {
                self.scroll += 1;
                OverlayAction::None
            }
            KeyCode::Char('x') if !ctrl => {
                // Stop this child only; parent and siblings keep running.
                if let Some(child) = self.child.clone()
                    && let Some(delegator) = app.delegator.as_mut()
                {
                    match delegator.stop_one(&child.agent_id) {
                        Ok(true) => {
                            self.transcript.push(format!(
                                "stopped {} — parent and siblings untouched",
                                child.agent_id
                            ));
                        }
                        Ok(false) => self
                            .transcript
                            .push(format!("{} is already stopped", child.agent_id)),
                        Err(error) => self.transcript.push(format!("stop failed: {error}")),
                    }
                    self.load(app, &child.agent_id);
                }
                OverlayAction::None
            }
            KeyCode::Char('r') if !ctrl => {
                if let Some(child) = self.child.clone()
                    && let Some(delegator) = app.delegator.as_mut()
                {
                    match delegator.revive(&child.agent_id) {
                        Ok(()) => self
                            .transcript
                            .push(format!("revived {} — log and history kept", child.agent_id)),
                        Err(error) => self.transcript.push(format!("revive failed: {error}")),
                    }
                    self.load(app, &child.agent_id);
                }
                OverlayAction::None
            }
            KeyCode::Char('i') if ctrl => {
                // Interrupt: the whole subtree stops, not one child.
                if let Some(child) = self.child.clone()
                    && let Some(delegator) = app.delegator.as_mut()
                {
                    match delegator.cancel(&child.agent_id) {
                        Ok(stopped) => self.transcript.push(format!(
                            "interrupted {} — {stopped} run(s) stopped",
                            child.agent_id
                        )),
                        Err(error) => self.transcript.push(format!("interrupt failed: {error}")),
                    }
                    self.load(app, &child.agent_id);
                }
                OverlayAction::None
            }
            KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                let mut draft = self.draft();
                draft.push(ch);
                self.set_draft(draft);
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        frame.render_widget(Clear, area);
        let block = Block::default().title("Focus").borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks: Vec<Rect> = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(inner)
            .to_vec();

        let mut lines = vec![self.title_line()];
        if self.transcript.is_empty() {
            lines.push(Line::from("no events yet"));
        } else {
            let start = self.scroll.min(self.transcript.len() - 1);
            for line in &self.transcript[start..] {
                lines.push(Line::from(line.clone()));
            }
        }
        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, chunks[0]);

        let composer = Paragraph::new(vec![
            Line::from(Span::styled(
                self.composer_chip(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(format!("draft: {}", self.draft())),
        ])
        .block(Block::default().title("composer").borders(Borders::ALL));
        frame.render_widget(composer, chunks[1]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("Esc/q", "return to exact parent state"),
            ("Tab / Shift+Tab", "sibling"),
            ("Ctrl+↑", "parent"),
            ("↑↓", "scroll"),
            ("type + Enter", "steer this child"),
            ("x", "stop"),
            ("r", "revive"),
            ("Ctrl+i", "interrupt subtree"),
        ]
    }
}

impl AgentFocus {
    fn title_line(&self) -> Line<'static> {
        match &self.child {
            Some(child) => Line::from(vec![
                Span::styled(
                    format!("{} — ", child.agent_id),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(child.task.clone()),
            ]),
            None => Line::from("Focus"),
        }
    }

    fn composer_chip(&self) -> String {
        match &self.child {
            Some(child) => format!("composer → {}", child.role),
            None => "composer → (no child)".to_string(),
        }
    }

    fn set_draft(&mut self, draft: String) {
        if let Some(child) = &self.child {
            self.drafts.insert(child.agent_id.clone(), draft);
        }
    }

    fn stash_draft_clear(&mut self, app: &mut App) {
        let id = self
            .child
            .as_ref()
            .map(|child| child.agent_id.clone())
            .unwrap_or_default();
        self.drafts.insert(id.clone(), String::new());
        self.transcript
            .push("steered — note recorded, draft cleared".to_string());
        self.load(app, &id);
    }
}
