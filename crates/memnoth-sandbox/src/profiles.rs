//! Sandbox profiles.
//!
//! Port note: the profile model (built-ins `workspace | devbox | read-only |
//! strict | off`, named `Custom` profiles with `extends`, additive-only
//! project merge, network inheritance, symlink-cautious grants) follows
//! grok-build's `xai-grok-sandbox/src/profiles.rs` (see
//! THIRD-PARTY-NOTICES.md), reimplemented as a pure policy engine: resolving
//! a profile yields data, never kernel state. Enforcement plans per OS live
//! in [`crate::backend`].
//!
//! Built-ins:
//! - `workspace`: read the world, write the workspace + state dir + temp.
//! - `devbox`: write almost everything except `/data` (which stays readable
//!   but is deliberately NOT a kernel-deny — it is excluded from the write
//!   set rather than added to `deny`, so custom profiles extending `devbox`
//!   must not inherit `/data` into their deny list).
//! - `read-only`: write only the state dir + temp; no child network.
//! - `strict`: read an allowlist of system paths + the workspace, write only
//!   the workspace + `sessions/` + temp; no child network.
//! - `off`: no confinement. Must never be a base profile for `extends`.

use crate::allow_path::normalize_allow_path;
use crate::deny::{effective_deny_paths, partition_deny_entries};
use crate::paths::{
    PROJECT_CONFIG_DIR, SANDBOX_CONFIG_FILENAME, essential_writable_paths,
    essential_writable_paths_minimal, essential_writable_paths_strict, memnoth_home,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A resolved sandbox profile: fully-specified allow/deny sets for logging,
/// backend plans, and path decisions.
#[derive(Debug, Clone)]
pub struct SandboxProfile {
    /// Display name.
    pub name: String,
    /// Paths the agent can read (but not write).
    pub read_only: Vec<PathBuf>,
    /// Paths the agent can read and write.
    pub read_write: Vec<PathBuf>,
    /// Paths denied entirely (overrides read_only/read_write).
    pub deny: Vec<PathBuf>,
    /// Whether to grant read access to the entire filesystem by default.
    pub default_read: bool,
    /// Whether child processes should have network blocked.
    pub restrict_network: bool,
}

/// One named profile block in `sandbox.toml`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ProfileConfig {
    #[serde(default)]
    pub extends: Option<String>,
    #[serde(default)]
    pub restrict_network: Option<bool>,
    #[serde(default)]
    pub read_only: Vec<String>,
    #[serde(default)]
    pub read_write: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// Whole `sandbox.toml` file: an optional enterprise lock plus named profiles.
///
/// `managed_by = "<governor>"` at the top level is §9 managed mode: the
/// project config is ignored (the global/MDM definition wins outright) and
/// the TUI banner shows the governor. Only the global file can set it — a
/// project file's `managed_by` is never read.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
pub struct SandboxConfig {
    #[serde(default)]
    pub managed_by: Option<String>,
    #[serde(default)]
    pub profiles: HashMap<String, ProfileConfig>,
}

impl SandboxConfig {
    /// Whether enterprise/MDM lock is active.
    pub fn is_managed(&self) -> bool {
        self.managed_by.as_ref().is_some_and(|g| !g.is_empty())
    }

    /// Governor name for the §9 banner, if managed.
    pub fn governor(&self) -> Option<&str> {
        self.managed_by.as_deref().filter(|g| !g.is_empty())
    }
}

/// A sandbox profile name: five built-ins plus user-defined customs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProfileName {
    #[default]
    Workspace,
    Devbox,
    ReadOnly,
    Strict,
    Off,
    Custom(String),
}

impl ProfileName {
    /// Whether this built-in restricts child-process network.
    pub fn restricts_network(&self) -> bool {
        matches!(self, Self::ReadOnly | Self::Strict)
    }

    /// Whether this profile confines at all (`off` does not).
    pub fn confines(&self) -> bool {
        !matches!(self, Self::Off)
    }
}

impl std::fmt::Display for ProfileName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Workspace => write!(f, "workspace"),
            Self::Devbox => write!(f, "devbox"),
            Self::ReadOnly => write!(f, "read-only"),
            Self::Strict => write!(f, "strict"),
            Self::Off => write!(f, "off"),
            Self::Custom(name) => write!(f, "{name}"),
        }
    }
}

impl std::str::FromStr for ProfileName {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "workspace" => Ok(Self::Workspace),
            "devbox" => Ok(Self::Devbox),
            "read-only" | "readonly" => Ok(Self::ReadOnly),
            "strict" => Ok(Self::Strict),
            "off" | "none" => Ok(Self::Off),
            // Validation happens when the custom profile is resolved.
            other => Ok(Self::Custom(other.to_string())),
        }
    }
}

/// Load sandbox config from `$MEMNOTH_HOME/sandbox.toml` (global) plus
/// `<workspace>/.memnoth/sandbox.toml` (project, additive only).
///
/// A project config may ADD new profile names only. It cannot redefine a
/// name already present globally: last-write-wins would let a malicious
/// workspace hollow out a user/enterprise custom profile while keeping the
/// trusted name.
///
/// A file that cannot be parsed is an error, not an empty config: a typo in a
/// deny list would otherwise be reported once on stderr and then silently
/// dropped, which is the one failure this policy engine exists to prevent. A
/// missing file is not an error — that is how a machine that never configured
/// a sandbox looks.
pub fn load_sandbox_config(workspace: &Path) -> anyhow::Result<SandboxConfig> {
    let mut config = SandboxConfig::default();

    let global_path = memnoth_home().join(SANDBOX_CONFIG_FILENAME);
    if let Some(global) = load_config_file(&global_path)? {
        config = global;
    }

    let project_path = workspace
        .join(PROJECT_CONFIG_DIR)
        .join(SANDBOX_CONFIG_FILENAME);
    let project = load_config_file(&project_path)?;

    Ok(merge_configs(&mut config, project))
}

/// Pure merge step behind [`load_sandbox_config`] (kept separate so the
/// managed early-return is unit-testable without touching `MEMNOTH_HOME`).
fn merge_configs(config: &mut SandboxConfig, project: Option<SandboxConfig>) -> SandboxConfig {
    // Managed mode: the global/MDM definition wins outright; a workspace
    // must not widen or rename enterprise policy.
    if config.is_managed() {
        return config.clone();
    }
    if let Some(project) = project {
        merge_project_profiles(config, project);
    }
    config.clone()
}

/// Names where a project config tried to redefine a global custom profile
/// with different content (for surfacing a loud warning, not for merging —
/// the global definition always wins).
pub fn sandbox_profile_conflicts(workspace: &Path) -> Vec<String> {
    let global = load_config_file(&memnoth_home().join(SANDBOX_CONFIG_FILENAME))
        .ok()
        .flatten()
        .unwrap_or_default();
    let project = load_config_file(
        &workspace
            .join(PROJECT_CONFIG_DIR)
            .join(SANDBOX_CONFIG_FILENAME),
    )
    .ok()
    .flatten()
    .unwrap_or_default();
    mismatched_profile_names(&global, &project)
}

fn mismatched_profile_names(global: &SandboxConfig, project: &SandboxConfig) -> Vec<String> {
    let mut names: Vec<String> = project
        .profiles
        .iter()
        .filter(|(name, _)| matches!(name.parse(), Ok(ProfileName::Custom(_))))
        .filter_map(|(name, project_profile)| {
            global
                .profiles
                .get(name)
                .filter(|global_profile| *global_profile != project_profile)
                .map(|_| name.to_owned())
        })
        .collect();
    names.sort_unstable();
    names
}

/// Names already defined globally are ignored: a workspace cannot replace a
/// global custom profile's policy. A project's `managed_by`, if present, is
/// likewise never merged — only the global file can lock management.
fn merge_project_profiles(config: &mut SandboxConfig, project: SandboxConfig) {
    for (name, profile) in project.profiles {
        config.profiles.entry(name).or_insert(profile);
    }
}

fn load_config_file(path: &Path) -> anyhow::Result<Option<SandboxConfig>> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow::anyhow!(
                "{} cannot be read ({error}); refusing to run with a policy this machine \
                 could not state",
                path.display()
            ));
        }
    };
    if content.trim().is_empty() {
        return Ok(Some(SandboxConfig::default()));
    }
    let config = toml::from_str(&content).map_err(|error| {
        anyhow::anyhow!(
            "{} does not parse ({error}); refusing to run with a policy that was silently \
             replaced by the defaults",
            path.display()
        )
    })?;
    Ok(Some(config))
}

impl ProfileName {
    /// Resolve this profile into a fully-specified [`SandboxProfile`].
    ///
    /// Fail-closed: unknown custom names, `off`-as-base, custom-extends-custom,
    /// and unresolvable entries are `Err` — callers must refuse startup for
    /// any profile that carries a deny list or network restriction rather
    /// than proceed unenforced.
    pub fn resolve_profile(
        &self,
        workspace: &Path,
        config: &SandboxConfig,
    ) -> anyhow::Result<SandboxProfile> {
        match self {
            // `off` is handled by callers (no plan at all). Reaching here is
            // almost always a custom profile with `extends = "off"`; Err, never panic.
            Self::Off => anyhow::bail!(
                "sandbox profile 'off' cannot be resolved as a base profile; \
                 choose a built-in base (workspace, devbox, read-only, strict)"
            ),

            Self::Workspace => Ok(SandboxProfile {
                name: "workspace".to_string(),
                read_only: vec![],
                read_write: essential_writable_paths(workspace),
                deny: vec![],
                default_read: true,
                restrict_network: false,
            }),

            Self::Devbox => {
                // Everything writable except /data. /data is excluded from
                // read_write (not writable) but deliberately NOT a deny: it
                // stays readable via default_read, and custom profiles
                // extending devbox must not inherit it into deny.
                let exclude = [PathBuf::from("/data")];
                let mut read_write = vec![workspace.to_path_buf()];
                if let Ok(entries) = std::fs::read_dir("/") {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if exclude.contains(&path) {
                            continue;
                        }
                        if matches!(path.to_str(), Some("/proc" | "/sys" | "/dev")) {
                            continue;
                        }
                        if path.is_dir() {
                            read_write.push(path);
                        }
                    }
                }
                Ok(SandboxProfile {
                    name: "devbox".to_string(),
                    read_only: vec![],
                    read_write,
                    deny: vec![],
                    default_read: true,
                    restrict_network: false,
                })
            }

            Self::ReadOnly => Ok(SandboxProfile {
                name: "read-only".to_string(),
                read_only: vec![],
                read_write: essential_writable_paths_minimal(),
                deny: vec![],
                default_read: true,
                restrict_network: true,
            }),

            Self::Strict => {
                let home = std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/root"));
                let system_read: Vec<PathBuf> = [
                    "/usr",
                    "/lib",
                    "/lib64",
                    "/bin",
                    "/sbin",
                    "/etc",
                    "/dev",
                    "/proc",
                    "/sys",
                    "/tmp",
                    "/run",
                    "/var",
                    "/System",
                    "/Library",
                    "/private",
                    "C:\\Windows\\System32",
                ]
                .iter()
                .map(PathBuf::from)
                .filter(|p| p.exists())
                .chain(std::iter::once(home.join("Library")))
                .filter(|p| p.exists())
                .chain(std::iter::once(workspace.to_path_buf()))
                .chain(std::iter::once(memnoth_home()))
                .collect();

                Ok(SandboxProfile {
                    name: "strict".to_string(),
                    read_only: system_read,
                    read_write: essential_writable_paths_strict(workspace),
                    deny: vec![],
                    default_read: false,
                    restrict_network: true,
                })
            }

            Self::Custom(name) => {
                let profile_config = config.profiles.get(name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Custom sandbox profile '{name}' not found. \
                         Define it in {} or {}/sandbox.toml:\n\n\
                         [profiles.{name}]\n\
                         extends = \"workspace\"\n\
                         read_only = [\"/data\"]\n",
                        memnoth_home().join(SANDBOX_CONFIG_FILENAME).display(),
                        PROJECT_CONFIG_DIR,
                    )
                })?;

                // Start from the base profile if `extends` is set.
                let (base, mut profile) = if let Some(base_name) = &profile_config.extends {
                    let base: ProfileName = base_name.parse().map_err(|e: String| {
                        anyhow::anyhow!("Profile '{name}' extends invalid base: {e}")
                    })?;
                    if matches!(base, Self::Off) {
                        anyhow::bail!(
                            "Profile '{name}' extends '{base_name}', but 'off'/'none' \
                             is not a valid base profile"
                        );
                    }
                    if matches!(base, Self::Custom(_)) {
                        anyhow::bail!(
                            "Profile '{name}' extends '{base_name}', but custom profiles \
                             cannot extend other custom profiles (only built-ins)"
                        );
                    }
                    let resolved = base.resolve_profile(workspace, config)?;
                    (base, resolved)
                } else {
                    (
                        Self::Workspace,
                        Self::Workspace.resolve_profile(workspace, config)?,
                    )
                };

                profile.name = name.clone();

                if let Some(restrict_net) = profile_config.restrict_network {
                    profile.restrict_network = restrict_net;
                }

                for path_str in &profile_config.read_only {
                    if let Some(path) = normalize_allow_path(path_str) {
                        profile.read_only.push(path);
                    }
                }
                for path_str in &profile_config.read_write {
                    if let Some(path) = normalize_allow_path(path_str) {
                        profile.read_write.push(path);
                    }
                }

                // Deny entries stay raw: globs are partitioned and enforced
                // as patterns, not directory grants.
                for path_str in &profile_config.deny {
                    profile.deny.push(PathBuf::from(path_str));
                }

                // Devbox has no hook/state write-deny concept in memnoth v1;
                // keep the marker behavior: extending devbox inherits no deny.
                let _ = &base;

                Ok(profile)
            }
        }
    }

    /// Resolve plus partition the deny list into exact paths (canonicalized
    /// against the workspace) and glob patterns. Fails closed on invalid
    /// globs: the caller must refuse startup.
    pub fn resolve_denied(
        &self,
        workspace: &Path,
        config: &SandboxConfig,
    ) -> anyhow::Result<ResolvedDeny> {
        let profile = self.resolve_profile(workspace, config)?;
        let (exact, globs) = partition_deny_entries(&profile.deny);
        for glob in &globs {
            crate::deny::validate_deny_glob(glob)
                .map_err(|e| anyhow::anyhow!("profile '{}': {e}", profile.name))?;
        }
        let exact_paths = effective_deny_paths(workspace, &exact);
        Ok(ResolvedDeny {
            exact: exact_paths,
            globs,
        })
    }
}

/// A profile's partitioned, validated deny list.
#[derive(Debug, Clone, Default)]
pub struct ResolvedDeny {
    /// Exact paths, resolved against the workspace, sorted, deduped.
    pub exact: Vec<PathBuf>,
    /// Validated glob patterns (raw form; expanded at plan build time).
    pub globs: Vec<String>,
}

impl ResolvedDeny {
    pub fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.globs.is_empty()
    }
}

/// Whether a `resolve_profile` failure must refuse startup: any profile that
/// enforces its own deny list or a network restriction cannot proceed with
/// an empty plan. Devbox/workspace resolution is infallible today, so those
/// arms are defense in depth.
pub fn resolve_failure_must_refuse(profile: &ProfileName, workspace: &Path) -> bool {
    if matches!(profile, ProfileName::Off) {
        return false;
    }
    let config = load_sandbox_config(workspace).unwrap_or_default();
    match profile {
        ProfileName::Custom(name) => config.profiles.get(name).is_some_and(|p| {
            if !p.deny.is_empty() {
                return true;
            }
            match p.restrict_network {
                Some(explicit) => explicit,
                None => p
                    .extends
                    .as_deref()
                    .and_then(|base| base.parse::<ProfileName>().ok())
                    .is_some_and(|base| base.restricts_network()),
            }
        }),
        ProfileName::ReadOnly | ProfileName::Strict => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_profile_names() {
        assert_eq!(
            "workspace".parse::<ProfileName>().unwrap(),
            ProfileName::Workspace
        );
        assert_eq!(
            "devbox".parse::<ProfileName>().unwrap(),
            ProfileName::Devbox
        );
        assert_eq!(
            "read-only".parse::<ProfileName>().unwrap(),
            ProfileName::ReadOnly
        );
        assert_eq!(
            "readonly".parse::<ProfileName>().unwrap(),
            ProfileName::ReadOnly
        );
        assert_eq!(
            "strict".parse::<ProfileName>().unwrap(),
            ProfileName::Strict
        );
        assert_eq!("off".parse::<ProfileName>().unwrap(), ProfileName::Off);
        assert_eq!("none".parse::<ProfileName>().unwrap(), ProfileName::Off);
        assert_eq!(
            "my-custom-profile".parse::<ProfileName>().unwrap(),
            ProfileName::Custom("my-custom-profile".to_string())
        );
    }

    #[test]
    fn display_roundtrip() {
        for profile in [
            ProfileName::Workspace,
            ProfileName::Devbox,
            ProfileName::ReadOnly,
            ProfileName::Strict,
            ProfileName::Off,
        ] {
            let s = profile.to_string();
            let parsed: ProfileName = s.parse().unwrap();
            assert_eq!(parsed, profile);
        }
    }

    #[test]
    fn built_in_network_restriction_values() {
        let workspace = std::env::temp_dir();
        let config = SandboxConfig::default();
        for (name, expected) in [
            (ProfileName::Workspace, false),
            (ProfileName::Devbox, false),
            (ProfileName::ReadOnly, true),
            (ProfileName::Strict, true),
        ] {
            let resolved = name.resolve_profile(&workspace, &config).unwrap();
            assert_eq!(resolved.restrict_network, expected, "{name}");
        }
    }

    #[test]
    fn custom_inherits_and_overrides_network() {
        let workspace = std::env::temp_dir();
        let mut profiles = HashMap::new();
        for (name, extends, restrict) in [
            ("strict-inherited", "strict", None),
            ("read-only-inherited", "read-only", None),
            ("strict-unrestricted", "strict", Some(false)),
            ("workspace-restricted", "workspace", Some(true)),
        ] {
            profiles.insert(
                name.to_string(),
                ProfileConfig {
                    extends: Some(extends.to_string()),
                    restrict_network: restrict,
                    read_only: vec![],
                    read_write: vec![],
                    deny: vec![],
                },
            );
        }
        let config = SandboxConfig {
            managed_by: None,
            profiles,
        };
        for (name, expected) in [
            ("strict-inherited", true),
            ("read-only-inherited", true),
            ("strict-unrestricted", false),
            ("workspace-restricted", true),
        ] {
            let resolved = ProfileName::Custom(name.to_string())
                .resolve_profile(&workspace, &config)
                .unwrap();
            assert_eq!(resolved.restrict_network, expected, "{name}");
        }
    }

    #[test]
    fn custom_extends_devbox_has_no_data_in_deny() {
        // Regression: devbox excludes /data via a local list, not profile.deny.
        // Inheriting it into the deny set would wrongly read-deny /data.
        let workspace = std::env::temp_dir();
        let config = SandboxConfig {
            managed_by: None,
            profiles: HashMap::from([(
                "mydev".to_string(),
                ProfileConfig {
                    extends: Some("devbox".to_string()),
                    restrict_network: None,
                    read_only: vec![],
                    read_write: vec![],
                    deny: vec![],
                },
            )]),
        };
        let resolved = ProfileName::Custom("mydev".to_string())
            .resolve_profile(&workspace, &config)
            .unwrap();
        assert!(
            !resolved.deny.contains(&PathBuf::from("/data")),
            "custom profile extending devbox must not inherit /data into deny: {:?}",
            resolved.deny
        );
    }

    #[test]
    fn custom_profile_not_found_fails_closed() {
        let workspace = std::env::temp_dir();
        let result = ProfileName::Custom("nonexistent".to_string())
            .resolve_profile(&workspace, &SandboxConfig::default());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn extends_off_returns_err_not_panic() {
        let workspace = std::env::temp_dir();
        let config = SandboxConfig {
            managed_by: None,
            profiles: HashMap::from([(
                "broken".to_string(),
                ProfileConfig {
                    extends: Some("off".to_string()),
                    restrict_network: None,
                    read_only: vec![],
                    read_write: vec![],
                    deny: vec![],
                },
            )]),
        };
        let err = ProfileName::Custom("broken".to_string())
            .resolve_profile(&workspace, &config)
            .expect_err("extends=off must Err");
        assert!(err.to_string().contains("off"), "unexpected error: {err}");
    }

    #[test]
    fn custom_extends_custom_returns_err() {
        let workspace = std::env::temp_dir();
        let config = SandboxConfig {
            managed_by: None,
            profiles: HashMap::from([
                (
                    "base".to_string(),
                    ProfileConfig {
                        extends: Some("workspace".to_string()),
                        restrict_network: None,
                        read_only: vec![],
                        read_write: vec![],
                        deny: vec![],
                    },
                ),
                (
                    "child".to_string(),
                    ProfileConfig {
                        extends: Some("base".to_string()),
                        restrict_network: None,
                        read_only: vec![],
                        read_write: vec![],
                        deny: vec![],
                    },
                ),
            ]),
        };
        assert!(
            ProfileName::Custom("child".to_string())
                .resolve_profile(&workspace, &config)
                .is_err()
        );
    }

    #[test]
    fn project_cannot_redefine_global_profile() {
        let mut config = SandboxConfig {
            managed_by: None,
            profiles: HashMap::from([(
                "secure".to_string(),
                ProfileConfig {
                    extends: Some("workspace".to_string()),
                    restrict_network: Some(true),
                    read_only: vec![],
                    read_write: vec![],
                    deny: vec!["/home/user/.ssh".to_string()],
                },
            )]),
        };
        let project = SandboxConfig {
            managed_by: None,
            profiles: HashMap::from([
                (
                    "secure".to_string(),
                    ProfileConfig {
                        extends: Some("workspace".to_string()),
                        restrict_network: Some(false),
                        read_only: vec![],
                        read_write: vec!["/".to_string()],
                        deny: vec![],
                    },
                ),
                (
                    "project-only".to_string(),
                    ProfileConfig {
                        extends: Some("workspace".to_string()),
                        restrict_network: None,
                        read_only: vec![],
                        read_write: vec![],
                        deny: vec!["./secrets".to_string()],
                    },
                ),
            ]),
        };
        merge_project_profiles(&mut config, project);
        assert_eq!(
            config.profiles["secure"].deny,
            vec!["/home/user/.ssh".to_string()],
            "global deny must be preserved"
        );
        assert_eq!(config.profiles["secure"].restrict_network, Some(true));
        assert!(config.profiles["secure"].read_write.is_empty());
        assert!(config.profiles.contains_key("project-only"));
    }

    #[test]
    fn strict_does_not_grant_default_read() {
        let resolved = ProfileName::Strict
            .resolve_profile(&std::env::temp_dir(), &SandboxConfig::default())
            .unwrap();
        assert!(!resolved.default_read);
        assert!(resolved.restrict_network);
    }

    #[test]
    fn mismatched_names_reports_only_changed_customs() {
        let profile = |restrict_network| ProfileConfig {
            extends: Some("workspace".to_string()),
            restrict_network: Some(restrict_network),
            read_only: vec![],
            read_write: vec![],
            deny: vec![],
        };
        let global = SandboxConfig {
            managed_by: None,
            profiles: HashMap::from([
                ("dev".to_string(), profile(false)),
                ("same".to_string(), profile(false)),
            ]),
        };
        let project = SandboxConfig {
            managed_by: None,
            profiles: HashMap::from([
                ("dev".to_string(), profile(true)),
                ("same".to_string(), profile(false)),
                ("project-only".to_string(), profile(true)),
                ("devbox".to_string(), profile(true)),
            ]),
        };
        assert_eq!(mismatched_profile_names(&global, &project), vec!["dev"]);
    }

    #[test]
    fn managed_config_parses_and_locks() {
        let config: SandboxConfig = toml::from_str(
            "managed_by = \"enterprise-mdm\"\n[profiles.secure]\nextends = \"strict\"\n",
        )
        .unwrap();
        assert!(config.is_managed());
        assert_eq!(config.governor(), Some("enterprise-mdm"));
        assert!(config.profiles.contains_key("secure"));

        // Absent or empty governor: unmanaged.
        let plain: SandboxConfig =
            toml::from_str("[profiles.dev]\nextends = \"workspace\"\n").unwrap();
        assert!(!plain.is_managed());
        assert_eq!(plain.governor(), None);
        assert!(SandboxConfig::default().governor().is_none());
    }

    #[test]
    fn managed_load_ignores_project_file() {
        let profile = || ProfileConfig {
            extends: Some("workspace".to_string()),
            restrict_network: None,
            read_only: vec![],
            read_write: vec![],
            deny: vec![],
        };
        let mut global = SandboxConfig {
            managed_by: Some("enterprise-mdm".to_string()),
            profiles: HashMap::from([("secure".to_string(), profile())]),
        };
        let project = SandboxConfig {
            // A project file cannot seize the lock: merge_configs never
            // reads project.managed_by (only load() reads the global one).
            managed_by: Some("workspace-impostor".to_string()),
            profiles: HashMap::from([("project-only".to_string(), profile())]),
        };
        let merged = merge_configs(&mut global, Some(project));
        assert!(merged.is_managed());
        assert_eq!(merged.governor(), Some("enterprise-mdm"));
        assert!(merged.profiles.contains_key("secure"));
        assert!(
            !merged.profiles.contains_key("project-only"),
            "managed mode must ignore the project file: {:?}",
            merged.profiles.keys().collect::<Vec<_>>()
        );

        // Unmanaged: project additions still merge.
        let mut open = SandboxConfig::default();
        let merged = merge_configs(
            &mut open,
            Some(SandboxConfig {
                managed_by: None,
                profiles: HashMap::from([("project-only".to_string(), profile())]),
            }),
        );
        assert!(!merged.is_managed());
        assert!(merged.profiles.contains_key("project-only"));
    }

    #[test]
    fn parse_toml_config() {
        let toml_str = r#"
[profiles.devbox]
extends = "workspace"
restrict_network = true
read_only = ["/data"]
deny = ["/data/private"]

[profiles.ci]
extends = "strict"
read_write = ["/tmp/ci-artifacts"]
"#;
        let config: SandboxConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.profiles.len(), 2);
        assert_eq!(config.profiles["devbox"].read_only, vec!["/data"]);
        assert_eq!(config.profiles["devbox"].deny, vec!["/data/private"]);
    }
}
