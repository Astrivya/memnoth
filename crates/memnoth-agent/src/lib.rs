//! memnoth-agent — the agent runtime, separate from the session message list.
//!
//! A session is an append-only transcript of messages; a run is a unit of
//! execution (a turn, a tool call, a delegated subagent task) with its own
//! lifecycle (pending → running → done / failed / cancelled / orphaned) that
//! lives outside that transcript. This crate holds the run registry so a run
//! can be tracked, queried and reported on without touching the session's
//! message list.
//!
//! Modules:
//! - [`runtime`] — the run registry (`RunRegistry`, `RunStatus`).
//! - [`spec`] — `AgentSpec` markdown + YAML frontmatter, precedence,
//!   tool whitelists, cycle detection.
//! - [`hooks`] — hook `Boundary`s, `HookSpec`, the `ToolGuard` gate.
//! - [`result`] — the `AgentResult` contract returned to a parent.
//! - [`builtin`] — the six compiled-in agent specs (W8-15).
//! - [`budget`] — budget ledger spanning children, refused pre-request.
//! - [`orphan`] — orphan recovery keyed on (pid, boot_id, start_time).
//! - [`error`] — typed `AgentError` and the stable `ErrorCode` set.
//! - [`executor`] — the child executor: steps, boundaries, hooks, result.

#![warn(missing_docs)]
// TODO: Change to #![deny(missing_docs)] after all public items are documented

pub mod budget;
pub mod builtin;
pub mod delegate;
pub mod error;
pub mod executor;
pub mod hooks;
pub mod orphan;
pub mod result;
pub mod runtime;
pub mod spec;

pub use budget::BudgetLedger;
pub use builtin::builtin_specs;
pub use delegate::{ChildEvent, ChildRun, Delegator, MAX_DEPTH};
pub use error::{AgentError, AgentResult, ErrorCode};
pub use executor::{Executor, Step, StepAttention};
pub use hooks::{Boundary, HookSpec, NoHooks, ToolGuard};
pub use orphan::OrphanProbe;
pub use result::{
    CommandRun, Cost, Evidence, Exit, Finding, ProposedBelief as AgentProposedBelief,
};
pub use runtime::{Run, RunEntry, RunRegistry, RunStatus};
pub use spec::{
    AgentSpec, ApprovalProfile, Budget, CallArgs, Effort, Isolation, Layer, MemoryAccess,
    ModelChoice, Permission, SpawnScope, ToolPattern, ToolSet,
};
