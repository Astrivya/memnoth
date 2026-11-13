//! memnoth-harness-api — harness API protocol types for memnoth.
//!
//! Minimal viable implementation to unblock memnoth-mcp compilation.
//! Provides the protocol types used by `AgentBridge` for the harness API socket.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// API protocol major version.
pub const API_VERSION_MAJOR: u32 = 1;

/// Path to the harness API socket.
pub fn api_socket_path() -> PathBuf {
    runtime_dir().join("harness.sock")
}

/// Path to the leader socket.
pub fn leader_socket_path() -> PathBuf {
    runtime_dir().join("leader.sock")
}

/// Get the runtime directory for memnoth socket files.
pub fn runtime_dir() -> PathBuf {
    std::env::temp_dir().join(format!("memnoth-runtime-{}", std::process::id()))
}

/// Client-to-server request frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ClientFrame {
    pub id: u64,
    pub request: ApiRequest,
}

impl ClientFrame {
    /// Create a new client frame with the given id and request.
    pub fn new(id: u64, req: ApiRequest) -> Self {
        ClientFrame { id, request: req }
    }
}

/// Server-to-client response frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ServerFrame {
    pub reply_to: Option<u64>,
    pub event: ApiEvent,
}

impl ServerFrame {
    /// A reply to the client frame with `id`.
    pub fn reply(id: u64, event: ApiEvent) -> Self {
        ServerFrame {
            reply_to: Some(id),
            event,
        }
    }

    /// An unsolicited event, not tied to a request.
    pub fn event(event: ApiEvent) -> Self {
        ServerFrame {
            reply_to: None,
            event,
        }
    }
}

/// Stable error codes for a failed request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request name or shape is not implemented by this server.
    UnknownRequest,
}

/// Lifecycle event in the harness API stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiEvent {
    /// Text delta from an agent turn.
    TextDelta { session_id: String, text: String },
    /// Tool execution done.
    ToolDone {
        session_id: String,
        call_id: String,
        name: String,
        output: String,
        error: Option<String>,
    },
    /// Turn complete.
    TurnDone { session_id: String },
    /// Session closed.
    SessionClosed { session_id: String },
    /// Pong response.
    Pong,
    /// Unknown event.
    Unknown,
    /// Connection phase change.
    ConnectionPhase { phase: String },
    /// Background progress.
    BackgroundProgress { progress: f64 },
    /// Compacted state.
    Compacted,
    /// Credential updated.
    CredentialUpdated { key: String },
    /// Error event.
    Error { code: ErrorCode, message: String },
    /// File content.
    FileContent { path: String, content: String },
    /// Files listing.
    Files { paths: Vec<String> },
    /// File status.
    FileStatus { path: String, status: String },
    /// Session attached.
    Attached { session: SessionInfo },
    /// Session status.
    SessionStatus { session_id: String, status: String },
    /// Sessions listing.
    Sessions { sessions: Vec<SessionInfo> },
    /// History.
    History {
        session_id: String,
        messages: Vec<HistoryMessage>,
    },
    /// Reasoning delta.
    ReasoningDelta { session_id: String, delta: String },
    /// Reasoning done.
    ReasoningDone { session_id: String },
    /// Tool start.
    ToolStart { session_id: String, name: String },
    /// Tool input delta.
    ToolInputDelta {
        session_id: String,
        name: String,
        delta: String,
    },
    /// Tool exec.
    ToolExec { session_id: String, name: String },
    /// Token usage.
    TokenUsage { session_id: String, tokens: u64 },
    /// Message accepted.
    MessageAccepted { session_id: String },
    /// Permission request.
    PermissionRequest {
        tool_name: String,
        description: String,
    },
    /// Model info.
    ModelInfo { provider: String, model: String },
    /// Models listing.
    Models { models: Vec<String> },
    /// Runtime info.
    RuntimeInfo { version: String },
    /// Text matches.
    TextMatches { matches: Vec<String> },
    /// Session renamed.
    SessionRenamed { session_id: String, name: String },
    /// Hello ok.
    HelloOk {
        version: u32,
        server: String,
        capabilities: Vec<String>,
    },
    /// Ok acknowledgement.
    Ok,
    /// A benchmark suite run started (replies to `RunSuite`).
    SuiteStarted { run_id: u64, suite: BenchSuite },
    /// Progress within a suite run (one task finished).
    SuiteProgress {
        run_id: u64,
        task: String,
        done: usize,
        total: usize,
    },
    /// One task's mechanical verifier result.
    TaskVerified {
        run_id: u64,
        task: String,
        passed: bool,
        verifier: VerifierOutcome,
    },
    /// A leakage-control flag raised during a run.
    LeakageFlag {
        run_id: u64,
        task: String,
        kind: LeakageKind,
        detail: String,
    },
    /// A suite run completed with aggregate counts.
    SuiteCompleted {
        run_id: u64,
        passed: usize,
        failed: usize,
        flagged: usize,
    },
    /// Full result of a suite run, replies to `GetSuiteResult`.
    SuiteResult { run_id: u64, result: SuiteResult },
}

impl ApiEvent {
    /// A tool call started. The name is the model's tool, not a verb spelling.
    pub fn tool_started(session_id: impl Into<String>, name: impl Into<String>) -> Self {
        ApiEvent::ToolStart {
            session_id: session_id.into(),
            name: name.into(),
        }
    }

    /// The tool is about to execute, after the person has been asked.
    pub fn tool_executing(session_id: impl Into<String>, name: impl Into<String>) -> Self {
        ApiEvent::ToolExec {
            session_id: session_id.into(),
            name: name.into(),
        }
    }

    /// A tool call finished, with the text the model will see.
    ///
    /// `error` is `Some` only when the tool failed: an empty output and a
    /// failure are different facts, and a surface that renders them the same
    /// way teaches the model to keep retrying a broken call.
    pub fn tool_finished(
        session_id: impl Into<String>,
        call_id: impl Into<String>,
        name: impl Into<String>,
        output: impl Into<String>,
        error: Option<String>,
    ) -> Self {
        ApiEvent::ToolDone {
            session_id: session_id.into(),
            call_id: call_id.into(),
            name: name.into(),
            output: output.into(),
            error,
        }
    }

    /// A write tool needs the person's decision before it runs.
    ///
    /// Emitted *before* the tool executes, so a surface can show the request
    /// even when the approval machinery refuses for want of anyone to ask.
    pub fn permission_request(
        tool_name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        ApiEvent::PermissionRequest {
            tool_name: tool_name.into(),
            description: description.into(),
        }
    }

    /// Cumulative provider token count for the turn so far.
    pub fn token_usage(session_id: impl Into<String>, tokens: u64) -> Self {
        ApiEvent::TokenUsage {
            session_id: session_id.into(),
            tokens,
        }
    }

    /// The turn produced its final answer.
    pub fn turn_completed(session_id: impl Into<String>) -> Self {
        ApiEvent::TurnDone {
            session_id: session_id.into(),
        }
    }
}

/// A request to the harness API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiRequest {
    /// Create a session.
    Hello {
        min_version: u32,
        max_version: u32,
        client: String,
    },
    /// Create a new session.
    CreateSession { working_dir: Option<String> },
    /// Send a message.
    SendMessage {
        session_id: String,
        content: String,
        images: Vec<String>,
        no_reply: bool,
    },
    /// Set model.
    SetModel { session_id: String, model: String },
    /// Read a session's history.
    GetHistory { session_id: String },
    /// Close session.
    CloseSession { session_id: String },
    /// Run a benchmark suite (MTS-60, TRS, KG) through mechanical verifiers.
    RunSuite {
        suite: BenchSuite,
        filter: Option<String>,
    },
    /// Fetch the full result of a suite run started with `RunSuite`.
    GetSuiteResult { run_id: u64 },
}

/// Information about a harness API session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub working_dir: Option<String>,
    pub title: Option<String>,
    pub status: String,
    pub transcript_bytes: Option<u64>,
    pub archived: bool,
    pub archived_at_ms: Option<u64>,
}
/// A message in the harness session history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
}

/// A benchmark suite the harness can run over the socket protocol.
///
/// `mts` is MTS-60 (60 tasks, 6 categories × 10, mechanical verifiers) and
/// `trs` is the Temporal Recall Suite (40 seeded histories × 5 question types).
/// `kg` is the knowledge-graph benchmark reserved for future use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchSuite {
    /// MTS-60: agentic coding tasks, each with a mechanical verifier.
    Mts,
    /// TRS: temporal recall over seeded project histories, leakage-controlled.
    Trs,
    /// KG benchmark: entity/relation precision and recall.
    Kg,
}

/// Outcome of one mechanical verifier check on a single task.
///
/// A task passes only when the verifier is green, no forbidden file was
/// edited, and the produced patch applies to a clean checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierOutcome {
    /// The verifier passed and all gate conditions held.
    Pass,
    /// The verifier failed for the given reason.
    Fail { reason: String },
    /// A forbidden file was edited (anti-gaming rule).
    ForbiddenFileEdit { path: String },
    /// The produced patch does not apply to a clean checkout.
    PatchDoesNotApply,
}

/// The result of one task within a suite run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TaskResult {
    pub task_id: String,
    pub suite: BenchSuite,
    pub passed: bool,
    pub verifier: VerifierOutcome,
    pub wall_ms: u64,
}

/// Kind of leakage control flag raised during a run.
///
/// TRS requires the model to reach the seeded history through tools; a run
/// that answers correctly without a memory tool call is flagged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeakageKind {
    /// The task was answered without any memory/tool call (TRS leakage).
    NoMemoryToolCall,
    /// The seeded history appeared in the prompt (TRS leakage).
    HistoryInPrompt,
    /// A verifier file was edited (MTS anti-gaming rule).
    VerifierFileEdited,
}

/// A leakage-control flag raised during a suite run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LeakageFlag {
    pub task_id: String,
    pub kind: LeakageKind,
    pub detail: String,
}

/// Full result of a completed suite run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SuiteResult {
    pub run_id: u64,
    pub suite: BenchSuite,
    pub passed: usize,
    pub failed: usize,
    pub flagged: usize,
    pub tasks: Vec<TaskResult>,
    pub leakage: Vec<LeakageFlag>,
}

pub use benchmarks::{
    AblationAxis, AblationCell, AblationMatrix, CellState, Scoreboard, ScoreboardEntry,
};
pub mod benchmarks;
pub mod fuzz;
pub mod security;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_socket_path_returns_path() {
        let path = api_socket_path();
        assert!(path.file_name().unwrap().to_str().unwrap() == "harness.sock");
    }

    #[test]
    fn leader_socket_path_returns_path() {
        let path = leader_socket_path();
        assert!(path.file_name().unwrap().to_str().unwrap() == "leader.sock");
    }

    #[test]
    fn session_info_serialization() {
        let info = SessionInfo {
            session_id: "s-1".to_string(),
            working_dir: Some("/tmp".to_string()),
            title: Some("work".to_string()),
            status: "idle".to_string(),
            transcript_bytes: Some(2048),
            archived: false,
            archived_at_ms: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        let deserialized: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.session_id, "s-1");
    }

    #[test]
    fn bench_suite_serialization() {
        let json = serde_json::to_string(&BenchSuite::Trs).unwrap();
        assert_eq!(json, "\"trs\"");
        let parsed: BenchSuite = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, BenchSuite::Trs);
    }

    #[test]
    fn suite_result_serialization() {
        let result = SuiteResult {
            run_id: 1,
            suite: BenchSuite::Mts,
            passed: 57,
            failed: 3,
            flagged: 0,
            tasks: vec![],
            leakage: vec![],
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: SuiteResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.run_id, 1);
        assert_eq!(deserialized.passed, 57);
    }

    #[test]
    fn leakage_kind_serialization() {
        let json = serde_json::to_string(&LeakageKind::NoMemoryToolCall).unwrap();
        assert_eq!(json, "\"no_memory_tool_call\"");
        let parsed: LeakageKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, LeakageKind::NoMemoryToolCall);
    }

    #[test]
    fn verifier_outcome_forbidden_edit() {
        let outcome = VerifierOutcome::ForbiddenFileEdit {
            path: "verifier.rs".into(),
        };
        assert!(!matches!(outcome, VerifierOutcome::Pass));
    }

    /// The turn lifecycle helpers exist so a driver does not hand-build eight
    /// variants with eight `session_id` clones, which is how one of them ends up
    /// with an empty id on the wire.
    #[test]
    fn lifecycle_helpers_carry_the_session_and_the_reason() {
        let event = ApiEvent::tool_finished("s-1", "c-1", "kg_search", "hit", Some("boom".into()));
        match &event {
            ApiEvent::ToolDone {
                session_id,
                call_id,
                name,
                output,
                error,
            } => {
                assert_eq!(session_id, "s-1");
                assert_eq!(call_id, "c-1");
                assert_eq!(name, "kg_search");
                assert_eq!(output, "hit");
                assert_eq!(error.as_deref(), Some("boom"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
        // The wire name is the variant, not the helper: a surface that switched
        // on `tool_finished` would break the moment the helper was renamed.
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("tool_done").is_some(), "{json}");

        assert!(matches!(
            ApiEvent::permission_request("edit_file", "edit a.txt"),
            ApiEvent::PermissionRequest { .. }
        ));
        assert!(matches!(
            ApiEvent::turn_completed("s-1"),
            ApiEvent::TurnDone { .. }
        ));
        assert!(matches!(
            ApiEvent::token_usage("s-1", 42),
            ApiEvent::TokenUsage { tokens: 42, .. }
        ));
        assert!(matches!(
            ApiEvent::tool_started("s-1", "kg_impact"),
            ApiEvent::ToolStart { .. }
        ));
        assert!(matches!(
            ApiEvent::tool_executing("s-1", "kg_impact"),
            ApiEvent::ToolExec { .. }
        ));
    }
}
