//! Agent Deck overlay: `Alt+A`. Roster of child agents, inspector,
//! attention inbox, flat/tree toggle, costs, context, lineage.
//!
//! `/agents` stays the definition library and does not become the live
//! roster — the deck is the live roster.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::tui::agents::{AgentStatus, ChildInfo};
use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// Which list the cursor is on: the roster, or the attention inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Roster,
    Inbox,
}

/// The agent deck: one child per row, with status glyph and task.
#[derive(Debug)]
pub struct AgentDeck {
    visible: bool,
    children: Vec<ChildInfo>,
    selected: usize,
    list_state: ListState,
    /// Tree order (parents first, indented) versus spawn order.
    tree_view: bool,
    mode: Mode,
    inbox_selected: usize,
    /// Peaked detail (log tail or receipt) shown in the inspector.
    peeked: Option<String>,
}

impl AgentDeck {
    pub fn new() -> Self {
        Self {
            visible: false,
            children: Vec::new(),
            selected: 0,
            list_state: ListState::default(),
            tree_view: false,
            mode: Mode::Roster,
            inbox_selected: 0,
            peeked: None,
        }
    }

    fn refresh(&mut self, app: &App) {
        self.children = crate::tui::agents::roster(app);
        if !self.children.is_empty() && self.selected >= self.children.len() {
            self.selected = self.children.len() - 1;
        }
        self.list_state.select(Some(self.selected));
    }

    /// Roster rows in view order: tree view sorts parents before children so
    /// indentation reads as a tree; flat view keeps spawn order.
    fn ordered(&self) -> Vec<&ChildInfo> {
        let mut rows: Vec<&ChildInfo> = self.children.iter().collect();
        if self.tree_view {
            rows.sort_by(|a, b| {
                a.depth
                    .cmp(&b.depth)
                    .then_with(|| a.spawn_seq.cmp(&b.spawn_seq))
            });
        }
        rows
    }

    fn selected_child(&self) -> Option<&ChildInfo> {
        self.ordered().into_iter().nth(self.selected)
    }

    fn children_lines(&self) -> Vec<ListItem<'static>> {
        self.ordered()
            .iter()
            .enumerate()
            .map(|(i, child)| {
                let glyph = child.status.glyph();
                let style = if i == self.selected && self.mode == Mode::Roster {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                let status_color = match child.status {
                    AgentStatus::Running => Color::Cyan,
                    AgentStatus::Done => Color::Green,
                    AgentStatus::Failed => Color::Red,
                    AgentStatus::Waiting => Color::Yellow,
                    AgentStatus::Killed => Color::DarkGray,
                    AgentStatus::Parked => Color::DarkGray,
                    AgentStatus::Starting => Color::DarkGray,
                };
                let indent = if self.tree_view {
                    "  ".repeat(child.depth)
                } else {
                    String::new()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {} ", glyph), Style::default().fg(status_color)),
                    Span::styled(
                        format!(" {}{}  {}", indent, child.agent_id, child.task),
                        style,
                    ),
                ]))
            })
            .collect()
    }

    /// Inspector lines for the selected child: status, task, lineage, model,
    /// worktree, cost, and any peeked detail (log tail or receipt).
    fn inspector_lines(&self, app: &App) -> Vec<String> {
        let Some(child) = self.selected_child() else {
            return vec!["nothing selected".to_string()];
        };
        let mut lines = vec![
            format!("{} — {}", child.agent_id, child.task),
            format!(
                "status {} · depth {} · session {}",
                child.status.glyph(),
                child.depth,
                child.session_id.as_deref().unwrap_or("-")
            ),
            format!("lineage: {}", self.lineage(child)),
        ];
        if let Some(model) = &child.model {
            lines.push(format!("model: {model}"));
        }
        if let Some(worktree) = &child.worktree {
            lines.push(format!("worktree: {worktree}"));
        }
        if let Some(cost) = self.cost_of(app, child) {
            lines.push(format!("cost: {cost}"));
        }
        if let Some(peeked) = &self.peeked {
            lines.push("---".to_string());
            lines.extend(peeked.lines().map(str::to_string));
        }
        lines
    }

    /// The root-to-child path, so a high fan-out reads as lineage rather than
    /// as a flat list of strangers.
    fn lineage(&self, child: &ChildInfo) -> String {
        let mut names = vec![child.role.clone()];
        let mut current = child.parent_id.as_deref();
        while let Some(parent) = current {
            match self.children.iter().find(|c| c.agent_id == parent) {
                Some(parent_child) => {
                    names.push(parent_child.role.clone());
                    current = parent_child.parent_id.as_deref();
                }
                None => break,
            }
        }
        names.reverse();
        names.join(" → ")
    }

    /// Cost from the child's `run.done` receipt, when it has one.
    fn cost_of(&self, app: &App, child: &ChildInfo) -> Option<String> {
        let delegator = app.delegator.as_ref()?;
        let events = delegator.read_log(&child.agent_id).ok()?;
        let done = events.iter().find(|e| e.event == "run.done")?;
        let value: serde_json::Value = serde_json::from_str(&done.detail).ok()?;
        let tokens = value.get("cost")?.get("tokens")?.as_u64()?;
        let usd = value.get("cost")?.get("usd")?.as_f64()?;
        Some(format!("{tokens} tokens · ${usd:.4}"))
    }

    /// Receipt text (the `run.done` payload) for the peek, or a line saying
    /// there is none yet.
    fn receipt_of(&self, app: &App, child: &ChildInfo) -> String {
        let Some(delegator) = app.delegator.as_ref() else {
            return "no delegator".to_string();
        };
        match delegator.read_log(&child.agent_id) {
            Ok(events) => match events.iter().find(|e| e.event == "run.done") {
                Some(done) => match serde_json::from_str::<serde_json::Value>(&done.detail) {
                    Ok(value) => {
                        serde_json::to_string_pretty(&value).unwrap_or_else(|_| done.detail.clone())
                    }
                    Err(_) => done.detail.clone(),
                },
                None => "no receipt yet — the run has not finished".to_string(),
            },
            Err(error) => format!("cannot read log: {error}"),
        }
    }

    /// Log tail for the peek: the last five events.
    fn log_tail_of(&self, app: &App, child: &ChildInfo) -> String {
        let Some(delegator) = app.delegator.as_ref() else {
            return "no delegator".to_string();
        };
        match delegator.read_log(&child.agent_id) {
            Ok(events) if !events.is_empty() => {
                let start = events.len().saturating_sub(5);
                events[start..]
                    .iter()
                    .map(|e| format!("{:<16} {}", e.event, e.detail))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            Ok(_) => "no events yet — the run is pending".to_string(),
            Err(error) => format!("cannot read log: {error}"),
        }
    }

    fn inbox_lines(&self, app: &App) -> Vec<String> {
        let Some(delegator) = app.delegator.as_ref() else {
            return Vec::new();
        };
        delegator
            .attention()
            .iter()
            .enumerate()
            .map(|(i, request)| {
                let cursor = if self.mode == Mode::Inbox && i == self.inbox_selected {
                    "▸ "
                } else {
                    "  "
                };
                format!(
                    "{cursor}{} · {} · {}",
                    request.kind, request.agent, request.summary
                )
            })
            .collect()
    }
}

impl Default for AgentDeck {
    fn default() -> Self {
        Self::new()
    }
}

impl Overlay for AgentDeck {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Deck
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.list_state.select(Some(0));
    }

    fn on_open(&mut self, app: &mut App) {
        self.refresh(app);
        // The deck opens on the newest run: Alt+A is how you inspect the
        // delegation you just made, not the session itself. With rows in spawn
        // order the newest is the last row in flat view, and selecting by
        // spawn_seq covers the tree view too, where depth decides position.
        let newest = self.children.iter().map(|child| child.spawn_seq).max();
        if let Some(newest) = newest
            && let Some(index) = self
                .ordered()
                .iter()
                .position(|child| child.spawn_seq == newest)
        {
            self.selected = index;
        }
        self.list_state.select(Some(self.selected));
    }

    fn on_tick(&mut self, app: &mut App) {
        // A run that finishes while the deck is open lands without a keypress.
        self.refresh(app);
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        match key.code {
            KeyCode::Esc => OverlayAction::Close,
            KeyCode::Char('q') => OverlayAction::Close,
            KeyCode::Tab => {
                let inbox_len = app
                    .delegator
                    .as_ref()
                    .map(|d| d.attention().len())
                    .unwrap_or(0);
                self.mode = match self.mode {
                    Mode::Roster if inbox_len > 0 => Mode::Inbox,
                    _ => Mode::Roster,
                };
                OverlayAction::None
            }
            KeyCode::Up => {
                match self.mode {
                    Mode::Roster => {
                        if !self.children.is_empty() {
                            self.selected = self.selected.saturating_sub(1);
                            self.list_state.select(Some(self.selected));
                        }
                    }
                    Mode::Inbox => {
                        self.inbox_selected = self.inbox_selected.saturating_sub(1);
                    }
                }
                OverlayAction::None
            }
            KeyCode::Down => {
                match self.mode {
                    Mode::Roster => {
                        if !self.children.is_empty() {
                            self.selected = (self.selected + 1).min(self.children.len() - 1);
                            self.list_state.select(Some(self.selected));
                        }
                    }
                    Mode::Inbox => {
                        let len = app
                            .delegator
                            .as_ref()
                            .map(|d| d.attention().len())
                            .unwrap_or(0);
                        if len > 0 {
                            self.inbox_selected = (self.inbox_selected + 1).min(len - 1);
                        }
                    }
                }
                OverlayAction::None
            }
            KeyCode::Enter => {
                if self.mode == Mode::Inbox {
                    // Answering a request removes it from the inbox and opens
                    // the child at the prompt that answers it (W13-23).
                    if let Some(delegator) = app.delegator.as_mut()
                        && let Some(id) = delegator
                            .attention()
                            .get(self.inbox_selected)
                            .map(|r| r.id.clone())
                        && let Some(taken) = delegator.take_attention(&id)
                    {
                        app.focus_agent = Some(taken.run_id.clone());
                        app.push(
                            crate::tui::app::EntryRole::Info,
                            format!(
                                "attention {} answered at {} — focus opened",
                                taken.id, taken.run_id
                            ),
                        );
                        return OverlayAction::Push(OverlayKind::Focus);
                    }
                    return OverlayAction::None;
                }
                // Focus is handed the run the cursor is on, so the transcript it
                // reads is the child the person was looking at.
                if let Some(child) = self.selected_child() {
                    app.focus_agent = Some(child.agent_id.clone());
                }
                OverlayAction::Push(OverlayKind::Focus)
            }
            KeyCode::Char(' ') => {
                if let Some(child) = self.selected_child().cloned() {
                    self.peeked = Some(self.log_tail_of(app, &child));
                }
                OverlayAction::None
            }
            KeyCode::Char('p') => {
                if let Some(child) = self.selected_child().cloned() {
                    self.peeked = Some(self.receipt_of(app, &child));
                }
                OverlayAction::None
            }
            KeyCode::Char('m') => {
                // Merge is a command: say where the work lives and what a merge
                // needs, in the transcript, rather than pretending it happened.
                if let Some(child) = self.selected_child().cloned() {
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        match &child.worktree {
                            Some(worktree) => format!(
                                "merge {} — worktree: {worktree}\n  git -C {} diff HEAD --stat, then git merge, then /vault conflicts",
                                child.agent_id, worktree
                            ),
                            None => format!(
                                "merge {} — no worktree; the child ran read-only, nothing to merge",
                                child.agent_id
                            ),
                        },
                    );
                }
                OverlayAction::None
            }
            KeyCode::Char('t') => {
                self.tree_view = !self.tree_view;
                OverlayAction::None
            }
            KeyCode::Char('s') => {
                // Steer: the note lands in the child's log, where the trace
                // shows it and the child's next step reads it.
                if let Some(child) = self.selected_child().cloned() {
                    let id = child.agent_id.clone();
                    if let Some(delegator) = app.delegator.as_mut() {
                        match delegator.steer(&id, "steered from the deck") {
                            Ok(()) => app.push(
                                crate::tui::app::EntryRole::Info,
                                format!("steered {id}: note recorded in its log"),
                            ),
                            Err(error) => app.push(
                                crate::tui::app::EntryRole::Error,
                                format!("steer {id}: {error}"),
                            ),
                        }
                    }
                }
                OverlayAction::None
            }
            KeyCode::Char('x') => {
                // Stop one child; the parent and siblings keep running.
                if let Some(child) = self.selected_child().cloned() {
                    let id = child.agent_id.clone();
                    if let Some(delegator) = app.delegator.as_mut() {
                        match delegator.stop_one(&id) {
                            Ok(true) => app.push(
                                crate::tui::app::EntryRole::Info,
                                format!("stopped {id} — parent and siblings untouched"),
                            ),
                            Ok(false) => app.push(
                                crate::tui::app::EntryRole::Info,
                                format!("{id} is already stopped"),
                            ),
                            Err(error) => app.push(
                                crate::tui::app::EntryRole::Error,
                                format!("stop {id}: {error}"),
                            ),
                        }
                    }
                    self.refresh(app);
                }
                OverlayAction::None
            }
            KeyCode::Char('r') => {
                if let Some(child) = self.selected_child().cloned() {
                    let id = child.agent_id.clone();
                    if let Some(delegator) = app.delegator.as_mut() {
                        match delegator.revive(&id) {
                            Ok(()) => app.push(
                                crate::tui::app::EntryRole::Info,
                                format!("revived {id} — log and history kept"),
                            ),
                            Err(error) => app.push(
                                crate::tui::app::EntryRole::Error,
                                format!("revive {id}: {error}"),
                            ),
                        }
                    }
                    self.refresh(app);
                }
                OverlayAction::None
            }
            KeyCode::Char('i') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Interrupt: the whole subtree stops, not one child.
                if let Some(child) = self.selected_child().cloned() {
                    let id = child.agent_id.clone();
                    if let Some(delegator) = app.delegator.as_mut() {
                        match delegator.cancel(&id) {
                            Ok(stopped) => app.push(
                                crate::tui::app::EntryRole::Info,
                                format!("interrupted {id} — {stopped} run(s) stopped"),
                            ),
                            Err(error) => app.push(
                                crate::tui::app::EntryRole::Error,
                                format!("interrupt {id}: {error}"),
                            ),
                        }
                    }
                    self.refresh(app);
                }
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, app: &App) {
        frame.render_widget(Clear, area);

        let inbox_len = app
            .delegator
            .as_ref()
            .map(|d| d.attention().len())
            .unwrap_or(0);
        let title = if inbox_len > 0 {
            format!("Agent Deck — {} waiting", inbox_len)
        } else {
            "Agent Deck".to_string()
        };
        let block = Block::default().title(title).borders(Borders::ALL);
        let inner = if self.children.is_empty() && inbox_len == 0 {
            frame.render_widget(
                Paragraph::new("no child runs — /delegate <agent> <task> spawns one").block(block),
                area,
            );
            return;
        } else {
            block.inner(area)
        };
        frame.render_widget(block, area);

        let mut constraints = vec![Constraint::Min(3)];
        let mut sections = vec!["roster"];
        if inbox_len > 0 {
            constraints.push(Constraint::Length(3 + inbox_len as u16));
            sections.push("inbox");
        }
        constraints.push(Constraint::Min(5));
        sections.push("inspector");
        let chunks: Vec<Rect> = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(inner)
            .to_vec();
        let _ = sections;
        let (roster_area, inbox_area, inspector_area) = if inbox_len > 0 {
            (chunks[0], Some(chunks[1]), chunks[2])
        } else {
            (chunks[0], None, chunks[1])
        };

        let items = self.children_lines();
        let list = List::new(items).block(Block::default().title("roster").borders(Borders::ALL));
        frame.render_stateful_widget(list, roster_area, &mut self.list_state);

        if let Some(area) = inbox_area {
            let lines = self.inbox_lines(app);
            let paragraph = Paragraph::new(lines.join("\n"))
                .block(Block::default().title("attention").borders(Borders::ALL));
            frame.render_widget(paragraph, area);
        }

        let inspector = self.inspector_lines(app);
        let paragraph = Paragraph::new(inspector.join("\n"))
            .block(Block::default().title("inspector").borders(Borders::ALL));
        frame.render_widget(paragraph, inspector_area);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑↓", "navigate"),
            ("Enter", "focus child / answer request"),
            ("Tab", "roster ⇄ attention inbox"),
            ("Space", "peek (log tail)"),
            ("m", "merge"),
            ("p", "receipt"),
            ("t", "flat/tree"),
            ("s", "steer"),
            ("x", "stop one child"),
            ("r", "revive"),
            ("Ctrl+i", "interrupt subtree"),
            ("Esc/q", "close"),
        ]
    }
}
