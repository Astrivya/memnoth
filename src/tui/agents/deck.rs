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

fn truncate_row(text: &str, width: usize) -> String {
    let mut chars = text.chars();
    let value: String = chars.by_ref().take(width).collect();
    if chars.next().is_some() && width > 0 {
        format!(
            "{}…",
            value
                .chars()
                .take(width.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        value
    }
}

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
    /// Owned indexes into `children` in the current flat/tree order.
    order: Vec<usize>,
    /// Filtered positions into `order`; navigation and render do not rebuild
    /// or allocate a complete roster on every frame.
    filtered_order: Vec<usize>,
    selected: usize,
    /// Tree order (parents first, indented) versus spawn order.
    tree_view: bool,
    /// Case-insensitive filter over role, task, run ID and model.
    query: String,
    /// The `/` filter field currently owns keyboard input.
    searching: bool,
    /// Case-insensitive filter over the source-backed attention request fields.
    inbox_query: String,
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
            order: Vec::new(),
            filtered_order: Vec::new(),
            selected: 0,
            tree_view: false,
            query: String::new(),
            searching: false,
            inbox_query: String::new(),
            mode: Mode::Roster,
            inbox_selected: 0,
            peeked: None,
        }
    }

    fn refresh(&mut self, app: &App) {
        let selected_id = self.selected_child().map(|child| child.agent_id.clone());
        self.children = crate::tui::agents::roster(app);
        self.rebuild_order();
        self.rebuild_filter();
        let selected_index = selected_id
            .as_ref()
            .and_then(|selected_id| self.visible_position(|child| child.agent_id == *selected_id));
        if let Some(index) = selected_index {
            self.selected = index;
        } else {
            self.selected = self
                .selected
                .min(self.filtered_order.len().saturating_sub(1));
        }
    }

    /// Roster rows in view order: tree view walks actual parent links; flat
    /// view keeps spawn order.
    #[cfg(test)]
    fn ordered(&self) -> Vec<&ChildInfo> {
        self.order
            .iter()
            .filter_map(|index| self.children.get(*index))
            .collect()
    }

    fn rebuild_order(&mut self) {
        if !self.tree_view {
            self.order = (0..self.children.len()).collect();
            return;
        }

        fn visit(
            root: usize,
            children: &std::collections::HashMap<Option<usize>, Vec<usize>>,
            seen: &mut std::collections::HashSet<usize>,
            ordered: &mut Vec<usize>,
        ) {
            let mut pending = vec![root];
            while let Some(node) = pending.pop() {
                if !seen.insert(node) {
                    continue;
                }
                ordered.push(node);
                if let Some(descendants) = children.get(&Some(node)) {
                    pending.extend(descendants.iter().rev().copied());
                }
            }
        }

        let ids: std::collections::HashMap<&str, usize> = self
            .children
            .iter()
            .enumerate()
            .map(|(index, child)| (child.agent_id.as_str(), index))
            .collect();
        let mut children: std::collections::HashMap<Option<usize>, Vec<usize>> =
            std::collections::HashMap::new();
        for (index, child) in self.children.iter().enumerate() {
            let parent = child
                .parent_id
                .as_deref()
                .and_then(|parent_id| ids.get(parent_id).copied());
            children.entry(parent).or_default().push(index);
        }
        for siblings in children.values_mut() {
            siblings.sort_by_key(|index| self.children[*index].spawn_seq);
        }

        let mut ordered = Vec::with_capacity(self.children.len());
        let mut seen = std::collections::HashSet::with_capacity(self.children.len());
        if let Some(roots) = children.get(&None) {
            for root in roots {
                visit(*root, &children, &mut seen, &mut ordered);
            }
        }
        // Corrupt/cyclic lineage should not hide runs. Display any unreachable
        // nodes as deterministic roots after the valid trees.
        let mut orphans: Vec<usize> = self
            .children
            .iter()
            .enumerate()
            .filter_map(|(index, _)| (!seen.contains(&index)).then_some(index))
            .collect();
        orphans.sort_by_key(|index| self.children[*index].spawn_seq);
        for orphan in orphans {
            visit(orphan, &children, &mut seen, &mut ordered);
        }
        self.order = ordered;
    }

    fn rebuild_filter(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.filtered_order = self
            .order
            .iter()
            .enumerate()
            .filter_map(|(position, index)| {
                let child = self.children.get(*index)?;
                (query.is_empty()
                    || child.role.to_lowercase().contains(&query)
                    || child.task.to_lowercase().contains(&query)
                    || child.agent_id.to_lowercase().contains(&query)
                    || child
                        .model
                        .as_deref()
                        .is_some_and(|model| model.to_lowercase().contains(&query)))
                .then_some(position)
            })
            .collect();
    }

    fn child_at_visible(&self, visible_index: usize) -> Option<&ChildInfo> {
        let order_position = *self.filtered_order.get(visible_index)?;
        let child_index = *self.order.get(order_position)?;
        self.children.get(child_index)
    }

    fn visible_position(&self, predicate: impl Fn(&ChildInfo) -> bool) -> Option<usize> {
        self.filtered_order
            .iter()
            .enumerate()
            .find_map(|(visible_index, _)| {
                self.child_at_visible(visible_index)
                    .filter(|child| predicate(child))
                    .map(|_| visible_index)
            })
    }

    fn visible_window(
        &self,
        window: std::ops::Range<usize>,
    ) -> impl Iterator<Item = (usize, &ChildInfo)> {
        let start = window.start.min(self.filtered_order.len());
        let end = window.end.min(self.filtered_order.len()).max(start);
        self.filtered_order[start..end]
            .iter()
            .enumerate()
            .filter_map(move |(offset, _)| {
                let visible_index = start + offset;
                self.child_at_visible(visible_index)
                    .map(|child| (visible_index, child))
            })
    }

    #[cfg(test)]
    fn visible(&self) -> Vec<&ChildInfo> {
        self.visible_window(0..self.filtered_order.len())
            .map(|(_, child)| child)
            .collect()
    }

    fn selected_child(&self) -> Option<&ChildInfo> {
        self.child_at_visible(self.selected)
    }

    /// Bound rendered roster rows to the available terminal height. The full
    /// selection index stays stable while only a centered viewport is built.
    fn row_window(total: usize, selected: usize, viewport: usize) -> std::ops::Range<usize> {
        let count = total.min(viewport);
        if count == 0 {
            return 0..0;
        }
        let start = selected
            .min(total.saturating_sub(1))
            .saturating_sub(count / 2)
            .min(total - count);
        start..start + count
    }

    /// Attention requests in their original inbox order, filtered by fields
    /// supplied by the delegator. Keep indices so Enter addresses the original
    /// request rather than a copied or synthesized row.
    fn visible_attention_indices(&self, app: &App) -> Vec<usize> {
        let query = self.inbox_query.trim().to_lowercase();
        app.delegator
            .as_ref()
            .map(|delegator| {
                delegator
                    .attention()
                    .iter()
                    .enumerate()
                    .filter_map(|(index, request)| {
                        (query.is_empty()
                            || request.agent.to_lowercase().contains(&query)
                            || request.kind.to_lowercase().contains(&query)
                            || request.summary.to_lowercase().contains(&query)
                            || request.run_id.to_lowercase().contains(&query)
                            || request.id.to_lowercase().contains(&query))
                        .then_some(index)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Inspector lines for the selected child: status, task, lineage, model,
    /// worktree, cost, recent event names, and any explicitly peeked detail.
    fn inspector_lines(&self, app: &App) -> Vec<String> {
        let Some(child) = self.selected_child() else {
            return vec!["nothing selected".to_string()];
        };
        let events = app
            .delegator
            .as_ref()
            .map(|delegator| delegator.read_log(&child.agent_id));
        let mut lines = vec![
            child.role.clone(),
            child.task.clone(),
            format!("state: {} {}", child.status.glyph(), child.status.label()),
            format!("depth: {}", child.depth),
            format!("lineage: {}", self.lineage(child)),
        ];
        if let Some(error) = app
            .delegator
            .as_ref()
            .and_then(|delegator| delegator.registry().get(&child.agent_id))
            .and_then(|entry| entry.error.as_deref())
        {
            lines.push(format!("failure: {error}"));
        }
        if child.status == AgentStatus::Orphaned {
            lines.push("process disappeared; revive to retry this run".to_string());
        }
        if let Some(model) = &child.model {
            lines.push(format!("model: {model}"));
        }
        if let Some(worktree) = &child.worktree {
            lines.push(format!("worktree: {worktree}"));
        }
        if let Some(cost) = events
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|events| Self::cost_from_events(events))
        {
            lines.push(format!("cost: {cost}"));
        }
        if let Some(peeked) = &self.peeked {
            lines.push("---".to_string());
            lines.extend(peeked.lines().map(str::to_string));
        } else {
            lines.push(String::new());
            lines.push("recent activity".to_string());
            lines.extend(match events.as_ref() {
                None => vec!["no activity yet".to_string()],
                Some(Err(_)) => vec!["activity unavailable".to_string()],
                Some(Ok(events)) => Self::activity_lines(events),
            });
        }
        lines
    }

    /// Show recent event kinds without their payloads. Payloads can contain
    /// user text, paths, or tool output; Space remains the explicit opt-in for
    /// the existing detailed log peek.
    fn activity_lines(events: &[memnoth_agent::ChildEvent]) -> Vec<String> {
        if events.is_empty() {
            return vec!["waiting for the first event".to_string()];
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or_default();
        events
            .iter()
            .rev()
            .take(5)
            .map(|event| {
                let age_seconds = now_ms.saturating_sub(event.at_ms).max(0) / 1_000;
                let age = if age_seconds < 60 {
                    format!("{age_seconds}s ago")
                } else if age_seconds < 3_600 {
                    format!("{}m ago", age_seconds / 60)
                } else {
                    format!("{}h ago", age_seconds / 3_600)
                };
                format!("{}  ·  {age}", event.event)
            })
            .collect()
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
    fn cost_from_events(events: &[memnoth_agent::ChildEvent]) -> Option<String> {
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
    }

    fn on_open(&mut self, app: &mut App) {
        self.refresh(app);
        // The deck opens on the newest run: Alt+A is how you inspect the
        // delegation you just made, not the session itself. With rows in spawn
        // order the newest is the last row in flat view, and selecting by
        // spawn_seq covers the tree view too, where depth decides position.
        let newest = self.children.iter().map(|child| child.spawn_seq).max();
        if let Some(newest) = newest
            && let Some(index) = self.visible_position(|child| child.spawn_seq == newest)
        {
            self.selected = index;
        }
    }

    fn on_tick(&mut self, app: &mut App) {
        // A run that finishes while the deck is open lands without a keypress.
        let selected_request_id = self
            .visible_attention_indices(app)
            .get(self.inbox_selected)
            .and_then(|index| {
                app.delegator
                    .as_ref()?
                    .attention()
                    .get(*index)
                    .map(|request| request.id.clone())
            });
        self.refresh(app);
        let visible = self.visible_attention_indices(app);
        self.inbox_selected = selected_request_id
            .and_then(|id| {
                visible.iter().position(|index| {
                    app.delegator
                        .as_ref()
                        .and_then(|delegator| delegator.attention().get(*index))
                        .is_some_and(|request| request.id == id)
                })
            })
            .unwrap_or_else(|| self.inbox_selected.min(visible.len().saturating_sub(1)));
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if self.searching {
            match key.code {
                KeyCode::Esc => {
                    self.searching = false;
                    match self.mode {
                        Mode::Roster => {
                            self.query.clear();
                            self.selected = 0;
                        }
                        Mode::Inbox => {
                            self.inbox_query.clear();
                            self.inbox_selected = 0;
                        }
                    }
                }
                KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => match self.mode {
                    Mode::Roster => {
                        self.query.pop();
                        self.selected = 0;
                    }
                    Mode::Inbox => {
                        self.inbox_query.pop();
                        self.inbox_selected = 0;
                    }
                },
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    match self.mode {
                        Mode::Roster => {
                            self.query.clear();
                            self.selected = 0;
                        }
                        Mode::Inbox => {
                            self.inbox_query.clear();
                            self.inbox_selected = 0;
                        }
                    }
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    match self.mode {
                        Mode::Roster => {
                            self.query.push(ch);
                            self.selected = 0;
                        }
                        Mode::Inbox => {
                            self.inbox_query.push(ch);
                            self.inbox_selected = 0;
                        }
                    }
                }
                _ => {}
            }
            if self.mode == Mode::Roster {
                self.rebuild_filter();
                self.selected = self
                    .selected
                    .min(self.filtered_order.len().saturating_sub(1));
            }
            return OverlayAction::None;
        }
        match key.code {
            KeyCode::Esc
                if match self.mode {
                    Mode::Roster => !self.query.is_empty(),
                    Mode::Inbox => !self.inbox_query.is_empty(),
                } =>
            {
                match self.mode {
                    Mode::Roster => {
                        self.query.clear();
                        self.rebuild_filter();
                        self.selected = 0;
                    }
                    Mode::Inbox => {
                        self.inbox_query.clear();
                        self.inbox_selected = 0;
                    }
                }
                OverlayAction::None
            }
            KeyCode::Esc => OverlayAction::Close,
            KeyCode::Char('q') => OverlayAction::Close,
            KeyCode::Char('/') => {
                self.searching = true;
                OverlayAction::None
            }
            KeyCode::Tab => {
                self.peeked = None;
                self.mode = match self.mode {
                    Mode::Roster => Mode::Inbox,
                    Mode::Inbox => Mode::Roster,
                };
                OverlayAction::None
            }
            KeyCode::Up => {
                match self.mode {
                    Mode::Roster => {
                        let len = self.filtered_order.len();
                        if len > 0 {
                            self.selected = self.selected.saturating_sub(1);
                            self.peeked = None;
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
                        let len = self.filtered_order.len();
                        if len > 0 {
                            self.selected = (self.selected + 1).min(len - 1);
                            self.peeked = None;
                        }
                    }
                    Mode::Inbox => {
                        let len = self.visible_attention_indices(app).len();
                        if len > 0 {
                            self.inbox_selected = (self.inbox_selected + 1).min(len - 1);
                        }
                    }
                }
                OverlayAction::None
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                let page = 8usize;
                let forward = key.code == KeyCode::PageDown;
                match self.mode {
                    Mode::Roster => {
                        let len = self.filtered_order.len();
                        if forward {
                            self.selected = self
                                .selected
                                .saturating_add(page)
                                .min(len.saturating_sub(1));
                        } else {
                            self.selected = self.selected.saturating_sub(page);
                        }
                        self.peeked = None;
                    }
                    Mode::Inbox => {
                        let len = self.visible_attention_indices(app).len();
                        if forward {
                            self.inbox_selected = self
                                .inbox_selected
                                .saturating_add(page)
                                .min(len.saturating_sub(1));
                        } else {
                            self.inbox_selected = self.inbox_selected.saturating_sub(page);
                        }
                    }
                }
                OverlayAction::None
            }
            KeyCode::Home | KeyCode::End => {
                let end = key.code == KeyCode::End;
                match self.mode {
                    Mode::Roster => {
                        self.selected = if end {
                            self.filtered_order.len().saturating_sub(1)
                        } else {
                            0
                        };
                        self.peeked = None;
                    }
                    Mode::Inbox => {
                        self.inbox_selected = if end {
                            self.visible_attention_indices(app).len().saturating_sub(1)
                        } else {
                            0
                        };
                    }
                }
                OverlayAction::None
            }
            KeyCode::F(5) => {
                self.refresh(app);
                OverlayAction::None
            }
            KeyCode::Enter => {
                if self.mode == Mode::Inbox {
                    // Keep it pending while the user reviews and drafts a
                    // response. Focus removes it only after the steer succeeds.
                    if let Some(request) = self
                        .visible_attention_indices(app)
                        .get(self.inbox_selected)
                        .and_then(|index| app.delegator.as_ref()?.attention().get(*index))
                        .cloned()
                    {
                        app.focus_agent = Some(request.run_id.clone());
                        app.focus_attention = Some(request.id.clone());
                        app.push(
                            crate::tui::app::EntryRole::Info,
                            format!(
                                "attention {} opened for {} — still waiting for your response",
                                request.id, request.run_id
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
                app.focus_attention = None;
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
                let selected_id = self.selected_child().map(|child| child.agent_id.clone());
                self.tree_view = !self.tree_view;
                self.rebuild_order();
                self.rebuild_filter();
                let selected_index = selected_id.and_then(|selected_id| {
                    self.visible_position(|child| child.agent_id == selected_id)
                });
                if let Some(index) = selected_index {
                    self.selected = index;
                }
                OverlayAction::None
            }
            KeyCode::Char('s') => {
                // Open the child composer; the user writes the actual direction
                // there instead of recording a fixed placeholder message.
                if self.mode == Mode::Roster
                    && let Some(child) = self.selected_child()
                {
                    app.focus_agent = Some(child.agent_id.clone());
                    app.focus_attention = None;
                    return OverlayAction::Push(OverlayKind::Focus);
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
        let active = self
            .children
            .iter()
            .filter(|child| matches!(child.status, AgentStatus::Running | AgentStatus::Starting))
            .count();
        let waiting = self
            .children
            .iter()
            .filter(|child| child.status == AgentStatus::Waiting)
            .count();
        let compact = area.width < 100;
        let header_height = 2.min(area.height);
        let footer_height = 1.min(area.height.saturating_sub(header_height));
        let body_height = area.height.saturating_sub(header_height + footer_height);
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_height),
                Constraint::Length(body_height),
                Constraint::Length(footer_height),
            ])
            .split(area);

        let summary = format!(
            "{} runs  ·  {active} active  ·  {waiting} waiting",
            self.children.len()
        );
        let heading = if self.searching {
            let query = match self.mode {
                Mode::Roster => &self.query,
                Mode::Inbox => &self.inbox_query,
            };
            format!("Agent Deck   ·   /{query}")
        } else if inbox_len > 0 {
            format!("Agent Deck   ·   {inbox_len} needs you")
        } else {
            "Agent Deck".to_string()
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    heading,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(summary, Style::default().fg(Color::DarkGray))),
            ]),
            vertical[0],
        );

        if self.mode == Mode::Roster && self.children.is_empty() && inbox_len == 0 {
            let empty = vec![
                Line::from(Span::styled(
                    "No delegated runs yet",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from("Delegate a bounded task from chat with /delegate <agent> <task>."),
                Line::from("Runs and their returned work will appear here."),
            ];
            frame.render_widget(Paragraph::new(empty), vertical[1]);
            frame.render_widget(
                Paragraph::new("/delegate start  ·  Esc return to chat"),
                vertical[2],
            );
            return;
        }

        let body = vertical[1];
        let show_rail = !compact && body.width >= 100;
        let columns = if show_rail {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
                .split(body)
        } else {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(100), Constraint::Length(0)])
                .split(body)
        };
        let (list_area, compact_detail_area) = if show_rail {
            (columns[0], None)
        } else if body.height >= 10 {
            let list_height = (body.height / 3).clamp(5, 8);
            let sections = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(list_height), Constraint::Min(4)])
                .split(body);
            (sections[0], Some(sections[1]))
        } else {
            (body, None)
        };
        let roster_total = self.filtered_order.len();
        let visible_attention = self.visible_attention_indices(app);
        let inbox_total = app
            .delegator
            .as_ref()
            .map(|delegator| delegator.attention().len())
            .unwrap_or(0);
        let row_height = if self.mode == Mode::Roster && list_area.width < 72 {
            2
        } else {
            1
        };
        let viewport = list_area.height.saturating_sub(1) as usize / row_height;
        let roster_window = Self::row_window(roster_total, self.selected, viewport);
        let inbox_window = Self::row_window(visible_attention.len(), self.inbox_selected, viewport);
        let mut rows: Vec<ListItem<'static>> = if self.mode == Mode::Inbox {
            app.delegator
                .as_ref()
                .map(|delegator| {
                    visible_attention
                        .iter()
                        .enumerate()
                        .skip(inbox_window.start)
                        .take(inbox_window.len())
                        .filter_map(|(visible_index, source_index)| {
                            let request = delegator.attention().get(*source_index)?;
                            let selected = visible_index == self.inbox_selected;
                            Some(ListItem::new(Line::from(vec![
                                Span::styled(
                                    if selected { "› " } else { "  " },
                                    Style::default().fg(Color::Cyan),
                                ),
                                Span::styled(
                                    format!("{}  ", request.agent),
                                    Style::default()
                                        .fg(Color::White)
                                        .add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(
                                    format!("{} · {}", request.kind, request.summary),
                                    Style::default().fg(Color::Gray),
                                ),
                            ])))
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            self.visible_window(roster_window.clone())
                .map(|(index, child)| {
                    let selected = index == self.selected;
                    let status_color = match child.status {
                        AgentStatus::Running => Color::Cyan,
                        AgentStatus::Done => Color::Green,
                        AgentStatus::Failed => Color::Red,
                        AgentStatus::Waiting => Color::Yellow,
                        AgentStatus::Orphaned => Color::Yellow,
                        AgentStatus::Killed | AgentStatus::Parked | AgentStatus::Starting => {
                            Color::DarkGray
                        }
                    };
                    let indent = if self.tree_view {
                        "  ".repeat(child.depth.min(8))
                    } else {
                        String::new()
                    };
                    let marker = Span::styled(
                        if selected { "› " } else { "  " },
                        Style::default().fg(Color::Cyan),
                    );
                    let glyph = Span::styled(
                        format!("{} ", child.status.glyph()),
                        Style::default().fg(status_color),
                    );
                    let role = Span::styled(
                        format!("{indent}{}", child.role),
                        Style::default()
                            .fg(if selected { Color::White } else { Color::Gray })
                            .add_modifier(if selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    );
                    if list_area.width < 72 {
                        ListItem::new(vec![
                            Line::from(vec![marker, glyph, role]),
                            Line::from(vec![
                                Span::raw("    "),
                                Span::styled(
                                    truncate_row(
                                        &child.task,
                                        list_area.width.saturating_sub(5) as usize,
                                    ),
                                    Style::default().fg(Color::DarkGray),
                                ),
                            ]),
                        ])
                    } else {
                        ListItem::new(Line::from(vec![
                            marker,
                            glyph,
                            role,
                            Span::styled(
                                format!(
                                    "   {}",
                                    truncate_row(
                                        &child.task,
                                        list_area
                                            .width
                                            .saturating_sub(child.role.chars().count() as u16 + 7)
                                            as usize,
                                    )
                                ),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]))
                    }
                })
                .collect()
        };
        if self.mode == Mode::Roster && rows.is_empty() {
            rows.push(ListItem::new(Line::from(Span::styled(
                if self.query.is_empty() {
                    "No delegated runs yet"
                } else {
                    "No runs match this search"
                },
                Style::default().fg(Color::DarkGray),
            ))));
        }
        if self.mode == Mode::Inbox && rows.is_empty() && !self.inbox_query.is_empty() {
            rows.push(ListItem::new(Line::from(Span::styled(
                "No requests match this search",
                Style::default().fg(Color::DarkGray),
            ))));
        } else if rows.is_empty() {
            rows.push(ListItem::new(Line::from(Span::styled(
                "No requests waiting for you",
                Style::default().fg(Color::DarkGray),
            ))));
        }
        let section_title = if self.mode == Mode::Inbox && !self.inbox_query.is_empty() {
            format!(
                "Needs you  ·  {} of {inbox_total}  ·  {}",
                visible_attention.len(),
                self.inbox_query
            )
        } else if self.mode == Mode::Inbox {
            format!("Needs you  ({inbox_total})")
        } else if !self.query.is_empty() {
            let counts = format!("Runs  ·  {roster_total} of {}", self.children.len());
            if self.searching {
                counts
            } else {
                format!("{counts}  ·  {}", self.query)
            }
        } else if inbox_len > 0 {
            format!("Runs  ·  {inbox_len} needs you")
        } else {
            "Runs".to_string()
        };
        let list = List::new(rows)
            .highlight_symbol("")
            .highlight_style(Style::default());
        frame.render_widget(
            Paragraph::new(section_title).style(Style::default().fg(Color::DarkGray)),
            Rect {
                height: 1,
                ..list_area
            },
        );
        let content_area = Rect {
            y: list_area.y.saturating_add(1),
            height: list_area.height.saturating_sub(1),
            ..list_area
        };
        let mut list_state = ListState::default();
        let selected_in_window = match self.mode {
            Mode::Roster if roster_total > 0 => {
                Some(self.selected.saturating_sub(roster_window.start))
            }
            Mode::Inbox if !visible_attention.is_empty() => {
                Some(self.inbox_selected.saturating_sub(inbox_window.start))
            }
            _ => None,
        };
        list_state.select(selected_in_window);
        frame.render_stateful_widget(list, content_area, &mut list_state);

        if show_rail || compact_detail_area.is_some() {
            let detail_area = if show_rail {
                columns[1]
            } else {
                compact_detail_area.expect("compact details were allocated")
            };
            if show_rail {
                frame.render_widget(
                    Block::default()
                        .borders(Borders::LEFT)
                        .border_style(Style::default().fg(Color::DarkGray)),
                    detail_area,
                );
            }
            let inset = if show_rail { 2 } else { 0 };
            let content_x = detail_area.x.saturating_add(inset);
            let content_width = detail_area.width.saturating_sub(inset);
            let detail_title = Rect {
                x: content_x,
                y: detail_area.y,
                width: content_width,
                height: 1.min(detail_area.height),
            };
            frame.render_widget(
                Paragraph::new(if self.mode == Mode::Inbox {
                    "REQUEST"
                } else {
                    "SELECTED RUN"
                })
                .style(
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ),
                detail_title,
            );
            let detail = if self.mode == Mode::Inbox {
                visible_attention
                    .get(self.inbox_selected)
                    .and_then(|index| app.delegator.as_ref()?.attention().get(*index))
                    .map(|request| {
                        vec![
                            format!("{} needs your input", request.agent),
                            String::new(),
                            format!("{} · {}", request.kind, request.summary),
                            String::new(),
                            "Enter opens the child run so you can respond.".to_string(),
                        ]
                    })
                    .unwrap_or_else(|| vec!["No open requests.".to_string()])
            } else {
                self.inspector_lines(app)
            };
            let detail_content = Rect {
                x: content_x,
                y: detail_area.y.saturating_add(1),
                width: content_width,
                height: detail_area.height.saturating_sub(1),
            };
            frame.render_widget(
                Paragraph::new(detail.join("\n"))
                    .style(Style::default().fg(Color::Gray))
                    .wrap(ratatui::widgets::Wrap { trim: true }),
                detail_content,
            );
        }

        frame.render_widget(
            Paragraph::new(if self.mode == Mode::Inbox {
                if self.searching {
                    "type to filter  ·  Enter finish  ·  Esc clear  ·  Ctrl+U clear"
                } else if !self.inbox_query.is_empty() {
                    "↑↓ move  ·  Enter review  ·  / search  ·  Esc clear"
                } else {
                    "↑↓ move  ·  Enter review  ·  / search  ·  Tab runs"
                }
            } else {
                if self.searching {
                    "type to filter  ·  Enter finish  ·  Esc clear  ·  Ctrl+U clear"
                } else if !self.query.is_empty() {
                    "↑↓ move  ·  Enter open  ·  / search  ·  Esc clear"
                } else if area.width < 72 {
                    "↑↓ move  ·  Enter open  ·  Tab view  ·  Esc close"
                } else {
                    "↑↓ move  ·  Enter open  ·  F5 refresh  ·  r revive  ·  x stop  ·  Tab inbox  ·  Esc close"
                }
            })
            .style(Style::default().fg(Color::DarkGray)),
            vertical[2],
        );
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑↓", "navigate"),
            ("Home/End", "first / last run"),
            ("PgUp/PgDn", "move by page"),
            ("F5", "refresh roster"),
            ("/", "search runs"),
            ("Enter", "open child / respond to request"),
            ("Tab", "runs ⇄ needs you"),
            ("Space", "peek recent events"),
            ("m", "show worktree merge instructions"),
            ("p", "show run receipt"),
            ("t", "flat/tree"),
            ("s", "open child composer to send direction"),
            ("x", "stop one child"),
            ("r", "revive"),
            ("Ctrl+i", "interrupt subtree"),
            ("Esc/q", "close"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn refresh_keeps_the_selected_run_when_a_child_is_added() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let root = delegator.root(None, "main", "parent run").unwrap();
        let sibling_root = delegator.root(None, "review", "another root").unwrap();
        let selected = delegator
            .spawn(Some(&root.agent_id), None, "scout", "selected child")
            .unwrap();

        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.tree_view = true;
        deck.refresh(&app);
        deck.selected = deck
            .ordered()
            .iter()
            .position(|child| child.agent_id == selected.agent_id)
            .unwrap();

        // Add a descendant to another branch while the selected run stays put.
        app.delegator
            .as_mut()
            .unwrap()
            .spawn(Some(&sibling_root.agent_id), None, "test", "new descendant")
            .unwrap();
        deck.refresh(&app);

        assert_eq!(
            deck.selected_child().map(|child| child.agent_id.as_str()),
            Some(selected.agent_id.as_str())
        );
    }

    #[test]
    fn selected_run_inspector_names_the_persisted_state() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        delegator
            .root(None, "review", "inspect the change")
            .expect("run");
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.refresh(&app);

        let details = deck.inspector_lines(&app).join("\n");
        assert!(details.contains("state: ? waiting"), "{details}");
        assert!(details.contains("depth: 0"), "{details}");
        assert!(details.contains("recent activity"), "{details}");

        deck.peeked = Some("{\"summary\": \"receipt visible\"}".to_string());
        let peek = deck.inspector_lines(&app).join("\n");
        assert!(peek.contains("receipt visible"), "{peek}");
        assert!(!peek.contains("recent activity"), "{peek}");
    }

    #[test]
    fn default_activity_shows_event_names_without_event_payloads() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let run = delegator
            .root(None, "review", "inspect the change")
            .unwrap();
        delegator
            .queue_attention(&run.agent_id, "question", "private prompt payload")
            .expect("attention event");
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.refresh(&app);

        let run = deck.selected_child().unwrap();
        let events = app
            .delegator
            .as_ref()
            .unwrap()
            .read_log(&run.agent_id)
            .unwrap();
        let activity = AgentDeck::activity_lines(&events).join("\n");
        assert!(activity.contains("attention.queued"), "{activity}");
        assert!(!activity.contains("private prompt payload"), "{activity}");
    }

    #[test]
    fn explicit_refresh_updates_roster_and_keeps_selected_identity() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let first = delegator.root(None, "first", "first task").unwrap();
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.refresh(&app);
        app.delegator
            .as_mut()
            .unwrap()
            .root(None, "second", "second task")
            .unwrap();

        deck.handle_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE), &mut app);

        assert_eq!(deck.children.len(), 2);
        assert_eq!(deck.selected_child().unwrap().agent_id, first.agent_id);
    }

    #[test]
    fn stop_and_revive_keys_only_change_the_selected_run() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let selected = delegator.root(None, "selected", "selected task").unwrap();
        let sibling = delegator.root(None, "sibling", "sibling task").unwrap();
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.refresh(&app);
        deck.selected = deck
            .visible()
            .iter()
            .position(|child| child.agent_id == selected.agent_id)
            .unwrap();

        deck.handle_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut app,
        );
        assert_eq!(
            app.delegator
                .as_ref()
                .unwrap()
                .registry()
                .get(&selected.agent_id)
                .unwrap()
                .status,
            memnoth_agent::RunStatus::Cancelled
        );
        assert_eq!(
            app.delegator
                .as_ref()
                .unwrap()
                .registry()
                .get(&sibling.agent_id)
                .unwrap()
                .status,
            memnoth_agent::RunStatus::Pending
        );

        deck.handle_key(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            &mut app,
        );
        assert_eq!(
            app.delegator
                .as_ref()
                .unwrap()
                .registry()
                .get(&selected.agent_id)
                .unwrap()
                .status,
            memnoth_agent::RunStatus::Pending
        );
    }

    #[test]
    fn enter_opens_the_selected_run_without_changing_the_parent_draft() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let first = delegator.root(None, "first", "first task").unwrap();
        let second = delegator.root(None, "second", "second task").unwrap();
        let mut app = App::new("s", false);
        app.input = "preserve this parent draft".to_string();
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.refresh(&app);
        deck.selected = deck
            .visible()
            .iter()
            .position(|child| child.agent_id == second.agent_id)
            .unwrap();

        let action = deck.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);

        assert!(matches!(action, OverlayAction::Push(OverlayKind::Focus)));
        assert_eq!(app.focus_agent.as_deref(), Some(second.agent_id.as_str()));
        assert_ne!(app.focus_agent.as_deref(), Some(first.agent_id.as_str()));
        assert_eq!(app.input, "preserve this parent draft");
    }

    #[test]
    fn roster_navigation_supports_home_end_and_page_keys() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        for index in 0..12 {
            delegator
                .root(None, &format!("agent-{index}"), "task")
                .unwrap();
        }
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.refresh(&app);

        deck.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &mut app);
        assert_eq!(deck.selected, 11);
        deck.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE), &mut app);
        assert_eq!(deck.selected, 3);
        deck.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE), &mut app);
        assert_eq!(deck.selected, 0);
        deck.handle_key(
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            &mut app,
        );
        assert_eq!(deck.selected, 8);
    }

    #[test]
    fn narrow_roster_uses_two_lines_per_run_without_clipping_the_task_row() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        delegator
            .root(None, "scout", "inspect the narrow terminal layout")
            .unwrap();
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.refresh(&app);

        let backend = ratatui::backend::TestBackend::new(60, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| deck.render(frame, frame.area(), &app))
            .expect("draw narrow agent deck");
        let buffer = terminal.backend().buffer();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("scout"), "{text}");
        assert!(
            text.contains("inspect the narrow terminal layout"),
            "{text}"
        );
        assert!(text.contains("recent activity"), "{text}");
    }

    #[test]
    fn wide_deck_uses_a_borderless_roster_and_selected_run_rail() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        delegator
            .root(None, "review", "inspect the selected run")
            .unwrap();
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.refresh(&app);

        let backend = ratatui::backend::TestBackend::new(120, 32);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| deck.render(frame, frame.area(), &app))
            .expect("draw wide agent deck");
        let buffer = terminal.backend().buffer();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("Agent Deck"), "{text}");
        assert!(text.contains("SELECTED RUN"), "{text}");
        assert!(text.contains("recent activity"), "{text}");
        assert!(!text.contains('┌') && !text.contains('┐'), "{text}");
        assert!(text.contains("1 runs  ·  0 active  ·  1 waiting"), "{text}");
    }

    #[test]
    fn high_fanout_roster_window_is_bounded_and_keeps_selection_visible() {
        let middle = AgentDeck::row_window(10_000, 5_000, 12);
        assert_eq!(middle.len(), 12);
        assert!(middle.contains(&5_000));

        let end = AgentDeck::row_window(10_000, 9_999, 12);
        assert_eq!(end, 9_988..10_000);

        assert_eq!(AgentDeck::row_window(10, 0, 0), 0..0);

        let window = AgentDeck::row_window(100, 48, 12);
        assert_eq!(window.len(), 12);
        assert!(window.contains(&48));
        assert_eq!(window, 42..54);
    }

    #[test]
    fn high_fanout_window_resolves_only_the_requested_filtered_rows() {
        let mut deck = AgentDeck::new();
        deck.children = (0..1_000)
            .map(|index| ChildInfo {
                agent_id: format!("run-{index}"),
                parent_id: None,
                root_id: None,
                session_id: None,
                role: "scout".to_string(),
                task: if index == 999 {
                    "needle at the end".to_string()
                } else {
                    format!("task {index}")
                },
                status: AgentStatus::Running,
                spawn_seq: index,
                depth: 0,
                model: None,
                worktree: None,
                unread: false,
            })
            .collect();
        deck.rebuild_order();
        deck.rebuild_filter();

        let rows: Vec<(usize, String)> = deck
            .visible_window(490..502)
            .map(|(index, child)| (index, child.agent_id.clone()))
            .collect();
        assert_eq!(rows.len(), 12);
        assert_eq!(rows.first().unwrap(), &(490, "run-490".to_string()));
        assert_eq!(rows.last().unwrap(), &(501, "run-501".to_string()));
        assert!(deck.visible_window(usize::MAX..usize::MAX).next().is_none());

        deck.query = "needle".to_string();
        deck.rebuild_filter();
        let filtered: Vec<(usize, String)> = deck
            .visible_window(0..12)
            .map(|(index, child)| (index, child.agent_id.clone()))
            .collect();
        assert_eq!(filtered, vec![(0, "run-999".to_string())]);
    }

    #[test]
    fn changing_flat_tree_order_keeps_the_same_run_selected() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let root = delegator.root(None, "main", "parent run").unwrap();
        delegator.root(None, "review", "another root").unwrap();
        let child = delegator
            .spawn(Some(&root.agent_id), None, "scout", "nested run")
            .unwrap();
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.refresh(&app);
        deck.selected = deck
            .ordered()
            .iter()
            .position(|run| run.agent_id == child.agent_id)
            .unwrap();

        deck.handle_key(
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
            &mut app,
        );

        assert!(deck.tree_view);
        assert_eq!(deck.selected, 1, "child moves ahead of the later root");
        assert_eq!(
            deck.selected_child().map(|run| run.agent_id.as_str()),
            Some(child.agent_id.as_str())
        );
    }

    #[test]
    fn tree_order_keeps_each_child_inside_its_actual_parent_group() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let first = delegator.root(None, "first", "first root").unwrap();
        let second = delegator.root(None, "second", "second root").unwrap();
        let first_child = delegator
            .spawn(Some(&first.agent_id), None, "child", "belongs to first")
            .unwrap();
        let second_child = delegator
            .spawn(Some(&second.agent_id), None, "child", "belongs to second")
            .unwrap();
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.tree_view = true;
        deck.refresh(&app);

        let order: Vec<&str> = deck
            .ordered()
            .iter()
            .map(|run| run.agent_id.as_str())
            .collect();
        assert_eq!(
            order,
            vec![
                first.agent_id.as_str(),
                first_child.agent_id.as_str(),
                second.agent_id.as_str(),
                second_child.agent_id.as_str(),
            ]
        );
    }

    #[test]
    fn search_filters_roster_and_preserves_the_selected_run_on_refresh() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let first = delegator
            .root(None, "review", "inspect the API")
            .expect("first run");
        delegator
            .root(None, "builder", "update the docs")
            .expect("second run");
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.refresh(&app);

        deck.handle_key(
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &mut app,
        );
        for ch in "api".chars() {
            deck.handle_key(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                &mut app,
            );
        }

        assert_eq!(deck.visible().len(), 1);
        assert_eq!(
            deck.selected_child().map(|run| run.agent_id.as_str()),
            Some(first.agent_id.as_str())
        );
        deck.refresh(&app);
        assert_eq!(deck.selected, 0);
        assert_eq!(deck.visible().len(), 1);

        deck.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
        assert!(!deck.searching);
        assert!(deck.query.is_empty());
        assert_eq!(deck.visible().len(), 2);
        assert!(matches!(
            deck.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app),
            OverlayAction::Close
        ));
    }

    #[test]
    fn opening_attention_keeps_it_pending_until_focus_sends_a_response() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let child = delegator
            .root(None, "review", "inspect the change")
            .expect("child run");
        let request = delegator
            .queue_attention(&child.agent_id, "question", "Which API should I check?")
            .expect("attention request");

        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.mode = Mode::Inbox;

        let action = deck.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);

        assert!(matches!(action, OverlayAction::Push(OverlayKind::Focus)));
        assert_eq!(app.focus_agent.as_deref(), Some(child.agent_id.as_str()));
        assert_eq!(app.focus_attention.as_deref(), Some(request.id.as_str()));
        assert_eq!(app.delegator.as_ref().unwrap().attention(), &[request]);
    }

    #[test]
    fn inbox_search_filters_source_requests_and_enter_opens_the_matching_request() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let billing = delegator
            .root(None, "review", "check billing")
            .expect("billing run");
        let first = delegator
            .queue_attention(&billing.agent_id, "permission", "Allow database migration?")
            .expect("permission request");
        let api = delegator
            .root(None, "scout", "inspect API")
            .expect("API run");
        let second = delegator
            .queue_attention(
                &api.agent_id,
                "question",
                "Which endpoint should I inspect?",
            )
            .expect("question request");

        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.mode = Mode::Inbox;
        deck.handle_key(
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &mut app,
        );
        for ch in "ENDPOINT".chars() {
            deck.handle_key(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                &mut app,
            );
        }

        assert_eq!(deck.visible_attention_indices(&app), vec![1]);
        deck.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        assert!(!deck.searching, "first Enter commits the filter");
        let action = deck.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        assert!(matches!(action, OverlayAction::Push(OverlayKind::Focus)));
        assert_eq!(app.focus_agent.as_deref(), Some(api.agent_id.as_str()));
        assert_eq!(app.focus_attention.as_deref(), Some(second.id.as_str()));
        let requests = app.delegator.as_ref().unwrap().attention();
        assert_eq!(requests, &[first, second]);
    }

    #[test]
    fn inbox_search_backspace_clear_and_refresh_never_invent_or_resolve_requests() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let run = delegator.root(None, "reviewer", "check API").expect("run");
        let request = delegator
            .queue_attention(&run.agent_id, "question", "Which API?")
            .expect("request");
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.mode = Mode::Inbox;
        deck.handle_key(
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &mut app,
        );
        for ch in "missing".chars() {
            deck.handle_key(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                &mut app,
            );
        }
        assert!(deck.visible_attention_indices(&app).is_empty());

        deck.handle_key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &mut app,
        );
        assert_eq!(deck.visible_attention_indices(&app), vec![0]);
        deck.on_tick(&mut app);
        assert_eq!(deck.inbox_selected, 0);
        assert_eq!(app.delegator.as_ref().unwrap().attention(), &[request]);

        deck.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
        assert!(!deck.searching);
        assert!(deck.inbox_query.is_empty());
        assert_eq!(deck.visible_attention_indices(&app), vec![0]);
    }

    #[test]
    fn empty_attention_view_does_not_fall_back_to_the_roster() {
        let app = App::new("s", false);
        let mut deck = AgentDeck::new();
        deck.mode = Mode::Inbox;
        deck.refresh(&app);

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| deck.render(frame, frame.area(), &app))
            .expect("draw agent deck");
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("No requests waiting for you"), "{text}");
        assert!(!text.contains("No delegated runs yet"), "{text}");
    }

    #[test]
    fn inbox_search_renders_an_explicit_empty_filter_result() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let run = delegator.root(None, "reviewer", "check API").expect("run");
        delegator
            .queue_attention(&run.agent_id, "question", "Which API?")
            .expect("request");
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.mode = Mode::Inbox;
        deck.inbox_query = "absent".to_string();

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| deck.render(frame, frame.area(), &app))
            .expect("draw agent deck");
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("No requests match this search"), "{text}");
        assert!(text.contains("0 of 1"), "{text}");
    }
}
