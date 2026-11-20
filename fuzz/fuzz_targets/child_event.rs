//! Fuzz target: child-event v1 NDJSON deserialization mirror (W12-04).
//!
//! `cargo fuzz run child_event`
//!
//! LIMITATION — this target MIRRORS the public `ChildEvent` deserialization
//! shape from `crates/memnoth-agent/src/delegate.rs` and the lenient `state`
//! deserializer from `.../delegate/event_schema.rs`. It does not call them:
//! they are not re-exported through `memnoth-harness-api`, the only protocol
//! crate this fuzz crate depends on, and adding `memnoth-agent` as a new
//! dependency is outside this crate's edit scope. The replay loader
//! (`delegate::event_replay::replay_events`) is likewise unreachable from this
//! crate, so loader integration is a follow-up. What this target does prove is
//! the deserialization contract the loader relies on:
//!
//!   * legacy v0 lines (only `event`, `agent_id`, `at_ms`, `detail`) parse with
//!     additive fields defaulted (`seq == 0`, empty `id`, `version == 0`,
//!     `state == Unknown`);
//!   * unknown `state` strings degrade to `Unknown` rather than failing a
//!     line, while a non-string `state` fails the line;
//!   * malformed lines are reported (counted) and never counted as valid
//!     events; every non-blank line lands in exactly one bucket;
//!   * a parsed event round-trips through serialization without a panic.

#![no_main]

use std::sync::Once;

use libfuzzer_sys::fuzz_target;
use serde::{Deserialize, Deserializer, Serialize};

const EVENT_SCHEMA_VERSION: u32 = 1;

/// Mirror of `delegate::event_schema::EventState`: a closed vocabulary that
/// degrades unknown values to `Unknown` instead of failing the whole line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MirrorState {
    #[default]
    Unknown,
    Spawned,
    Started,
    ToolUse,
    Completed,
    Failed,
    Cancelled,
    Stopped,
    Revived,
    Steered,
    AttentionQueued,
}

impl MirrorState {
    fn parse(raw: &str) -> Self {
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
}

fn deserialize_state<'de, D>(deserializer: D) -> Result<MirrorState, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(MirrorState::parse(&raw))
}

/// Mirror of `delegate::ChildEvent`. The first four fields are mandatory; all
/// v1 additions carry a serde default so old logs load unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MirrorChildEvent {
    event: String,
    agent_id: String,
    at_ms: i64,
    detail: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    run_id: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    source: String,
    #[serde(default, deserialize_with = "deserialize_state")]
    state: MirrorState,
    #[serde(default)]
    version: u32,
}

/// Classification of one NDJSON input. `lines` must always equal
/// `blank + valid + malformed.len()` — no line is silently dropped.
#[derive(Debug)]
struct Outcome {
    lines: usize,
    blank: usize,
    valid: usize,
    /// One-based line numbers that failed to deserialize. These are reported,
    /// never admitted to the `valid` count.
    malformed: Vec<usize>,
}

fn replay_deserialize(data: &[u8]) -> Outcome {
    let text = String::from_utf8_lossy(data);
    let mut out = Outcome {
        lines: 0,
        blank: 0,
        valid: 0,
        malformed: Vec::new(),
    };
    for (index, line) in text.lines().enumerate() {
        out.lines += 1;
        if line.trim().is_empty() {
            out.blank += 1;
            continue;
        }
        match serde_json::from_str::<MirrorChildEvent>(line) {
            Ok(event) => {
                // A parsed event must round-trip. Both sides are our own type,
                // so use `if let` rather than `unwrap` to stay panic-free.
                if let Ok(encoded) = serde_json::to_string(&event)
                    && let Ok(again) = serde_json::from_str::<MirrorChildEvent>(&encoded)
                {
                    debug_assert_eq!(event, again);
                }
                // Read every field so the mirror stays a faithful, complete
                // copy of the public struct, not a trimmed subset.
                let _ = (
                    event.event.len(),
                    event.agent_id.len(),
                    event.at_ms,
                    event.detail.len(),
                    event.id.len(),
                    event.seq,
                    event.run_id.len(),
                    event.session_id.is_some(),
                    event.turn_id.is_some(),
                    event.parent_id.is_some(),
                    event.source.len(),
                    event.state,
                    event.version,
                );
                out.valid += 1;
            }
            Err(_) => {
                // A JSON value can still be an invalid event (missing a
                // mandatory field, wrong type, non-string state). It goes in
                // the malformed bucket and nowhere else.
                let _ = serde_json::from_str::<serde_json::Value>(line).is_ok();
                out.malformed.push(index + 1);
            }
        }
    }
    out
}

static SELF_CHECK: Once = Once::new();

/// Prove the mirror matches the documented v0/v1 behaviour before fuzzing.
fn self_check() {
    SELF_CHECK.call_once(|| {
        let legacy = r#"{"event":"run.start","agent_id":"old","at_ms":1,"detail":"go"}"#;
        let event: MirrorChildEvent =
            serde_json::from_str(legacy).expect("legacy v0 line must parse");
        assert_eq!(event.seq, 0);
        assert_eq!(event.id, "");
        assert_eq!(event.version, 0);
        assert_eq!(event.state, MirrorState::Unknown);

        let v1 = r#"{"event":"tool.use","agent_id":"r","at_ms":2,"detail":"read","id":"r:1","seq":1,"run_id":"r","source":"delegator","state":"tool_use","version":1}"#;
        let event: MirrorChildEvent = serde_json::from_str(v1).expect("v1 line must parse");
        assert_eq!(event.id, "r:1");
        assert_eq!(event.seq, 1);
        assert_eq!(event.version, EVENT_SCHEMA_VERSION);
        assert_eq!(event.state, MirrorState::ToolUse);

        let future_state = r#"{"event":"x","agent_id":"r","at_ms":0,"detail":"","state":"future.state"}"#;
        let event: MirrorChildEvent =
            serde_json::from_str(future_state).expect("unknown state degrades, not fails");
        assert_eq!(event.state, MirrorState::Unknown);

        // Malformed lines are reported, never accepted as valid events. A
        // non-string `state` fails the line in the real schema too.
        let out = replay_deserialize(b"not json\n");
        assert_eq!(out.valid, 0);
        assert_eq!(out.malformed, vec![1]);
        let out = replay_deserialize(
            br#"{"event":"x","agent_id":"r","at_ms":0,"detail":"","state":7}"#,
        );
        assert_eq!(out.valid, 0);
        assert_eq!(out.malformed, vec![1]);
    });
}

fuzz_target!(|data: &[u8]| {
    self_check();
    let outcome = replay_deserialize(data);
    // Every line is accounted for exactly once; a malformed line can never be
    // counted as a valid event (the two buckets are disjoint by construction).
    debug_assert_eq!(
        outcome.lines,
        outcome.blank + outcome.valid + outcome.malformed.len()
    );
    let _ = std::hint::black_box(outcome);
});
