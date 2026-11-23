//! Agent Deck overlay: `Alt+A`. Roster of child agents, inspector,
//! attention inbox, flat/tree toggle, costs, context, lineage.
//!
//! `/agents` stays the definition library and does not become the live
//! roster — the deck is the live roster.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::tui::agents::{AgentStatus, ChildInfo, detail_view, steer_composer};
use crate::tui::app::{App, EntryRole};
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use crate::tui::theme::tokens;

use detail_view::DetailPage;
use steer_composer::SteerComposer;

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

#[derive(Debug, Default)]
struct AttentionFilterCache {
    query: String,
    revision: Option<u64>,
    source_indexes: Vec<usize>,
    #[cfg(test)]
    rebuilds: usize,
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
    attention_filter: AttentionFilterCache,
    mode: Mode,
    inbox_selected: usize,
    inbox_selected_id: Option<String>,
    /// Peaked detail (log tail or receipt) shown in the inspector.
    peeked: Option<String>,
    /// A full-screen, read-only run detail page over the roster.
    detail_open: bool,
    detail_run_id: Option<String>,
    detail_page: DetailPage,
    detail_scroll: usize,
    /// The steer composer, opened with `s` for the selected run.
    steer: SteerComposer,
    steer_open: bool,
    /// Honest feedback after an allow-once or deny decision.
    review_notice: Option<String>,
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
            attention_filter: AttentionFilterCache::default(),
            mode: Mode::Roster,
            inbox_selected: 0,
            inbox_selected_id: None,
            peeked: None,
            detail_open: false,
            detail_run_id: None,
            detail_page: DetailPage::Overview,
            detail_scroll: 0,
            steer: SteerComposer::default(),
            steer_open: false,
            review_notice: None,
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
    fn refresh_attention_filter(&mut self, app: &App) {
        let delegator = app.delegator.as_ref();
        let requests = delegator
            .map(|delegator| delegator.attention())
            .unwrap_or_default();
        let revision = delegator.map(|delegator| delegator.attention_revision());
        let query = self.inbox_query.trim().to_lowercase();
        let cache = &mut self.attention_filter;
        let query_changed = cache.query != query;
        let revision_changed = cache.revision != revision;
        if query_changed || revision_changed {
            let selected_id = (!query_changed && revision_changed)
                .then(|| self.inbox_selected_id.clone())
                .flatten();
            cache.query.clone_from(&query);
            cache.revision = revision;
            cache.source_indexes.clear();
            cache
                .source_indexes
                .extend(requests.iter().enumerate().filter_map(|(index, request)| {
                    (query.is_empty()
                        || request.agent.to_lowercase().contains(&query)
                        || request.kind.to_lowercase().contains(&query)
                        || request.summary.to_lowercase().contains(&query)
                        || request.run_id.to_lowercase().contains(&query)
                        || request.id.to_lowercase().contains(&query))
                    .then_some(index)
                }));
            #[cfg(test)]
            {
                cache.rebuilds += 1;
            }
            if let Some(selected_id) = selected_id {
                self.inbox_selected = cache
                    .source_indexes
                    .iter()
                    .position(|index| requests[*index].id == selected_id)
                    .unwrap_or_else(|| {
                        self.inbox_selected
                            .min(cache.source_indexes.len().saturating_sub(1))
                    });
            } else {
                self.inbox_selected = self
                    .inbox_selected
                    .min(cache.source_indexes.len().saturating_sub(1));
            }
        }
        self.inbox_selected_id = cache
            .source_indexes
            .get(self.inbox_selected)
            .and_then(|index| requests.get(*index))
            .map(|request| request.id.clone());
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

    /// The attention row under the cursor, resolved by source index so search
    /// and refresh can never answer a different request than the one shown.
    fn selected_attention(&self, app: &App) -> Option<memnoth_agent::delegate::AttentionRequest> {
        let index = *self
            .attention_filter
            .source_indexes
            .get(self.inbox_selected)?;
        app.delegator.as_ref()?.attention().get(index).cloned()
    }

    /// Record an allow-once or deny decision in the child's own log, then take
    /// the request out of the inbox. A decision is never a silent drop: if the
    /// write fails the request stays pending and an error is surfaced.
    fn record_attention_decision(&mut self, app: &mut App, decision: &'static str) {
        let Some(request) = self.selected_attention(app) else {
            return;
        };
        let note = format!(
            "attention decision: {decision} request {} ({}) — {}",
            request.id, request.kind, request.summary
        );
        let recorded = match app.delegator.as_mut() {
            Some(delegator) => match delegator.steer(&request.run_id, &note) {
                Ok(()) => true,
                Err(error) => {
                    self.review_notice = Some(format!("could not record {decision}: {error}"));
                    false
                }
            },
            None => {
                self.review_notice = Some("no delegator — decision not recorded".to_string());
                false
            }
        };
        if recorded {
            let removed = app
                .delegator
                .as_mut()
                .is_some_and(|delegator| delegator.take_attention(&request.id).is_some());
            let notice = format!(
                "{decision} request {} — recorded in the child log{}",
                request.id,
                if removed { "" } else { " (already closed)" }
            );
            app.push(EntryRole::Info, notice.clone());
            self.review_notice = Some(notice);
        } else {
            let error = self
                .review_notice
                .clone()
                .unwrap_or_else(|| "decision not recorded".to_string());
            app.push(EntryRole::Error, error);
        }
        self.refresh_attention_filter(app);
    }

    fn review_allow_once(&mut self, app: &mut App) {
        self.record_attention_decision(app, "allow once");
    }

    fn review_deny(&mut self, app: &mut App) {
        self.record_attention_decision(app, "deny");
    }

    /// Structured review of the selected request: the fields the delegator
    /// actually supplies, then three explicit actions. No reason or target is
    /// invented from a vague log line.
    fn attention_review_lines(&self, app: &App) -> Vec<String> {
        let Some(request) = self.selected_attention(app) else {
            return vec!["No open requests.".to_string()];
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or_default();
        let seconds = now_ms.saturating_sub(request.at_ms).max(0) / 1_000;
        let age = if seconds < 60 {
            format!("{seconds}s ago")
        } else if seconds < 3_600 {
            format!("{}m ago", seconds / 60)
        } else {
            format!("{}h ago", seconds / 3_600)
        };
        let mut lines = vec![
            format!("{} needs your decision", request.agent),
            String::new(),
            format!("Request   {}", request.kind),
            format!("Summary   {}", request.summary),
            format!("Run       {}", request.run_id),
            format!("Id        {}", request.id),
            "Scope     one request on this run".to_string(),
            format!("Requested {age}"),
            String::new(),
            "[a] Allow once   [d] Deny   [Enter] Open run".to_string(),
            String::new(),
            "Decisions are recorded in the child log; nothing is approved silently.".to_string(),
        ];
        if let Some(notice) = &self.review_notice {
            lines.push(String::new());
            lines.push(notice.clone());
        }
        lines
    }

    /// Enter on a roster row opens the read-only detail page. It does not touch
    /// the parent draft or the attention inbox.
    fn open_detail(&mut self) {
        let Some(child) = self.selected_child().cloned() else {
            return;
        };
        self.detail_run_id = Some(child.agent_id);
        self.detail_page = DetailPage::Overview;
        self.detail_scroll = 0;
        self.detail_open = true;
    }

    /// Resolve the detailed run by id, so a refresh that reorders the roster
    /// cannot switch the page to a different child.
    fn detail_child(&self) -> Option<ChildInfo> {
        let id = self.detail_run_id.as_ref()?;
        self.children
            .iter()
            .find(|child| &child.agent_id == id)
            .cloned()
    }

    fn detail_events(&self, app: &App) -> Vec<memnoth_agent::ChildEvent> {
        let Some(id) = self.detail_run_id.as_deref() else {
            return Vec::new();
        };
        app.delegator
            .as_ref()
            .and_then(|delegator| delegator.read_log(id).ok())
            .unwrap_or_default()
    }

    fn detail_lines(&self, app: &App) -> Vec<String> {
        let Some(child) = self.detail_child() else {
            return vec!["run is no longer in the roster".to_string()];
        };
        let events = self.detail_events(app);
        detail_view::page_lines(self.detail_page, &child, &events)
    }

    fn open_steer(&mut self, app: &App, child: &ChildInfo) {
        self.steer.open_for(app, child);
        self.steer_open = true;
    }

    fn handle_detail_key(&mut self, key: KeyEvent, _app: &mut App) -> OverlayAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.detail_open = false,
            KeyCode::Tab | KeyCode::Right => {
                self.detail_page = self.detail_page.next();
                self.detail_scroll = 0;
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.detail_page = self.detail_page.previous();
                self.detail_scroll = 0;
            }
            KeyCode::Char(ch @ '1'..='4') => {
                if let Some(page) = DetailPage::from_index(ch as usize - '1' as usize) {
                    self.detail_page = page;
                    self.detail_scroll = 0;
                }
            }
            KeyCode::Up | KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::PageDown => self.detail_scroll += 1,
            KeyCode::Home => self.detail_scroll = 0,
            KeyCode::End => self.detail_scroll = usize::MAX,
            _ => {}
        }
        OverlayAction::None
    }

    fn handle_steer_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.steer.is_wired() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                self.steer_open = false;
            }
            return OverlayAction::None;
        }
        match key.code {
            KeyCode::Esc => self.steer_open = false,
            KeyCode::Enter => {
                if self.steer.submit(app) {
                    self.steer_open = false;
                }
            }
            KeyCode::Backspace => self.steer.backspace(),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.steer.clear();
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.steer.push_char(ch);
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn render_detail_panel(&mut self, frame: &mut Frame, area: Rect, app: &App) {
        frame.render_widget(Clear, area);
        let t = tokens();
        let child = self.detail_child();
        let title = match &child {
            Some(child) => format!("Agent Deck / {} / {}", child.role, child.agent_id),
            None => "Agent Deck / run".to_string(),
        };
        let state = match &child {
            Some(child) => format!("state: {} {}", child.status.glyph(), child.status.label()),
            None => "state: unavailable".to_string(),
        };
        let tabs = detail_view::DetailPage::ALL
            .iter()
            .map(|page| {
                if *page == self.detail_page {
                    format!("[{}]", page.label())
                } else {
                    format!(" {}", page.label())
                }
            })
            .collect::<Vec<_>>()
            .join("   ");
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    title,
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(state, Style::default().fg(t.muted))),
                Line::from(Span::styled(tabs, Style::default().fg(t.muted))),
            ]),
            vertical[0],
        );
        let body = self.detail_lines(app);
        let height = vertical[1].height as usize;
        let max_scroll = body.len().saturating_sub(height);
        self.detail_scroll = self.detail_scroll.min(max_scroll);
        let window: String = body
            .iter()
            .skip(self.detail_scroll)
            .take(height)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        frame.render_widget(
            Paragraph::new(window)
                .style(Style::default().fg(t.muted))
                .wrap(ratatui::widgets::Wrap { trim: true }),
            vertical[1],
        );
        frame.render_widget(
            Paragraph::new("1-4 / Tab sections  ·  ↑↓ scroll  ·  s steer  ·  Esc deck")
                .style(Style::default().fg(t.muted)),
            vertical[2],
        );
    }

    fn render_steer_panel(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let t = tokens();
        let wired = self.steer.is_wired();
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Agent Deck / Steer",
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    if wired {
                        "the note goes to the child's own log"
                    } else {
                        "the runtime cannot accept a steer"
                    },
                    Style::default().fg(t.muted),
                )),
            ]),
            vertical[0],
        );
        let body = self.steer.lines().join("\n");
        frame.render_widget(
            Paragraph::new(body)
                .style(Style::default().fg(t.muted))
                .wrap(ratatui::widgets::Wrap { trim: true }),
            vertical[1],
        );
        frame.render_widget(
            Paragraph::new(if wired {
                "type  ·  Backspace  ·  Ctrl+U clear  ·  Enter send  ·  Esc back"
            } else {
                "Esc back"
            })
            .style(Style::default().fg(t.muted)),
            vertical[2],
        );
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
        self.attention_filter = AttentionFilterCache::default();
        self.inbox_selected_id = None;
        self.inbox_selected = 0;
        self.refresh(app);
        self.refresh_attention_filter(app);
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
        self.refresh(app);
        self.refresh_attention_filter(app);
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        self.refresh_attention_filter(app);
        if self.steer_open {
            return self.handle_steer_key(key, app);
        }
        if self.detail_open {
            return self.handle_detail_key(key, app);
        }
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
            } else {
                self.refresh_attention_filter(app);
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
                        let len = self.attention_filter.source_indexes.len();
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
                        let len = self.attention_filter.source_indexes.len();
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
                            self.attention_filter.source_indexes.len().saturating_sub(1)
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
                        .attention_filter
                        .source_indexes
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
                // Enter inspects: open the read-only run detail page. Steering
                // is a separate, explicit action on `s`, so looking at a run
                // can never send it a direction by accident.
                self.open_detail();
                OverlayAction::None
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
                // Open the real steer composer when the runtime can deliver a
                // note; otherwise show the not-wired answer, never a dead box.
                if self.mode == Mode::Roster
                    && let Some(child) = self.selected_child().cloned()
                {
                    self.open_steer(app, &child);
                }
                OverlayAction::None
            }
            KeyCode::Char('a') if self.mode == Mode::Inbox => {
                self.review_allow_once(app);
                OverlayAction::None
            }
            KeyCode::Char('d') if self.mode == Mode::Inbox => {
                self.review_deny(app);
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
        self.refresh_attention_filter(app);
        let t = tokens();
        if self.steer_open {
            self.render_steer_panel(frame, area);
            return;
        }
        if self.detail_open {
            self.render_detail_panel(frame, area, app);
            return;
        }
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
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(summary, Style::default().fg(t.muted))),
            ]),
            vertical[0],
        );

        if self.mode == Mode::Roster && self.children.is_empty() && inbox_len == 0 {
            let empty = vec![
                Line::from(Span::styled(
                    "No delegated runs yet",
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
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
        let visible_attention = &self.attention_filter.source_indexes;
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
                                    Style::default().fg(t.accent),
                                ),
                                Span::styled(
                                    format!("{}  ", request.agent),
                                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(
                                    format!("{} · {}", request.kind, request.summary),
                                    Style::default().fg(t.muted),
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
                    // Colour is decoration only: the run phase is carried by
                    // `status.glyph()` / `status.label()`, so mapping these to
                    // semantic roles never changes what a phase means.
                    let status_color = match child.status {
                        AgentStatus::Running => t.text,
                        AgentStatus::Done => t.success,
                        AgentStatus::Failed => t.error,
                        AgentStatus::Waiting => t.warning,
                        AgentStatus::Orphaned => t.warning,
                        AgentStatus::Killed | AgentStatus::Parked | AgentStatus::Starting => {
                            t.muted
                        }
                    };
                    let indent = if self.tree_view {
                        "  ".repeat(child.depth.min(8))
                    } else {
                        String::new()
                    };
                    let marker = Span::styled(
                        if selected { "› " } else { "  " },
                        Style::default().fg(t.accent),
                    );
                    let glyph = Span::styled(
                        format!("{} ", child.status.glyph()),
                        Style::default().fg(status_color),
                    );
                    let role = Span::styled(
                        format!("{indent}{}", child.role),
                        Style::default()
                            .fg(if selected { t.text } else { t.muted })
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
                                    Style::default().fg(t.muted),
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
                                Style::default().fg(t.muted),
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
                Style::default().fg(t.muted),
            ))));
        }
        if self.mode == Mode::Inbox && rows.is_empty() && !self.inbox_query.is_empty() {
            rows.push(ListItem::new(Line::from(Span::styled(
                "No requests match this search",
                Style::default().fg(t.muted),
            ))));
        } else if rows.is_empty() {
            rows.push(ListItem::new(Line::from(Span::styled(
                "No requests waiting for you",
                Style::default().fg(t.muted),
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
            Paragraph::new(section_title).style(Style::default().fg(t.muted)),
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
                        .border_style(Style::default().fg(t.border)),
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
                .style(Style::default().fg(t.muted).add_modifier(Modifier::BOLD)),
                detail_title,
            );
            let detail = if self.mode == Mode::Inbox {
                self.attention_review_lines(app)
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
                    .style(Style::default().fg(t.muted))
                    .wrap(ratatui::widgets::Wrap { trim: true }),
                detail_content,
            );
        }

        frame.render_widget(
            Paragraph::new(if self.mode == Mode::Inbox {
                if self.searching {
                    "type to filter  ·  Enter finish  ·  Esc clear  ·  Ctrl+U clear"
                } else if !self.inbox_query.is_empty() {
                    "↑↓ move  ·  a allow once  ·  d deny  ·  Enter open run  ·  Esc clear"
                } else {
                    "↑↓ move  ·  a allow once  ·  d deny  ·  Enter open run  ·  Tab runs"
                }
            } else {
                if self.searching {
                    "type to filter  ·  Enter finish  ·  Esc clear  ·  Ctrl+U clear"
                } else if !self.query.is_empty() {
                    "↑↓ move  ·  Enter detail  ·  / search  ·  Esc clear"
                } else if area.width < 72 {
                    "↑↓ move  ·  Enter detail  ·  s steer  ·  Esc close"
                } else {
                    "↑↓ move  ·  Enter detail  ·  s steer  ·  F5 refresh  ·  r revive  ·  x stop  ·  Tab inbox  ·  Esc close"
                }
            })
            .style(Style::default().fg(t.muted)),
            vertical[2],
        );
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑↓", "navigate"),
            ("Home/End", "first / last run"),
            ("PgUp/PgDn", "move by page"),
            ("F5", "refresh roster"),
            ("/", "search runs or requests"),
            ("Enter", "open run detail / open the reviewed run"),
            ("Tab", "runs ⇄ needs you; page detail tabs"),
            ("Space", "peek recent events"),
            ("m", "show worktree merge instructions"),
            ("p", "show run receipt"),
            ("t", "flat/tree"),
            (
                "s",
                "open the real steer composer, or an honest not-wired state",
            ),
            ("a / d", "allow once / deny the focused attention request"),
            ("1-4", "Overview / Activity / Result / Details"),
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
    use ratatui::style::Color;

    /// Foreground of the first cell drawn with `symbol`, or `None`.
    fn first_cell_fg(buffer: &ratatui::buffer::Buffer, symbol: &str) -> Option<Color> {
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if buffer[(x, y)].symbol() == symbol {
                    return Some(buffer[(x, y)].fg);
                }
            }
        }
        None
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

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
    fn enter_opens_the_selected_run_detail_without_changing_the_parent_draft() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let _first = delegator.root(None, "first", "first task").unwrap();
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

        assert_eq!(action, OverlayAction::None);
        assert!(deck.detail_open);
        assert_eq!(
            deck.detail_run_id.as_deref(),
            Some(second.agent_id.as_str())
        );
        assert_eq!(deck.detail_page, DetailPage::Overview);
        assert_eq!(app.input, "preserve this parent draft");
        assert!(app.focus_agent.is_none(), "inspecting never steals focus");

        // Esc returns to the roster, and `s` opens the real composer for the
        // same run — looking and steering are separate, explicit actions.
        deck.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
        assert!(!deck.detail_open);
        deck.handle_key(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
            &mut app,
        );
        assert!(deck.steer_open);
        assert_eq!(deck.steer.target.as_deref(), Some(second.agent_id.as_str()));
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

        deck.refresh_attention_filter(&app);
        assert_eq!(deck.attention_filter.source_indexes, vec![1]);
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
        deck.refresh_attention_filter(&app);
        assert!(deck.attention_filter.source_indexes.is_empty());

        deck.handle_key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &mut app,
        );
        deck.refresh_attention_filter(&app);
        assert_eq!(deck.attention_filter.source_indexes, vec![0]);
        deck.on_tick(&mut app);
        assert_eq!(deck.inbox_selected, 0);
        assert_eq!(app.delegator.as_ref().unwrap().attention(), &[request]);

        deck.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
        assert!(!deck.searching);
        assert!(deck.inbox_query.is_empty());
        deck.refresh_attention_filter(&app);
        assert_eq!(deck.attention_filter.source_indexes, vec![0]);
    }

    #[test]
    fn inbox_filter_caches_large_source_indexes_and_bounds_the_viewport() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let run = delegator.root(None, "scout", "inbox fixture").expect("run");
        for index in 0..1_000 {
            let summary = if matches!(index, 0 | 500 | 999) {
                format!("match request {index}")
            } else {
                format!("request {index}")
            };
            delegator
                .queue_attention(&run.agent_id, "question", &summary)
                .expect("queue request");
        }
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.mode = Mode::Inbox;

        deck.refresh_attention_filter(&app);
        assert_eq!(deck.attention_filter.source_indexes.len(), 1_000);
        let window = AgentDeck::row_window(1_000, 500, 16);
        assert_eq!(window, 492..508);
        assert_eq!(
            &deck.attention_filter.source_indexes[window.clone()],
            &(492..508).collect::<Vec<_>>()
        );
        let allocation = deck.attention_filter.source_indexes.as_ptr();
        let rebuilds = deck.attention_filter.rebuilds;
        deck.refresh_attention_filter(&app);
        assert_eq!(
            deck.attention_filter.source_indexes.as_ptr(),
            allocation,
            "unchanged inbox and query reuse the cached index allocation"
        );
        assert_eq!(deck.attention_filter.rebuilds, rebuilds);

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| deck.render(frame, frame.area(), &app))
            .expect("first cached render");
        terminal
            .draw(|frame| deck.render(frame, frame.area(), &app))
            .expect("second cached render");
        assert_eq!(
            deck.attention_filter.rebuilds, rebuilds,
            "rendering with a stable revision and query does not rebuild the filter"
        );

        app.delegator
            .as_mut()
            .unwrap()
            .queue_attention(&run.agent_id, "question", "request after cache")
            .expect("mutate inbox");
        deck.refresh_attention_filter(&app);
        assert_eq!(deck.attention_filter.rebuilds, rebuilds + 1);
        assert_eq!(deck.attention_filter.source_indexes.len(), 1_001);

        deck.inbox_query = "match".to_string();
        deck.refresh_attention_filter(&app);
        assert_eq!(deck.attention_filter.source_indexes, vec![0, 500, 999]);
        let filtered_window = AgentDeck::row_window(3, 2, 2);
        assert_eq!(filtered_window, 1..3);
        assert_eq!(
            &deck.attention_filter.source_indexes[filtered_window],
            &[500, 999]
        );
    }

    #[test]
    fn inbox_refresh_keeps_selection_by_request_identity_when_source_indexes_shift() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let run = delegator
            .root(None, "scout", "inbox selection")
            .expect("run");
        let first = delegator
            .queue_attention(&run.agent_id, "question", "first")
            .expect("first request");
        let second = delegator
            .queue_attention(&run.agent_id, "question", "second")
            .expect("second request");
        let third = delegator
            .queue_attention(&run.agent_id, "question", "third")
            .expect("third request");
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.mode = Mode::Inbox;
        deck.refresh_attention_filter(&app);
        deck.inbox_selected = 2;
        deck.refresh_attention_filter(&app);

        app.delegator
            .as_mut()
            .unwrap()
            .take_attention(&first.id)
            .expect("remove the earlier request");
        app.delegator
            .as_mut()
            .unwrap()
            .queue_attention(&run.agent_id, "question", "fourth")
            .expect("append a request while preserving inbox length");
        deck.on_tick(&mut app);

        assert_eq!(deck.attention_filter.source_indexes.len(), 3);
        assert_eq!(deck.inbox_selected, 1);
        let selected_source = deck.attention_filter.source_indexes[deck.inbox_selected];
        assert_eq!(
            app.delegator.as_ref().unwrap().attention()[selected_source],
            third,
            "the selected request remains the same even when source indexes shift"
        );
        assert_ne!(
            app.delegator.as_ref().unwrap().attention()[selected_source],
            second,
            "selection must not silently stick to the old numeric index"
        );
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

    #[test]
    fn detail_renders_from_events_and_a_missing_result_is_not_recorded() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let run = delegator.root(None, "scout", "map the endpoints").unwrap();
        delegator.begin(&run.agent_id).unwrap();
        delegator
            .tool_boundary(&run.agent_id, "read_file src/auth.rs")
            .unwrap();

        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.refresh(&app);
        deck.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);

        let overview = deck.detail_lines(&app).join("\n");
        assert!(overview.contains("map the endpoints"), "{overview}");
        assert!(overview.contains("[tool_use]"), "{overview}");
        assert!(!overview.to_lowercase().contains("reasoning"), "{overview}");
        assert!(!overview.to_lowercase().contains("thinking"), "{overview}");

        // The Activity page is the ordered event list, with typed states and
        // the tool name — never a fabricated thought.
        deck.handle_key(
            KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE),
            &mut app,
        );
        let activity = deck.detail_lines(&app).join("\n");
        assert!(activity.contains("[started"), "{activity}");
        assert!(activity.contains("[tool_use"), "{activity}");
        assert!(
            activity.contains("tool: read_file src/auth.rs"),
            "{activity}"
        );
        assert!(!activity.to_lowercase().contains("reasoning"), "{activity}");
        assert!(!activity.to_lowercase().contains("thinking"), "{activity}");

        // No `run.done` was written, so the Result page says so honestly.
        deck.handle_key(
            KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE),
            &mut app,
        );
        assert_eq!(deck.detail_page, DetailPage::Result);
        assert_eq!(deck.detail_lines(&app), vec!["not recorded".to_string()]);

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| deck.render(frame, frame.area(), &app))
            .expect("draw run detail");
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("Overview"), "{text}");
        assert!(text.contains("Result"), "{text}");
        assert!(text.contains("not recorded"), "{text}");
        assert!(!text.to_lowercase().contains("reasoning"), "{text}");
    }

    #[test]
    fn deny_is_recorded_in_the_child_log_instead_of_being_dropped() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let run = delegator.root(None, "review", "check the change").unwrap();
        let request = delegator
            .queue_attention(&run.agent_id, "permission", "Read private fixtures?")
            .unwrap();

        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.mode = Mode::Inbox;
        deck.refresh_attention_filter(&app);
        assert_eq!(deck.selected_attention(&app).unwrap().id, request.id);

        let action = deck.handle_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &mut app,
        );

        assert_eq!(action, OverlayAction::None);
        assert!(
            app.delegator.as_ref().unwrap().attention().is_empty(),
            "the request is removed only after the decision is recorded"
        );
        let events = app
            .delegator
            .as_ref()
            .unwrap()
            .read_log(&run.agent_id)
            .unwrap();
        let decision = events
            .iter()
            .find(|event| event.event == "steer")
            .expect("a deny writes a steer decision to the child log");
        assert!(decision.detail.contains("deny"), "{}", decision.detail);
        assert!(decision.detail.contains(&request.id), "{}", decision.detail);
        assert_eq!(
            decision.state,
            memnoth_agent::delegate::event_schema::EventState::Steered
        );
        assert!(
            deck.review_notice
                .as_deref()
                .is_some_and(|n| n.contains("deny"))
        );
        assert!(
            app.focus_agent.is_none(),
            "a decision never silently opens focus"
        );
        assert!(
            app.entries
                .iter()
                .any(|entry| entry.role == EntryRole::Info && entry.text.contains("deny request"))
        );
    }

    #[test]
    fn allow_once_records_only_the_selected_request() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut delegator = memnoth_agent::Delegator::new(temp.path().join("agents"));
        let run = delegator.root(None, "review", "check the change").unwrap();
        let first = delegator
            .queue_attention(&run.agent_id, "permission", "Read fixtures?")
            .unwrap();
        let second = delegator
            .queue_attention(&run.agent_id, "question", "Which endpoint?")
            .unwrap();

        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        let mut deck = AgentDeck::new();
        deck.mode = Mode::Inbox;
        deck.refresh_attention_filter(&app);

        deck.handle_key(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            &mut app,
        );

        let remaining = app.delegator.as_ref().unwrap().attention();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, second.id);
        assert_ne!(remaining[0].id, first.id);
        let events = app
            .delegator
            .as_ref()
            .unwrap()
            .read_log(&run.agent_id)
            .unwrap();
        let decision = events.iter().find(|event| event.event == "steer").unwrap();
        assert!(
            decision.detail.contains("allow once"),
            "{}",
            decision.detail
        );
        assert!(decision.detail.contains(&first.id), "{}", decision.detail);
    }

    #[test]
    fn steer_without_a_runtime_shows_not_wired_instead_of_a_dead_text_box() {
        let mut app = App::new("s", false);
        let mut deck = AgentDeck::new();
        deck.children = vec![ChildInfo {
            agent_id: "run-1".to_string(),
            parent_id: None,
            root_id: Some("run-1".to_string()),
            session_id: None,
            role: "scout".to_string(),
            task: "inspect the API".to_string(),
            status: AgentStatus::Running,
            spawn_seq: 1,
            depth: 0,
            model: None,
            worktree: None,
            unread: true,
        }];
        deck.rebuild_order();
        deck.rebuild_filter();

        deck.handle_key(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
            &mut app,
        );

        assert!(deck.steer_open);
        assert!(!deck.steer.is_wired());
        assert!(deck.steer.not_wired.is_some());
        deck.handle_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut app,
        );
        assert!(
            deck.steer.draft.is_empty(),
            "no text is accepted when not wired"
        );

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| deck.render(frame, frame.area(), &app))
            .expect("draw not wired steer");
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("status: not wired"), "{text}");
        assert!(text.contains("needs:"), "{text}");
        assert!(text.contains("meanwhile:"), "{text}");
    }

    #[test]
    fn roster_status_glyphs_and_focus_marker_use_semantic_tokens() {
        let t = tokens();
        let child = |id: &str, status: AgentStatus| ChildInfo {
            agent_id: id.to_string(),
            parent_id: None,
            root_id: None,
            session_id: None,
            role: "worker".to_string(),
            task: "task".to_string(),
            status,
            spawn_seq: 0,
            depth: 0,
            model: None,
            worktree: None,
            unread: false,
        };
        let mut deck = AgentDeck::new();
        deck.children = vec![
            child("running", AgentStatus::Running),
            child("done", AgentStatus::Done),
            child("failed", AgentStatus::Failed),
            child("waiting", AgentStatus::Waiting),
        ];
        deck.rebuild_order();
        deck.rebuild_filter();
        deck.selected = 0;

        let app = App::new("s", false);
        let backend = ratatui::backend::TestBackend::new(120, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| deck.render(frame, frame.area(), &app))
            .expect("draw tokenised roster");
        let buffer = terminal.backend().buffer();

        // Colour is decoration: the glyphs still name the phase, and only the
        // focused row's marker reaches for the accent role.
        assert_eq!(first_cell_fg(buffer, "›"), Some(t.accent));
        assert_eq!(first_cell_fg(buffer, "✓"), Some(t.success));
        assert_eq!(first_cell_fg(buffer, "✗"), Some(t.error));
        assert_eq!(first_cell_fg(buffer, "?"), Some(t.warning));
        assert_eq!(first_cell_fg(buffer, "│"), Some(t.border));
        let text = buffer_text(buffer);
        assert!(text.contains("state: ▌ running"), "{text}");
        assert!(text.contains("SELECTED RUN"), "{text}");
    }
}
