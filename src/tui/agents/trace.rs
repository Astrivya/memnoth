//! Trace overlay: `Alt+T`. Nested agent tree, active-path bullets, labels,
//! filters, phase groups, and a timeline of tool calls and model time.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::tui::agents::{AgentStatus, ChildInfo};
use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// Timeline filter: all events, tool calls, or system events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Tool,
    System,
}

impl Filter {
    fn next(self) -> Self {
        match self {
            Filter::All => Filter::Tool,
            Filter::Tool => Filter::System,
            Filter::System => Filter::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Tool => "tool",
            Filter::System => "system",
        }
    }

    fn matches(self, event: &str) -> bool {
        match self {
            Filter::All => true,
            Filter::Tool => event.starts_with("tool."),
            Filter::System => {
                event.starts_with("run.") || event.starts_with("attention.") || event == "steer"
            }
        }
    }
}

/// Trace: nested agent tree with active path and timeline.
#[derive(Debug)]
pub struct AgentTrace {
    visible: bool,
    children: Vec<ChildInfo>,
    selected: usize,
    list_state: ListState,
    filter: Filter,
    /// Cached timeline lines for the selected child, rebuilt on open/tick.
    timeline: Vec<String>,
}

impl AgentTrace {
    pub fn new() -> Self {
        Self {
            visible: false,
            children: Vec::new(),
            selected: 0,
            list_state: ListState::default(),
            filter: Filter::All,
            timeline: Vec::new(),
        }
    }

    fn refresh(&mut self, app: &App) {
        self.children = crate::tui::agents::roster(app);
        if !self.children.is_empty() && self.selected >= self.children.len() {
            self.selected = self.children.len() - 1;
        }
        self.list_state.select(Some(self.selected));
        self.rebuild_timeline(app);
    }

    /// Tree order: parents before children, indented by depth.
    fn ordered(&self) -> Vec<&ChildInfo> {
        let mut rows: Vec<&ChildInfo> = self.children.iter().collect();
        rows.sort_by(|a, b| {
            a.depth
                .cmp(&b.depth)
                .then_with(|| a.spawn_seq.cmp(&b.spawn_seq))
        });
        rows
    }

    /// The active path: the selected child and every ancestor, so the
    /// bullets show the chain the person is looking at.
    fn active_path(&self) -> std::collections::HashSet<String> {
        let mut active = std::collections::HashSet::new();
        let Some(selected) = self.ordered().into_iter().nth(self.selected) else {
            return active;
        };
        active.insert(selected.agent_id.clone());
        let mut current = selected.parent_id.as_deref();
        while let Some(parent) = current {
            active.insert(parent.to_string());
            current = self
                .children
                .iter()
                .find(|c| c.agent_id == parent)
                .and_then(|c| c.parent_id.as_deref());
        }
        active
    }

    fn rebuild_timeline(&mut self, app: &App) {
        self.timeline.clear();
        let Some(selected) = self.ordered().into_iter().nth(self.selected).cloned() else {
            return;
        };
        let Some(delegator) = app.delegator.as_ref() else {
            return;
        };
        let Ok(events) = delegator.read_log(&selected.agent_id) else {
            return;
        };
        let start = events.first().map(|e| e.at_ms).unwrap_or(0);
        let mut last_phase = String::new();
        for event in &events {
            if !self.filter.matches(&event.event) {
                continue;
            }
            // Phase groups: consecutive events under one phase share a header.
            let phase = event
                .event
                .split('.')
                .next()
                .unwrap_or(&event.event)
                .to_string();
            if phase != last_phase {
                self.timeline.push(format!("── {} ──", phase));
                last_phase = phase;
            }
            let offset = (event.at_ms - start).max(0);
            self.timeline.push(format!(
                "  +{offset:>6}ms  {:<16} {}",
                event.event, event.detail
            ));
        }
        if self.timeline.is_empty() {
            self.timeline
                .push("no events match this filter".to_string());
        }
    }

    fn rows(&self) -> Vec<ListItem<'static>> {
        let active = self.active_path();
        self.ordered()
            .iter()
            .enumerate()
            .map(|(i, child)| {
                let glyph = child.status.glyph();
                let style = if i == self.selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                let status_color = match child.status {
                    AgentStatus::Running => Color::Cyan,
                    AgentStatus::Done => Color::Green,
                    AgentStatus::Failed => Color::Red,
                    AgentStatus::Waiting => Color::Yellow,
                    _ => Color::DarkGray,
                };
                let bullet = if active.contains(&child.agent_id) {
                    "● "
                } else {
                    "○ "
                };
                let indent = "  ".repeat(child.depth);
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{}{}", indent, bullet)),
                    Span::styled(format!(" {} ", glyph), Style::default().fg(status_color)),
                    Span::styled(format!("{} — {}", child.role, child.task), style),
                ]))
            })
            .collect()
    }
}

impl Default for AgentTrace {
    fn default() -> Self {
        Self::new()
    }
}

impl Overlay for AgentTrace {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Trace
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
        self.selected = 0;
        self.list_state.select(Some(0));
    }

    fn on_tick(&mut self, app: &mut App) {
        // A run that advances while the trace is open lands without a keypress.
        self.refresh(app);
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        match key.code {
            KeyCode::Esc => OverlayAction::Close,
            KeyCode::Char('q') => OverlayAction::Close,
            KeyCode::Up => {
                if !self.children.is_empty() {
                    self.selected = self.selected.saturating_sub(1);
                    self.list_state.select(Some(self.selected));
                    self.rebuild_timeline(app);
                }
                OverlayAction::None
            }
            KeyCode::Down => {
                if !self.children.is_empty() {
                    self.selected = (self.selected + 1).min(self.children.len() - 1);
                    self.list_state.select(Some(self.selected));
                    self.rebuild_timeline(app);
                }
                OverlayAction::None
            }
            KeyCode::Char('f') => {
                self.filter = self.filter.next();
                self.rebuild_timeline(app);
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        frame.render_widget(Clear, area);

        if self.children.is_empty() {
            let block = Block::default().title("Agent Trace").borders(Borders::ALL);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(
                Paragraph::new("no child runs — /delegate <agent> <task> spawns one"),
                inner,
            );
            return;
        }

        let block = Block::default()
            .title(format!("Agent Trace — filter: {}", self.filter.label()))
            .borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks: Vec<Rect> = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Min(3)])
            .split(inner)
            .to_vec();

        let list =
            List::new(self.rows()).block(Block::default().title("tree").borders(Borders::ALL));
        frame.render_stateful_widget(list, chunks[0], &mut self.list_state);

        let timeline = Paragraph::new(self.timeline.join("\n"))
            .block(Block::default().title("timeline").borders(Borders::ALL));
        frame.render_widget(timeline, chunks[1]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑↓", "navigate tree"),
            ("f", "filter timeline (all/tool/system)"),
            ("Esc/q", "close"),
        ]
    }
}
