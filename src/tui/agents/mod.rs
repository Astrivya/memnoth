//! Agent subsystem: child-session deck, focus, trace, and result cards.
//!
//! The parent session keeps the conversation; the deck shows the fleet of
//! child agents; focus gives one child the composer; trace shows the
//! delegation tree. Child-session persistence and status events belong to
//! `memnoth-agent`.

pub mod cards;
pub mod deck;
pub mod detail_view;
pub mod focus;
pub mod goal;
pub mod plan;
pub mod steer_composer;
pub mod timeline_model;
pub mod trace;

/// Status of a child agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentStatus {
    /// The child is starting up.
    #[default]
    Starting,
    /// The child is actively running a turn.
    Running,
    /// The child is waiting for the person (approval, steer, queue).
    Waiting,
    /// The child finished successfully.
    Done,
    /// The child failed.
    Failed,
    /// The child was killed by the person.
    Killed,
    /// The child process disappeared without a terminal lifecycle event.
    Orphaned,
    /// The child is parked (suspended, resumable).
    Parked,
}

impl AgentStatus {
    /// Glyph for the status, used in deck rows and trace nodes.
    pub fn glyph(self) -> &'static str {
        match self {
            AgentStatus::Starting => "…",
            AgentStatus::Running => "▌",
            AgentStatus::Waiting => "?",
            AgentStatus::Done => "✓",
            AgentStatus::Failed => "✗",
            AgentStatus::Killed => "☓",
            AgentStatus::Orphaned => "!",
            AgentStatus::Parked => "‖",
        }
    }

    /// Human-readable lifecycle state shown in selected-run details.
    pub fn label(self) -> &'static str {
        match self {
            AgentStatus::Starting => "starting",
            AgentStatus::Running => "running",
            AgentStatus::Waiting => "waiting",
            AgentStatus::Done => "returned",
            AgentStatus::Failed => "failed",
            AgentStatus::Killed => "stopped",
            AgentStatus::Orphaned => "orphaned",
            AgentStatus::Parked => "parked",
        }
    }

    /// Whether the child can accept new input.
    pub fn accepting_input(self) -> bool {
        matches!(self, AgentStatus::Running | AgentStatus::Waiting)
    }
}

/// The live roster, read from the session's delegator.
///
/// The Deck and the Trace both draw this list; neither invents a row. An empty
/// roster means nothing has been delegated, and the screens say so.
///
/// Rows are in spawn order, not registry order: the delegator stores nodes in
/// a HashMap, so an unsorted roster put a random run at index 0 and the Deck
/// opened on the session or the child by a per-process coin flip. Both the
/// deck and the trace select from this one list, so stability belongs here.
pub fn roster(app: &crate::tui::app::App) -> Vec<ChildInfo> {
    let Some(delegator) = app.delegator.as_ref() else {
        return Vec::new();
    };
    let mut roster: Vec<ChildInfo> = delegator
        .nodes()
        .into_iter()
        .map(|node| {
            let status = delegator
                .registry()
                .get(&node.agent_id)
                .map(|entry| AgentStatus::from_run_status(entry.status))
                .unwrap_or(AgentStatus::Starting);
            ChildInfo {
                agent_id: node.agent_id.clone(),
                parent_id: node.parent_id.clone(),
                root_id: Some(node.root_id.clone()),
                session_id: node.session_id.clone(),
                role: node.agent.clone(),
                task: node.task.clone(),
                status,
                spawn_seq: node.spawn_seq,
                depth: node.depth,
                model: node.model.clone(),
                worktree: node.worktree.clone(),
                unread: true,
            }
        })
        .collect();
    roster.sort_by_key(|child| child.spawn_seq);
    roster
}

impl AgentStatus {
    fn from_run_status(status: memnoth_agent::runtime::RunStatus) -> Self {
        match status {
            memnoth_agent::runtime::RunStatus::Running => AgentStatus::Running,
            memnoth_agent::runtime::RunStatus::Done => AgentStatus::Done,
            memnoth_agent::runtime::RunStatus::Failed => AgentStatus::Failed,
            memnoth_agent::runtime::RunStatus::Cancelled => AgentStatus::Killed,
            memnoth_agent::runtime::RunStatus::Orphaned => AgentStatus::Orphaned,
            memnoth_agent::runtime::RunStatus::Pending => AgentStatus::Waiting,
        }
    }
}

/// One child agent in the deck.
#[derive(Debug, Clone)]
pub struct ChildInfo {
    pub agent_id: String,
    pub parent_id: Option<String>,
    pub root_id: Option<String>,
    pub session_id: Option<String>,
    pub role: String,
    pub task: String,
    pub status: AgentStatus,
    pub spawn_seq: u64,
    pub depth: usize,
    pub model: Option<String>,
    pub worktree: Option<String>,
    pub unread: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_roster_reads_the_delegator_and_names_every_child() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut app = crate::tui::app::App::new("s", false);
        let mut delegator = memnoth_agent::Delegator::new(dir.path());
        let root = delegator.root(Some("s"), "memnoth", "session").unwrap();
        let child = delegator
            .spawn(
                Some(&root.agent_id),
                Some("s"),
                "scout",
                "find the redirect",
            )
            .unwrap();
        app.delegator = Some(delegator);
        app.delegation_root = Some(root.agent_id.clone());

        let rows = roster(&app);
        assert_eq!(rows.len(), 2, "root and child");
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].agent_id, child.agent_id);
        assert_eq!(rows[1].task, "find the redirect");
        // A spawned run is pending until an executor calls `begin`.
        assert_eq!(rows[1].status, AgentStatus::Waiting);
    }

    #[test]
    fn an_app_without_a_delegator_has_an_empty_roster() {
        let app = crate::tui::app::App::new("s", false);
        assert!(roster(&app).is_empty());
    }

    #[test]
    fn persisted_orphaned_status_remains_distinct_from_user_stop() {
        let orphaned = AgentStatus::from_run_status(memnoth_agent::RunStatus::Orphaned);
        let cancelled = AgentStatus::from_run_status(memnoth_agent::RunStatus::Cancelled);

        assert_eq!(orphaned, AgentStatus::Orphaned);
        assert_eq!(orphaned.label(), "orphaned");
        assert_eq!(cancelled, AgentStatus::Killed);
        assert_eq!(cancelled.label(), "stopped");
    }
}
