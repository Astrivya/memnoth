//! memnoth-sandbox: profiles, fail-closed deny-globs, per-OS backend plans,
//! rule-language v1, and denial tracking.
//!
//! Lane F. Policy engine only in v1 (no kernel enforcement): resolving a
//! profile yields data, [`backend::decide`] answers per-path, and
//! [`rules::decide`] answers per-command. Kernel application per OS backend
//! is phase 2 and consumes these plans without changing policy.

pub mod allow_path;
pub mod backend;
pub mod denials;
pub mod deny;
pub mod escalation;
pub mod paths;
pub mod profiles;
pub mod rules;
pub mod types;

pub use backend::{Access, BackendPlan, OsBackend, PathDecision, decide};
pub use denials::{DenialTracker, RuleSuggestion, command_shape};
pub use escalation::{DeniedCall, denial_text};
pub use profiles::{ProfileName, SandboxConfig, SandboxProfile};
pub use rules::{AskKind, Behavior, Rule, RuleDecision, RuleSet};
pub use types::{SandboxEvent, SandboxEventType, SandboxMetrics};
