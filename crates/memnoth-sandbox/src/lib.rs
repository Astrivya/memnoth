//! memnoth-sandbox: profiles, fail-closed deny-globs, per-OS backend plans,
//! rule-language v1, denial tracking, and the one backend that applies a plan.
//!
//! Resolving a profile yields data, [`backend::decide`] answers per-path,
//! [`egress::decide`] answers per-host, and [`rules::decide`] answers
//! per-command. [`confine`] is the one place a plan becomes a process boundary:
//! `bwrap` on Linux, `sandbox-exec`/Seatbelt on macOS, and — where neither
//! exists — the reason it cannot, which the caller prints instead of the word
//! "sandboxed".
//!
//! Two entry points, and the difference matters:
//!
//! - [`confine`] describes what will hold a command. A caller that prints
//!   [`Confinement::describe`] may run a policy-checked command on purpose.
//! - [`prepare`] is the fail-closed one. A profile that promised a boundary
//!   gets `Refuse` on a machine that cannot apply it, so "we said strict" never
//!   becomes "we ran it unconfined anyway".

pub mod allow_path;
pub mod approval;
pub mod audit;
pub mod backend;
pub mod confine;
pub mod denials;
pub mod deny;
pub mod egress;
pub mod escalation;
pub mod linux;
pub mod paths;
pub mod profiles;
pub mod rules;
pub mod seatbelt;
pub mod seccomp;
pub mod selftest;
pub mod status;
pub mod types;
pub mod windows;

pub use approval::{
    ApprovalBroker, ApprovalCard, ApprovalKey, DiffStat, approval_key, policy_version,
};
pub use audit::{AuditRecord, append_path_decision, record_for_path_decision};
pub use backend::{
    Access, BackendPlan, EnforcementReport, NetworkPolicy, OsBackend, PathDecision, decide,
    decide_record, enforcement_report, recheck_after_open,
};
pub use confine::Confinement;
pub use denials::{DenialTracker, RuleSuggestion, command_shape};
pub use egress::{EgressDecision, EgressPlan, EgressRecord, HostSpec};
pub use escalation::{DeniedCall, denial_text};
pub use profiles::{ProfileName, SandboxConfig, SandboxProfile};
pub use rules::{AskKind, Behavior, Rule, RuleDecision, RuleSet};
pub use selftest::{SandboxTestReport, sandbox_test};
pub use status::SandboxStatus;
pub use types::{SandboxEvent, SandboxEventType, SandboxMetrics};
pub use windows::{HelperState, WindowsPlan};

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
        OsBackend::Windows => windows::confine(plan, workspace, argv),
    }
}

/// What a caller should do with a command under a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Execution {
    /// Run this confinement. `Confinement::Unavailable` inside it means the
    /// plan is policy-checked by design (`workspace`, `devbox`, `off`), and the
    /// caller prints [`Confinement::describe`] next to the command.
    Run(Confinement),
    /// Do not run. The plan promised a boundary this machine cannot apply.
    Refuse { reason: String },
}

impl Execution {
    /// The confinement, when the command may run.
    pub fn confinement(&self) -> Option<&Confinement> {
        match self {
            Self::Run(confinement) => Some(confinement),
            Self::Refuse { .. } => None,
        }
    }

    /// The refusal text, when the command must not run.
    pub fn refusal(&self) -> Option<&str> {
        match self {
            Self::Run(_) => None,
            Self::Refuse { reason } => Some(reason),
        }
    }
}

/// Fail-closed execution decision for one command.
///
/// [`confine`] answers "what will hold it"; this answers "may it run at all".
/// A plan that promised a kernel boundary ([`BackendPlan::requires_kernel_boundary`])
/// and does not get one is refused here. The alternative — running it anyway
/// with a note on stderr — is the silent fallback that made the word "sandboxed"
/// untrustworthy in the first place.
pub fn prepare(plan: &BackendPlan, workspace: &Path, argv: &[String]) -> Execution {
    let confinement = confine(plan, workspace, argv);
    if confinement.is_enforced() || !plan.requires_kernel_boundary() {
        return Execution::Run(confinement);
    }
    let report = backend::enforcement_report(plan);
    let next = report
        .next_step
        .map(|step| format!(" Next: {step}."))
        .unwrap_or_default();
    Execution::Refuse {
        reason: format!(
            "profile '{}' promised a boundary this machine cannot apply, so the command was not \
             run rather than run unconfined: {}.{next}",
            plan.profile, report.detail
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::SandboxConfig;

    fn plan_for(backend: OsBackend, profile: ProfileName, workspace: &Path) -> BackendPlan {
        BackendPlan::build(backend, &profile, workspace, &SandboxConfig::default()).unwrap()
    }

    #[test]
    fn a_strict_plan_without_a_boundary_refuses_instead_of_running() {
        // Windows never applies a kernel boundary in this build, and `strict`
        // promised one, so the command must not run.
        let plan = plan_for(
            OsBackend::Windows,
            ProfileName::Strict,
            Path::new("C:/repo"),
        );
        let execution = prepare(&plan, Path::new("C:/repo"), &["ls".to_string()]);
        let reason = execution
            .refusal()
            .expect("strict must refuse on a backend with no boundary");
        assert!(reason.contains("strict"), "{reason}");
        assert!(reason.contains("not run"), "{reason}");
    }

    #[test]
    fn a_workspace_plan_is_policy_checked_by_design_and_still_runs() {
        let plan = plan_for(
            OsBackend::Windows,
            ProfileName::Workspace,
            Path::new("C:/repo"),
        );
        let execution = prepare(&plan, Path::new("C:/repo"), &["ls".to_string()]);
        let confinement = execution
            .confinement()
            .expect("workspace is policy-checked, not refused");
        assert!(!confinement.is_enforced());
        assert!(confinement.describe().starts_with("not confined:"));
    }

    #[test]
    fn profile_off_runs_and_says_nothing_is_confined() {
        let plan = plan_for(OsBackend::Windows, ProfileName::Off, Path::new("C:/repo"));
        let execution = prepare(&plan, Path::new("C:/repo"), &["ls".to_string()]);
        assert!(execution.refusal().is_none());
    }
}
