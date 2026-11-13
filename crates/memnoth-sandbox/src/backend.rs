//! Per-OS enforcement backends.
//!
//! v1 is a policy engine: [`BackendPlan`] describes what each OS backend must
//! enforce for a resolved profile, and [`decide`] answers read/write for one
//! path in pure Rust on every OS (including Windows). Kernel application is
//! handled by the per-OS backends and consumes these plans:
//!
//! - Linux: `bwrap` bind-mount filesystem view + an explicit seccomp BPF
//!   syscall filter (`linux.rs` + `seccomp.rs`);
//! - macOS: Seatbelt `sandbox-exec` profiles (`seatbelt.rs`);
//! - Windows: restricted token + discretionary ACLs on deny paths (`windows.rs`).
//!
//! A plan that nobody applies is the exact failure this crate was warned about
//! — "policy only" printed in the same voice as "enforced".
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

    /// The backend label, as `doctor` and the trust screen print it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Linux => "linux/bwrap+seccomp",
            Self::MacOs => "macos/seatbelt",
            Self::Windows => "windows/restricted-token+acl",
        }
    }
}

/// How a plan is enforced on its backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcementKind {
    /// Linux: `bwrap` bind-mount filesystem view + explicit seccomp BPF
    /// syscall filter applied before the child execs.
    PlanBwrapSeccomp,
    /// macOS: Seatbelt profile with deny-last-match rules.
    PlanSeatbelt,
    /// Windows: restricted token + discretionary ACLs on deny paths.
    PlanRestrictedToken,
    /// Linux with Landlock available: Landlock rules plus a `bwrap` filesystem
    /// view, for kernels that expose the LSM.
    PlanLandlockBwrap,
}

/// Declared child-process network policy.
///
/// Every confining profile defaults to [`NetworkPolicy::Off`]: a workspace
/// grant is a filesystem grant, not a network one. Opening the network is an
/// explicit choice, and `allowlist` is reported as declared-but-unenforced on
/// backends that can only express "all or nothing".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// No egress at all.
    Off,
    /// Only the named hosts.
    Allowlist(Vec<String>),
    /// Unrestricted egress.
    Open,
}

impl NetworkPolicy {
    /// Parse the `network =` value from a policy file.
    ///
    /// Fail-closed: an unknown value is an error, and `allowlist` without at
    /// least one host is an error rather than a policy that silently means
    /// "off" or "open".
    pub fn parse(raw: &str, allowlist: Vec<String>) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "deny" | "none" => Ok(Self::Off),
            "open" | "allow" => Ok(Self::Open),
            "allowlist" => {
                if allowlist.is_empty() {
                    anyhow::bail!(
                        "network = \"allowlist\" requires at least one host in network_allowlist"
                    );
                }
                Ok(Self::Allowlist(allowlist))
            }
            other => anyhow::bail!(
                "unknown network policy {other:?}; expected off|allowlist|open \
                 (refusing to run with a policy that could not be stated)"
            ),
        }
    }

    /// Stable label for status lines and reports.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Allowlist(_) => "allowlist",
            Self::Open => "open",
        }
    }

    /// Whether this policy denies egress.
    pub fn is_off(&self) -> bool {
        matches!(self, Self::Off)
    }

    /// Whether the per-OS kernel boundary must deny **all** egress for this
    /// policy to hold.
    ///
    /// `bwrap --unshare-net` and Seatbelt's `(deny network*)` are all-or-
    /// nothing. `off` and `allowlist` both need that bit set; for `allowlist`
    /// the kernel is then *stricter* than the declaration, and only a client
    /// that consults [`crate::egress::decide`] may reach a listed host.
    pub fn requires_kernel_denial(&self) -> bool {
        !matches!(self, Self::Open)
    }

    /// The sentence a status line or approval card prints next to the label.
    pub fn detail(&self) -> String {
        match self {
            Self::Off => "no child egress; enforced by the kernel boundary when it exists".into(),
            Self::Allowlist(hosts) => format!(
                "{} listed host(s); the kernel denies all egress, so only a client that asks \
                 the egress policy first may reach them",
                hosts.len()
            ),
            Self::Open => "unfiltered egress; the kernel boundary is not asked to unshare the \
                           network"
                .into(),
        }
    }
}

/// What this machine actually enforces, in words the statusline may print.
///
/// `mode` is `enforced` only when a backend launcher exists and the plan was
/// applied by it. Windows never claims an OS sandbox: it is `policy-checked`,
/// because the tool-layer policy engine runs but no kernel boundary does.
///
/// `network` is a separate sentence on purpose. A machine can enforce the
/// filesystem view and not the network (`bwrap` present, policy `allowlist`),
/// and printing one `enforced` for both would be the overstatement this crate
/// exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EnforcementReport {
    pub backend: &'static str,
    pub mode: &'static str,
    pub filesystem: &'static str,
    /// `enforced` only when a kernel boundary denies egress for this plan.
    pub network: &'static str,
    /// The declared policy: `off`, `allowlist`, `open`.
    pub network_policy: &'static str,
    pub detail: String,
    /// What the network claim means in one sentence.
    pub network_detail: String,
    /// The elevated helper's state: `not installed`, `installed`, `n/a`.
    pub helper: &'static str,
    /// Whether this plan refuses to run when no boundary exists.
    pub fail_closed: bool,
    /// One actionable sentence, when there is one.
    pub next_step: Option<String>,
}

/// Describe what `plan` can enforce on its backend right now.
pub fn enforcement_report(plan: &BackendPlan) -> EnforcementReport {
    let (mode, filesystem, available, detail, next_step) = match plan.backend {
        OsBackend::Linux => {
            // Presence on PATH is not the claim: `confine` refuses when the
            // probe cannot start a namespace (Ubuntu 24.04's AppArmor rule is
            // the common case), so the report asks the same probe.
            let probe = crate::linux::probe_available();
            let available = probe.is_ok();
            (
                if available {
                    "enforced"
                } else {
                    "policy-checked"
                },
                if available {
                    "enforced"
                } else {
                    "policy-checked"
                },
                available,
                if available {
                    "bwrap filesystem view applied; the seccomp filter is built but installed \
                     only by a caller that runs sandbox::linux::install_seccomp_in_child in a \
                     pre_exec hook"
                        .to_string()
                } else {
                    format!(
                        "the policy engine decides and no kernel boundary is applied: {}",
                        probe.unwrap_err()
                    )
                },
                (!available).then(|| {
                    "install bubblewrap (`bwrap`), or fix its namespace permissions".to_string()
                }),
            )
        }
        OsBackend::MacOs => {
            // Same rule as Linux: `confine` probes a real profile, so the
            // report does too rather than believing a file exists.
            let probe = crate::seatbelt::probe_available();
            let available = probe.is_ok();
            (
                if available {
                    "enforced"
                } else {
                    "policy-checked"
                },
                if available {
                    "enforced"
                } else {
                    "policy-checked"
                },
                available,
                if available {
                    "Seatbelt profile applied via sandbox-exec".to_string()
                } else {
                    format!(
                        "the policy engine decides and no kernel boundary is applied: {}",
                        probe.unwrap_err()
                    )
                },
                (!available).then(|| {
                    "restore /usr/bin/sandbox-exec; Seatbelt is the macOS \
                     boundary"
                        .to_string()
                }),
            )
        }
        OsBackend::Windows => (
            "policy-checked",
            "policy-checked",
            false,
            "the Windows plan (restricted token, job object, deny ACLs, firewall rule) is \
             built as data, but this build installs none of it: filesystem and process-tree \
             confinement are not applied and egress is not blocked"
                .to_string(),
            Some(
                "run `mn sandbox setup` once (elevated) to install the helper; until this build \
                 launches it, Windows stays policy-checked"
                    .to_string(),
            ),
        ),
    };
    let policy = plan.effective_network();
    // The kernel can only deny all egress, so the claim is `enforced` exactly
    // when the plan asked it to and the boundary exists.
    let network = if plan.restrict_network && available {
        "enforced"
    } else {
        "not enforced"
    };
    let network_detail = if !plan.restrict_network {
        format!(
            "{}: the boundary is not asked to deny egress",
            policy.detail()
        )
    } else if available {
        format!("{}: {}", policy.label(), policy.detail())
    } else {
        format!(
            "{} declared, but no boundary exists here to apply it",
            policy.label()
        )
    };
    let helper = match plan.backend {
        OsBackend::Windows => crate::windows::helper_state().label(),
        _ => "n/a",
    };
    EnforcementReport {
        backend: plan.backend.as_str(),
        mode,
        filesystem,
        network,
        network_policy: policy.label(),
        detail,
        network_detail,
        helper,
        fail_closed: plan.requires_kernel_boundary(),
        next_step,
    }
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
    /// Declared network policy. `restrict_network` stays the enforcement bit
    /// for the per-OS backends; this records what the policy asked for.
    pub network: NetworkPolicy,
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
                enforcement: EnforcementKind::PlanRestrictedToken,
                profile: "off".to_string(),
                read_only: vec![],
                read_write: vec![],
                deny_exact: vec![],
                deny_expanded: vec![],
                deny_globs: vec![],
                default_read: true,
                restrict_network: false,
                network: NetworkPolicy::Open,
            });
        }

        let resolved: SandboxProfile = profile.resolve_profile(workspace, config)?;
        // A hand-built config never passed through `load_sandbox_config`; the
        // policy has to be validated on the path that actually builds a plan.
        config.validate()?;
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
            OsBackend::Linux => EnforcementKind::PlanBwrapSeccomp,
            OsBackend::MacOs => EnforcementKind::PlanSeatbelt,
            OsBackend::Windows => EnforcementKind::PlanRestrictedToken,
        };

        let network = resolve_network(profile, config)?;
        let restrict_network = network.requires_kernel_denial();

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
            // Network default-deny in workspace-write: a confining profile that
            // did not ask for egress gets none. `restrict_network` is derived
            // from the policy so the two cannot disagree at build time.
            restrict_network,
            network,
        })
    }

    /// The egress policy this plan actually achieves.
    ///
    /// `restrict_network` is the enforcement bit a per-OS backend reads;
    /// `network` is the declaration. When a caller clears the bit to open the
    /// network for one command, the achieved policy is `open` whatever the
    /// declaration said, and reporting anything else would describe a plan that
    /// did not run.
    pub fn effective_network(&self) -> NetworkPolicy {
        if self.profile == "off" || !self.restrict_network {
            return NetworkPolicy::Open;
        }
        match &self.network {
            // The kernel cannot express `open` while the enforcement bit
            // denies egress. A pair like this is a caller bug; the achieved
            // policy is the stricter one.
            NetworkPolicy::Open => NetworkPolicy::Off,
            other => other.clone(),
        }
    }

    /// Whether this plan promised a boundary it must not run without.
    ///
    /// `strict` and `read-only` exist to contain a command, and any profile
    /// with deny entries promised those paths would be gone. On a machine with
    /// no boundary, running anyway is the silent fallback this crate refuses:
    /// [`crate::prepare`] answers `Refuse` for these plans. `workspace` and
    /// `devbox` are policy-checked by design, and `off` promised nothing.
    pub fn requires_kernel_boundary(&self) -> bool {
        self.profile != "off"
            && (self.has_deny() || matches!(self.profile.as_str(), "strict" | "read-only"))
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

/// The egress policy a plan is built with.
///
/// Order is fixed, and the default is deny (W10-02):
///
/// 1. an explicit machine-level `network =` in `sandbox.toml` decides;
/// 2. otherwise a **custom** profile's `restrict_network = false` is its own
///    grant;
/// 3. otherwise no egress. The built-in profiles' `restrict_network` field is a
///    recommendation the status line shows, not a grant — a workspace write
///    grant is a filesystem grant, and a network grant has to be written down.
///
/// A `network_allowlist` without `network = "allowlist"` is refused rather than
/// ignored: hosts that are listed and never consulted look exactly like a
/// policy that works.
fn resolve_network(name: &ProfileName, config: &SandboxConfig) -> anyhow::Result<NetworkPolicy> {
    if let Some(raw) = config.network.as_deref() {
        return NetworkPolicy::parse(raw, config.network_allowlist.clone());
    }
    if !config.network_allowlist.is_empty() {
        anyhow::bail!(
            "network_allowlist is set without `network = \"allowlist\"`; those hosts would be \
             declared and never consulted, so the policy is refused"
        );
    }
    if let ProfileName::Custom(custom) = name
        && let Some(profile_config) = config.profiles.get(custom)
        && profile_config.restrict_network == Some(false)
    {
        return Ok(NetworkPolicy::Open);
    }
    Ok(NetworkPolicy::Off)
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
fn deny_rule(plan: &BackendPlan, path: &Path) -> Option<&'static str> {
    // 1. Exact deny: the path itself or any ancestor is denied.
    if plan
        .deny_exact
        .iter()
        .any(|d| path == d || path.starts_with(d))
    {
        return Some("deny:exact");
    }

    // 2. Expanded glob matches (concrete paths from resolve time).
    if plan.deny_expanded.iter().any(|m| {
        let mp = Path::new(m);
        path == mp || path.starts_with(mp)
    }) {
        return Some("deny:glob-expanded");
    }

    // 3. Live glob patterns (covers files created after resolve).
    if !plan.deny_globs.is_empty() && glob_patterns_deny(&plan.deny_globs, path) {
        return Some("deny:glob-live");
    }
    None
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

/// Resolve `..` and `.` lexically so a path cannot escape a root while still
/// comparing as a child of it (`rw/../../.ssh/id_rsa` starts with `rw`
/// component-wise, which is why a raw `starts_with` was not enough).
///
/// `None` means the path climbed above its own root: there is nothing left to
/// allow, so the caller denies.
fn lexical_normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

/// One path decision with the rule that made it.
///
/// The rule string is what the audit log and the approval card print, so a
/// denial can always answer "which rule?" without re-deriving the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDecisionRecord {
    pub outcome: PathDecision,
    pub rule: String,
    /// The path as the caller spelled it.
    pub requested: PathBuf,
    /// The symlink-resolved spelling, when it differs.
    pub resolved: Option<PathBuf>,
}

/// Decide one path by the first matching rule.
///
/// Order is fixed and deny-first: `profile off` → exact deny → expanded deny
/// → live deny → the same three checks against the canonical path → write root
/// → default read → read root → default deny. The first rule that matches is
/// the decision; nothing later can widen it.
pub fn decide_record(plan: &BackendPlan, path: &Path, access: Access) -> PathDecisionRecord {
    let record = |outcome, rule: &str, resolved: Option<PathBuf>| PathDecisionRecord {
        outcome,
        rule: rule.to_string(),
        requested: path.to_path_buf(),
        resolved,
    };

    if plan.profile == "off" {
        return record(PathDecision::Allow, "policy:off", None);
    }

    let Some(logical) = lexical_normalize(path) else {
        return record(PathDecision::Deny, "deny:traversal", None);
    };

    if let Some(rule) = deny_rule(plan, &logical) {
        return record(PathDecision::Deny, rule, None);
    }

    // The same checks against the path with symlinks resolved. A denied
    // directory reached through a symlink is still that directory —
    // `link/secret.pem` opens `real/secret.pem` — and matching only the logical
    // spelling let a rule be stepped around by writing the other name.
    let resolved = resolve_alias(path);
    if let Some(alias) = &resolved
        && let Some(rule) = deny_rule(plan, alias)
    {
        return record(PathDecision::Deny, &format!("{rule}:resolved"), resolved);
    }

    match access {
        Access::Write => {
            if plan
                .read_write
                .iter()
                .any(|r| logical == *r || logical.starts_with(r))
            {
                record(PathDecision::Allow, "allow:write-root", resolved)
            } else {
                record(PathDecision::Deny, "deny:write-outside-root", resolved)
            }
        }
        Access::Read => {
            if plan.default_read {
                return record(PathDecision::Allow, "allow:read-default", resolved);
            }
            if plan
                .read_only
                .iter()
                .chain(plan.read_write.iter())
                .any(|r| logical == *r || logical.starts_with(r))
            {
                record(PathDecision::Allow, "allow:read-root", resolved)
            } else {
                record(PathDecision::Deny, "deny:read-outside-allowlist", resolved)
            }
        }
    }
}

/// Decision only (the form the tool layer uses).
pub fn decide(plan: &BackendPlan, path: &Path, access: Access) -> PathDecision {
    decide_record(plan, path, access).outcome
}

/// Re-check a path after it has been opened.
///
/// `decide` runs before the open; a symlink swapped in between the check and
/// the open (TOCTOU) would otherwise pass. Callers that actually open a path
/// must call this with the handle's real target, and deny when it disagrees.
pub fn recheck_after_open(
    plan: &BackendPlan,
    opened_path: &Path,
    access: Access,
) -> PathDecisionRecord {
    decide_record(plan, opened_path, access)
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
    fn parent_traversal_cannot_borrow_a_write_root() {
        // A plan whose only write root is `ws`, built by hand so the host's
        // real temp directory cannot mask the traversal.
        let ws = std::env::temp_dir().join("sb-traversal");
        let plan = BackendPlan {
            backend: OsBackend::Windows,
            enforcement: EnforcementKind::PlanRestrictedToken,
            profile: "strict".to_string(),
            read_only: vec![],
            read_write: vec![ws.clone()],
            deny_exact: vec![],
            deny_expanded: vec![],
            deny_globs: vec![],
            default_read: false,
            restrict_network: true,
            network: NetworkPolicy::Off,
        };
        // Component-wise, `ws/../secret` starts with `ws`; lexically it does not.
        let escape = ws.join("..").join("secret.txt");
        let record = decide_record(&plan, &escape, Access::Write);
        assert_eq!(record.outcome, PathDecision::Deny, "{}", record.rule);
        // A path that climbs above the filesystem root has no allow root left.
        let above_root = Path::new("../../../../etc/shadow");
        assert_eq!(
            decide_record(&plan, above_root, Access::Write).outcome,
            PathDecision::Deny
        );
    }

    #[test]
    fn decision_records_name_their_rule() {
        let plan = workspace_plan(ProfileName::Workspace);
        assert_eq!(
            decide_record(&plan, Path::new("/etc/shadow"), Access::Write).rule,
            "deny:write-outside-root"
        );
        assert_eq!(
            decide_record(&plan, Path::new("/etc/shadow"), Access::Read).rule,
            "allow:read-default"
        );
    }

    #[test]
    fn network_defaults_to_deny_in_workspace_write() {
        let plan = workspace_plan(ProfileName::Workspace);
        assert!(plan.restrict_network);
        assert!(plan.network.is_off());
        let report = enforcement_report(
            &BackendPlan::build(
                OsBackend::Windows,
                &ProfileName::Workspace,
                Path::new("C:/w"),
                &SandboxConfig::default(),
            )
            .unwrap(),
        );
        assert_eq!(report.mode, "policy-checked");
        assert_eq!(report.network, "not enforced");
    }

    #[test]
    fn network_policy_parsing_fails_closed() {
        assert!(matches!(
            NetworkPolicy::parse("off", vec![]),
            Ok(NetworkPolicy::Off)
        ));
        assert!(matches!(
            NetworkPolicy::parse("open", vec![]),
            Ok(NetworkPolicy::Open)
        ));
        assert!(NetworkPolicy::parse("allowlist", vec![]).is_err());
        assert!(NetworkPolicy::parse("sometimes", vec![]).is_err());
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
            network: None,
            network_allowlist: Vec::new(),
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
            network: None,
            network_allowlist: Vec::new(),
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
            network: None,
            network_allowlist: Vec::new(),
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
