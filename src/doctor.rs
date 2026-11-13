//! `memnoth doctor`: what is wired, and what is not.
//!
//! The failure this exists for is the quiet one. An agent registers a client
//! against a binary that has since moved, indexes the parent of the repository
//! instead of its root, or points at a vault that was never created — and every
//! answer still looks fluent. So doctor prints paths rather than names, and says
//! which half is missing before printing anything that could be mistaken for an
//! answer.
//!
//! It reads; it never initialises. A diagnostic that creates the vault it is
//! diagnosing answers the next question wrong.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::auth::create_credential_store;
use crate::cli::DoctorArgs;
use crate::completion;
use crate::context::AppContext;
use crate::kg;
use crate::mcp::clients::{self, ClientKind, Entry};
use crate::output::{Outcome, human_bytes};
use crate::vault;
use memnoth_auth;
use memnoth_context::Scope;
/// `memnoth doctor`.
pub fn report(ctx: &AppContext) -> Result<Outcome> {
    let vault = vault_section(ctx);
    let graph = graph_section(ctx);
    let mcp = mcp_section(ctx);
    let provider = provider_section(ctx);
    let profiles = profile_section();
    let instructions = instruction_section(ctx);
    let sandbox = sandbox_section(ctx);
    let rows = checks(ctx, false)?;

    let mut text = String::new();
    text.push_str(&format!(
        "memnoth {} · {} · {}\n\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    text.push_str(&format!("workspace   {}\n", ctx.root.display()));
    text.push_str(&format!("state       {}\n", ctx.home.display()));
    text.push_str(&format!("config      {}\n\n", ctx.config_path.display()));

    for section in [
        &vault,
        &graph,
        &provider,
        &mcp,
        &profiles,
        &instructions,
        &sandbox,
    ] {
        text.push_str(section.text.trim_end());
        text.push_str("\n\n");
    }
    text.push_str(&checks_text(&rows));
    text.push('\n');

    let next = next_step(&vault, &graph, &provider, &mcp);
    if !next.is_empty() {
        text.push_str("next\n");
        for line in next {
            text.push_str(&format!("  {line}\n"));
        }
    }

    Ok(Outcome::new(
        "doctor",
        text,
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "workspace": ctx.root.display().to_string(),
            "state": ctx.home.display().to_string(),
            "config": ctx.config_path.display().to_string(),
            "vault": vault.data,
            "graph": graph.data,
            "provider": provider.data,
            "mcp": mcp.data,
            "profiles": profiles.data,
            "instructions": instructions.data,
            "sandbox": sandbox.data,
            "checks": checks_json(&rows),
        }),
    ))
}
/// `memnoth doctor` with optional `--fix` or `--bundle`.
pub fn run(ctx: &AppContext, args: &DoctorArgs) -> Result<Outcome> {
    if let Some(path) = &args.bundle {
        bundle(ctx, path, ctx.json)
    } else if args.fix {
        fix(ctx, args)
    } else {
        report(ctx)
    }
}

/// `doctor --bundle <path>` — the support artifact that cannot leak the workspace.
///
/// A support bundle is read by someone outside the machine, so it is built from
/// an allow-list rather than by redacting a report: counts, versions and the
/// namespace label go in; paths, environment values, prompts, session rows and
/// vault content never get read at all. Redaction that starts from everything
/// is a filter with a hole; this starts from nothing.
pub fn bundle(ctx: &AppContext, out: &str, as_json: bool) -> Result<Outcome> {
    let document = bundle_document(ctx)?;
    let body = format!("{}\n", serde_json::to_string_pretty(&document)?);
    if let Some(parent) = std::path::Path::new(out).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(out, &body).with_context(|| format!("write support bundle {out}"))?;
    let data = json!({
        "schema": "mn-support/1",
        "path": out,
        "bytes": body.len(),
        "redacted": true,
    });
    let text = if as_json {
        serde_json::to_string_pretty(&data)?
    } else {
        format!(
            "Wrote redacted support bundle to {out} ({} bytes)\n\
             Included: versions, platform, workspace label, store sizes, audit counts, profile.\n\
             Excluded: absolute paths, credentials, prompts, session rows and vault content.",
            body.len()
        )
    };
    Ok(Outcome::new("doctor.bundle", text, data))
}

/// The bundle's content, built only from values that are safe to share.
fn bundle_document(ctx: &AppContext) -> Result<Value> {
    let audit_path = ctx.audit_path();
    let audit = if audit_path.is_file() {
        memnoth_engine::verify_log(&audit_path)?
    } else {
        memnoth_engine::AuditVerification {
            events: 0,
            ok: true,
            first_bad_seq: None,
            reason: None,
        }
    };
    let generated_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Ok(json!({
        "schema": "mn-support/1",
        "generated_at_ms": generated_at_ms,
        "version": env!("CARGO_PKG_VERSION"),
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "redaction": {
            "paths": "absolute paths replaced by labels",
            "secrets": "no environment or config value is read",
            "content": "no prompts, session rows, file contents or vault rows",
        },
        // The directory name is the one human label that helps a maintainer
        // recognise the report; the absolute path is where identity leaks.
        "workspace": {
            "label": ctx
                .root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("<root>"),
            "root": "<workspace>",
        },
        "state": {
            "home": "<state>",
            "config_exists": ctx.config_path.is_file(),
        },
        "vault": {
            "namespace": ctx.namespace,
            "exists": ctx.vault_path.is_file(),
            "bytes": file_len(&ctx.vault_path),
        },
        "graph": {
            "exists": ctx.graph_path.is_file(),
            "bytes": file_len(&ctx.graph_path),
        },
        "audit": {
            "path": "<state>/audit/<namespace>.jsonl",
            "exists": audit_path.is_file(),
            "events": audit.events,
            "ok": audit.ok,
        },
        "profile": ctx.profile(),
    }))
}

fn file_len(path: &std::path::Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|meta| meta.len())
}
/// One diagnostic check, in the shape `doctor --fix` needs: a state, and whether
/// a fix exists for it.
///
/// `Ok` is a fact, not a pass mark: it says the thing was observed. `Warn` is
/// something the tool can live without, `Fail` something it cannot answer past.
/// Both carry `fixable`, because "broken" and "broken and unfixable here" need
/// different next steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Ok,
    Warn,
    Fail,
}

impl CheckState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// One line of diagnosis, and whether `--fix` can act on it.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub state: CheckState,
    pub fixable: bool,
    pub detail: String,
}

fn observed(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        state: CheckState::Ok,
        fixable: false,
        detail: detail.into(),
    }
}

fn warn(name: &'static str, detail: impl Into<String>, fixable: bool) -> Check {
    Check {
        name,
        state: CheckState::Warn,
        fixable,
        detail: detail.into(),
    }
}

fn failed(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        state: CheckState::Fail,
        fixable: false,
        detail: detail.into(),
    }
}

/// Every check, without applying anything.
///
/// `auth` is passed rather than always computed: a plain `mn doctor` reads files
/// and paths, and a diagnostic should not touch the keyring to print a path.
fn checks(ctx: &AppContext, auth: bool) -> Result<Vec<Check>> {
    let mut rows = Vec::new();

    rows.push(if ctx.home.is_dir() {
        observed("state", ctx.home.display().to_string())
    } else {
        warn("state", format!("{} missing", ctx.home.display()), true)
    });

    rows.push(if ctx.config_path.is_file() {
        observed("config", ctx.config_path.display().to_string())
    } else {
        warn(
            "config",
            "no config file; built-in defaults are in use".to_string(),
            true,
        )
    });

    rows.push(if ctx.vault_path.is_file() {
        observed("vault", "initialised".to_string())
    } else {
        warn(
            "vault",
            format!("{} missing", ctx.vault_path.display()),
            true,
        )
    });

    rows.push(match crate::kg::status(ctx) {
        Ok(status) if status.indexed => observed(
            "graph",
            format!("{} files · {} symbols", status.files, status.symbols),
        ),
        Ok(_) => warn("graph", "not indexed", true),
        Err(error) => failed("graph", format!("{error:#}")),
    });

    let shell = completion::detect_shell();
    rows.push(path_check(ctx, shell));
    rows.push(completions_check(shell)?);

    rows.push(mcp_config_check(ctx)?);
    rows.push(keyring_check(ctx));
    rows.push(stale_lock_files_check(ctx));
    rows.push(telemetry_check(ctx));
    rows.push(trust_check(ctx));

    if auth {
        rows.push(
            match crate::auth::auth_status(ctx, &crate::cli::AuthStatusArgs {}) {
                Ok(status) if status.data["authenticated"].as_bool().unwrap_or(false) => {
                    observed("auth", "signed in".to_string())
                }
                Ok(_) => warn(
                    "auth",
                    "not signed in — run `mn auth login` (never run automatically by --fix)"
                        .to_string(),
                    false,
                ),
                Err(error) => warn("auth", format!("{error}"), false),
            },
        );
    }

    Ok(rows)
}

/// Whether this folder is trusted, named as a state rather than a problem.
///
/// The first launch in an untrusted folder asks before it reads anything, so
/// the answer here is what the next launch will do. `--trust-folder` is the
/// one-run escape and is deliberately not written by `--fix`.
fn trust_check(ctx: &AppContext) -> Check {
    if crate::trust::is_trusted(&ctx.root) {
        observed("trust", "this folder is trusted".to_string())
    } else {
        warn(
            "trust",
            "not trusted — the next launch in this folder asks; `mn trust list` shows the store, `--trust-folder` allows one run"
                .to_string(),
            false,
        )
    }
}

/// Telemetry state, with the local off switch named in the same line.
///
/// A fresh install is off, so the check is `ok` rather than a warning: the
/// promise is the default, and `doctor` reports it instead of asking for trust.
fn telemetry_check(ctx: &AppContext) -> Check {
    if ctx.config.telemetry_enabled() {
        warn(
            "telemetry",
            "on — set `privacy.telemetry_enabled = false` in config.toml to stop it".to_string(),
            false,
        )
    } else {
        observed("telemetry", "off — nothing leaves this machine".to_string())
    }
}

/// Whether MCP client configs hold a `memnoth` entry, and whether one can be
/// written.
///
/// A client whose config file does not exist is not fixable here: `mcp add`
/// creates it. A client whose config exists but has no entry is fixable.
fn mcp_config_check(ctx: &AppContext) -> Result<Check> {
    let home = match clients::home_dir() {
        Some(home) => home,
        None => {
            return Ok(warn(
                "mcp_config",
                "no home directory; cannot check MCP client configs".to_string(),
                false,
            ));
        }
    };

    let mut missing = Vec::new();
    let mut unregistered = Vec::new();
    let mut parse_errors = Vec::new();

    for kind in ClientKind::ALL {
        let status = clients::status(kind, &home, &ctx.mcp_url());
        if !status.present {
            missing.push(kind.name().to_string());
        } else if !status.registered {
            unregistered.push(kind.name().to_string());
        }
        if !status.ok && status.detail.contains("cannot parse") {
            parse_errors.push(kind.name().to_string());
        }
    }

    if parse_errors.is_empty() && unregistered.is_empty() && missing.is_empty() {
        return Ok(observed("mcp_config", "MCP clients registered".to_string()));
    }

    let mut detail = String::new();
    if !unregistered.is_empty() {
        detail.push_str(&format!("not registered: {}", unregistered.join(", ")));
    }
    if !missing.is_empty() {
        if !detail.is_empty() {
            detail.push_str("; ");
        }
        detail.push_str(&format!("no config: {}", missing.join(", ")));
    }
    if !parse_errors.is_empty() {
        if !detail.is_empty() {
            detail.push_str("; ");
        }
        detail.push_str(&format!("unparseable: {}", parse_errors.join(", ")));
    }

    Ok(warn("mcp_config", detail, !unregistered.is_empty()))
}

/// Whether the credential store can be opened and its files have sane
/// permissions. The keyring is never written to by --fix; only file-store
/// permissions are corrected.
fn keyring_check(_ctx: &AppContext) -> Check {
    let store = match create_credential_store() {
        Ok(store) => store,
        Err(error) => {
            return warn(
                "keyring",
                format!("credential store cannot be opened: {error:#}"),
                false,
            );
        }
    };

    // The file store is the one whose permissions we can correct. Try a
    // no-op read to confirm it is readable; if it is not, a permission fix
    // may help.
    // A no-op read, not `list_keys`: the store's listing is async and this
    // diagnostic is not, and a read is the same permission the keyring needs.
    let file_store = match store.get_sync("whoami") {
        Ok(_) => None,
        Err(error) => Some(format!("{error:#}")),
    };

    match file_store {
        None => observed("keyring", "credential store readable".to_string()),
        Some(error) => warn(
            "keyring",
            format!("credential store unreadable: {error}"),
            true,
        ),
    }
}

/// Stale lock files left behind by a crashed indexer or server.
///
/// SQLite `-wal`/`-shm` files beside a missing database, and orphaned lock
/// files in the state directory, are removed by --fix.
fn stale_lock_files_check(ctx: &AppContext) -> Check {
    let mut stale: Vec<PathBuf> = Vec::new();

    // SQLite sidecar files whose primary database is gone.
    for db in [&ctx.graph_path, &ctx.coord_path()] {
        for sidecar in [db.with_extension("db-wal"), db.with_extension("db-shm")] {
            if sidecar.is_file() && !db.is_file() {
                stale.push(sidecar);
            }
        }
    }

    // Orphaned lock files in the state directory.
    if let Ok(entries) = std::fs::read_dir(&ctx.home) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path.extension().is_some_and(|ext| ext == "lock")
                && !path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .contains("coord")
            {
                stale.push(path);
            }
        }
    }

    if stale.is_empty() {
        return observed("lock_files", "no stale lock files".to_string());
    }

    let detail = stale
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    warn("lock_files", format!("stale lock file(s): {detail}"), true)
}

fn path_check(ctx: &AppContext, shell: Option<completion::Shell>) -> Check {
    let dir = ctx.exe_path.parent().unwrap_or(std::path::Path::new(""));
    if in_path(&ctx.exe_path) {
        return observed("path", format!("{} is on PATH", dir.display()));
    }
    if let Some(rc) = shell_rc(shell)
        && let Ok(content) = std::fs::read_to_string(&rc)
        && content.contains(&dir.display().to_string())
    {
        return warn(
            "path",
            format!(
                "the PATH line is already in {}; open a new shell",
                rc.display()
            ),
            false,
        );
    }
    warn(
        "path",
        format!(
            "{} is not on PATH — `mn doctor --fix` prints the line, `--fix --yes` adds it",
            dir.display()
        ),
        true,
    )
}

fn completions_check(shell: Option<completion::Shell>) -> Result<Check> {
    match shell {
        Some(shell) => {
            let path = completion::completion_path(shell)?;
            if completion::is_installed(shell)? {
                Ok(observed("completions", path.display().to_string()))
            } else {
                Ok(warn(
                    "completions",
                    format!("none for {shell:?} — `mn completion install`"),
                    true,
                ))
            }
        }
        None => Ok(warn(
            "completions",
            "shell not detected; set SHELL or run `mn completion install --shell <shell>`"
                .to_string(),
            false,
        )),
    }
}

/// The automatic fixes, one per fixable check.
enum Fix {
    StateDir,
    Config,
    Vault,
    Graph,
    PathLine,
    Completions,
    McpConfig,
    Keyring,
    LockFiles,
}

impl Fix {
    fn from_check(name: &str) -> Option<Self> {
        Some(match name {
            "state" => Self::StateDir,
            "config" => Self::Config,
            "vault" => Self::Vault,
            "graph" => Self::Graph,
            "path" => Self::PathLine,
            "completions" => Self::Completions,
            "mcp_config" => Self::McpConfig,
            "keyring" => Self::Keyring,
            "lock_files" => Self::LockFiles,
            _ => return None,
        })
    }

    fn label(&self) -> &'static str {
        match self {
            Self::StateDir => "create state directory",
            Self::Config => "write default config",
            Self::Vault => "initialise vault",
            Self::Graph => "build code graph",
            Self::PathLine => "PATH line",
            Self::Completions => "install shell completions",
            Self::McpConfig => "MCP client config",
            Self::Keyring => "keyring permissions",
            Self::LockFiles => "stale lock files",
        }
    }
}

/// What one fix did. `Manual` is not a failure: a line the user has to paste is
/// the correct outcome for a dotfile we must not edit.
enum Applied {
    Done(String),
    Manual(String),
    Skipped(String),
}

/// `doctor --fix`: run each fix once, then re-check and say what changed.
///
/// Three properties matter more than the list of fixes. It is **idempotent** — a
/// second run finds nothing to do. It is **dry-runnable** — `--dry-run` prints
/// the plan and touches nothing. And it **never edits a dotfile or signs anyone
/// in** without `--yes`: a diagnostic that rewrites `.zshrc` or opens a browser
/// has become a side effect.
fn fix(ctx: &AppContext, args: &DoctorArgs) -> Result<Outcome> {
    let before = checks(ctx, true)?;
    let plan: Vec<Fix> = before
        .iter()
        .filter(|check| check.fixable && check.state != CheckState::Ok)
        .filter_map(|check| Fix::from_check(check.name))
        .collect();

    let mut fixed = Vec::new();
    let mut manual = Vec::new();
    let mut skipped = Vec::new();

    if !args.dry_run {
        for fix in &plan {
            match apply(ctx, fix, args.yes)? {
                Applied::Done(detail) => fixed.push(format!("{}: {detail}", fix.label())),
                Applied::Manual(line) => manual.push(format!("{}: {line}", fix.label())),
                Applied::Skipped(reason) => skipped.push(format!("{}: {reason}", fix.label())),
            }
        }
    }

    let after = checks(ctx, true)?;

    let mut text = format!(
        "memnoth {} · {} · {}\n\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    if args.dry_run {
        text.push_str("would fix (nothing applied)\n");
        if plan.is_empty() {
            text.push_str("  none\n");
        }
        for fix in &plan {
            text.push_str(&format!("  → {}\n", fix.label()));
        }
        text.push('\n');
    } else {
        if !fixed.is_empty() {
            text.push_str("fixed\n");
            for item in &fixed {
                text.push_str(&format!("  ✓ {item}\n"));
            }
            text.push('\n');
        }
        if !manual.is_empty() {
            text.push_str("needs you\n");
            for item in &manual {
                text.push_str(&format!("  ! {item}\n"));
            }
            text.push('\n');
        }
        if !skipped.is_empty() {
            text.push_str("skipped\n");
            for item in &skipped {
                text.push_str(&format!("  ○ {item}\n"));
            }
            text.push('\n');
        }
    }

    text.push_str(&checks_text(&after));
    text.push('\n');
    if after.iter().all(|check| check.state == CheckState::Ok) {
        text.push_str("nothing to fix — every check is green\n");
    } else if !args.dry_run && fixed.is_empty() && manual.is_empty() {
        text.push_str("no automatic fixes left; the warn and fail lines above need you\n");
    }

    let report = report(ctx)?;
    Ok(Outcome::new(
        "doctor.fix",
        text,
        json!({
            "dry_run": args.dry_run,
            "would_fix": plan.iter().map(|fix| fix.label()).collect::<Vec<_>>(),
            "fixed": fixed,
            "manual": manual,
            "skipped": skipped,
            "checks": checks_json(&after),
            "before": checks_json(&before),
            "report": report.data,
        }),
    ))
}

fn apply(ctx: &AppContext, fix: &Fix, yes: bool) -> Result<Applied> {
    match fix {
        Fix::StateDir => {
            std::fs::create_dir_all(&ctx.home)
                .with_context(|| format!("create {}", ctx.home.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&ctx.home, std::fs::Permissions::from_mode(0o700));
            }
            Ok(Applied::Done(format!("created {}", ctx.home.display())))
        }
        Fix::Config => {
            std::fs::create_dir_all(&ctx.home)?;
            if let Some(backup) = backup_if_exists(&ctx.config_path)? {
                eprintln!(
                    "Backed up {} to {}",
                    ctx.config_path.display(),
                    backup.display()
                );
            }
            crate::context::Config::default().save(&ctx.config_path)?;
            Ok(Applied::Done(format!(
                "wrote {}",
                ctx.config_path.display()
            )))
        }
        Fix::Vault => {
            // `vault::init` speaks the CLI's error envelope; this function speaks
            // `anyhow`, so the message is carried across without losing it.
            vault::init(ctx).map_err(|error| anyhow::anyhow!("{}", error.message))?;
            Ok(Applied::Done(format!(
                "initialised {}",
                ctx.vault_path.display()
            )))
        }
        Fix::Graph => {
            let mut progress = |msg: &str| eprintln!("{msg}");
            kg::index(ctx, false, false, &mut progress)?;
            Ok(Applied::Done("indexed the code graph".to_string()))
        }
        Fix::PathLine => path_fix(ctx, yes),
        Fix::McpConfig => mcp_config_fix(ctx),
        Fix::Keyring => keyring_fix(ctx),
        Fix::LockFiles => stale_lock_files_fix(ctx),
        Fix::Completions => match completion::detect_shell() {
            Some(shell) => {
                let path = completion::install_completions(shell)?;
                Ok(Applied::Done(format!("wrote {}", path.display())))
            }
            None => Ok(Applied::Skipped("no shell detected".to_string())),
        },
    }
}

/// Write MCP client configs for every detected client that lacks a `memnoth`
/// entry.
///
/// Only clients whose config file already exists are written: `mcp add`
/// creates missing ones. The local `memnoth-mcp` binary is preferred over the
/// hosted endpoint when it is available.
fn mcp_config_fix(ctx: &AppContext) -> Result<Applied> {
    let home = match clients::home_dir() {
        Some(home) => home,
        None => {
            return Ok(Applied::Skipped("no home directory".to_string()));
        }
    };

    let binary = ctx.mcp_binary();
    let url = ctx.mcp_url();
    let token = ctx.mcp_token();
    let profile = ctx.profile();

    let entry = if let Some(path) = binary {
        let path = path.display().to_string();
        let mut env = std::collections::BTreeMap::new();
        env.insert("MEMNOTH_ROOT".to_string(), ctx.root.display().to_string());
        env.insert("MEMNOTH_MCP_READ_ONLY".to_string(), "1".to_string());
        if let Some(t) = token {
            env.insert("MEMNOTH_MCP_TOKEN".to_string(), t);
        }
        Entry::stdio(
            path,
            vec!["serve".to_string(), "--profile".to_string(), profile],
            env,
        )
    } else {
        Entry::http(url.clone(), token.clone())
    };

    let mut written = Vec::new();
    for kind in ClientKind::ALL {
        let status = clients::status(kind, &home, &url);
        if !status.present {
            continue;
        }
        if status.registered {
            continue;
        }
        clients::write(kind, &home, &entry, false)?;
        written.push(kind.name().to_string());
    }

    if written.is_empty() {
        Ok(Applied::Skipped(
            "no unregistered client configs found".to_string(),
        ))
    } else {
        Ok(Applied::Done(format!("wrote {}", written.join(", "))))
    }
}

/// Correct file-store credential permissions: make the credentials file
/// owner-readable only on Unix.
fn keyring_fix(_ctx: &AppContext) -> Result<Applied> {
    let path = memnoth_auth::default_credentials_path();
    if !path.is_file() {
        return Ok(Applied::Skipped("no credentials file".to_string()));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms)
            .with_context(|| format!("set permissions on {}", path.display()))?;
    }

    Ok(Applied::Done(format!(
        "fixed permissions on {}",
        path.display()
    )))
}

/// Remove stale lock files: SQLite sidecars whose primary database is gone,
/// and orphaned `.lock` files in the state directory.
fn stale_lock_files_fix(ctx: &AppContext) -> Result<Applied> {
    let mut removed: Vec<PathBuf> = Vec::new();

    for db in [&ctx.graph_path, &ctx.coord_path()] {
        for sidecar in [db.with_extension("db-wal"), db.with_extension("db-shm")] {
            if sidecar.is_file() && !db.is_file() {
                std::fs::remove_file(&sidecar)
                    .with_context(|| format!("remove {}", sidecar.display()))?;
                removed.push(sidecar);
            }
        }
    }

    if ctx.home.is_dir()
        && let Ok(entries) = std::fs::read_dir(&ctx.home)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path.extension().is_some_and(|ext| ext == "lock")
                && !path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .contains("coord")
            {
                std::fs::remove_file(&path)
                    .with_context(|| format!("remove {}", path.display()))?;
                removed.push(path);
            }
        }
    }

    if removed.is_empty() {
        Ok(Applied::Skipped("none found".to_string()))
    } else {
        let detail = removed
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Ok(Applied::Done(format!("removed {detail}")))
    }
}

/// The PATH fix prints by default and edits only when told twice: `--fix --yes`.
///
/// A diagnostic that silently appends to a shell rc has changed the user's
/// environment in a way `mn doctor` cannot undo, so the default is a line they
/// paste and the opt-in is explicit.
fn path_fix(ctx: &AppContext, yes: bool) -> Result<Applied> {
    let dir = ctx.exe_path.parent().unwrap_or(std::path::Path::new(""));
    let line = path_line(dir, completion::detect_shell());
    #[cfg(unix)]
    {
        if yes && let Some(rc) = shell_rc(completion::detect_shell()) {
            if let Some(backup) = backup_if_exists(&rc)? {
                eprintln!("Backed up {} to {}", rc.display(), backup.display());
            }
            let mut content = std::fs::read_to_string(&rc).unwrap_or_default();
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(&line);
            content.push('\n');
            std::fs::write(&rc, content).with_context(|| format!("write {}", rc.display()))?;
            return Ok(Applied::Done(format!("added to {}", rc.display())));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = yes;
    }
    Ok(Applied::Manual(line))
}

fn path_line(dir: &std::path::Path, shell: Option<completion::Shell>) -> String {
    #[cfg(windows)]
    {
        let _ = shell;
        format!("setx PATH \"{};%PATH%\"", dir.display())
    }
    #[cfg(not(windows))]
    {
        match shell {
            Some(completion::Shell::Fish) => format!("fish_add_path {}", dir.display()),
            _ => format!("export PATH=\"{}:$PATH\"", dir.display()),
        }
    }
}

fn shell_rc(shell: Option<completion::Shell>) -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        let _ = shell;
        None
    }
    #[cfg(not(windows))]
    {
        let home = dirs::home_dir()?;
        Some(match shell {
            Some(completion::Shell::Fish) => home.join(".config/fish/config.fish"),
            Some(completion::Shell::Zsh) => home.join(".zshrc"),
            _ => home.join(".bashrc"),
        })
    }
}

/// Copy a file aside before it is overwritten, never clobbering an older backup.
fn backup_if_exists(path: &std::path::Path) -> Result<Option<std::path::PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut candidate = {
        let mut name = path.as_os_str().to_os_string();
        name.push(".bak");
        std::path::PathBuf::from(name)
    };
    let mut n = 1;
    while candidate.exists() {
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".bak.{n}"));
        candidate = std::path::PathBuf::from(name);
        n += 1;
        if n > 99 {
            anyhow::bail!("too many backups beside {}", path.display());
        }
    }
    std::fs::copy(path, &candidate)
        .with_context(|| format!("back up {} to {}", path.display(), candidate.display()))?;
    Ok(Some(candidate))
}

fn checks_json(rows: &[Check]) -> Value {
    Value::Array(
        rows.iter()
            .map(|check| {
                json!({
                    "name": check.name,
                    "state": check.state.as_str(),
                    "fixable": check.fixable,
                    "detail": check.detail,
                })
            })
            .collect(),
    )
}

fn checks_text(rows: &[Check]) -> String {
    let mut text = String::from("checks\n");
    for check in rows {
        let suffix = if check.fixable && check.state != CheckState::Ok {
            " — fixable"
        } else {
            ""
        };
        text.push_str(&format!(
            "  {:<4} {:<12} {}{}\n",
            check.state.as_str(),
            check.name,
            check.detail,
            suffix
        ));
    }
    text
}

/// Check if a path is in PATH environment variable.
fn in_path(exe_path: &std::path::Path) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir == exe_path.parent().unwrap_or(std::path::Path::new("")) {
                return true;
            }
        }
    }
    false
}

struct Section {
    text: String,
    data: Value,
}

fn vault_section(ctx: &AppContext) -> Section {
    let exists = ctx.vault_path.exists();
    let bytes = std::fs::metadata(&ctx.vault_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let mut text = format!(
        "vault\n  path       {}\n  namespace  {}\n",
        ctx.vault_path.display(),
        ctx.namespace
    );
    let mut data = json!({
        "path": ctx.vault_path.display().to_string(),
        "namespace": ctx.namespace,
        "initialised": exists,
        "bytes": bytes,
    });
    if exists {
        match ctx.open_vault() {
            Ok(engine) => match engine.stats() {
                Ok(stats) => {
                    text.push_str(&format!(
                        "  entities   {}\n  facts      {}\n  size       {}\n",
                        stats.entities_current,
                        stats.observations_total,
                        human_bytes(bytes)
                    ));
                    data["entities_current"] = json!(stats.entities_current);
                    data["observations_total"] = json!(stats.observations_total);
                }
                Err(error) => {
                    text.push_str(&format!("  unreadable {error}\n"));
                    data["error"] = json!(error.to_string());
                }
            },
            Err(error) => {
                text.push_str(&format!("  unreadable {error}\n"));
                data["error"] = json!(error.to_string());
            }
        }
    } else {
        text.push_str("  state      missing — `memnoth vault init`\n");
    }
    Section { text, data }
}

fn graph_section(ctx: &AppContext) -> Section {
    let path = ctx.graph_path.display().to_string();
    let mut text = format!("code graph\n  path       {path}\n");
    let mut data = json!({ "path": path });
    match crate::kg::status(ctx) {
        Ok(status) => {
            if status.indexed {
                text.push_str(&format!(
                    "  indexed    {} files · {} symbols · {} references\n  built      {}\n  stale      {} file(s)\n",
                    status.files,
                    status.symbols,
                    status.references,
                    crate::output::human_age(status.built_at_ms, memnoth_engine::now_ms()),
                    status.stale_files
                ));
            } else {
                text.push_str("  state      not indexed — `memnoth kg index`\n");
            }
            data["indexed"] = json!(status.indexed);
            data["files"] = json!(status.files);
            data["symbols"] = json!(status.symbols);
            data["references"] = json!(status.references);
            data["stale_files"] = json!(status.stale_files);
        }
        Err(error) => {
            text.push_str(&format!("  unreadable {error}\n"));
            data["error"] = json!(error.to_string());
        }
    }
    Section { text, data }
}

/// Which model a session turn would use, and whether its key is present.
///
/// A configured provider with no key is the quiet failure this section exists
/// for: a turn would fall back to store search, and the screen would read as the
/// model declining to answer rather than as nothing being wired.
fn provider_section(ctx: &AppContext) -> Section {
    let mut text = String::from("model\n");
    let settings = match crate::provider::Settings::load(&ctx.config) {
        Ok(settings) => settings,
        Err(error) => {
            text.push_str(&format!("  misconfigured {error:#}\n"));
            return Section {
                text,
                data: json!({ "error": format!("{error:#}") }),
            };
        }
    };

    text.push_str(&format!("  provider   {}\n", settings.kind.name()));
    text.push_str(&format!("  model      {}\n", settings.model));
    text.push_str(&format!("  endpoint   {}\n", settings.base_url));
    match settings.pricing {
        Some(pricing) => text.push_str(&format!(
            "  rates      ${:.2} in / ${:.2} out per Mtok (estimate)\n",
            pricing.input_per_mtok, pricing.output_per_mtok
        )),
        None => text.push_str("  rates      unset — turn cost is reported as unknown\n"),
    }
    text.push_str(&format!(
        "  key        {}\n",
        if settings.ready() {
            "set".to_string()
        } else {
            format!("missing — export {}", settings.key_hint())
        }
    ));
    Section {
        text,
        data: json!({
            "kind": settings.kind.name(),
            "model": settings.model,
            "base_url": settings.base_url,
            "max_tokens": settings.max_tokens,
            "priced": settings.pricing.is_some(),
            "ready": settings.ready(),
            "key_env": settings.key_hint(),
            "describe": settings.describe(),
        }),
    }
}

fn mcp_section(ctx: &AppContext) -> Section {
    let server = ctx.mcp_server();
    let mut text = String::from("mcp\n");
    let mut data = json!({
        "url": ctx.mcp_url(),
        "profile": ctx.profile(),
        "token_present": ctx.mcp_token().is_some(),
    });
    match &server {
        Some(server) => {
            text.push_str(&format!("  server     {}\n", server.path.display()));
            // A PATH hit can be a server from an older install; the version and
            // where it came from are what turn "found" into "the right one".
            text.push_str(&format!(
                "  version    {}\n  found via  {}\n",
                server.version_line(),
                server.source.name()
            ));
            data["server"] = json!(server.path.display().to_string());
            data["server_version"] = json!(server.version());
            data["server_source"] = json!(server.source.name());
            data["server_matches_cli"] = json!(server.matches_this_build());
        }
        None => {
            text.push_str(
                "  server     not found — build `crates/memnoth-mcp` or set MEMNOTH_MCP_BIN\n",
            );
            data["server"] = Value::Null;
        }
    }
    text.push_str(&format!(
        "  endpoint   {}\n  profile    {}\n",
        ctx.mcp_url(),
        ctx.profile()
    ));
    text.push_str(&format!(
        "  token      {}\n",
        if ctx.mcp_token().is_some() {
            "MEMNOTH_MCP_TOKEN is set".to_string()
        } else {
            format!(
                "missing — mint one at {}, then export MEMNOTH_MCP_TOKEN",
                crate::context::console_url("api-keys")
            )
        }
    ));
    Section { text, data }
}

fn profile_section() -> Section {
    let rows = memnoth_capability::surface();
    let mut text = String::from("tool profiles\n");
    let mut data = Vec::new();
    for row in &rows {
        text.push_str(&format!(
            "  {:<8} {:>2} tools\n",
            row.profile, row.tool_count
        ));
        data.push(json!({ "profile": row.profile, "tools": row.tool_count }));
    }
    text.push_str("  agent    transport-gated (HTTP only)\n");
    Section {
        text,
        data: json!(data),
    }
}

fn instruction_section(ctx: &AppContext) -> Section {
    let files = [
        (Scope::Enterprise, ctx.home.join("enterprise-policy.md")),
        (Scope::Team, ctx.root.join("TEAM.md")),
        (Scope::Project, ctx.root.join("AGENTS.md")),
        (Scope::User, ctx.home.join("AGENTS.md")),
    ];
    let present: Vec<String> = files
        .iter()
        .filter(|(_, path)| path.exists())
        .map(|(_, path)| path.display().to_string())
        .collect();
    let mut text = String::from("instructions\n");
    let mut data = json!({ "files": present });
    match memnoth_context::load_from(&files) {
        Ok(set) => {
            if present.is_empty() {
                text.push_str("  none       (no AGENTS.md, TEAM.md or enterprise policy found)\n");
            } else {
                text.push_str(&format!(
                    "  effective  {} rules · {} dropped\n",
                    set.rules.len(),
                    set.dropped.len()
                ));
                for path in &present {
                    text.push_str(&format!("  source     {path}\n"));
                }
            }
            data["rules"] = json!(set.rules.len());
            data["dropped"] = json!(set.dropped.len());
        }
        Err(error) => {
            text.push_str(&format!("  unreadable {error}\n"));
            data["error"] = json!(error.to_string());
        }
    }
    Section { text, data }
}

/// The sandbox config for this machine, read the same way the governor reads
/// it — including the refusal: a file that does not parse is reported here
/// rather than quietly becoming "0 profiles defined", because that number is
/// exactly what a dropped deny list looks like.
fn sandbox_config(ctx: &AppContext) -> Result<memnoth_sandbox::SandboxConfig, String> {
    memnoth_sandbox::profiles::load_sandbox_config(&ctx.root).map_err(|error| {
        // A TOML parse error is several lines; the report is one line per fact,
        // so it is flattened rather than allowed to break the section.
        format!("{error:#}")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    })
}

fn sandbox_section(ctx: &AppContext) -> Section {
    let (config, config_error) = match sandbox_config(ctx) {
        Ok(config) => (config, None),
        Err(error) => (memnoth_sandbox::SandboxConfig::default(), Some(error)),
    };
    let conflicts = memnoth_sandbox::profiles::sandbox_profile_conflicts(&ctx.root);
    // The backend question is answered by asking it, not by looking at a config
    // file: `profiles 3 defined` printed a number for a year while nothing was
    // confined, which is how a policy engine came to read as a sandbox.
    let mut plan_error: Option<String> = None;
    let plan = if config_error.is_some() {
        None
    } else {
        match memnoth_sandbox::backend::BackendPlan::build(
            memnoth_sandbox::backend::OsBackend::host(),
            &memnoth_sandbox::profiles::ProfileName::Workspace,
            &ctx.root,
            &config,
        ) {
            Ok(plan) => Some(plan),
            Err(error) => {
                plan_error = Some(format!("{error:#}"));
                None
            }
        }
    };
    let confinement = match (&config_error, &plan) {
        // A policy that cannot be read is not a policy: the commands that
        // would have run under it are refused, so the report says that instead
        // of describing a plan nobody will use.
        (Some(_), _) => memnoth_sandbox::Confinement::Unavailable {
            reason: "the sandbox policy cannot be read, so commands are refused (see `policy` \
                     above)"
                .to_string(),
        },
        (None, Some(plan)) => memnoth_sandbox::confine(plan, &ctx.root, &["true".to_string()]),
        (None, None) => memnoth_sandbox::Confinement::Unavailable {
            reason: plan_error
                .clone()
                .unwrap_or_else(|| "no backend plan could be built".to_string()),
        },
    };
    // One status model for `doctor`, `/status`, and the statusline. The rows
    // come from the same plan `confine` read, so a report cannot say `enforced`
    // while execution refuses.
    let sandbox_status = plan.as_ref().map(memnoth_sandbox::status::status);
    let mut text = String::from("sandbox\n");
    text.push_str(&format!("  profiles   {} defined\n", config.profiles.len()));
    if let Some(error) = &config_error {
        text.push_str(&format!("  policy     {error}\n"));
    }
    text.push_str("  rules      enforced (every !cmd, /tasks run and session exec)\n");
    match &sandbox_status {
        Some(status) => {
            for line in status.lines() {
                text.push_str(&line);
                text.push('\n');
            }
        }
        None => text.push_str(&format!("  backend    {}\n", confinement.describe())),
    }
    match config.governor() {
        Some(governor) => text.push_str(&format!(
            "  governed   {governor} (project config ignored)\n"
        )),
        None => text.push_str("  governed   locally\n"),
    }
    if !conflicts.is_empty() {
        text.push_str(&format!(
            "  conflicts  {} project profile(s) tried to redefine a managed name\n",
            conflicts.len()
        ));
    }
    Section {
        text,
        data: json!({
            "profiles": config.profiles.len(),
            "governor": config.governor(),
            "conflicts": conflicts,
            "rules_enforced": true,
            "backend_enforced": confinement.is_enforced(),
            "backend": confinement.describe(),
            "status": sandbox_status.as_ref().map(|status| status.to_json()),
            "config_error": config_error,
        }),
    }
}

fn next_step(vault: &Section, graph: &Section, provider: &Section, mcp: &Section) -> Vec<String> {
    let mut lines = Vec::new();
    if vault.data["initialised"] == false {
        lines.push("memnoth vault init            # create the temporal vault".to_string());
    }
    if graph.data["indexed"] == false {
        lines.push("memnoth kg index              # build the code graph".to_string());
    }
    if provider.data["ready"] == false {
        lines.push(
            "export MEMNOTH_API_KEY        # let the session answer with a model".to_string(),
        );
    }
    if mcp.data["server"].is_null() {
        lines.push(
            "memnoth mcp doctor            # registration is still checkable without the server"
                .to_string(),
        );
    }
    lines
}

#[cfg(test)]
mod bundle_tests {
    use super::*;

    fn context(root: &std::path::Path) -> AppContext {
        AppContext {
            root: root.to_path_buf(),
            home: root.join("home"),
            vault_path: root.join("home").join("vault.db"),
            namespace: "default".to_string(),
            graph_path: root.join(".memnoth").join("graph.db"),
            sessions_dir: root.join("home").join("sessions"),
            config_path: root.join("home").join("config.toml"),
            config: crate::context::Config::default(),
            json: false,
            quiet: true,
            exe_path: std::path::PathBuf::from("memnoth"),
        }
    }

    #[test]
    fn a_bundle_carries_counts_not_paths_or_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        std::fs::write(dir.path().join("home").join("vault.db"), "SECRET-VAULT-ROW").unwrap();
        let ctx = context(dir.path());
        let text = bundle_document(&ctx).unwrap().to_string();
        assert!(!text.contains("SECRET-VAULT-ROW"), "{text}");
        assert!(
            !text.contains(&dir.path().display().to_string()),
            "an absolute path leaked: {text}"
        );
        assert!(text.contains("<workspace>"), "{text}");
        assert!(text.contains("<state>"), "{text}");
        assert!(text.contains("\"exists\":true"), "{text}");
    }

    #[test]
    fn writing_a_bundle_reports_the_path_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let out = dir.path().join("support.json");
        let outcome = bundle(&ctx, out.to_str().unwrap(), false).expect("bundle");
        let body = std::fs::read_to_string(&out).expect("written");
        assert!(body.contains("mn-support/1"), "{body}");
        assert_eq!(outcome.data["redacted"], true);
        assert!(
            !body.contains(&ctx.home.display().to_string()),
            "the state path leaked: {body}"
        );
    }

    #[test]
    fn backups_never_overwrite_an_earlier_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "first").unwrap();
        let first = backup_if_exists(&file).unwrap().expect("a backup");
        std::fs::write(&file, "second").unwrap();
        let second = backup_if_exists(&file).unwrap().expect("a backup");
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "second");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second");
    }
}
