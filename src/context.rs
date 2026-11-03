//! Where everything lives, resolved once.
//!
//! The paths are the product's memory of itself, so they are decided in one
//! place and printed by `memnoth doctor` rather than re-derived per command:
//!
//! | What | Default | Override |
//! |---|---|---|
//! | vault | `~/.memnoth/vault.db` | `--vault`, `MEMNOTH_VAULT`, config `vault` |
//! | vault namespace | `default` | `--namespace` (`vault-<name>.db`) |
//! | code graph | `<root>/.memnoth/graph.db` | `MEMNOTH_GRAPH` |
//! | sessions | `~/.memnoth/sessions/` | — |
//! | config | `~/.memnoth/config.toml` | — |
//! | MCP server binary | next to `memnoth`, then `PATH` | `MEMNOTH_MCP_BIN` |
//!
//! The vault is per-machine (`~/.memnoth`), because memory is about the person
//! and their machine; the graph is per-workspace, because it is about the code.
//! That split is deliberate and `SKILL.md` documents the same two paths.
//!
//! A token is never stored here. `MEMNOTH_MCP_TOKEN` is the only source, so a
//! config file can be read, copied or committed without carrying a credential.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::cli::Cli;

/// The default endpoint clients are registered against.
pub const DEFAULT_MCP_URL: &str = "https://memnoth.astrivya.ai/mcp";

/// The default tool profile when neither the flag nor the config names one.
pub const DEFAULT_PROFILE: &str = "full";

/// The CLI's own settings. Never holds a secret.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Endpoint `mcp add` writes into client configs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_url: Option<String>,
    /// Tool profile to register and to serve, when no flag says otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Vault file, when the user wants a different default than `~/.memnoth`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault: Option<String>,
    /// `[provider]`: which model answers a session turn. Holds the endpoint and
    /// the model, never the key — that is `MEMNOTH_API_KEY`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<crate::provider::ProviderConfig>,
    /// `[sync]`: cloud sync configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncConfig>,
    /// `[privacy]`: privacy and telemetry configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privacy: Option<PrivacyConfig>,
}

/// Sync configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncConfig {
    /// Whether sync is enabled (privacy gate)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Privacy configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrivacyConfig {
    /// Privacy mode: metadata-only, balanced, open
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Whether telemetry is enabled
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry_enabled: Option<bool>,
}

impl Config {
    /// Read the config, or return defaults when it does not exist.
    ///
    /// A malformed config is an error rather than a silent reset: the file holds
    /// the endpoint every client was registered against, so ignoring it would
    /// point `mcp add` somewhere the user did not choose.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("{} is not valid config TOML", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Write the config with `create_dir_all` on its parent.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("cannot serialize config")?;
        std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
    }

    /// The configured endpoint, or the published one.
    pub fn mcp_url(&self) -> String {
        self.mcp_url
            .clone()
            .unwrap_or_else(|| DEFAULT_MCP_URL.to_string())
    }

    /// The configured profile, or `full`.
    pub fn profile(&self) -> String {
        self.profile
            .clone()
            .unwrap_or_else(|| DEFAULT_PROFILE.to_string())
    }
}

/// Everything a command needs to know about the machine it is on.
#[derive(Debug, Clone)]
pub struct AppContext {
    /// Workspace root: what `kg` indexes and what `--root` names.
    pub root: PathBuf,
    /// `~/.memnoth`: the per-machine state directory.
    pub home: PathBuf,
    /// The vault file this invocation reads and writes.
    pub vault_path: PathBuf,
    /// The namespace `vault_path` was derived for.
    pub namespace: String,
    /// The code graph for `root`.
    pub graph_path: PathBuf,
    /// Where session files live.
    pub sessions_dir: PathBuf,
    /// `~/.memnoth/config.toml`.
    pub config_path: PathBuf,
    /// Loaded settings.
    pub config: Config,
    /// Machine-readable output.
    pub json: bool,
    /// Suppress progress lines.
    pub quiet: bool,
    /// Path to the current executable (for PATH fix).
    pub exe_path: PathBuf,
}

impl AppContext {
    /// Resolve every path for this invocation.
    pub fn resolve(cli: &Cli) -> Result<Self> {
        let root = match &cli.root {
            Some(path) => absolute(path)?,
            None => std::env::current_dir().context("cannot resolve the working directory")?,
        };
        let home = memnoth_home()?;
        let config_path = home.join("config.toml");
        let config = Config::load(&config_path)?;

        if cli.namespace.trim().is_empty() {
            bail!("--namespace cannot be empty");
        }

        // An explicit vault and a namespace are two answers to one question. The
        // engine's namespace resolver validates the name (it reaches a filename),
        // so this is also where traversal is refused.
        let explicit = cli
            .vault
            .clone()
            .or_else(|| std::env::var_os("MEMNOTH_VAULT").map(PathBuf::from))
            .or_else(|| config.vault.clone().map(PathBuf::from));
        let vault_path = match explicit {
            Some(path) => {
                if cli.namespace != memnoth_engine::DEFAULT_NAMESPACE {
                    bail!("--vault and --namespace name two different vaults; pass one of them");
                }
                path
            }
            None => memnoth_engine::vault_path(&home, &cli.namespace)
                .context("cannot resolve the vault path")?,
        };

        let graph_path = std::env::var_os("MEMNOTH_GRAPH")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join(".memnoth").join("graph.db"));

        Ok(Self {
            root,
            sessions_dir: home.join("sessions"),
            config_path,
            config,
            home,
            vault_path,
            namespace: cli.namespace.clone(),
            graph_path,
            json: cli.json,
            quiet: cli.quiet,
            exe_path: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("memnoth")),
        })
    }

    /// Open (creating if needed) this invocation's vault.
    pub fn open_vault(&self) -> Result<memnoth_engine::MemoryEngine> {
        if let Some(parent) = self.vault_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        memnoth_engine::open_vault(&self.vault_path)
            .with_context(|| format!("cannot open the vault at {}", self.vault_path.display()))
    }

    /// The token the MCP client headers use, from the environment only.
    pub fn mcp_token(&self) -> Option<String> {
        std::env::var("MEMNOTH_MCP_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    /// The authenticated user's API key from the credential store.
    pub fn auth_token(&self) -> Option<String> {
        use crate::auth::create_credential_store;
        let store = create_credential_store().ok()?;
        store.get_sync("api_key").ok().flatten()
    }

    /// Where the `memnoth-mcp` server binary is, if this machine has one.
    ///
    /// The server is a separate process in this build because its crate is not
    /// published here (see the workspace manifest). Looking beside the CLI first
    /// means an installed bundle can ship both and never depend on `PATH`.
    pub fn mcp_binary(&self) -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("MEMNOTH_MCP_BIN").map(PathBuf::from) {
            return path.is_file().then_some(path);
        }
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        for name in binary_names() {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        let path = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path) {
            for name in binary_names() {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    /// Create the per-machine state directory, if it does not exist.
    pub fn ensure_home(&self) -> Result<()> {
        std::fs::create_dir_all(&self.home)
            .with_context(|| format!("cannot create {}", self.home.display()))
    }

    /// Get a reference to the config
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Get a mutable reference to the config
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }

    /// Get the config file path
    pub fn config_path(&self) -> &PathBuf {
        &self.config_path
    }
}

/// `memnoth-mcp`, with the platform's executable suffix.
fn binary_names() -> Vec<String> {
    let base = "memnoth-mcp";
    if cfg!(windows) {
        vec![format!("{base}.exe"), base.to_string()]
    } else {
        vec![base.to_string()]
    }
}

/// `MEMNOTH_HOME` (the state directory itself), else `$HOME/.memnoth`.
fn memnoth_home() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("MEMNOTH_HOME").map(PathBuf::from) {
        if explicit.as_os_str().is_empty() {
            bail!("MEMNOTH_HOME is set but empty");
        }
        return Ok(explicit);
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .context("cannot find a home directory (set HOME, USERPROFILE or MEMNOTH_HOME)")?;
    Ok(home.join(".memnoth"))
}

/// Make `path` absolute without requiring that it exists.
///
/// `--root` is often a workspace a user is about to create a `.memnoth` in, and
/// `canonicalize` fails on a path that exists only after the first index run.
fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().context("cannot resolve the working directory")?;
    Ok(cwd.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests mutate process-wide environment variables, and the harness
    /// runs them on parallel threads. Without this lock one test's
    /// `MEMNOTH_HOME` becomes another test's vault path — the kind of flake that
    /// makes a green run meaningless.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn cli(root: Option<PathBuf>, vault: Option<PathBuf>, namespace: &str) -> Cli {
        Cli {
            root,
            vault,
            namespace: namespace.to_string(),
            json: false,
            quiet: false,
            resume: None,
            verb: None,
        }
    }

    #[test]
    fn the_default_vault_is_per_machine_and_the_graph_is_per_workspace() {
        let _env = env_lock();
        let tmp = std::env::temp_dir().join(format!("memnoth-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // SAFETY: the env lock is held, and the value is restored implicitly by
        // the process ending.
        unsafe {
            std::env::set_var("MEMNOTH_HOME", tmp.join("state"));
            std::env::remove_var("MEMNOTH_VAULT");
            std::env::remove_var("MEMNOTH_GRAPH");
        }
        let ctx = AppContext::resolve(&cli(Some(tmp.clone()), None, "default")).unwrap();
        assert!(ctx.vault_path.starts_with(tmp.join("state")));
        assert!(ctx.vault_path.ends_with("vault.db"));
        assert_eq!(ctx.graph_path, tmp.join(".memnoth").join("graph.db"));
        assert_eq!(ctx.root, tmp);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_namespace_selects_its_own_vault_file() {
        let _env = env_lock();
        let tmp = std::env::temp_dir().join(format!("memnoth-ns-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe {
            std::env::set_var("MEMNOTH_HOME", tmp.join("state"));
            std::env::remove_var("MEMNOTH_VAULT");
        }
        let ctx = AppContext::resolve(&cli(Some(tmp.clone()), None, "team-1")).unwrap();
        assert!(ctx.vault_path.ends_with("vault-team-1.db"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_vault_and_a_namespace_are_refused_together() {
        let _env = env_lock();
        let tmp = std::env::temp_dir().join(format!("memnoth-both-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe {
            std::env::set_var("MEMNOTH_HOME", tmp.join("state"));
        }
        let error = AppContext::resolve(&cli(
            Some(tmp.clone()),
            Some(tmp.join("other.db")),
            "team-1",
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("two different vaults"), "{error}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_namespace_cannot_escape_the_state_directory() {
        let _env = env_lock();
        let tmp = std::env::temp_dir().join(format!("memnoth-esc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe {
            std::env::set_var("MEMNOTH_HOME", tmp.join("state"));
        }
        assert!(AppContext::resolve(&cli(Some(tmp.clone()), None, "../secrets")).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_config_round_trips_and_never_gains_a_token_field() {
        let dir = std::env::temp_dir().join(format!("memnoth-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        let config = Config {
            mcp_url: Some("http://127.0.0.1:8788/mcp".into()),
            profile: Some("code".into()),
            vault: None,
            provider: None,
        };
        config.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.to_lowercase().contains("token"));
        let back = Config::load(&path).unwrap();
        assert_eq!(back.mcp_url(), "http://127.0.0.1:8788/mcp");
        assert_eq!(back.profile(), "code");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
