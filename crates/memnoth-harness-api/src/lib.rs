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

/// Lifecycle event in the harness API stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiEvent {
    /// Text delta from an agent turn.
    TextDelta { session_id: String, text: String },
    /// Tool execution done.
    ToolDone {
        session_id: String,
        name: String,
        output: String,
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
    Error { message: String },
    /// File content.
    FileContent { path: String, content: String },
    /// Files listing.
    Files { paths: Vec<String> },
    /// File status.
    FileStatus { path: String, status: String },
    /// Session attached.
    Attached { session: SessionInfo },
    /// Session status.
    SessionStatus { status: String },
    /// Sessions listing.
    Sessions { sessions: Vec<SessionInfo> },
    /// History.
    History { messages: Vec<HistoryMessage> },
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
        server_name: String,
        version: String,
    },
    /// Ok acknowledgement.
    Ok,
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
    /// Close session.
    CloseSession { session_id: String },
}

/// Information about a harness API session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub workspace: Option<String>,
    pub model: Option<String>,
    pub created_at: u64,
    pub last_active: u64,
}

/// A message in the harness session history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub id: u64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub timestamp: u64,
}

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
            workspace: Some("/tmp".to_string()),
            model: Some("claude-3".to_string()),
            created_at: 1000,
            last_active: 2000,
        };
        let json = serde_json::to_string(&info).unwrap();
        let deserialized: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.session_id, "s-1");
    }
}
