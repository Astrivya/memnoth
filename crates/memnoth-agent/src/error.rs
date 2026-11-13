//! Error codes for delegation, in one place.
//!
//! Every refusal in this crate and in `memnoth-coord` names a code from
//! [`ErrorCode`]. The codes are the same strings the deep design pins
//! (`plans/10a-deep-design.md` S-DLG-10), so a test can assert a code rather
//! than a sentence someone will reword next week. [`ErrorCode::as_str`] is the
//! only place the wire spelling exists; nothing else may spell one by hand.

use std::fmt;
use std::path::{Path, PathBuf};

/// A typed refusal.
///
/// `file` and `line` are always present when the refusal came from a
/// definition on disk, because "unknown field" without a location is a bug
/// report the user cannot act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentError {
    /// The stable code.
    pub code: ErrorCode,
    /// The file the refusal came from, when it came from a file.
    pub file: Option<PathBuf>,
    /// The 1-based line in `file`, when the refusal came from a file.
    pub line: Option<usize>,
    /// The human explanation, without the code or the location.
    pub message: String,
}

impl AgentError {
    /// A refusal with no file attached (runtime failures, policy decisions).
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            file: None,
            line: None,
            message: message.into(),
        }
    }

    /// A refusal located at `file:line`.
    pub fn at(
        code: ErrorCode,
        file: impl AsRef<Path>,
        line: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            file: Some(file.as_ref().to_path_buf()),
            line: Some(line),
            message: message.into(),
        }
    }

    /// Attach or replace the location.
    pub fn with_location(mut self, file: impl AsRef<Path>, line: usize) -> Self {
        self.file = Some(file.as_ref().to_path_buf());
        self.line = Some(line);
        self
    }

    /// The stable code as it appears in JSON and in a terminal.
    pub fn code_str(&self) -> &'static str {
        self.code.as_str()
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.file, self.line) {
            (Some(file), Some(line)) => {
                write!(
                    f,
                    "{} at {}:{}: {}",
                    self.code.as_str(),
                    file.display(),
                    line,
                    self.message
                )
            }
            (Some(file), None) => {
                write!(
                    f,
                    "{} at {}: {}",
                    self.code.as_str(),
                    file.display(),
                    self.message
                )
            }
            _ => write!(f, "{}: {}", self.code.as_str(), self.message),
        }
    }
}

impl std::error::Error for AgentError {}

/// The stable code set.
///
/// Adding a code is a compatible change; renaming one is not, because tests
/// and the failure taxonomy both name these strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// The name is missing, malformed, or not unique after precedence.
    Name,
    /// The description is missing or over-long.
    Description,
    /// The prompt (markdown body) is empty.
    Prompt,
    /// A tool pattern does not parse, or is an unanchored `mcp__*`.
    Tools,
    /// An error raised while reading the front matter delimiters.
    FrontMatter,
    /// A front-matter key this build does not know.
    UnknownField,
    /// A field's value is outside its documented range.
    Range,
    /// The `Agent(...)` relation is cyclic, or names the spec itself.
    Cycle,
    /// The spec names a model this build cannot resolve.
    Model,
    /// `isolation: worktree` without a git work tree.
    Worktree,
    /// The run's files did not lie under its isolation root, or a worktree
    /// command failed.
    WorktreeFailed,
    /// The worktree was dirty after a run that claimed to be clean.
    WorktreeDirty,
    /// The spec is invalid for a reason the taxonomy does not split further.
    SpecInvalid,
    /// A child would be more powerful than its parent (INV-P).
    PermissionEscalation,
    /// The session's budget cannot cover a request, refused before asking.
    Budget,
    /// A finding arrived without `path:line` or a command.
    FindingUnevidenced,
    /// A belief write arrived without `{run_id, agent, user, turn, file, line}`.
    ProvenanceMissing,
    /// The belief duplicates a pending or accepted one; dropped, not queued.
    DuplicateBelief,
    /// The run was cancelled by the user.
    Cancelled,
    /// The recorded state cannot be reconciled (impossible owner/pid pair).
    StateCorrupt,
    /// A process that looked alive is not the one recorded at spawn.
    IdentityUnproven,
    /// The child handshake did not complete inside the deadline.
    IpcHandshake,
    /// The scheduler admitted the run but had no slot to give it.
    SlotUnavailable,
    /// The spawn depth or fan-out limit was reached.
    DepthExceeded,
    /// A run-level policy refused the call (profile ceiling, hook block).
    Policy,
    /// The model hit its context window before the request was sent.
    ContextOverflow,
    /// The sandbox denied the call.
    Sandbox,
}

impl ErrorCode {
    /// The wire spelling, and the only place it exists.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Name => "E_AGENT_NAME",
            ErrorCode::Description => "E_AGENT_DESCRIPTION",
            ErrorCode::Prompt => "E_AGENT_PROMPT",
            ErrorCode::Tools => "E_AGENT_TOOLS",
            ErrorCode::FrontMatter => "E_AGENT_FRONTMATTER",
            ErrorCode::UnknownField => "E_AGENT_UNKNOWN_FIELD",
            ErrorCode::Range => "E_AGENT_RANGE",
            ErrorCode::Cycle => "E_AGENT_CYCLE",
            ErrorCode::Model => "E_AGENT_MODEL",
            ErrorCode::Worktree => "E_WORKTREE",
            ErrorCode::WorktreeFailed => "E_WORKTREE_FAILED",
            ErrorCode::WorktreeDirty => "E_WORKTREE_DIRTY",
            ErrorCode::SpecInvalid => "E_SPEC_INVALID",
            ErrorCode::PermissionEscalation => "E_PERMISSION_ESCALATION",
            ErrorCode::Budget => "E_BUDGET",
            ErrorCode::FindingUnevidenced => "E_FINDING_UNEVIDENCED",
            ErrorCode::ProvenanceMissing => "E_PROVENANCE_MISSING",
            ErrorCode::DuplicateBelief => "E_DUPLICATE_BELIEF",
            ErrorCode::Cancelled => "E_CANCELLED",
            ErrorCode::StateCorrupt => "E_STATE_CORRUPT",
            ErrorCode::IdentityUnproven => "E_IDENTITY_UNPROVEN",
            ErrorCode::IpcHandshake => "E_IPC_HANDSHAKE",
            ErrorCode::SlotUnavailable => "E_NO_SLOT",
            ErrorCode::DepthExceeded => "E_DEPTH_EXCEEDED",
            ErrorCode::Policy => "E_POLICY",
            ErrorCode::ContextOverflow => "E_MODEL_CONTEXT",
            ErrorCode::Sandbox => "E_SANDBOX",
        }
    }

    /// Parse a wire spelling back to a code.
    pub fn parse(raw: &str) -> Option<Self> {
        const ALL: &[ErrorCode] = &[
            ErrorCode::Name,
            ErrorCode::Description,
            ErrorCode::Prompt,
            ErrorCode::Tools,
            ErrorCode::FrontMatter,
            ErrorCode::UnknownField,
            ErrorCode::Range,
            ErrorCode::Cycle,
            ErrorCode::Model,
            ErrorCode::Worktree,
            ErrorCode::WorktreeFailed,
            ErrorCode::WorktreeDirty,
            ErrorCode::SpecInvalid,
            ErrorCode::PermissionEscalation,
            ErrorCode::Budget,
            ErrorCode::FindingUnevidenced,
            ErrorCode::ProvenanceMissing,
            ErrorCode::DuplicateBelief,
            ErrorCode::Cancelled,
            ErrorCode::StateCorrupt,
            ErrorCode::IdentityUnproven,
            ErrorCode::IpcHandshake,
            ErrorCode::SlotUnavailable,
            ErrorCode::DepthExceeded,
            ErrorCode::Policy,
            ErrorCode::ContextOverflow,
            ErrorCode::Sandbox,
        ];
        ALL.iter().copied().find(|c| c.as_str() == raw)
    }
}

/// A result whose failure is a typed [`AgentError`].
pub type AgentResult<T> = std::result::Result<T, AgentError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_spellings_are_unique_and_round_trip() {
        let samples = [
            ErrorCode::Name,
            ErrorCode::Cycle,
            ErrorCode::Budget,
            ErrorCode::ProvenanceMissing,
            ErrorCode::WorktreeDirty,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for code in samples {
            assert!(
                seen.insert(code.as_str()),
                "duplicate spelling {}",
                code.as_str()
            );
            assert_eq!(ErrorCode::parse(code.as_str()), Some(code));
        }
        assert_eq!(ErrorCode::parse("E_NOT_A_CODE"), None);
    }

    #[test]
    fn display_carries_the_location() {
        let err = AgentError::at(
            ErrorCode::UnknownField,
            "agents/x.md",
            7,
            "unknown key `colour`",
        );
        let rendered = err.to_string();
        assert!(rendered.contains("E_AGENT_UNKNOWN_FIELD"));
        assert!(rendered.contains("agents/x.md:7"));
        assert!(rendered.contains("colour"));
    }
}
