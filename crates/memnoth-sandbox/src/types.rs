//! Sandbox telemetry: events and counters.
//!
//! Port note: the event/counter *shape* follows grok-build's
//! `xai-grok-sandbox/src/types.rs` (see THIRD-PARTY-NOTICES.md),
//! reimplemented with memnoth naming and rule-engine fields.

use std::sync::atomic::{AtomicU64, Ordering};

/// A recorded sandbox event for telemetry and debugging.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SandboxEvent {
    pub timestamp: String,
    pub event_type: SandboxEventType,
    pub profile: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enforced: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restrict_network: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_write_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny_paths: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn now_rfc3339() -> String {
    // No chrono dependency: seconds since epoch is enough for ordering;
    // human rendering happens at display time.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("{}", d.as_secs()))
        .unwrap_or_else(|_| "0".to_string())
}

impl SandboxEvent {
    fn base(event_type: SandboxEventType, profile: &str) -> Self {
        Self {
            timestamp: now_rfc3339(),
            event_type,
            profile: profile.to_string(),
            workspace: None,
            backend: None,
            enforced: None,
            restrict_network: None,
            read_write_paths: None,
            read_only_paths: None,
            deny_paths: None,
            operation: None,
            target: None,
            command: None,
            rule_id: None,
            error: None,
        }
    }

    pub fn profile_applied(
        profile: &crate::profiles::SandboxProfile,
        workspace: &std::path::Path,
        backend: crate::backend::OsBackend,
    ) -> Self {
        let mut event = Self::base(SandboxEventType::ProfileApplied, &profile.name);
        event.workspace = Some(workspace.display().to_string());
        event.backend = Some(backend.as_str().to_string());
        event.enforced = Some(true);
        event.restrict_network = Some(profile.restrict_network);
        event.read_write_paths = Some(
            profile
                .read_write
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        );
        if !profile.read_only.is_empty() {
            event.read_only_paths = Some(
                profile
                    .read_only
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
            );
        }
        if !profile.deny.is_empty() {
            event.deny_paths = Some(
                profile
                    .deny
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
            );
        }
        event
    }

    pub fn apply_failed(profile: &str, workspace: &std::path::Path, error: &str) -> Self {
        let mut event = Self::base(SandboxEventType::ApplyFailed, profile);
        event.workspace = Some(workspace.display().to_string());
        event.enforced = Some(false);
        event.error = Some(error.to_string());
        event
    }

    pub fn fs_violation(profile: &str, target: &str, operation: &str) -> Self {
        let mut event = Self::base(SandboxEventType::FsViolation, profile);
        event.operation = Some(operation.to_string());
        event.target = Some(target.to_string());
        event
    }

    pub fn rule_denial(profile: &str, command: &str, rule_id: &str) -> Self {
        let mut event = Self::base(SandboxEventType::RuleDenial, profile);
        event.command = Some(command.to_string());
        event.rule_id = Some(rule_id.to_string());
        event
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum SandboxEventType {
    ProfileApplied,
    ApplyFailed,
    FsViolation,
    NetViolation,
    RuleDenial,
    BypassGranted,
    BypassDenied,
}

/// Counters for sandbox activity, used for telemetry dashboards.
#[derive(Debug, Default)]
pub struct SandboxMetrics {
    pub fs_violations: AtomicU64,
    pub net_violations: AtomicU64,
    pub rule_denials: AtomicU64,
    pub bypasses_granted: AtomicU64,
    pub bypasses_denied: AtomicU64,
}

impl SandboxMetrics {
    pub fn inc_fs_violation(&self) {
        self.fs_violations.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_net_violation(&self) {
        self.net_violations.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_rule_denial(&self) {
        self.rule_denials.fetch_add(1, Ordering::Relaxed);
    }

    pub fn fs_violation_count(&self) -> u64 {
        self.fs_violations.load(Ordering::Relaxed)
    }

    pub fn rule_denial_count(&self) -> u64 {
        self.rule_denials.load(Ordering::Relaxed)
    }
}
