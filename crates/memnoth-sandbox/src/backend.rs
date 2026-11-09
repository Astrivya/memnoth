//! Per-OS enforcement backends.
//!
//! v1 is a policy engine: [`BackendPlan`] describes what each OS backend must
//! enforce for a resolved profile, and [`decide`] answers read/write for one
//! path in pure Rust on every OS (including Windows). Kernel application —
//! Landlock/bwrap on Linux, Seatbelt on macOS, job objects / integrity levels
//! on Windows — is phase 2 and consumes these plans without changing policy.

use crate::deny::{
    DENY_GLOB_CAPS, effective_deny_paths, expand_deny_globs, partition_deny_entries,
};
use crate::profiles::{ProfileName, SandboxConfig, SandboxProfile};
use std::path::{Path, PathBuf};

/// Target OS backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsBackend {
    Linux,
    MacOs,
    Windows,
}

impl OsBackend {
    /// The backend for the compiling host.
    pub fn host() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux/landlock+bwrap",
            Self::MacOs => "macos/seatbelt",
            Self::Windows => "windows/policy",
        }
    }
}

/// How a plan is enforced on its backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcementKind {
    /// Linux: Landlock allow sets + bwrap bind-over for deny paths (phase 2).
    PlanLandlockBwrap,
    /// macOS: Seatbelt profile with deny-last-match rules (phase 2).
    PlanSeatbelt,
    /// Windows + fallback: tool-layer policy decisions via [`decide`].
    /// This is real enforcement at the agent's own file-op boundary today,
    /// not a stub: every tool call checks `decide` before touching disk.
    Policy,
}

/// A buildable enforcement plan for one profile on one backend.
///
/// Fail-closed construction: [`BackendPlan::build`] returns `Err` on any
/// unresolvable deny/glob/symlink case, and the caller must refuse startup
/// (a `None`/empty plan must never mean "unenforced but active").
#[derive(Debug, Clone)]
pub struct BackendPlan {
    pub backend: OsBackend,
    pub enforcement: EnforcementKind,
    pub profile: String,
    pub read_only: Vec<PathBuf>,
    pub read_write: Vec<PathBuf>,
    pub deny_exact: Vec<PathBuf>,
    /// Concrete paths from glob expansion (all backends, v1).
    pub deny_expanded: Vec<String>,
    /// Raw glob patterns (kept for Seatbelt-regex emission in phase 2).
    pub deny_globs: Vec<String>,
    pub default_read: bool,
    pub restrict_network: bool,
}

impl BackendPlan {
    /// Build the plan for `profile` on `backend`.
    ///
    /// `Off` yields a permissive empty plan (documented, not fail-closed —
    /// the user asked for no confinement). Every other profile fails closed
    /// on invalid globs, exceeded expansion caps, or walk errors.
    pub fn build(
        backend: OsBackend,
        profile: &ProfileName,
        workspace: &Path,
        config: &SandboxConfig,
    ) -> anyhow::Result<Self> {
        if matches!(profile, ProfileName::Off) {
            return Ok(Self {
                backend,
                enforcement: EnforcementKind::Policy,
                profile: "off".to_string(),
                read_only: vec![],
                read_write: vec![],
                deny_exact: vec![],
                deny_expanded: vec![],
                deny_globs: vec![],
                default_read: true,
                restrict_network: false,
            });
        }

        let resolved: SandboxProfile = profile.resolve_profile(workspace, config)?;
        let (exact, globs) = partition_deny_entries(&resolved.deny);
        // Validate every glob before expanding: fail closed, identically on
        // all backends.
        for glob in &globs {
            crate::deny::validate_deny_glob(glob)
                .map_err(|e| anyhow::anyhow!("profile '{}': {e}", resolved.name))?;
        }
        let deny_exact = effective_deny_paths(workspace, &exact);
        // Mask symlink aliases of exact denies: a denied dir reachable via
        // another path (symlinked parent, `subst` drive, bind mount) stays
        // covered at decide() time. Best-effort per entry — an unresolvable
        // path keeps its logical form (still denied logically; fail-closed
        // means never dropping the entry, not refusing the plan).
        let deny_exact = mask_symlink_aliases(&deny_exact);
        let deny_expanded = if globs.is_empty() {
            Vec::new()
        } else {
            expand_deny_globs(workspace, &globs, DENY_GLOB_CAPS).map_err(|reason| {
                anyhow::anyhow!(
                    "profile '{}': deny-glob expansion failed; refusing to start: {reason}",
                    resolved.name
                )
            })?
        };

        let enforcement = match backend {
            OsBackend::Linux => EnforcementKind::PlanLandlockBwrap,
            OsBackend::MacOs => EnforcementKind::PlanSeatbelt,
            OsBackend::Windows => EnforcementKind::Policy,
        };

        Ok(Self {
            backend,
            enforcement,
            profile: resolved.name.clone(),
            read_only: resolved.read_only.clone(),
            read_write: resolved.read_write.clone(),
            deny_exact,
            deny_expanded,
            deny_globs: globs,
            default_read: resolved.default_read,
            restrict_network: resolved.restrict_network,
        })
    }

    /// Convenience: build for the compiling host.
    pub fn for_host(
        profile: &ProfileName,
        workspace: &Path,
        config: &SandboxConfig,
    ) -> anyhow::Result<Self> {
        Self::build(OsBackend::host(), profile, workspace, config)
    }

    /// Whether this plan denies anything at all.
    pub fn has_deny(&self) -> bool {
        !self.deny_exact.is_empty() || !self.deny_expanded.is_empty() || !self.deny_globs.is_empty()
    }
}

/// Add canonical forms of exact deny paths so symlink aliases are covered.
///
/// Every input path is kept; a canonical form is added only when it resolves
/// and differs. Entries are sorted and deduped so plans stay comparable.
fn mask_symlink_aliases(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = paths.to_vec();
    out.extend(paths.iter().filter_map(|path| {
        dunce::canonicalize(path)
            .ok()
            .filter(|canonical| canonical.as_path() != path)
    }));
    out.sort();
    out.dedup();
    out
}

/// Whether this plan denies `path`, on the three rules `decide` applies.
///
/// Split out because it is asked twice per decision — once for the path as
/// written and once for its resolved form — and a deny that ran a subset of the
/// rules on the second spelling would be the same bug in a new place.
fn deny_matches(plan: &BackendPlan, path: &Path) -> bool {
    // 1. Exact deny: the path itself or any ancestor is denied.
    if plan
        .deny_exact
        .iter()
        .any(|d| path == d || path.starts_with(d))
    {
        return true;
    }

    // 2. Expanded glob matches (concrete paths from resolve time).
    if plan.deny_expanded.iter().any(|m| {
        let mp = Path::new(m);
        path == mp || path.starts_with(mp)
    }) {
        return true;
    }

    // 3. Live glob patterns (covers files created after resolve).
    !plan.deny_globs.is_empty() && glob_patterns_deny(&plan.deny_globs, path)
}

/// The same path with symlinks in its existing prefix resolved, when that is a
/// different path.
///
/// `canonicalize` fails for a file that does not exist yet, and refusing a write
/// is exactly when that happens, so the parent is resolved and the file name
/// appended. Without the fallback, a denied directory would be denied only for
/// paths that already existed.
fn resolve_alias(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = dunce::canonicalize(path) {
        return (canonical.as_path() != path).then_some(canonical);
    }
    let parent = path.parent()?;
    let name = path.file_name()?;
    let canonical_parent = dunce::canonicalize(parent).ok()?;
    let joined = canonical_parent.join(name);
    (joined.as_path() != path).then_some(joined)
}

/// Access requested for one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// Tool-layer decision for one path under a plan.
///
/// Deny wins over everything (exact paths, expanded glob matches, and glob
/// patterns matched live so files created after resolve are still covered).
/// Writes require a containing `read_write` root; reads require
/// `default_read`, a containing `read_only`/`read_write` root, or the path
/// being the workspace itself. `Off` allows everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathDecision {
    Allow,
    Deny,
}

pub fn decide(plan: &BackendPlan, path: &Path, access: Access) -> PathDecision {
    if plan.profile == "off" {
        return PathDecision::Allow;
    }

    if deny_matches(plan, path) {
        return PathDecision::Deny;
    }

    // The same checks against the path with symlinks resolved. A denied
    // directory reached through a symlink is still that directory —
    // `link/secret.pem` opens `real/secret.pem` — and matching only the logical
    // spelling let a rule be stepped around by writing the other name.
    if let Some(resolved) = resolve_alias(path)
        && deny_matches(plan, &resolved)
    {
        return PathDecision::Deny;
    }

    match access {
        Access::Write => {
            if plan
                .read_write
                .iter()
                .any(|r| path == r || path.starts_with(r))
            {
                PathDecision::Allow
            } else {
                PathDecision::Deny
            }
        }
        Access::Read => {
            if plan.default_read {
                return PathDecision::Allow;
            }
            if plan
                .read_only
                .iter()
                .chain(plan.read_write.iter())
                .any(|r| path == r || path.starts_with(r))
            {
                PathDecision::Allow
            } else {
                PathDecision::Deny
            }
        }
    }
}

/// Match live deny-glob patterns against one path.
///
/// Relative patterns anchor at every read/write root plus the exact path's
/// own ancestors (workspace-relative intent); absolute patterns match the
/// full path. Uses the same `globset` + `literal_separator` semantics as
/// expansion, so live matching cannot drift from launch-time expansion.
/// A pattern that fails to compile here denies (fail closed) — it passed
/// validation at plan build, so this is defense in depth.
fn glob_patterns_deny(patterns: &[String], path: &Path) -> bool {
    let Some(path_str) = path.to_str() else {
        return true; // non-UTF8: fail closed
    };
    for pattern in patterns {
        // Absolute patterns match the path directly.
        if pattern.starts_with('/') {
            if glob_match(pattern, path_str) {
                return true;
            }
            continue;
        }
        // Relative patterns match the trailing components: try the full path
        // and every suffix below each separator (equivalent to anchoring at
        // any ancestor root).
        let mut candidate = path_str;
        loop {
            if glob_match(pattern, candidate) {
                return true;
            }
            match candidate.find('/') {
                Some(i) => candidate = &candidate[i + 1..],
                None => break,
            }
        }
        // Also try basename-anchored match for `**`-prefixed patterns.
        let _ = &path_str;
    }
    false
}

fn glob_match(pattern: &str, candidate: &str) -> bool {
    let compiled = match globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
    {
        Ok(g) => g,
        Err(_) => return true, // fail closed
    };
    compiled.compile_matcher().is_match(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::SandboxConfig;

    fn workspace_plan(profile: ProfileName) -> BackendPlan {
        let ws = std::env::temp_dir();
        BackendPlan::build(OsBackend::Windows, &profile, &ws, &SandboxConfig::default()).unwrap()
    }

    #[test]
    fn off_allows_everything() {
        let plan = workspace_plan(ProfileName::Off);
        assert_eq!(
            decide(&plan, Path::new("/etc/shadow"), Access::Read),
            PathDecision::Allow
        );
        assert_eq!(
            decide(&plan, Path::new("C:\\Windows\\System32\\x"), Access::Write),
            PathDecision::Allow
        );
    }

    #[test]
    fn workspace_writes_inside_denies_outside() {
        let ws = std::env::temp_dir().join("sb-test-ws");
        let plan = BackendPlan::build(
            OsBackend::Windows,
            &ProfileName::Workspace,
            &ws,
            &SandboxConfig::default(),
        )
        .unwrap();
        assert_eq!(
            decide(&plan, &ws.join("a.txt"), Access::Write),
            PathDecision::Allow
        );
        assert_eq!(
            decide(&plan, Path::new("/etc/shadow"), Access::Write),
            PathDecision::Deny
        );
        // Workspace profile grants default read.
        assert_eq!(
            decide(&plan, Path::new("/etc/shadow"), Access::Read),
            PathDecision::Allow
        );
    }

    #[test]
    fn strict_denies_reads_outside_allowlist() {
        let plan = workspace_plan(ProfileName::Strict);
        assert_eq!(
            decide(&plan, Path::new("/etc/shadow"), Access::Write),
            PathDecision::Deny
        );
    }

    #[test]
    fn exact_deny_overrides_allow() {
        use std::collections::HashMap;
        let ws = std::env::temp_dir().join("sb-test-deny");
        let config = SandboxConfig {
            managed_by: None,
            profiles: HashMap::from([(
                "node".to_string(),
                crate::profiles::ProfileConfig {
                    extends: Some("workspace".to_string()),
                    restrict_network: None,
                    read_only: vec![],
                    read_write: vec![],
                    deny: vec!["secrets".to_string()],
                },
            )]),
        };
        let plan = BackendPlan::build(
            OsBackend::Windows,
            &ProfileName::Custom("node".to_string()),
            &ws,
            &config,
        )
        .unwrap();
        assert_eq!(
            decide(&plan, &ws.join("secrets").join("k.pem"), Access::Read),
            PathDecision::Deny
        );
        assert_eq!(
            decide(&plan, &ws.join("src").join("a.rs"), Access::Read),
            PathDecision::Allow
        );
    }

    #[test]
    fn invalid_glob_fails_plan_build() {
        use std::collections::HashMap;
        let ws = std::env::temp_dir();
        let config = SandboxConfig {
            managed_by: None,
            profiles: HashMap::from([(
                "bad".to_string(),
                crate::profiles::ProfileConfig {
                    extends: Some("workspace".to_string()),
                    restrict_network: None,
                    read_only: vec![],
                    read_write: vec![],
                    deny: vec!["**/*.{pem,key}".to_string()],
                },
            )]),
        };
        assert!(
            BackendPlan::build(
                OsBackend::Windows,
                &ProfileName::Custom("bad".to_string()),
                &ws,
                &config
            )
            .is_err()
        );
    }

    #[test]
    fn symlink_alias_of_exact_deny_stays_denied() {
        use std::collections::HashMap;
        // Scratch tree: <dir>/real (denied) + <dir>/link -> real (alias).
        // Symlink creation needs privileges on some Windows hosts: skip
        // gracefully there (the walk-time canonical masking in deny/glob.rs
        // still covers the glob half on those machines).
        let base = std::env::temp_dir().join(format!("sb-alias-{}", std::process::id()));
        let real = base.join("real");
        let link = base.join("link");
        std::fs::create_dir_all(&real).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&real, &link).is_ok();
        #[cfg(windows)]
        if !made {
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        let config = SandboxConfig {
            managed_by: None,
            profiles: HashMap::from([(
                "node".to_string(),
                crate::profiles::ProfileConfig {
                    extends: Some("workspace".to_string()),
                    restrict_network: None,
                    read_only: vec![],
                    read_write: vec![],
                    deny: vec![real.to_string_lossy().into_owned()],
                },
            )]),
        };
        let plan = BackendPlan::build(
            OsBackend::Windows,
            &ProfileName::Custom("node".to_string()),
            &base,
            &config,
        )
        .unwrap();
        // Both the logical path and the alias deny reads and writes.
        for p in [real.join("k.pem"), link.join("k.pem")] {
            assert_eq!(decide(&plan, &p, Access::Read), PathDecision::Deny, "{p:?}");
            assert_eq!(
                decide(&plan, &p, Access::Write),
                PathDecision::Deny,
                "{p:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn all_backends_build_same_deny() {
        let ws = std::env::temp_dir();
        let config = SandboxConfig::default();
        let plans: Vec<BackendPlan> = [OsBackend::Linux, OsBackend::MacOs, OsBackend::Windows]
            .iter()
            .map(|b| BackendPlan::build(*b, &ProfileName::Strict, &ws, &config).unwrap())
            .collect();
        assert!(plans.iter().all(|p| p.profile == "strict"));
        assert!(plans.iter().all(|p| p.restrict_network));
    }
}
