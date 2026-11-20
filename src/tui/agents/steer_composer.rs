//! Steer composer for the Agent Deck.
//!
//! The real steer path is [`memnoth_agent::Delegator::steer`]: it appends a
//! `steer` event to the child's own JSONL, which the trace and the run detail
//! then read back. There is no placeholder note. When that path is not
//! available — no delegator, or a run that can no longer accept direction —
//! the composer shows the same honest "not wired" shape the registered verbs
//! use, never a text box that would silently swallow what was typed.

use crate::tui::agents::ChildInfo;
use crate::tui::app::{App, EntryRole};

/// Whether the current runtime can actually deliver a steer to this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerCapability {
    Available,
    NotWired,
}

/// A delegator that owns the run and a child still able to take direction is
/// the whole contract. A finished/failed/stopped run cannot be steered, and
/// claiming otherwise would be a fake affordance.
pub fn capability(app: &App, child: &ChildInfo) -> SteerCapability {
    if app.delegator.is_some() && child.status.accepting_input() {
        SteerCapability::Available
    } else {
        SteerCapability::NotWired
    }
}

/// The honest gap message, in the same shape as the command layer's not-wired
/// answer: what was asked, what is missing, and what works today.
pub fn not_wired_message(child: &ChildInfo) -> String {
    format!(
        "steer {}\n  status: not wired\n  needs: a delegator and a running or waiting run that can accept direction\n  meanwhile: open the run to read its recorded activity and result",
        child.agent_id
    )
}

/// State for one steer attempt. Kept on the deck instance; no global state.
#[derive(Debug, Default)]
pub struct SteerComposer {
    /// The run a submitted note would steer.
    pub target: Option<String>,
    /// The note the person is actually typing.
    pub draft: String,
    /// Short status line after a submit attempt.
    pub notice: Option<String>,
    /// Set instead of the draft when the runtime cannot accept a steer.
    pub not_wired: Option<String>,
}

impl SteerComposer {
    /// Open a composer for `child`, or record the not-wired state. Clears any
    /// previous draft so a stale note cannot be sent to a different run.
    pub fn open_for(&mut self, app: &App, child: &ChildInfo) {
        self.draft.clear();
        self.notice = None;
        self.not_wired = None;
        self.target = None;
        match capability(app, child) {
            SteerCapability::Available => self.target = Some(child.agent_id.clone()),
            SteerCapability::NotWired => self.not_wired = Some(not_wired_message(child)),
        }
    }

    /// Whether a typed note would go anywhere.
    pub fn is_wired(&self) -> bool {
        self.target.is_some() && self.not_wired.is_none()
    }

    pub fn push_char(&mut self, ch: char) {
        if self.is_wired() {
            self.draft.push(ch);
        }
    }

    pub fn backspace(&mut self) {
        if self.is_wired() {
            self.draft.pop();
        }
    }

    pub fn clear(&mut self) {
        if self.is_wired() {
            self.draft.clear();
        }
    }

    /// Deliver the draft through the real delegator path. Returns `true` only
    /// when a `steer` event was actually written. A blank note is refused
    /// rather than sent as a placeholder.
    pub fn submit(&mut self, app: &mut App) -> bool {
        let Some(target) = self.target.clone() else {
            return false;
        };
        let note = self.draft.trim().to_string();
        if note.is_empty() {
            self.notice = Some("type a direction before Enter".to_string());
            return false;
        }
        let Some(delegator) = app.delegator.as_mut() else {
            self.not_wired = Some(not_wired_message_for(&target));
            return false;
        };
        match delegator.steer(&target, &note) {
            Ok(()) => {
                app.push(EntryRole::Info, format!("steered {target}: {note}"));
                self.notice = Some("steer request recorded in the child log".to_string());
                self.draft.clear();
                true
            }
            Err(error) => {
                app.push(EntryRole::Error, format!("steer {target}: {error}"));
                self.notice = Some(format!("steer failed: {error}"));
                false
            }
        }
    }

    /// Lines for the composer panel: either the not-wired answer, or the
    /// composer with the exact note being written.
    pub fn lines(&self) -> Vec<String> {
        if let Some(not_wired) = &self.not_wired {
            return not_wired.lines().map(str::to_string).collect();
        }
        let mut lines = vec![
            format!("Steer {}", self.target.as_deref().unwrap_or("(no run)")),
            String::new(),
            "Write the direction the child should take.".to_string(),
            String::new(),
            format!("> {}\u{2588}", self.draft),
        ];
        if let Some(notice) = &self.notice {
            lines.push(String::new());
            lines.push(notice.clone());
        }
        lines
    }
}

fn not_wired_message_for(run_id: &str) -> String {
    format!(
        "steer {run_id}\n  status: not wired\n  needs: a delegator that owns this run\n  meanwhile: open the run to read its recorded activity and result"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::agents::AgentStatus;

    fn child(status: AgentStatus) -> ChildInfo {
        ChildInfo {
            agent_id: "run-1".to_string(),
            parent_id: None,
            root_id: Some("run-1".to_string()),
            session_id: None,
            role: "scout".to_string(),
            task: "inspect the API".to_string(),
            status,
            spawn_seq: 1,
            depth: 0,
            model: None,
            worktree: None,
            unread: true,
        }
    }

    fn app_with_delegator(temp: &std::path::Path) -> App {
        let mut delegator = memnoth_agent::Delegator::new(temp.join("agents"));
        delegator
            .root(None, "scout", "inspect the API")
            .expect("run");
        let mut app = App::new("s", false);
        app.delegator = Some(delegator);
        app
    }

    #[test]
    fn deck_steer_emits_a_real_request_when_the_path_exists() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut app = app_with_delegator(temp.path());
        let run = crate::tui::agents::roster(&app)[0].clone();
        let mut composer = SteerComposer::default();
        composer.open_for(&app, &run);
        assert!(composer.is_wired());
        for ch in "follow the refresh callers".chars() {
            composer.push_char(ch);
        }

        assert!(composer.submit(&mut app));

        let id = run.agent_id.clone();
        let events = app.delegator.as_ref().unwrap().read_log(&id).unwrap();
        let steer = events.iter().find(|event| event.event == "steer").unwrap();
        assert_eq!(steer.detail, "follow the refresh callers");
        assert_eq!(
            steer.state,
            memnoth_agent::delegate::event_schema::EventState::Steered
        );
        assert!(composer.draft.is_empty());
    }

    #[test]
    fn deck_steer_shows_not_wired_instead_of_a_dead_text_box() {
        let app = App::new("s", false);
        let run = child(AgentStatus::Done);
        let mut composer = SteerComposer::default();
        composer.open_for(&app, &run);

        assert!(!composer.is_wired());
        assert!(composer.not_wired.is_some());
        let text = composer.lines().join("\n");
        assert!(text.contains("status: not wired"), "{text}");
        assert!(text.contains("needs:"), "{text}");
        assert!(text.contains("meanwhile:"), "{text}");
        composer.push_char('x');
        assert!(
            composer.draft.is_empty(),
            "a dead composer never accepts text"
        );
    }

    #[test]
    fn deck_steer_blank_note_is_never_sent_as_a_placeholder() {
        let temp = tempfile::tempdir().expect("temporary agent directory");
        let mut app = app_with_delegator(temp.path());
        let run = crate::tui::agents::roster(&app)[0].clone();
        let mut composer = SteerComposer::default();
        composer.open_for(&app, &run);
        assert!(!composer.submit(&mut app));
        let id = run.agent_id.clone();
        let events = app.delegator.as_ref().unwrap().read_log(&id).unwrap();
        assert!(!events.iter().any(|event| event.event == "steer"));
    }
}
