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

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io::{self, Write};

use memnoth_command::CommandRegistry;
use memnoth_context::Scope;

use crate::auth::login;
use crate::cli::{AuthLoginArgs, AuthStatusArgs, DoctorArgs};
use crate::context::AppContext;
use crate::kg;
use crate::output::{Outcome, human_bytes};
use crate::vault;
/// `memnoth doctor`.
pub fn report(ctx: &AppContext) -> Result<Outcome> {
    let vault = vault_section(ctx);
    let graph = graph_section(ctx);
    let mcp = mcp_section(ctx);
    let provider = provider_section(ctx);
    let profiles = profile_section();
    let instructions = instruction_section(ctx);
    let sandbox = sandbox_section(ctx);

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
        }),
    ))
}
/// `memnoth doctor` with optional `--fix`.
pub fn run(ctx: &AppContext, args: &DoctorArgs) -> Result<Outcome> {
    if args.fix {
        fix(ctx, args.yes)
    } else {
        report(ctx)
    }
}
/// Apply fixes for detected issues.
fn fix(ctx: &AppContext, yes: bool) -> Result<Outcome> {
    let mut fixed = Vec::new();
    let mut skipped = Vec::new();

    // 1. Fix vault: initialize if missing
    if !ctx.vault_path.exists() {
        if confirm_or_yes("Initialize vault?", yes) {
            // `vault::init` speaks the CLI's error envelope; this function speaks
            // `anyhow`, so the message is carried across without losing it.
            vault::init(ctx).map_err(|error| anyhow::anyhow!("{}", error.message))?;
            fixed.push("vault init".to_string());
        } else {
            skipped.push("vault init (skipped)".to_string());
        }
    }

    // 2. Fix code graph: index if not indexed
    let graph_status = kg::status(ctx)?;
    if !graph_status.indexed {
        if confirm_or_yes("Index code graph?", yes) {
            let mut progress = |msg: &str| eprintln!("{msg}");
            kg::index(ctx, false, false, &mut progress)?;
            fixed.push("kg index".to_string());
        } else {
            skipped.push("kg index (skipped)".to_string());
        }
    }

    // 3. Fix config: write defaults if missing
    if !ctx.config_path.exists() {
        if confirm_or_yes("Write default config?", yes) {
            let config = crate::context::Config::default();
            config.save(&ctx.config_path)?;
            fixed.push("config write defaults".to_string());
        } else {
            skipped.push("config write defaults (skipped)".to_string());
        }
    }

    // 4. Fix PATH: add memnoth to shell rc if not in PATH
    if !in_path(&ctx.exe_path) {
        if confirm_or_yes("Add memnoth to shell PATH?", yes) {
            add_to_shell_rc(&ctx.exe_path)?;
            fixed.push("PATH add to shell rc".to_string());
        } else {
            skipped.push("PATH add to shell rc (skipped)".to_string());
        }
    }

    // 5. Fix completions: install shell completions if not installed
    if !completions_installed()? {
        if confirm_or_yes("Install shell completions?", yes) {
            install_completions()?;
            fixed.push("completions install".to_string());
        } else {
            skipped.push("completions install (skipped)".to_string());
        }
    }

    // 6. Fix auth: prompt login if no token
    let auth_status = crate::auth::auth_status(ctx, &crate::cli::AuthStatusArgs {})?;
    if !auth_status.data["authenticated"].as_bool().unwrap_or(false) {
        if confirm_or_yes("Run auth login?", yes) {
            let login_args = AuthLoginArgs {
                no_browser: false,
                with_token: None,
                client_name: None,
                tier: None,
            };
            login(ctx, &login_args)?;
            fixed.push("auth login".to_string());
        } else {
            skipped.push("auth login (skipped)".to_string());
        }
    }

    // Build report
    let mut text = String::new();
    text.push_str(&format!(
        "memnoth {} · {} · {}\n\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    ));

    if !fixed.is_empty() {
        text.push_str("fixed\n");
        for item in &fixed {
            text.push_str(&format!("  ✓ {item}\n"));
        }
        text.push_str("\n");
    }

    if !skipped.is_empty() {
        text.push_str("skipped\n");
        for item in &skipped {
            text.push_str(&format!("  ○ {item}\n"));
        }
        text.push_str("\n");
    }

    if fixed.is_empty() && skipped.is_empty() {
        text.push_str("nothing to fix — all checks passed\n");
    }

    // Also run the regular report to show current state
    let report = report(ctx)?;
    text.push_str(&report.text);

    Ok(Outcome::new(
        "doctor.fix",
        text,
        json!({
            "fixed": fixed,
            "skipped": skipped,
            "report": report.data,
        }),
    ))
}

/// Confirm an action, or auto-confirm if `yes` is true.
fn confirm_or_yes(prompt: &str, yes: bool) -> bool {
    if yes {
        return true;
    }
    print!("{prompt} [y/N] ");
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
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

/// Add memnoth to shell rc file.
fn add_to_shell_rc(exe_path: &std::path::Path) -> Result<()> {
    let home = dirs::home_dir().context("cannot find home directory")?;
    let bin_dir = exe_path.parent().context("cannot get exe parent")?;
    let export_line = format!("\nexport PATH=\"{}:$PATH\"\n", bin_dir.display());

    // Try to detect shell
    let shell = std::env::var("SHELL").unwrap_or_default();
    let rc_file = if shell.contains("fish") {
        home.join(".config/fish/config.fish")
    } else if shell.contains("zsh") {
        home.join(".zshrc")
    } else {
        home.join(".bashrc")
    };

    // Read existing content
    let mut content = std::fs::read_to_string(&rc_file).unwrap_or_default();

    // Check if already present
    if content.contains(&format!("{}", bin_dir.display())) {
        return Ok(());
    }

    content.push_str(&export_line);
    std::fs::write(&rc_file, content)
        .with_context(|| format!("cannot write {}", rc_file.display()))?;

    eprintln!("Added to {}: {}", rc_file.display(), export_line.trim());
    Ok(())
}

/// Check if completions are already installed.
fn completions_installed() -> Result<bool> {
    let home = dirs::home_dir().context("cannot find home directory")?;
    let shell = std::env::var("SHELL").unwrap_or_default();

    let completion_files = if shell.contains("fish") {
        vec![home.join(".config/fish/completions/memnoth.fish")]
    } else if shell.contains("zsh") {
        vec![home.join(".zsh/completions/_memnoth"), home.join(".zshrc")]
    } else {
        vec![home.join(".bash_completion"), home.join(".bashrc")]
    };

    for file in &completion_files {
        if file.exists() {
            if let Ok(content) = std::fs::read_to_string(file) {
                if content.contains("memnoth") {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Install shell completions.
fn install_completions() -> Result<()> {
    let home = dirs::home_dir().context("cannot find home directory")?;
    let shell = std::env::var("SHELL").unwrap_or_default();

    let registry = CommandRegistry::new();
    let shell_type = if shell.contains("fish") {
        clap_complete::Shell::Fish
    } else if shell.contains("zsh") {
        clap_complete::Shell::Zsh
    } else {
        clap_complete::Shell::Bash
    };

    let completion = registry.build_completions(shell_type);

    let (dir, filename) = if shell.contains("fish") {
        let dir = home.join(".config/fish/completions");
        std::fs::create_dir_all(&dir)?;
        (dir, "memnoth.fish".to_string())
    } else if shell.contains("zsh") {
        let dir = home.join(".zsh/completions");
        std::fs::create_dir_all(&dir)?;
        (dir, "_memnoth".to_string())
    } else {
        // Bash: we just append to .bashrc
        let rc_file = home.join(".bashrc");
        let mut content = std::fs::read_to_string(&rc_file).unwrap_or_default();
        if !content.contains("memnoth") {
            content.push_str(&format!("\n# memnoth completions\n{}\n", completion));
            std::fs::write(&rc_file, content)
                .with_context(|| format!("cannot write {}", rc_file.display()))?;
            eprintln!("Added completions to {}", rc_file.display());
        }
        return Ok(());
    };

    let file_path = dir.join(filename);
    std::fs::write(&file_path, completion)
        .with_context(|| format!("cannot write {}", file_path.display()))?;
    eprintln!("Installed completions to {}", file_path.display());

    Ok(())
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
    let binary = ctx.mcp_binary();
    let mut text = String::from("mcp\n");
    let mut data = json!({
        "url": ctx.mcp_url(),
        "profile": ctx.profile(),
        "token_present": ctx.mcp_token().is_some(),
    });
    match &binary {
        Some(path) => {
            text.push_str(&format!("  server     {}\n", path.display()));
            data["server"] = json!(path.display().to_string());
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
            "MEMNOTH_MCP_TOKEN is set"
        } else {
            "missing — mint one at /api-keys, then export MEMNOTH_MCP_TOKEN"
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
/// it. A missing or unparseable file is the ungoverned default, which is why
/// this reports "0 profiles defined" rather than failing the whole report.
fn sandbox_config(ctx: &AppContext) -> memnoth_sandbox::SandboxConfig {
    let path = ctx.home.join("sandbox.toml");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

fn sandbox_section(ctx: &AppContext) -> Section {
    let config = sandbox_config(ctx);
    let conflicts = memnoth_sandbox::profiles::sandbox_profile_conflicts(&ctx.root);
    let mut text = String::from("sandbox\n");
    text.push_str(&format!("  profiles   {} defined\n", config.profiles.len()));
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
