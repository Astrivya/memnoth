//! The one status model `doctor`, `/status`, and the statusline read.
//!
//! Three surfaces must not disagree about whether anything is confined, and the
//! honest answer is rarely one word. This module turns a [`BackendPlan`] into
//! the same rows for all of them:
//!
//! ```text
//! sandbox
//!   profile    strict
//!   mode       policy-checked
//!   filesystem policy-checked
//!   network    not enforced (off) — install bubblewrap (`bwrap`), or fix its
//!              namespace permissions
//!   helper     not installed
//!   fail-closed yes
//! ```
//!
//! The rule the wording follows (`D15`): a declared gap is acceptable, an
//! implied guarantee is not. `network` reads `enforced` only when a kernel
//! boundary actually denies egress; on Windows it reads `not enforced` until a
//! launcher applies the helper's plan, which is a stronger statement than "the
//! helper is installed".

use crate::backend::{BackendPlan, EnforcementReport, NetworkPolicy, enforcement_report};
use crate::windows::{HelperState, helper_state};

/// What this machine will do with a plan, in rows and in JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxStatus {
    pub profile: String,
    pub report: EnforcementReport,
    pub helper: HelperState,
    /// Proxy variables still set in this process. They do not stop the kernel
    /// boundary, but a confined child that inherited one could reach a host no
    /// allowlist named, so the fact is printed.
    pub proxy_env: Vec<String>,
}

/// Build the status for one plan.
pub fn status(plan: &BackendPlan) -> SandboxStatus {
    SandboxStatus {
        profile: plan.profile.clone(),
        report: enforcement_report(plan),
        helper: helper_state(),
        proxy_env: crate::egress::proxy_env_present(),
    }
}

impl SandboxStatus {
    /// The egress line, with the reason and the one remedy when there is one.
    ///
    /// `not enforced` is a prefix, not a synonym: the parenthetical names the
    /// declared policy so `not enforced (off)` and `not enforced (allowlist)`
    /// cannot be read as the same machine state.
    pub fn network_line(&self) -> String {
        let policy = self.report.network_policy;
        if self.report.network == "enforced" {
            return format!("enforced ({policy})");
        }
        let mut line = format!("not enforced ({policy})");
        if let Some(step) = self
            .report
            .next_step
            .as_deref()
            .filter(|_| self.report.mode != "enforced")
        {
            line.push_str(&format!(" — {step}"));
        }
        line
    }

    /// The helper line. Only Windows has one.
    pub fn helper_line(&self) -> String {
        match &self.helper {
            HelperState::NotInstalled => {
                if self.report.backend.starts_with("windows") {
                    "not installed (run `mn sandbox setup` once, elevated)".to_string()
                } else {
                    "n/a (no elevated helper on this platform)".to_string()
                }
            }
            HelperState::Installed { firewall_rule, .. } => {
                format!("installed ({firewall_rule}); this build does not launch it yet")
            }
            HelperState::Unreadable { reason } => format!("unreadable ({reason})"),
        }
    }

    /// The rows `doctor` prints under its `sandbox` heading.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            row("profile", &self.profile),
            row("mode", self.report.mode),
            row("filesystem", self.report.filesystem),
            row("network", &self.network_line()),
            row("helper", &self.helper_line()),
            row(
                "fail-closed",
                if self.report.fail_closed {
                    "yes (refuses to run without a boundary)"
                } else {
                    "no (policy-checked by design)"
                },
            ),
        ];
        if !self.proxy_env.is_empty() {
            lines.push(row("proxy", &format!("set: {}", self.proxy_env.join(", "))));
        }
        lines
    }

    /// One line for the statusline: the facts, no reassurance.
    pub fn statusline(&self) -> String {
        let helper = if self.report.backend.starts_with("windows") {
            format!(" · helper {}", self.helper.label())
        } else {
            String::new()
        };
        format!(
            "sandbox {} · fs {} · net {}{helper}",
            self.profile, self.report.filesystem, self.report.network
        )
    }

    /// The JSON object `doctor --json` merges into its sandbox section.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "profile": self.profile,
            "backend": self.report.backend,
            "mode": self.report.mode,
            "filesystem": self.report.filesystem,
            "network": self.report.network,
            "network_policy": self.report.network_policy,
            "network_detail": self.report.network_detail,
            "helper": self.helper.label(),
            "helper_detail": self.helper_line(),
            "fail_closed": self.report.fail_closed,
            "next_step": self.report.next_step,
            "proxy_env": self.proxy_env,
            "statusline": self.statusline(),
        })
    }

    /// Whether the egress policy is a declaration the kernel cannot enforce.
    ///
    /// `allowlist` is always this: the kernel denies everything and only a
    /// tool-layer client may reach a listed host. A surface that prints
    /// `network: enforced` next to an allowlist would be claiming the list is
    /// in the kernel, which it is not.
    pub fn allowlist_is_tool_layer_only(&self) -> bool {
        matches!(self.report.network_policy, "allowlist")
    }
}

/// One doctor row, aligned like the rest of the report.
fn row(key: &str, value: &str) -> String {
    format!("  {key:<11}{value}")
}

/// The policy the status reports, for a caller that wants the value rather than
/// the rows (the trust screen quotes it).
pub fn declared_policy(plan: &BackendPlan) -> NetworkPolicy {
    plan.effective_network()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::OsBackend;
    use crate::profiles::{ProfileName, SandboxConfig};
    use std::path::Path;

    fn plan(backend: OsBackend, profile: ProfileName) -> BackendPlan {
        BackendPlan::build(
            backend,
            &profile,
            Path::new("C:/repo"),
            &SandboxConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn windows_says_network_not_enforced_and_names_the_setup_step() {
        let status = status(&plan(OsBackend::Windows, ProfileName::Strict));
        assert_eq!(status.report.mode, "policy-checked");
        assert!(
            status.network_line().starts_with("not enforced (off)"),
            "{}",
            status.network_line()
        );
        assert!(
            status.network_line().contains("mn sandbox setup"),
            "{}",
            status.network_line()
        );
        assert!(status.report.fail_closed);
    }

    #[test]
    fn workspace_is_not_fail_closed_and_says_so() {
        let status = status(&plan(OsBackend::Windows, ProfileName::Workspace));
        assert!(!status.report.fail_closed);
        assert!(
            status
                .lines()
                .iter()
                .any(|line| line.contains("no (policy-checked by design)")),
            "{:?}",
            status.lines()
        );
    }

    #[test]
    fn every_row_names_its_fact_and_never_says_sandboxed_alone() {
        let status = status(&plan(OsBackend::Windows, ProfileName::Strict));
        let joined = status.lines().join("\n");
        for key in [
            "profile",
            "mode",
            "filesystem",
            "network",
            "helper",
            "fail-closed",
        ] {
            assert!(joined.contains(key), "missing `{key}` in:\n{joined}");
        }
        assert!(!joined.to_ascii_lowercase().contains("sandbox on"));
    }

    #[test]
    fn the_statusline_is_one_line_and_states_network_state() {
        let status = status(&plan(OsBackend::Windows, ProfileName::Strict));
        let line = status.statusline();
        assert!(!line.contains('\n'));
        assert!(line.contains("net not enforced"), "{line}");
        assert!(line.contains("helper not installed"), "{line}");
    }
}
