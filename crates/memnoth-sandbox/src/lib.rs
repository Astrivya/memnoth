//! memnoth-sandbox: profiles, fail-closed deny-globs, per-OS backend plans,
//! rule-language v1, denial tracking, and the one backend that applies a plan.
//!
//! Resolving a profile yields data, [`backend::decide`] answers per-path, and
//! [`rules::decide`] answers per-command. [`confine`] is the one place a plan
//! becomes a process boundary: `bwrap` on Linux, `sandbox-exec`/Seatbelt on
//! macOS, and — where neither exists — the reason it cannot, which the caller
//! prints instead of the word "sandboxed".

pub mod allow_path;
pub mod backend;
pub mod confine;
pub mod denials;
pub mod deny;
pub mod escalation;
pub mod linux;
pub mod paths;
pub mod profiles;
pub mod rules;
pub mod seatbelt;
pub mod types;

pub use backend::{Access, BackendPlan, OsBackend, PathDecision, decide};
pub use confine::Confinement;
pub use denials::{DenialTracker, RuleSuggestion, command_shape};
pub use escalation::{DeniedCall, denial_text};
pub use profiles::{ProfileName, SandboxConfig, SandboxProfile};
pub use rules::{AskKind, Behavior, Rule, RuleDecision, RuleSet};
pub use types::{SandboxEvent, SandboxEventType, SandboxMetrics};

use std::path::Path;

/// Apply `plan` to `argv` on this machine, or say why nothing will.
///
/// One entry point so a caller cannot pick the wrong backend for the plan it
/// was given: the plan already names the OS it was built for, and this is the
/// only thing that reads that field.
pub fn confine(plan: &BackendPlan, workspace: &Path, argv: &[String]) -> Confinement {
    match plan.backend {
        OsBackend::Linux => linux::confine(plan, workspace, argv),
        OsBackend::MacOs => seatbelt::confine(plan, workspace, argv),
        OsBackend::Windows => Confinement::Unavailable {
            reason: "this build applies the Windows plan with the policy engine only: there is \
                     no AppContainer or restricted-token launcher yet, so filesystem and \
                     network access are not confined"
                .to_string(),
        },
    }
}
