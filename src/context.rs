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

/// Path prefix the console bundle is served under on the cloud origin.
///
/// The console is an SPA mounted at `/app` (`nest_service("/app", …)` in
/// `memnoth-cloud`, matching its Vite `base` and router `basepath`). The API
/// (`/api/v1/*`) and the MCP endpoint (`/mcp`) stay at the origin root, so this
/// prefix belongs to console *pages* only.
pub const CONSOLE_BASE_PATH: &str = "/app";

/// Console origin: `MEMNOTH_CONSOLE_URL` when set, else production.
///
/// An **origin**, never a page path — [`console_url`] adds the mount, so a
/// self-hosted override gets the same `/app` prefix rather than a doubled one.
pub fn console_origin() -> String {
    std::env::var("MEMNOTH_CONSOLE_URL")
        .map(|url| url.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| "https://memnoth.astrivya.ai".to_string())
}

/// Absolute URL of one console page, e.g. `console_url("api-keys")`.
///
/// One definition of the mount, so a page link cannot drift back to the origin
/// root where the console answers nothing.
pub fn console_url(page: &str) -> String {
    format!(
        "{}{CONSOLE_BASE_PATH}/{}",
        console_origin(),
        page.trim_start_matches('/')
    )
}

/// Origin the published documentation lives on.
///
/// Docs are served by the marketing site, not the control plane, so they keep
/// their own origin: `MEMNOTH_CONSOLE_URL` sends a self-hoster's console links
/// to their own deployment and must not drag documentation along with them.
pub const DOCS_ORIGIN: &str = "https://memnoth.astrivya.ai";

/// Absolute URL of one documentation page, e.g. `docs_url("vault/init")`.
///
/// One definition of the docs path, so a help link in an error message cannot
/// drift to a page the site does not serve.
pub fn docs_url(page: &str) -> String {
    format!("{DOCS_ORIGIN}/docs/{}", page.trim_start_matches('/'))
}

/// Console pages the CLI links to — the SPA's own top-level routes.
const CONSOLE_ROUTES: &[&str] = &[
    "api-keys",
    "connectors",
    "dashboard",
    "org",
    "projects",
    "sessions",
    "settings",
    "sharing",
    "teams",
    "usage",
    "workspaces",
];

/// Put a console page the cloud handed us back under the mount.
///
/// The device-authorization response carries its own `verification_uri`, built
/// by whichever server answered. A deployment on an older build — or one whose
/// public origin is configured as a bare host — returns `/api-keys`, the origin
/// root, where the console bundle is not served: the browser lands on a 404 and
/// the sign-in cannot be approved. Rather than trust the server's idea of the
/// mount, any URL whose first path segment is a console route gets
/// [`CONSOLE_BASE_PATH`] inserted. Everything else (the API, `/auth/*`, `/mcp`,
/// an already-mounted `/app/…`) is returned untouched.
pub fn mounted_console_page(url: &str) -> String {
    // The authority ends at the first `/`, `?` or `#` after the scheme. A URL
    // with none of them is a bare origin and holds no page to mount.
    let authority_start = url.find("://").map(|i| i + 3).unwrap_or(0);
    let rest = &url[authority_start..];
    let Some(offset) = rest.find(['/', '?', '#']) else {
        return url.to_string();
    };
    let path = &rest[offset..];
    if !path.starts_with('/') {
        return url.to_string();
    }
    let end = path.find(['?', '#']).unwrap_or(path.len());
    let first = path[1..end].split('/').next().unwrap_or("");
    if !CONSOLE_ROUTES
        .iter()
        .any(|route| route.eq_ignore_ascii_case(first))
    {
        return url.to_string();
    }
    let insert_at = authority_start + offset;
    format!(
        "{}{CONSOLE_BASE_PATH}{}",
        &url[..insert_at],
        &url[insert_at..]
    )
}

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
    /// The cloud project every session from this machine is filed under, as
    /// `mn projects use` stores it. Read by `report` and `sync` when a row does
    /// not name one; `MEMNOTH_PROJECT_ID` still wins over it for a single run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
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
    /// `[ui]`: presentation preferences that outlive a single session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui: Option<UiConfig>,
    /// `[tools]`: what the chat agent may not do. The one gate that is a
    /// person's decision rather than a code path's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsConfig>,
}

/// `[tools]` — the agent's denials, written by `/permissions`.
///
/// Only denials are stored. Everything else is already a decision the code
/// makes (a write asks first, a read does not), and a config file that repeats
/// a default goes stale the moment the default changes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsConfig {
    /// Tool names the agent may not call at all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied: Vec<String>,
}

/// Presentation preferences written by `mn set-theme` and
/// `mn set-screen-reader`, read when a session starts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiConfig {
    /// `default`, `high-contrast` or `no-color`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// `off` or `plain`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_reader: Option<String>,
    /// Whether the first-run panel has been shown on this machine.
    ///
    /// `None` means it has not: the next session opens on the three lines that
    /// name the commands worth running first. `mn config set ui.onboarded
    /// false` brings it back, which is why the absence and the `false` case are
    /// the same answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onboarded: Option<bool>,
    /// Model ids pinned in `/models`, so the Favorites tab survives a restart.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub favorite_models: Vec<String>,
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
    /// The single gate every outbound path consults before it sends anything.
    ///
    /// Defaults to **off**. `OPEN_CORE.md` and
    /// `web/content/docs/concepts.mdx` both promise that nothing leaves the
    /// machine by default; the call sites this replaces used `.unwrap_or(true)`,
    /// which made that promise false for every fresh install.
    ///
    /// One function on purpose: with the default written out at each call site,
    /// a third egress path could quietly pick a different one. Any new path that
    /// sends data asks this, and `mn privacy show-data` is expected to list it.
    pub fn telemetry_enabled(&self) -> bool {
        self.privacy
            .as_ref()
            .and_then(|p| p.telemetry_enabled)
            .unwrap_or(false)
    }

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

    /// Resolve every path for this invocation.
    pub fn new(cli: &Cli) -> Result<Self> {
        Self::resolve(cli)
    }

    /// The one coordination board every surface reads.
    ///
    /// It lives in the state directory rather than beside the vault so `mn
    /// team`, the TUI and worker reviews see the same rows across workspaces;
    /// the workspace is a column on a row, not a file path. Before this, three
    /// surfaces opened three different databases and none of them agreed.
    pub fn coord_path(&self) -> PathBuf {
        self.home.join("coord.db")
    }

    /// Open (creating if needed) this invocation's vault.
    pub fn open_vault(&self) -> Result<memnoth_engine::MemoryEngine> {
        if let Some(parent) = self.vault_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let engine = memnoth_engine::open_vault(&self.vault_path)
            .with_context(|| format!("cannot open the vault at {}", self.vault_path.display()))?;
        // Every vault open attaches the same destination, which is what makes
        // `mn audit` a record of the vault rather than of whichever command
        // happened to remember to write it. A malformed log fails the open: a
        // vault that silently stops auditing is worse than one that refuses to
        // start until the break is looked at (`mn audit verify` names it).
        let audit_path = self.audit_path();
        let mut ledger = memnoth_engine::AuditLedger::resume(&audit_path).with_context(|| {
            format!(
                "cannot resume the audit chain at {}; run `mn audit verify`",
                audit_path.display()
            )
        })?;
        ledger.set_actor(&self.actor());
        engine.attach_audit(ledger);
        Ok(engine)
    }

    /// The hash-chained audit destination for this namespace.
    ///
    /// One file per namespace, beside the vault, because a shared trail for
    /// every namespace would make `mn audit verify` report one tenant's break
    /// as everyone's.
    pub fn audit_path(&self) -> PathBuf {
        self.home
            .join("audit")
            .join(format!("{}.jsonl", self.namespace))
    }

    /// The person at the keyboard, as recorded on approvals and audit events.
    ///
    /// `MEMNOTH_USER` first so a shared workstation or a scripted run can name
    /// the accountable person; the OS identity is the fallback. `"unknown"` is
    /// stated rather than guessed.
    pub fn actor(&self) -> String {
        for key in ["MEMNOTH_USER", "USERNAME", "USER"] {
            if let Ok(value) = std::env::var(key) {
                let value = value.trim();
                if !value.is_empty() {
                    return value.to_string();
                }
            }
        }
        "unknown".to_string()
    }

    /// The endpoint `mcp add` writes into client configs.
    pub fn mcp_url(&self) -> String {
        self.config
            .mcp_url
            .clone()
            .unwrap_or_else(|| DEFAULT_MCP_URL.to_string())
    }

    /// The tool profile this invocation serves: `MEMNOTH_MCP_PROFILE`, then
    /// config, then `full` — the same order the server uses.
    pub fn profile(&self) -> String {
        std::env::var("MEMNOTH_MCP_PROFILE")
            .ok()
            .or_else(|| self.config.profile.clone())
            .unwrap_or_else(|| "full".to_string())
    }

    /// A mutable copy of the loaded config. Callers persist it with `save`;
    /// `AppContext` itself is shared immutably for the length of a command.
    pub fn config_mut(&self) -> Config {
        self.config.clone()
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
            // Absolutized before it is returned: the server is spawned with
            // `current_dir(workspace)`, so a relative `MEMNOTH_MCP_BIN` such as
            // CI's `target/debug/memnoth-mcp` would be resolved inside the
            // workspace, where no such file exists.
            let path = std::path::absolute(&path).unwrap_or(path);
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

    /// Get the config file path
    pub fn config_path(&self) -> &PathBuf {
        &self.config_path
    }

    /// Tool names the agent may not call, as of the config this invocation
    /// loaded. The gate in `provider` re-reads the file, so a denial saved
    /// mid-session applies to the next call rather than the next process.
    pub fn denied_tools(&self) -> Vec<String> {
        self.config
            .tools
            .as_ref()
            .map(|tools| tools.denied.clone())
            .unwrap_or_default()
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
    fn a_console_page_from_the_cloud_gains_the_mount() {
        assert_eq!(
            mounted_console_page("https://memnoth.astrivya.ai/api-keys?device=DRGA-YRKN"),
            "https://memnoth.astrivya.ai/app/api-keys?device=DRGA-YRKN"
        );
        assert_eq!(
            mounted_console_page("https://memnoth.astrivya.ai/projects/abc"),
            "https://memnoth.astrivya.ai/app/projects/abc"
        );
        assert_eq!(
            mounted_console_page("https://memnoth.astrivya.ai/app/api-keys?device=DRGA-YRKN"),
            "https://memnoth.astrivya.ai/app/api-keys?device=DRGA-YRKN"
        );
    }

    #[test]
    fn a_non_console_url_is_left_alone() {
        for url in [
            "https://memnoth.astrivya.ai/api/v1/usage",
            "https://memnoth.astrivya.ai/mcp",
            "https://memnoth.astrivya.ai/auth/callback?code=abc",
            "https://memnoth.astrivya.ai",
            "https://memnoth.astrivya.ai/pricing",
        ] {
            assert_eq!(mounted_console_page(url), url);
        }
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
            ..Default::default()
        };
        config.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.to_lowercase().contains("token"));
        let back = Config::load(&path).unwrap();
        assert_eq!(back.mcp_url(), "http://127.0.0.1:8788/mcp");
        assert_eq!(back.profile(), "code");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn telemetry_is_off_unless_the_user_turned_it_on() {
        // The published promise is "nothing by default". The call sites used to
        // spell their own default (`.unwrap_or(true)`), which is how that
        // promise became false for every fresh install. This pins the one gate.
        assert!(
            !Config::default().telemetry_enabled(),
            "a fresh install must not report"
        );

        let explicit_off = Config {
            privacy: Some(PrivacyConfig {
                mode: None,
                telemetry_enabled: Some(false),
            }),
            ..Default::default()
        };
        assert!(!explicit_off.telemetry_enabled());

        let explicit_on = Config {
            privacy: Some(PrivacyConfig {
                mode: Some("metadata-only".into()),
                telemetry_enabled: Some(true),
            }),
            ..Default::default()
        };
        assert!(explicit_on.telemetry_enabled());
    }
}
