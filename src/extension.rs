//! `mn extension new|list|uninstall|doctor` — manage extensions.
//!
//! One extension format: a directory with manifest.json plus agents/, skills/,
//! commands/, hooks/, mcp/, themes/, README.md. Install/uninstall/doctor.

use anyhow::Result;
use serde_json::json;

use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};
use memnoth_skills::extension::ExtensionStore;

/// `mn extension` — dispatch the extension subcommand.
pub fn run(
    ctx: &AppContext,
    action: &crate::cli::ExtensionCommand,
) -> Result<Outcome, ErrorOutcome> {
    match action {
        crate::cli::ExtensionCommand::New(args) => extension_new(ctx, args),
        crate::cli::ExtensionCommand::List(args) => extension_list(args),
        crate::cli::ExtensionCommand::Uninstall(args) => extension_uninstall(args),
        crate::cli::ExtensionCommand::Doctor(args) => extension_doctor(args),
        crate::cli::ExtensionCommand::Run(args) => extension_run(ctx, args),
    }
}

/// `mn extension run <EXT>:<COMMAND>` — print the prompt the command resolves
/// to.
///
/// Printing rather than executing is the honest half: the prompt is the
/// extension's contribution, and running it needs a session. `/ext:<name>` in a
/// session resolves the same file through the same resolver.
fn extension_run(
    ctx: &AppContext,
    args: &crate::cli::ExtensionRunArgs,
) -> Result<Outcome, ErrorOutcome> {
    let prompt = crate::extension_commands::resolve(&ctx.home, &args.command)
        .map_err(|error| ErrorOutcome::from_anyhow(&error))?;
    let text = format!(
        "extension command {}:{} ({} bytes)\n\n{}",
        prompt.extension,
        prompt.name,
        prompt.body.len(),
        prompt.body
    );
    Ok(Outcome::new(
        "extension.run",
        text,
        serde_json::json!({
            "extension": prompt.extension,
            "command": prompt.name,
            "path": prompt.path.display().to_string(),
            "body": prompt.body,
        }),
    ))
}

fn extensions_dir(ctx: &AppContext) -> Result<std::path::PathBuf> {
    Ok(ctx.home.join("extensions"))
}

/// `mn extension new <path>` — install an extension from a directory.
fn extension_new(
    ctx: &AppContext,
    args: &crate::cli::ExtensionNewArgs,
) -> Result<Outcome, ErrorOutcome> {
    let dir = extensions_dir(ctx)?;
    let store = ExtensionStore::new(dir);
    let installed = store
        .install(std::path::Path::new(&args.path))
        .map_err(|e| ErrorOutcome::new(crate::output::ExitCode::Unexpected, format!("{e}")))?;
    let text = format!(
        "extension {}\n  version: {}\n  path: {}\n  installed: agents={} skills={} commands={}\n",
        installed.name,
        installed.version,
        installed.path.display(),
        installed.manifest.agents.len(),
        installed.manifest.skills.len(),
        installed.manifest.commands.len(),
    );
    let data = json!({
        "name": installed.name,
        "version": installed.version,
        "path": installed.path.display().to_string(),
    });
    Ok(Outcome::new("extension.new", text.trim_end(), data))
}

/// `mn extension list` — list installed extensions.
fn extension_list(args: &crate::cli::ExtensionListArgs) -> Result<Outcome, ErrorOutcome> {
    let dir = std::env::var("MEMNOTH_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".memnoth"))
        .join("extensions");
    let store = ExtensionStore::new(dir);
    let list = store
        .list()
        .map_err(|e| ErrorOutcome::new(crate::output::ExitCode::Unexpected, format!("{e}")))?;
    if args.json {
        let data = json!({
            "extensions": list.iter().map(|e| json!({
                "name": e.name,
                "version": e.version,
                "path": e.path.display().to_string(),
            })).collect::<Vec<_>>(),
        });
        return Ok(Outcome::new(
            "extension.list",
            serde_json::to_string_pretty(&data).unwrap(),
            data,
        ));
    }
    let mut text = String::from("extensions\n");
    for e in &list {
        text.push_str(&format!(
            "  {} v{} ({})\n",
            e.name,
            e.version,
            e.path.display()
        ));
    }
    if list.is_empty() {
        text.push_str("  none installed\n");
    }
    Ok(Outcome::new(
        "extension.list",
        text.trim_end(),
        json!({"count": list.len()}),
    ))
}

/// `mn extension uninstall <name>` — remove an extension.
fn extension_uninstall(args: &crate::cli::ExtensionUninstallArgs) -> Result<Outcome, ErrorOutcome> {
    let dir = std::env::var("MEMNOTH_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".memnoth"))
        .join("extensions");
    let store = ExtensionStore::new(dir);
    store
        .uninstall(&args.name)
        .map_err(|e| ErrorOutcome::new(crate::output::ExitCode::Unexpected, format!("{e}")))?;
    Ok(Outcome::new(
        "extension.uninstall",
        format!("extension {} removed", args.name),
        json!({"name": args.name}),
    ))
}

/// `mn extension doctor` — verify installed extension manifests.
fn extension_doctor(args: &crate::cli::ExtensionDoctorArgs) -> Result<Outcome, ErrorOutcome> {
    let dir = std::env::var("MEMNOTH_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".memnoth"))
        .join("extensions");
    let store = ExtensionStore::new(dir);
    let problems = store
        .doctor()
        .map_err(|e| ErrorOutcome::new(crate::output::ExitCode::Unexpected, format!("{e}")))?;
    if args.json {
        let data = json!({
            "problems": problems,
        });
        return Ok(Outcome::new(
            "extension.doctor",
            serde_json::to_string_pretty(&data).unwrap(),
            data,
        ));
    }
    let mut text = String::from("extension doctor\n");
    if problems.is_empty() {
        text.push_str("  all extensions valid\n");
    } else {
        for p in &problems {
            text.push_str(&format!("  {}\n", p));
        }
    }
    Ok(Outcome::new(
        "extension.doctor",
        text.trim_end(),
        json!({"problems": problems.len()}),
    ))
}
