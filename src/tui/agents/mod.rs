//! Agent subsystem: child-session deck, focus, trace, and result cards.
//!
//! The parent session keeps the conversation; the deck shows the fleet of
//! child agents; focus gives one child the composer; trace shows the
//! delegation tree. Child-session persistence and status events belong to
//! `memnoth-agent`.

pub mod cards;
pub mod deck;
pub mod focus;
pub mod goal;
pub mod plan;
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
            AgentStatus::Parked => "‖",
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
pub fn roster(app: &crate::tui::app::App) -> Vec<ChildInfo> {
    let Some(delegator) = app.delegator.as_ref() else {
        return Vec::new();
    };
    delegator
        .nodes()
        .into_iter()
        .map(|node| {
            let status = delegator
                .registry()
                .get(&node.agent_id)
                .map(|entry| match entry.status {
                    memnoth_agent::runtime::RunStatus::Running => AgentStatus::Running,
                    memnoth_agent::runtime::RunStatus::Done => AgentStatus::Done,
                    memnoth_agent::runtime::RunStatus::Failed => AgentStatus::Failed,
                    memnoth_agent::runtime::RunStatus::Cancelled
                    | memnoth_agent::runtime::RunStatus::Orphaned => AgentStatus::Killed,
                    memnoth_agent::runtime::RunStatus::Pending => AgentStatus::Waiting,
                })
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
        .collect()
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
}
