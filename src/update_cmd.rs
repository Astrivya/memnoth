//! Update, install, and uninstall commands for memnoth.
//!
//! This command is a sequence of refusals, and the order is the point: a
//! managed install, a release on another channel, a version that walks the
//! machine backwards, an unsigned release, a digest for some other platform, a
//! checksum that does not match, a binary that does not run — each decided
//! before the step it guards, so a refusal costs nothing and nothing is
//! replaced until every check has passed.
//!
//! Progress is returned as text rather than printed. `println!` in the CLI is
//! harmless and `println!` in the TUI writes over the alternate screen, so the
//! log is the outcome and each front end decides where it goes.

use anyhow::{Result, anyhow};
use memnoth_update::{
    Channel, Manifest, UpdateConfig, check_for_update, detect_managed_install, fetch_manifest,
    fetch_manifest_at,
};
use std::env;
use std::fs;

use crate::cli::{InstallArgs, UninstallArgs, UpdateArgs};
use crate::completion::{self, Shell};
use crate::output::{ErrorOutcome, ExitCode, Outcome, confirm};

/// The installer line this build is published behind.
const INSTALLER_URL: &str = "https://memnoth.astrivya.ai/install";

/// Map an engine or I/O failure into the CLI's error envelope.
fn failure(error: impl std::fmt::Display) -> ErrorOutcome {
    ErrorOutcome::new(ExitCode::Unexpected, error.to_string())
}

/// The configuration every branch shares.
///
/// `MEMNOTH_UPDATE_BASE` exists so a staging host — the same one the installer
/// already accepts — can be tested end to end. Without it the only reachable
/// release host was the production one, which made the update path untestable
/// except in production.
fn config_for(channel: Channel) -> UpdateConfig {
    UpdateConfig {
        channel,
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        minisign_pubkey: std::env::var("MEMNOTH_MINISIGN_PUBKEY")
            .ok()
            .filter(|key| !key.trim().is_empty())
            .or_else(memnoth_update::compiled_in_pubkey),
        ..Default::default()
    }
}

/// Execute update verb.
pub fn execute_update(
    _ctx: &crate::context::AppContext,
    args: &UpdateArgs,
) -> Result<Outcome, ErrorOutcome> {
    let channel = match args.channel.as_deref() {
        Some(name) => Channel::parse(name).map_err(|e| ErrorOutcome::usage(e.to_string()))?,
        None => Channel::Stable,
    };
    let config = config_for(channel);

    if args.rollback {
        let running = env::current_exe().map_err(|error| {
            failure(anyhow::Error::msg(format!(
                "cannot locate the running binary: {error}"
            )))
        })?;
        let restored = memnoth_update::rollback_binary(&running).map_err(failure)?;
        return Ok(Outcome::text(format!(
            "Restored {} from {}.\nStart it again to run the previous build.",
            running.display(),
            restored.display()
        )));
    }

    if args.check {
        return check_update(&config);
    }

    if let Some(version) = args.version.as_deref() {
        return install_specific_version(&config, version, args);
    }

    match check_for_update(&config) {
        Ok(result) if result.update_available => {
            apply(&config, &result.manifest, "latest", false, args)
        }
        Ok(result) => Ok(Outcome::text(format!(
            "memnoth {} is up to date (channel: {})",
            config.current_version,
            result.manifest.channel.dist_subdir()
        ))),
        Err(e) => Err(failure(e)),
    }
}

/// Execute install verb.
///
/// This prints the one supported route rather than pretending to install: the
/// script knows the platform table, the checksum and the `mn` alias, and a
/// second implementation here would be a second answer to those questions.
pub fn execute_install(
    _ctx: &crate::context::AppContext,
    _args: &InstallArgs,
) -> Result<Outcome, ErrorOutcome> {
    let running = env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "an unknown path".to_string());

    #[cfg(windows)]
    let command = format!("irm {INSTALLER_URL}.ps1 | iex");
    #[cfg(not(windows))]
    let command = format!("curl -fsSL {INSTALLER_URL} | sh");

    Ok(Outcome::text(format!(
        "memnoth is running from {running}\n\n\
         To install (or reinstall) the released build:\n  {command}\n\n\
         The installer verifies the release checksum and installs both names, \
         memnoth and mn."
    )))
}

/// Execute uninstall verb.
///
/// Removes the local binaries and shell completions, and removes config.
/// Vault data (`~/.memnoth` — vault.db, sessions, audit logs, coord.db) is
/// always preserved, regardless of `--keep-state`.
pub fn execute_uninstall(
    ctx: &crate::context::AppContext,
    args: &UninstallArgs,
) -> Result<Outcome, ErrorOutcome> {
    // Ask before anything else. A remote uninstaller URL, a managed-install
    // error, or a local removal are all answers to a destructive request; with
    // no terminal there is nobody to ask, so a piped run refuses with exit 2
    // instead of blocking or proceeding.
    // A dry run removes nothing, so it never asks: treating it like `--yes`
    // here keeps `uninstall --dry-run` usable from a pipe.
    if !confirm(
        "Uninstall memnoth from this machine? Your vault and sessions are kept.",
        args.yes || args.dry_run,
    )? {
        return Ok(Outcome::new(
            "uninstall",
            "Cancelled; nothing was removed.
",
            serde_json::json!({ "cancelled": true }),
        ));
    }

    let config = config_for(Channel::Stable);

    // The release's own uninstall page, if one is published, is the official
    // route. Offer it before doing anything local.
    if let Ok(manifest) = fetch_manifest(&config, config.channel)
        && let Some(uninstall) = &manifest.uninstall
        && !uninstall.url.is_empty()
    {
        return Ok(Outcome::new(
            "uninstall",
            format!(
                "Official uninstaller: {}\n\nRun it to remove memnoth, \
                         or continue below to remove the local binaries.",
                uninstall.url
            ),
            serde_json::json!({"uninstall_url": uninstall.url}),
        ));
    }

    let current_exe = env::current_exe().map_err(failure)?;

    if let Some(manager) = detect_managed_install() {
        return Err(ErrorOutcome::usage(format!(
            "memnoth is managed by {}. Use {} to uninstall.",
            manager, manager
        )));
    }

    if args.dry_run {
        let exe_path = current_exe.clone();
        let install_dir = exe_path
            .parent()
            .ok_or_else(|| ErrorOutcome::usage("Could not determine install directory"))?;
        let suffix = env::consts::EXE_SUFFIX;
        let mut would_remove: Vec<String> = Vec::new();
        for name in ["memnoth", "mn"] {
            let path = install_dir.join(format!("{name}{suffix}"));
            if path.exists() {
                would_remove.push(name.to_string());
            }
        }
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Elvish,
        ] {
            if let Ok(path) = completion::completion_path(shell)
                && path.exists()
            {
                would_remove.push(format!("completion ({shell:?})"));
            }
        }
        if !args.keep_state {
            let config_path = ctx.home.join("config.toml");
            if config_path.exists() {
                would_remove.push("config".to_string());
            }
        }
        let summary = if would_remove.is_empty() {
            "nothing".to_string()
        } else {
            would_remove.join(", ")
        };
        return Ok(Outcome::new(
            "uninstall.dry_run",
            format!("Would uninstall: {summary}\nNothing was removed.\n"),
            serde_json::json!({"dry_run": true}),
        ));
    }

    let question =
        "Uninstall memnoth? This removes the binaries and completions. Vault data is preserved.";
    if !confirm(question, args.yes)? {
        return Ok(Outcome::new(
            "uninstall",
            "Cancelled; nothing was removed.\n",
            serde_json::json!({"cancelled": true}),
        ));
    }

    let exe_path = current_exe.clone();
    let install_dir = exe_path
        .parent()
        .ok_or_else(|| ErrorOutcome::usage("Could not determine install directory"))?;

    // The installed names carry the platform's executable suffix: on Windows
    // the file is `memnoth.exe`, so looking for `memnoth` removed nothing and
    // then reported success.
    let suffix = env::consts::EXE_SUFFIX;
    let mut removed: Vec<String> = Vec::new();
    for name in ["memnoth", "mn"] {
        let path = install_dir.join(format!("{name}{suffix}"));
        if path.exists() {
            remove_installed_binary(&path, &exe_path)?;
            removed.push(name.to_string());
        }
    }

    for shell in [
        Shell::Bash,
        Shell::Zsh,
        Shell::Fish,
        Shell::PowerShell,
        Shell::Elvish,
    ] {
        if let Ok(path) = completion::completion_path(shell)
            && path.exists()
        {
            fs::remove_file(&path).map_err(failure)?;
            removed.push(format!("completion ({shell:?})"));
        }
    }

    // The vault (`ctx.home` — `~/.memnoth`) holds vault.db, sessions,
    // audit logs and coord.db. Only config.toml is removed; vault data is
    // always preserved, regardless of --keep-state.
    if !args.keep_state {
        let config_path = ctx.home.join("config.toml");
        if config_path.exists() {
            fs::remove_file(&config_path).map_err(failure)?;
            removed.push("config".to_string());
        }
    }

    Ok(Outcome::text(format!(
        "Uninstalled: {}\nInstall directory: {}",
        if removed.is_empty() {
            "nothing (the binary is not in this directory)".to_string()
        } else {
            removed.join(", ")
        },
        install_dir.display()
    )))
}

/// Remove one installed binary. The running executable cannot be deleted on
/// Windows (the image is locked), so deleting *this* process's own file goes
/// through the same post-exit helper the update rollback uses: a short-lived
/// shell that waits for us to exit and then deletes it. Every other file is a
/// plain remove.
fn remove_installed_binary(
    path: &std::path::Path,
    running: &std::path::Path,
) -> Result<(), ErrorOutcome> {
    // Only Windows refuses to delete the running image, so on other platforms
    // the argument carries no information and is not read.
    #[cfg(not(windows))]
    let _ = running;

    #[cfg(windows)]
    if path == running {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .arg("/C")
            .raw_arg(format!(
                "ping -n 3 127.0.0.1 >NUL & del /F \"{}\"",
                path.display()
            ))
            .spawn()
            .map_err(|error| failure(anyhow::Error::from(error)))?;
        return Ok(());
    }
    fs::remove_file(path).map_err(failure)
}

fn check_update(config: &UpdateConfig) -> Result<Outcome, ErrorOutcome> {
    match check_for_update(config) {
        Ok(result) if result.update_available => {
            let manifest = &result.manifest;
            let mut previous = String::new();
            if !manifest.previous_versions.is_empty() {
                previous = format!(
                    "\n\nPrevious versions still available:\n{}",
                    manifest
                        .previous_versions
                        .iter()
                        .map(|p| format!("  {} → {}", p.version, p.url))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
            Ok(Outcome::text(format!(
                "Update available: {} → {} (channel: {})\nPublished: {}\n\n{}{}\n\n\
                 Run `memnoth update` to install it.",
                config.current_version,
                manifest.version,
                manifest.channel.dist_subdir(),
                manifest.published_at.format("%Y-%m-%d %H:%M UTC"),
                manifest.notes.as_deref().unwrap_or("No release notes"),
                previous
            )))
        }
        Ok(_) => Ok(Outcome::text(format!(
            "memnoth {} is up to date (channel: {})",
            config.current_version,
            config.channel.dist_subdir()
        ))),
        Err(e) => Err(failure(e)),
    }
}

/// Fetch one release by name and install it.
///
/// The directory is `v<version>` in the published bundle and the plain version
/// on some static hosts, so both are tried before giving up. Naming a version
/// is what makes a downgrade deliberate, which is why `explicit` is true here
/// and only here.
fn install_specific_version(
    config: &UpdateConfig,
    version: &str,
    args: &UpdateArgs,
) -> Result<Outcome, ErrorOutcome> {
    let bare = version.trim().trim_start_matches('v').to_string();
    let mut last_error = None;
    for dir in [format!("v{bare}"), bare.clone()] {
        match fetch_manifest_at(&config.dist_base_url, &dir) {
            Ok(manifest) => {
                if manifest.version.trim_start_matches('v') != bare {
                    return Err(ErrorOutcome::usage(format!(
                        "the manifest at {dir} describes {} instead of {bare}",
                        manifest.version
                    )));
                }
                return apply(config, &manifest, &dir, true, args);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(failure(anyhow!(
        "no release named {version} at {}: {}",
        config.dist_base_url,
        last_error.map(|e| e.to_string()).unwrap_or_default()
    )))
}

/// Refuse what can be refused without the artifact, then hand the verified
/// release to the update engine.
fn apply(
    config: &UpdateConfig,
    manifest: &Manifest,
    artifact_dir: &str,
    explicit: bool,
    args: &UpdateArgs,
) -> Result<Outcome, ErrorOutcome> {
    if let Some(manager) = detect_managed_install()
        && !args.no_verify
    {
        return Err(ErrorOutcome::usage(format!(
            "memnoth is managed by {manager}. Use {manager} to update, or pass --no-verify to override."
        )));
    }

    // A release channel is a promise about what the artifact is. Installing a
    // stable build someone asked for by beta name would be a silent lie about
    // which channel is on the machine.
    if manifest.channel != config.channel {
        return Err(ErrorOutcome::usage(format!(
            "this release is on the {} channel, but {} was requested",
            manifest.channel.dist_subdir(),
            config.channel.dist_subdir()
        )));
    }

    memnoth_update::apply_update(config, manifest, explicit, artifact_dir, args.no_verify)
        .map_err(failure)?;

    Ok(Outcome::text(format!(
        "Updated to memnoth {} (channel: {}){}\n\n\
         This process is the old binary until it exits. Run `memnoth --version` \
         to confirm the new one, then restart any open session.",
        manifest.version,
        manifest.channel.dist_subdir(),
        if args.no_verify {
            "\nThe release signature was not checked (--no-verify); the artifact checksum was."
        } else {
            ""
        }
    )))
}

/// One sentence about a newer release, for a screen that has already opened.
///
/// `None` for every reason there is nothing to say: an opted-out machine, an
/// install a package manager owns, a release host that cannot be reached, and
/// the ordinary case of being current. A launch is never slower or noisier
/// because of this, and the day-long cache in [`memnoth_update::passive_check`]
/// is what keeps it from asking the network on every start.
///
/// `MEMNOTH_UPDATE_BASE` decides where the answer comes from, so a release
/// staged in a directory on this machine exercises the same path a published
/// one does.
pub fn passive_notice(channel: Channel) -> Option<String> {
    let config = config_for(channel);
    // A managed install is not ours to replace: the package manager would put
    // its copy back, so the notice would be advice nobody can take.
    if detect_managed_install().is_some() {
        return None;
    }
    let found = memnoth_update::passive_check(&config).ok()??;
    if !found.update_available {
        return None;
    }
    Some(notice_text(
        &found.current_version.to_string(),
        &found.latest_version.to_string(),
    ))
}

/// The sentence a session shows when a newer release exists.
///
/// Three facts, in the order a person needs them: which build is newer, which
/// one is running, and what happens if they say yes. It deliberately offers no
/// `[y]` key: the update downloads and swaps while they keep working, so
/// "install now" would be a question with no cost either way.
fn notice_text(current: &str, latest: &str) -> String {
    format!(
        "memnoth {latest} is available — this build is {current}.\n\
         /update installs it in the background; this session keeps running, and the \
         new build is what starts next time."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_names_both_versions_and_the_door_to_use() {
        let text = notice_text("0.1.0-beta.2", "0.1.0-beta.3");
        assert!(text.contains("0.1.0-beta.3"), "{text}");
        assert!(text.contains("0.1.0-beta.2"), "{text}");
        assert!(text.contains("/update"), "{text}");
        // It must not claim the session is replaced under the person's hands.
        assert!(text.contains("keeps running"), "{text}");
        assert!(text.contains("next time"), "{text}");
    }

    #[test]
    fn test_channel_parse() {
        assert!(Channel::parse("stable").is_ok());
        assert!(Channel::parse("beta").is_ok());
        assert!(Channel::parse("nightly").is_ok());
        assert!(Channel::parse("unknown").is_err());
    }

    #[test]
    fn the_version_directory_is_the_published_name() {
        // dist.sh publishes `v<version>/`, and a person types the version
        // without the prefix. Both spellings have to resolve.
        let args = UpdateArgs {
            check: false,
            channel: None,
            version: Some("0.1.0-beta.1".to_string()),
            no_verify: false,
            rollback: false,
        };
        assert_eq!(args.version.as_deref(), Some("0.1.0-beta.1"));
        assert_eq!("v0.1.0-beta.1".trim_start_matches('v'), "0.1.0-beta.1");
    }
}
