//! What a tool call costs, and what it is allowed to do.
//!
//! A receipt exists because "the agent used a tool" is not observable enough to
//! optimise. The TUI shows `kg_impact · 6 ms · 1.8 KB · saved ~3.1k tok`, the CLI
//! puts the same object in its `--json` envelope, and the session log keeps it,
//! so `/usage` is a query rather than a guess.
//!
//! Two rules keep the numbers honest:
//!
//! * `tokens_out` is an **estimate** from bytes and is always rendered with `~`;
//!   provider-reported usage is the ledger's business (`docs/DECISIONS.md` D3).
//! * `tokens_saved_est` is per tool and states its method in the tool itself —
//!   a saving with no method is a marketing number.

use std::time::Instant;

use serde::{Deserialize, Serialize};

/// How much work a tool does, for routing hints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostClass {
    /// Directory listing, clock, scratchpad: no query, no IO beyond a stat.
    Cheap,
    /// A file read or a search: bounded IO.
    Moderate,
    /// Graph traversal, retrieval, ingestion: can touch the whole store.
    Expensive,
}

/// Where a tool's result may be cached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheClass {
    /// The answer changes every call (clock, scratchpad).
    NoStore,
    /// Valid for the session — a directory listing, a status.
    Session,
    /// Persisted and shared between sessions — a graph query, a recall.
    Persistent,
}

/// What a caller must consent to before the tool runs.
///
/// The three classes are the ones the approval card and the MCP write-approval
/// protocol both speak; keeping them in one enum is what stops the card and the
/// protocol from disagreeing about what "write" means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    /// Reads only.
    ReadOnly,
    /// Changes the workspace or a store.
    MutatesWorkspace,
    /// Deletes, overwrites or executes.
    Destructive,
}

/// One tool call, measured.
/// `PartialEq` exists so a turn event can carry a receipt without turning every surface's event
/// enum into one that cannot be compared. Two receipts are equal only when they agree on the
/// timing too, which is why tests assert on fields rather than on whole receipts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// Tool name.
    pub tool: String,
    /// Wall time of the call.
    pub ms: u64,
    /// Bytes of JSON the tool produced.
    pub bytes_out: u64,
    /// Estimated tokens from `bytes_out`, rendered with `~`.
    pub tokens_out: u64,
    /// Estimated tokens the model would have spent doing this itself, when the
    /// tool states a method.
    pub tokens_saved_est: Option<u64>,
    /// Cache class of the answer, as declared by the tool.
    pub cache: CacheClass,
    /// Profile the call was made under.
    pub profile: String,
    /// True when the profile or an approval rule stopped the call.
    pub refused: bool,
    /// Why it was refused; `None` on success.
    pub reason: Option<String>,
}

impl Receipt {
    /// Measure a completed call. `started` is taken before the tool runs.
    pub fn measured(
        tool: &str,
        profile: &str,
        cache: CacheClass,
        bytes_out: u64,
        started: Instant,
    ) -> Self {
        Self {
            tool: tool.to_string(),
            ms: started.elapsed().as_millis() as u64,
            bytes_out,
            tokens_out: bytes_out / 4,
            tokens_saved_est: None,
            cache,
            profile: profile.to_string(),
            refused: false,
            reason: None,
        }
    }

    /// A refusal, with the reason a caller can act on.
    pub fn refused(
        tool: &str,
        profile: &str,
        cache: CacheClass,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            tool: tool.to_string(),
            ms: 0,
            bytes_out: 0,
            tokens_out: 0,
            tokens_saved_est: None,
            cache,
            profile: profile.to_string(),
            refused: true,
            reason: Some(reason.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_receipt_estimates_tokens_from_bytes_and_says_so() {
        let receipt =
            Receipt::measured("fs_list", "code", CacheClass::Session, 401, Instant::now());
        assert_eq!(receipt.tokens_out, 100);
        assert!(receipt.tokens_saved_est.is_none(), "no method, no claim");
        assert!(!receipt.refused);
    }

    #[test]
    fn a_refusal_carries_a_reason() {
        let receipt = Receipt::refused("fs_list", "memory", CacheClass::Session, "not in profile");
        assert!(receipt.refused);
        assert_eq!(receipt.reason.as_deref(), Some("not in profile"));
        assert_eq!(receipt.bytes_out, 0);
    }
}
