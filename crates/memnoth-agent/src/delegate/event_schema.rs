//! Additive, versioned schema for child-log events.
//!
//! This module holds no I/O. It owns the event-kind vocabulary, the schema
//! version, the producer label and the typed lifecycle state carried by every
//! [`super::ChildEvent`]. Keeping the vocabulary in one place means a log
//! writer and a Trace reader cannot drift apart, and it gives the serde
//! defaults used for old logs a single documented home.
//!
//! Version 1 adds a stable id, a per-run sequence, run/session/turn/parent
//! ids, a source label and a typed state to the four original fields. Version
//! 0 is an old line with only `event`, `agent_id`, `at_ms` and `detail`; it
//! parses with every new field left at its default. A missing event history
//! means "not recorded", never "no tools ran".

use serde::{Deserialize, Deserializer, Serialize};

/// Current child-event schema version. Old lines default to `0`.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Producer label for rows written by the delegator itself.
///
/// A `tool.use` row with this source is a fact observed at a tool boundary.
/// It is not a tool result and not reasoning; neither is stored in the log.
pub const SOURCE_DELEGATOR: &str = "delegator";

/// A spawn was recorded. A spawn is not execution: no tool has run yet.
pub const RUN_SPAWN: &str = "run.spawn";
/// An executor actually picked the run up and began executing it.
pub const RUN_START: &str = "run.start";
/// A tool boundary was crossed. A fact about tool *use*, never a result.
pub const TOOL_USE: &str = "tool.use";
/// The run finished successfully.
pub const RUN_DONE: &str = "run.done";
/// The run failed.
pub const RUN_FAILED: &str = "run.failed";
/// The run was cancelled.
pub const RUN_CANCELLED: &str = "run.cancelled";
/// One child was stopped from the deck.
pub const RUN_STOP: &str = "run.stop";
/// A stopped child was revived in place.
pub const RUN_REVIVE: &str = "run.revive";
/// A person steered the child with a note.
pub const STEER: &str = "steer";
/// A permission/question was queued for the root inbox.
pub const ATTENTION_QUEUED: &str = "attention.queued";

/// Typed lifecycle state attached to a child event.
///
/// This is deliberately a small, closed vocabulary. The free-form `event`
/// string stays as the kind for backward compatibility; `state` is the typed
/// projection a reader can match on without string comparisons. An unknown
/// kind or an unknown serialised state degrades to [`EventState::Unknown`]
/// rather than failing to parse a whole log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventState {
    /// No state was recorded (old line, unknown kind, or future state).
    #[default]
    Unknown,
    /// A spawn was recorded; nothing has executed.
    Spawned,
    /// Execution actually began.
    Started,
    /// A tool boundary was crossed (a fact, not a result).
    ToolUse,
    /// The run completed successfully.
    Completed,
    /// The run failed.
    Failed,
    /// The run was cancelled.
    Cancelled,
    /// One child was stopped from the deck.
    Stopped,
    /// A stopped child was revived in place.
    Revived,
    /// A person steered the child.
    Steered,
    /// A permission/question was queued for the root inbox.
    AttentionQueued,
}

impl EventState {
    /// Parse a serialised state, degrading unknown values to [`Self::Unknown`].
    pub fn parse(raw: &str) -> Self {
        match raw {
            "spawned" => Self::Spawned,
            "started" => Self::Started,
            "tool_use" => Self::ToolUse,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "stopped" => Self::Stopped,
            "revived" => Self::Revived,
            "steered" => Self::Steered,
            "attention_queued" => Self::AttentionQueued,
            _ => Self::Unknown,
        }
    }

    /// The stable serialised label for this state.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Spawned => "spawned",
            Self::Started => "started",
            Self::ToolUse => "tool_use",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Stopped => "stopped",
            Self::Revived => "revived",
            Self::Steered => "steered",
            Self::AttentionQueued => "attention_queued",
        }
    }
}

/// The typed state a newly written event kind carries.
///
/// Unknown kinds map to [`EventState::Unknown`] so a future kind never makes
/// an old reader fail, and never gets mistaken for a terminal state.
pub fn state_for_kind(kind: &str) -> EventState {
    match kind {
        RUN_SPAWN => EventState::Spawned,
        RUN_START => EventState::Started,
        TOOL_USE => EventState::ToolUse,
        RUN_DONE => EventState::Completed,
        RUN_FAILED => EventState::Failed,
        RUN_CANCELLED => EventState::Cancelled,
        RUN_STOP => EventState::Stopped,
        RUN_REVIVE => EventState::Revived,
        STEER => EventState::Steered,
        ATTENTION_QUEUED => EventState::AttentionQueued,
        _ => EventState::Unknown,
    }
}

/// Lenient deserializer for a `state` field: an unrecognised value becomes
/// [`EventState::Unknown`] instead of failing the whole log line.
pub fn deserialize_state<'de, D>(deserializer: D) -> Result<EventState, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(EventState::parse(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_kind_has_a_typed_state() {
        assert_eq!(state_for_kind(RUN_SPAWN), EventState::Spawned);
        assert_eq!(state_for_kind(RUN_START), EventState::Started);
        assert_eq!(state_for_kind(TOOL_USE), EventState::ToolUse);
        assert_eq!(state_for_kind(RUN_DONE), EventState::Completed);
        assert_eq!(state_for_kind(RUN_FAILED), EventState::Failed);
        assert_eq!(state_for_kind(RUN_CANCELLED), EventState::Cancelled);
        assert_eq!(state_for_kind(RUN_STOP), EventState::Stopped);
        assert_eq!(state_for_kind(RUN_REVIVE), EventState::Revived);
        assert_eq!(state_for_kind(STEER), EventState::Steered);
        assert_eq!(
            state_for_kind(ATTENTION_QUEUED),
            EventState::AttentionQueued
        );
        assert_eq!(state_for_kind("some.future.kind"), EventState::Unknown);
    }

    #[test]
    fn a_tool_use_state_round_trips_as_a_fact() {
        let json = serde_json::to_string(&EventState::ToolUse).expect("serialize");
        assert_eq!(json, "\"tool_use\"");
        assert_eq!(
            EventState::parse(json.trim_matches('"')),
            EventState::ToolUse
        );
        assert_ne!(EventState::ToolUse, EventState::Completed);
    }

    #[test]
    fn unknown_and_missing_states_default_without_failing() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(default, deserialize_with = "deserialize_state")]
            state: EventState,
        }
        let unknown: Holder = serde_json::from_str(r#"{"state":"future.state"}"#).expect("value");
        assert_eq!(unknown.state, EventState::Unknown);
        let missing: Holder = serde_json::from_str("{}").expect("missing");
        assert_eq!(missing.state, EventState::Unknown);
        assert_eq!(EVENT_SCHEMA_VERSION, 1);
    }
}
