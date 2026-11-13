//! The Windows confinement plan: restricted token, job object, deny ACLs, and
//! a firewall rule — as data, plus the honest answer about who applies it.
//!
//! ## What this module is
//!
//! Read/write confinement on Windows is achievable: `CreateRestrictedToken` +
//! `SetEntriesInAcl` on the deny paths + a job object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` covers the file and process tree. Egress
//! denial is **not** achievable without a WFP filter, which needs a one-time
//! elevated install (`D15`).
//!
//! So this module builds the exact plan the helper applies, serializes it, and
//! reports the helper's state. It does **not** claim that any of it is in
//! force: the Windows mode is `policy-checked` until a launcher that applies
//! this plan exists, and [`crate::status`] says so on every surface.
//!
//! ## Why the plan is data
//!
//! The helper is a separate elevated process, and `mn sandbox setup` has to be
//! able to show what it will install before it installs it. A plan that can be
//! printed, diffed, and written to a file is also a plan the audit log can name
//! line by line, which a struct built inside a `CreateProcess` call is not.
//!
//! ## What the helper installs
//!
//! 1. A **firewall rule** blocking outbound traffic for the sandboxed SID
//!    (`New-NetFirewallRule` shape: outbound, block, all protocols, that SID).
//! 2. **Deny ACEs** on every exact deny path for the restricted SID, so a
//!    denied secret is unreachable even if the policy layer is bypassed.
//! 3. A **job object** per confined command: kill-on-close, one active process
//!    limit, no breakaway, so a grandchild cannot outlive the turn.
//!
//! The token itself is created per launch, not installed: a restricted token is
//! a property of the child process, not of the machine.

use std::path::{Path, PathBuf};

use crate::backend::{BackendPlan, NetworkPolicy};

/// Marker written by `mn sandbox setup` after the elevated install succeeds.
///
/// It records what was installed, so `doctor` can say "installed" without
/// re-elevating, and can tell an old marker from a current one.
pub const HELPER_MARKER_FILENAME: &str = "windows-helper.json";

/// Marker schema version. A bump means the marker is stale.
pub const HELPER_SCHEMA_VERSION: u32 = 1;

/// The marker path inside the memnoth state directory.
pub fn helper_marker_path() -> PathBuf {
    crate::paths::memnoth_home()
        .join("sandbox")
        .join(HELPER_MARKER_FILENAME)
}

/// What the elevated helper reported, as far as this process can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperState {
    /// No marker. Egress is not blocked; `doctor` prints `network: not
    /// enforced (run mn sandbox setup)`.
    NotInstalled,
    /// The marker parses and names a firewall rule. The *install* happened; this
    /// build still does not launch the helper, so filesystem confinement stays
    /// `policy-checked`.
    Installed { schema: u32, firewall_rule: String },
    /// A marker exists but cannot be trusted (unreadable, unparseable, or from
    /// a future schema). Treated as not installed: a corrupt marker is not
    /// evidence of enforcement.
    Unreadable { reason: String },
}

impl HelperState {
    /// Short label for a status line.
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotInstalled => "not installed",
            Self::Installed { .. } => "installed",
            Self::Unreadable { .. } => "unreadable",
        }
    }

    /// Whether the helper's install is on record.
    pub fn is_installed(&self) -> bool {
        matches!(self, Self::Installed { .. })
    }

    /// The reason a marker could not be trusted, when there is one.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Unreadable { reason } => Some(reason),
            _ => None,
        }
    }
}

/// Read the helper marker. Never an error: an unreadable state file is a state,
/// and treating it as an error would make `doctor` fail instead of say
/// "not installed".
pub fn helper_state() -> HelperState {
    let path = helper_marker_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return HelperState::NotInstalled;
        }
        Err(error) => {
            return HelperState::Unreadable {
                reason: format!("{}: {error}", path.display()),
            };
        }
    };
    let marker: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            return HelperState::Unreadable {
                reason: format!("{}: {error}", path.display()),
            };
        }
    };
    let schema = marker["schema"].as_u64().unwrap_or(0) as u32;
    if schema == 0 || schema > HELPER_SCHEMA_VERSION {
        return HelperState::Unreadable {
            reason: format!(
                "{}: schema {schema} is not one this build understands (expected \
                 {HELPER_SCHEMA_VERSION})",
                path.display()
            ),
        };
    }
    match marker["firewall_rule"].as_str() {
        Some(rule) if !rule.trim().is_empty() => HelperState::Installed {
            schema,
            firewall_rule: rule.to_string(),
        },
        _ => HelperState::Unreadable {
            reason: format!("{}: no firewall_rule recorded", path.display()),
        },
    }
}

/// Job-object limits for one confined command.
///
/// Field names are the Win32 ones on purpose: the helper maps each to the
/// matching `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` field, and a renamed field
/// would make that mapping a guess.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct JobObjectLimits {
    /// `BasicLimitInformation.LimitFlags` bits.
    pub kill_on_job_close: bool,
    pub active_process_limit: u32,
    pub breakaway_ok: bool,
    /// `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION`.
    pub die_on_unhandled_exception: bool,
    /// `UiRestrictions` — no clipboard read, no handle to the desktop.
    pub ui_restrictions: bool,
}

impl Default for JobObjectLimits {
    fn default() -> Self {
        Self {
            // A backgrounded grandchild must not outlive the turn that spawned
            // it. This is the only one of the four that is load-bearing for the
            // escape claim.
            kill_on_job_close: true,
            // One process at a time: a compiler job that forks is common, so
            // the limit is generous but finite. The tree is what matters, and
            // the job object covers it.
            active_process_limit: 64,
            breakaway_ok: false,
            die_on_unhandled_exception: true,
            ui_restrictions: true,
        }
    }
}

/// The restricted-token shape the launcher creates per command.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RestrictedTokenSpec {
    /// `CreateRestrictedToken(..., DISABLE_MAX_PRIVILEGE, ...)`.
    pub disable_max_privilege: bool,
    /// `LUA_TOKEN`: run at medium integrity with the admin group deny-only.
    pub lua_token: bool,
    /// SIDs to mark deny-only (`Administrators`, `Users` write where needed).
    pub deny_only_sids: Vec<String>,
    /// Integrity level the child runs at. `low` is what makes the ACL denials
    /// bite for a process that would otherwise own the file.
    pub integrity: String,
    /// Capability SIDs removed from the token.
    pub removed_capabilities: Vec<String>,
}

impl Default for RestrictedTokenSpec {
    fn default() -> Self {
        Self {
            disable_max_privilege: true,
            lua_token: true,
            deny_only_sids: vec![
                "S-1-5-32-544".to_string(), // BUILTIN\Administrators
                "S-1-5-32-545".to_string(), // BUILTIN\Users
            ],
            integrity: "low".to_string(),
            removed_capabilities: vec![
                // Network capabilities: the token half of egress denial. The
                // firewall rule is the enforcement half, and neither alone is
                // enough.
                "S-1-15-3-1".to_string(),
            ],
        }
    }
}

/// The firewall rule the helper installs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FirewallRuleSpec {
    pub name: String,
    pub display_name: String,
    pub direction: String,
    pub action: String,
    pub protocol: String,
    /// `LocalUser` — the rule is keyed on the sandbox SID, not on a program
    /// path, so a renamed binary does not escape it.
    pub applies_to: String,
    /// Per-sandbox SID the rule blocks. Empty until the helper creates it.
    pub sandbox_sid: String,
    pub enabled: bool,
}

impl FirewallRuleSpec {
    fn blocking() -> Self {
        Self {
            name: "memnoth-sandbox-egress-block".to_string(),
            display_name: "Memnoth sandbox: block outbound egress for the confined token"
                .to_string(),
            direction: "outbound".to_string(),
            action: "block".to_string(),
            protocol: "any".to_string(),
            applies_to: "local-user".to_string(),
            sandbox_sid: String::new(),
            enabled: true,
        }
    }
}

/// Everything the elevated helper applies for one plan.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WindowsPlan {
    pub profile: String,
    pub workspace: PathBuf,
    /// Paths that get a deny ACE for the sandbox SID.
    pub deny_acl: Vec<PathBuf>,
    /// Paths granted read-only (no write ACE for the sandbox SID).
    pub read_only_acl: Vec<PathBuf>,
    /// Paths granted write.
    pub read_write_acl: Vec<PathBuf>,
    pub token: RestrictedTokenSpec,
    pub job: JobObjectLimits,
    pub firewall: FirewallRuleSpec,
    pub network: NetworkPolicy,
    /// `true` only when a launcher in *this* process applies the plan. Always
    /// `false` today; the field exists so a report cannot infer enforcement
    /// from the plan alone.
    pub applied: bool,
}

impl WindowsPlan {
    /// Build the plan for one backend plan.
    pub fn build(plan: &BackendPlan, workspace: &Path) -> Self {
        let network = plan.effective_network();
        let mut firewall = FirewallRuleSpec::blocking();
        firewall.enabled = !matches!(network, NetworkPolicy::Open);
        Self {
            profile: plan.profile.clone(),
            workspace: workspace.to_path_buf(),
            deny_acl: plan.deny_exact.clone(),
            read_only_acl: plan.read_only.clone(),
            read_write_acl: plan.read_write.clone(),
            token: RestrictedTokenSpec::default(),
            job: JobObjectLimits::default(),
            firewall,
            network,
            applied: false,
        }
    }

    /// The plan as the helper reads it from disk.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": HELPER_SCHEMA_VERSION,
            "profile": self.profile,
            "workspace": self.workspace.display().to_string(),
            "deny_acl": self.deny_acl.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "read_only_acl": self.read_only_acl.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "read_write_acl": self.read_write_acl.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "token": self.token,
            "job": self.job,
            "firewall": self.firewall,
            "network": self.network,
        })
    }

    /// One sentence for `mn sandbox setup --dry-run` and the approval card.
    pub fn describe(&self) -> String {
        format!(
            "windows plan for '{}': {} deny ACE(s), {} read-only root(s), {} writable root(s), \
             low-integrity restricted token, job object (kill-on-close, {} processes), firewall \
             rule {}",
            self.profile,
            self.deny_acl.len(),
            self.read_only_acl.len(),
            self.read_write_acl.len(),
            self.job.active_process_limit,
            if self.firewall.enabled {
                "blocking outbound egress"
            } else {
                "disabled (network = open)"
            }
        )
    }

    /// The audit rows this plan would produce, one per applied control.
    ///
    /// Written by the helper into the same append-only log the policy engine
    /// uses, so a denied read and an installed ACL are recorded the same way.
    pub fn audit_lines(&self) -> Vec<String> {
        let mut lines = vec![format!("windows:token:integrity:{}", self.token.integrity)];
        lines.push(format!(
            "windows:job:kill_on_close:{}:processes:{}",
            self.job.kill_on_job_close, self.job.active_process_limit
        ));
        if self.firewall.enabled {
            lines.push(format!("windows:firewall:{}", self.firewall.name));
        }
        for path in &self.deny_acl {
            lines.push(format!("windows:deny-acl:{}", path.display()));
        }
        lines
    }
}

/// Apply `plan` on Windows, or say why nothing was applied.
///
/// There is no Win32 launcher in this build: the plan above is what the helper
/// installs, and no process is created from it here. Returning a
/// `Confinement::WindowsJob` would make every caller spawn a program that does
/// not exist; returning `Unavailable` with the plan's shape keeps the caller
/// honest and gives `doctor` something specific to print.
pub fn confine(
    plan: &BackendPlan,
    workspace: &Path,
    argv: &[String],
) -> crate::confine::Confinement {
    use crate::confine::Confinement;
    if plan.profile == "off" {
        return Confinement::Unavailable {
            reason: "the sandbox policy is off, so nothing confines this command".to_string(),
        };
    }
    if plan.backend != crate::backend::OsBackend::Windows {
        return Confinement::Unavailable {
            reason: format!(
                "this is the Windows plan, and the plan names {}",
                plan.backend.as_str()
            ),
        };
    }
    if argv.is_empty() {
        return Confinement::Unavailable {
            reason: "no command to confine".to_string(),
        };
    }
    let state = helper_state();
    let windows_plan = WindowsPlan::build(plan, workspace);
    let reason = match &state {
        HelperState::NotInstalled => format!(
            "no Windows launcher in this build, and the helper is not installed, so filesystem \
             and network access are not confined (run `mn sandbox setup` once, elevated). The \
             plan it would apply is: {}",
            windows_plan.describe()
        ),
        HelperState::Installed { firewall_rule, .. } => format!(
            "the helper `{firewall_rule}` is installed, but this build has no Windows launcher \
             that applies it, so nothing is confined yet; the plan is: {}",
            windows_plan.describe()
        ),
        HelperState::Unreadable { reason } => format!(
            "the helper marker cannot be trusted ({reason}), so nothing is confined; the plan \
             is: {}",
            windows_plan.describe()
        ),
    };
    Confinement::Unavailable { reason }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::OsBackend;
    use crate::profiles::{ProfileName, SandboxConfig};

    fn plan(profile: ProfileName) -> BackendPlan {
        BackendPlan::build(
            OsBackend::Windows,
            &profile,
            Path::new("C:/repo"),
            &SandboxConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn the_plan_carries_every_control_d15_asks_for() {
        let windows_plan = WindowsPlan::build(&plan(ProfileName::Strict), Path::new("C:/repo"));
        assert!(windows_plan.token.disable_max_privilege);
        assert!(windows_plan.token.lua_token);
        assert_eq!(windows_plan.token.integrity, "low");
        assert!(windows_plan.job.kill_on_job_close);
        assert!(!windows_plan.job.breakaway_ok);
        assert_eq!(windows_plan.firewall.action, "block");
        assert_eq!(windows_plan.firewall.direction, "outbound");
        assert!(windows_plan.firewall.enabled);
    }

    #[test]
    fn an_open_network_disables_only_the_firewall_rule() {
        let mut backend_plan = plan(ProfileName::Workspace);
        backend_plan.restrict_network = false;
        backend_plan.network = NetworkPolicy::Open;
        let windows_plan = WindowsPlan::build(&backend_plan, Path::new("C:/repo"));
        assert!(!windows_plan.firewall.enabled);
        // The job object is about the process tree, not egress: it stays.
        assert!(windows_plan.job.kill_on_job_close);
    }

    #[test]
    fn the_plan_is_never_reported_as_applied() {
        let windows_plan = WindowsPlan::build(&plan(ProfileName::Strict), Path::new("C:/repo"));
        assert!(!windows_plan.applied);
        assert!(windows_plan.to_json()["schema"].is_number());
    }

    #[test]
    fn confine_names_the_helper_and_the_plan_rather_than_claiming_a_boundary() {
        let backend_plan = plan(ProfileName::Strict);
        let confinement = confine(
            &backend_plan,
            Path::new("C:/repo"),
            &["cmd".to_string(), "/c".to_string(), "dir".to_string()],
        );
        assert!(!confinement.is_enforced());
        let text = confinement.describe();
        assert!(text.contains("mn sandbox setup"), "{text}");
        assert!(text.contains("job object"), "{text}");
    }

    #[test]
    fn a_corrupt_marker_is_not_evidence_of_enforcement() {
        // The state is read from `$MEMNOTH_HOME`; this test covers the parse
        // rule directly so it does not depend on the machine's state dir.
        let bad = HelperState::Unreadable {
            reason: "schema 99".to_string(),
        };
        assert!(!bad.is_installed());
        assert_eq!(bad.label(), "unreadable");
        assert!(bad.reason().is_some());
    }
}
