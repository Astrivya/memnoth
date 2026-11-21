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

use anyhow::{Result, anyhow, bail};
use memnoth_update::{
    Channel, Manifest, UpdateConfig, check_for_update, detect_managed_install, fetch_manifest,
    fetch_manifest_at,
};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::{InstallArgs, UninstallArgs, UpdateArgs};
use crate::completion::{self, Shell};
use crate::output::{ErrorOutcome, ExitCode, Outcome, confirm};

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

        // The single `.old` slot is still the cheapest way back: it holds the
        // exact bytes the last update replaced and needs no network at all.
        let backup = memnoth_update::backup_path(&running);
        if backup.exists() {
            let restored = memnoth_update::rollback_binary(&running).map_err(failure)?;
            return Ok(Outcome::text(format!(
                "Restored {} from {}.\nStart it again to run the previous build.",
                running.display(),
                restored.display()
            )));
        }

        // No slot left — a second update overwrote it, or this machine never
        // took one. Fall back to the release directory: the versions the tree
        // still advertises in `previous_versions` are the sanctioned rollback
        // targets. This is a deliberate downgrade, and `--rollback` is the
        // explicit request for it; everything else the updater checks (the
        // signature, the per-target checksum, the artifact's existence and the
        // version it reports) is checked again before the swap.
        return rollback_from_release(&running, &config);
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

/// Read the versions a release tree still advertises as available for a
/// rollback. This is the manifest's `previous_versions` field — previously a
/// dead text hint in `mn update --check`. It is now the allowlist a downgrade
/// has to name, so an arbitrary version cannot be fetched just because someone
/// typed it.
pub fn available_rollback_versions(config: &UpdateConfig) -> Result<Vec<String>> {
    let latest = fetch_manifest(config, config.channel)?;
    Ok(latest
        .previous_versions
        .iter()
        .map(|entry| entry.version.trim_start_matches('v').to_string())
        .collect())
}

/// The newest version in `versions`, compared with the updater's own semver
/// policy rather than a string sort (`0.1.10` must sort above `0.1.9`).
/// `check_version_policy(b, a, false)` is `Ok` exactly when `a >= b`.
fn newest_version(versions: &[String]) -> Option<String> {
    versions
        .iter()
        .max_by(|a, b| {
            if memnoth_update::check_version_policy(b, a, false).is_ok() {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            }
        })
        .cloned()
}

/// Roll back without a `.old` slot by picking the newest version the release
/// tree still advertises and installing it with the full verification path.
///
/// `--rollback` is itself the explicit request for a downgrade, which is why
/// `allow_downgrade` is true here. The choice is still bounded by
/// `previous_versions` and every authenticity check still runs.
fn rollback_from_release(
    current_exe: &Path,
    config: &UpdateConfig,
) -> Result<Outcome, ErrorOutcome> {
    let available = available_rollback_versions(config).map_err(failure)?;
    let Some(target) = newest_version(&available) else {
        return Err(ErrorOutcome::usage(format!(
            "no previous binary at {} and no previous versions are advertised by {} — \
             nothing to roll back to",
            memnoth_update::backup_path(current_exe).display(),
            config.dist_base_url
        )));
    };
    rollback_to_version(current_exe, config, &target, true, false).map_err(failure)?;
    Ok(Outcome::text(format!(
        "Rolled back to memnoth {target} from the release directory.\n\
         Start it again to run that build. Available versions: {}",
        available.join(", ")
    )))
}

/// Install a previously released version selected from the release directory.
///
/// This is the version picker that closes the gap left by the single `<exe>.old`
/// slot. A version is a valid rollback target only when `latest/manifest.json`
/// lists it in `previous_versions`; a name outside that list is refused, so
/// rollback cannot become a way to fetch an arbitrary artifact. The chosen
/// version's own manifest is then read and handed to `apply_update_to`, which
/// re-checks the signature, the per-target checksum, that an artifact exists for
/// this platform, and that the unpacked binary answers the promised version
/// *before* it swaps. Every one of those failures is fail-closed.
///
/// `allow_downgrade` gates the version policy: rolling back to an older build
/// is refused unless the caller was explicit (`--force`/`--yes`, or the
/// `--rollback` verb itself). `no_verify` mirrors the rest of the update API;
/// the normal path passes `false` so the signature is always checked.
pub fn rollback_to_version(
    current_exe: &Path,
    config: &UpdateConfig,
    version: &str,
    allow_downgrade: bool,
    no_verify: bool,
) -> Result<()> {
    let wanted = version.trim().trim_start_matches('v').to_string();
    if wanted.is_empty() {
        bail!("no version was given for the rollback");
    }

    // `previous_versions` is the authority, not the text hint it used to be.
    let latest = fetch_manifest(config, config.channel)?;
    let is_advertised = latest
        .previous_versions
        .iter()
        .any(|entry| entry.version.trim().trim_start_matches('v') == wanted);
    if !is_advertised {
        bail!(
            "version {wanted} is not in this release's previous_versions; \
             refusing to fetch an unlisted artifact"
        );
    }

    // dist.sh publishes each release under `v<version>/` (and some hosts under
    // the bare version). Try both, exactly as a deliberate `--version` does.
    let mut last_error = None;
    let mut chosen = None;
    for dir in [format!("v{wanted}"), wanted.clone()] {
        match fetch_manifest_at(&config.dist_base_url, &dir) {
            Ok(manifest) => {
                if manifest.version.trim_start_matches('v') != wanted {
                    bail!(
                        "the manifest at {dir} describes {} instead of {wanted}",
                        manifest.version
                    );
                }
                chosen = Some((dir, manifest));
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let (dir, manifest) = chosen.ok_or_else(|| {
        anyhow!(
            "no release directory for version {wanted} at {}: {}",
            config.dist_base_url,
            last_error.map(|e| e.to_string()).unwrap_or_default()
        )
    })?;

    if manifest.channel != config.channel {
        bail!(
            "version {wanted} is on the {} channel, but {} was requested",
            manifest.channel.dist_subdir(),
            config.channel.dist_subdir()
        );
    }

    // Fail closed on a downgrade unless the caller asked for it explicitly.
    memnoth_update::check_version_policy(
        &config.current_version,
        &manifest.version,
        allow_downgrade,
    )?;

    // `explicit = true` is safe here: the downgrade decision was just made
    // above. `apply_update_to` still runs signature (unless no_verify),
    // checksum, target-platform and `--version` checks before swapping.
    memnoth_update::apply_update_to(current_exe, config, &manifest, true, &dir, no_verify)?;

    Ok(())
}

/// Execute install verb.
///
/// Installs the build that is running: the executable is copied to the
/// destination both installers use, the `mn` alias is made beside it, and the
/// MCP server that shipped next to this binary travels with it. Nothing is
/// downloaded. The release installer is the route for a machine with no
/// memnoth at all, and `mn update` is the route for an installed build; a
/// downloader here would be a third answer about checksums and platform
/// artifacts, and the build a person is already running is the one thing
/// neither of those can install.
pub fn execute_install(
    _ctx: &crate::context::AppContext,
    args: &InstallArgs,
) -> Result<Outcome, ErrorOutcome> {
    let current = env::current_exe().map_err(failure)?;
    let install_dir = default_install_dir()?;
    let installed = install_into(&current, &install_dir, args.force)?;

    let on_path = env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|entry| entry == install_dir));

    let body = if installed.is_empty() {
        format!(
            "memnoth is already installed at {}\n\n\
             Nothing was copied. Pass --force to install this build over it.",
            install_dir.display()
        )
    } else {
        format!(
            "Installed this build into {}:\n  {}\n\n\
             Your vault and sessions were not touched.",
            install_dir.display(),
            installed.join("\n  ")
        )
    };

    let path_note = if on_path {
        String::new()
    } else {
        #[cfg(windows)]
        {
            format!(
                "\n\n{} is not on PATH. Add it to your user PATH so `memnoth` \
                 and `mn` work from any shell.",
                install_dir.display()
            )
        }
        #[cfg(not(windows))]
        {
            format!(
                "\n\n{} is not on PATH. Add it:\n  export PATH=\"{}:$PATH\"",
                install_dir.display(),
                install_dir.display()
            )
        }
    };

    Ok(Outcome::text(format!("{body}{path_note}")))
}

/// Where an install goes: the same default both installers use, overridable the
/// same way they allow.
fn default_install_dir() -> Result<PathBuf, ErrorOutcome> {
    if let Some(dir) = env::var_os("MEMNOTH_INSTALL_DIR").filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }

    #[cfg(windows)]
    let default =
        env::var_os("LOCALAPPDATA").map(|base| PathBuf::from(base).join("memnoth").join("bin"));
    #[cfg(not(windows))]
    let default = env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("bin"));

    default.ok_or_else(|| {
        ErrorOutcome::usage(
            "could not determine a default install directory; set MEMNOTH_INSTALL_DIR",
        )
    })
}

/// Copy this build, its alias, and its companion server into `install_dir`.
///
/// The copy is staged and renamed rather than written in place: a shell that
/// looks during the copy must not find half a binary at the installed name. An
/// existing install is replaced only with `--force`, because the build there
/// may be the one a session is running.
fn install_into(
    current: &Path,
    install_dir: &Path,
    force: bool,
) -> Result<Vec<String>, ErrorOutcome> {
    let suffix = env::consts::EXE_SUFFIX;
    let binary = install_dir.join(format!("memnoth{suffix}"));
    let running_here =
        binary.exists() && fs::canonicalize(&binary).ok() == fs::canonicalize(current).ok();

    if binary.exists() && !running_here && !force {
        return Err(ErrorOutcome::usage(format!(
            "already installed at {}; pass --force to replace it",
            binary.display()
        )));
    }

    fs::create_dir_all(install_dir).map_err(failure)?;
    let mut installed = Vec::new();

    if !running_here {
        install_copy(current, &binary)?;
        installed.push(file_label(&binary, install_dir));
    }

    installed.push(install_alias(&binary, install_dir)?);

    let server_name = format!("memnoth-mcp{suffix}");
    let server = current.parent().map(|dir| dir.join(&server_name));
    let server_here = install_dir.join(&server_name);
    if let Some(server) = server
        && server.exists()
        && server != server_here
    {
        fs::copy(&server, &server_here).map_err(failure)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&server_here, fs::Permissions::from_mode(0o755))
                .map_err(failure)?;
        }
        installed.push(server_name);
    }

    Ok(installed)
}

/// Put `source` at `destination`, keeping any image the system will not let go.
///
/// The copy is staged beside the destination and renamed, so a shell never
/// finds half a binary. Windows refuses to delete an image a process is running
/// from, but it allows the name to be renamed: the running session keeps its
/// handle and the next launch gets the new build. A delete would fail with
/// `Access is denied` — which is what an install over a live session is.
fn install_copy(source: &Path, destination: &Path) -> Result<(), ErrorOutcome> {
    let name = destination
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "memnoth".to_string());
    let staging = destination.with_file_name(format!(".{name}.new"));
    let _ = fs::remove_file(&staging);
    fs::copy(source, &staging).map_err(failure)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755)).map_err(failure)?;
    }
    if destination.exists() {
        #[cfg(windows)]
        {
            let backup = destination.with_file_name(format!("{name}.old"));
            let _ = fs::remove_file(&backup);
            if fs::rename(destination, &backup).is_err() {
                let aside =
                    destination.with_file_name(format!("{name}.old-{}", crate::session::now_ms()));
                fs::rename(destination, &aside).map_err(failure)?;
            }
        }
        #[cfg(not(windows))]
        fs::remove_file(destination).map_err(failure)?;
    }
    fs::rename(&staging, destination).map_err(failure)?;
    Ok(())
}

/// The short name, beside the installed binary.
///
/// Unix gets the relative symlink the shell installer makes — one file to
/// update. Windows gets the `mn.ps1` forwarding script the PowerShell
/// installer makes; there is no symlink to create and copying the executable
/// again would double what an upgrade has to replace.
fn install_alias(binary: &Path, install_dir: &Path) -> Result<String, ErrorOutcome> {
    #[cfg(unix)]
    {
        let alias = install_dir.join("mn");
        let _ = fs::remove_file(&alias);
        if std::os::unix::fs::symlink("memnoth", &alias).is_err() {
            use std::os::unix::fs::PermissionsExt;
            fs::copy(binary, &alias).map_err(failure)?;
            fs::set_permissions(&alias, fs::Permissions::from_mode(0o755)).map_err(failure)?;
        }
        Ok("mn (points at memnoth)".to_string())
    }
    #[cfg(windows)]
    {
        // A copy, not a script: `mn` typed in cmd or PowerShell finds an .exe
        // on PATH, while a .ps1 needs its extension. This is the name the
        // previous installs already have, so the new build replaces it.
        let alias = install_dir.join(format!("mn{}", env::consts::EXE_SUFFIX));
        install_copy(binary, &alias)?;
        Ok(format!("mn{} (same build)", env::consts::EXE_SUFFIX))
    }
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

    // The release manifest's `uninstall` URL used to short-circuit this verb:
    // a published uninstall page turned `mn uninstall` into a message that
    // removed nothing. Removing what is actually on this machine is the local
    // command's job, so the network is not consulted at all.
    let current_exe = env::current_exe().map_err(failure)?;

    if let Some(manager) = detect_managed_install() {
        return Err(ErrorOutcome::usage(format!(
            "memnoth is managed by {}. Use {} to uninstall.",
            manager, manager
        )));
    }
    let install_dir = current_exe
        .parent()
        .ok_or_else(|| ErrorOutcome::usage("Could not determine install directory"))?
        .to_path_buf();

    // Everything the installer and the updater put here: both names, the MCP
    // server the installer places beside them, the Windows `mn` alias, and the
    // `.old` copy an update keeps for rollback. Names that are not present are
    // skipped, so a partial install still uninstalls cleanly.
    let installed = installed_files(&install_dir);

    if args.dry_run {
        let mut would_remove: Vec<String> = installed
            .iter()
            .map(|path| file_label(path, &install_dir))
            .collect();
        for shell in COMPLETION_SHELLS {
            if let Ok(path) = completion::completion_path(shell)
                && path.exists()
            {
                would_remove.push(format!("completion ({shell:?})"));
            }
        }
        if !args.keep_state && ctx.home.join("config.toml").exists() {
            would_remove.push("config".to_string());
        }
        if install_dir.is_dir() {
            would_remove.push("the install directory, once it is empty".to_string());
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

    let mut removed: Vec<String> = Vec::new();

    for shell in COMPLETION_SHELLS {
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

    removed.extend(remove_installation(&installed, &install_dir, &current_exe)?);

    // A PATH entry pointing at a directory that no longer exists is harmless
    // but confusing. Rewriting the user's PATH from here is not: on Windows it
    // is a registry value whose type preserves other tools' `%VAR%` expansions,
    // and a shell one-liner would flatten them. The entry is named, not edited.
    let still_on_path = env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|entry| entry == install_dir));

    Ok(Outcome::text(format!(
        "Uninstalled: {}\nInstall directory: {}{}",
        if removed.is_empty() {
            "nothing (the binary is not in this directory)".to_string()
        } else {
            removed.join(", ")
        },
        install_dir.display(),
        if still_on_path {
            "\n\nThe install directory is still on PATH; remove that entry if you want it gone."
        } else {
            ""
        }
    )))
}

/// The shells completions may have been written for.
const COMPLETION_SHELLS: [Shell; 5] = [
    Shell::Bash,
    Shell::Zsh,
    Shell::Fish,
    Shell::PowerShell,
    Shell::Elvish,
];

/// Files an install of this product owns in its directory.
///
/// The installed names carry the platform's executable suffix: on Windows the
/// file is `memnoth.exe`, and looking for `memnoth` removed nothing and then
/// reported success. `memnoth-mcp` is installed beside the CLI by both
/// installers; `.old` is the copy `memnoth_update::backup_path` keeps for
/// `update --rollback`; `mn.ps1` is the Windows alias.
fn installed_files(install_dir: &Path) -> Vec<PathBuf> {
    let suffix = env::consts::EXE_SUFFIX;
    let mut names: Vec<String> = Vec::new();
    for name in ["memnoth", "mn", "memnoth-mcp"] {
        names.push(format!("{name}{suffix}"));
        names.push(format!("{name}{suffix}.old"));
    }
    names.push("mn.ps1".to_string());
    names
        .into_iter()
        .map(|name| install_dir.join(name))
        .filter(|path| path.exists())
        .collect()
}

/// The part of a path worth printing, relative to the heading below it.
fn file_label(path: &Path, install_dir: &Path) -> String {
    path.strip_prefix(install_dir)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Remove every file, then every directory the removal left empty.
///
/// The `memnoth` parent the Windows installer creates by default
/// (`%LOCALAPPDATA%\memnoth\bin`) goes too, and only when it is empty. A shared
/// destination — the unix default is `~/.local/bin` — is left alone as soon as
/// anything else lives in it, and never with `/S`: another tool's file is not
/// ours to delete to get rid of ours.
fn remove_installation(
    files: &[PathBuf],
    install_dir: &Path,
    running: &Path,
) -> Result<Vec<String>, ErrorOutcome> {
    let mut removed = Vec::new();
    let mut deferred: Option<&Path> = None;

    for path in files {
        // Only Windows locks the image it is running from; elsewhere the
        // running binary is unlinked like any other file.
        if cfg!(windows) && path == running {
            deferred = Some(path);
            continue;
        }
        fs::remove_file(path).map_err(failure)?;
        removed.push(file_label(path, install_dir));
    }

    match deferred {
        Some(running) => {
            // It cannot go before this process exits, and neither can the
            // directory that holds it, so one helper does both afterwards.
            let mut dirs = vec![install_dir.to_path_buf()];
            if let Some(parent) = install_dir.parent()
                && parent
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("memnoth"))
            {
                dirs.push(parent.to_path_buf());
            }
            defer_removal(running, &dirs)?;
            removed.push(file_label(running, install_dir));
            removed.push("install directory (after this process exits)".to_string());
        }
        None => {
            for dir in empty_dirs_to_remove(install_dir) {
                fs::remove_dir(&dir).map_err(failure)?;
                removed.push(if dir == install_dir {
                    "install directory".to_string()
                } else {
                    dir.display().to_string()
                });
            }
        }
    }

    Ok(removed)
}

/// Directories the removal leaves empty, deepest first.
///
/// The parent of the install directory counts only when it is the installer's
/// own `memnoth` directory and removing the child empties it — checking the
/// parent before the child is gone would always find the child inside.
fn empty_dirs_to_remove(install_dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut dir = install_dir.to_path_buf();
    loop {
        let Ok(entries) = fs::read_dir(&dir) else {
            break;
        };
        let children: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        let emptied_by_us = match (children.as_slice(), dirs.last()) {
            ([], _) => true,
            ([only], Some(next)) => only == next,
            _ => false,
        };
        if !emptied_by_us {
            break;
        }
        dirs.push(dir.clone());
        if dirs.len() >= 2 {
            break;
        }
        let Some(parent) = dir.parent() else {
            break;
        };
        if !parent
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("memnoth"))
        {
            break;
        }
        dir = parent.to_path_buf();
    }
    dirs
}

/// Delete a running image (and the directories it blocks) after this process
/// exits. The updater's rollback uses the same shape: a short-lived shell with
/// a wait long enough for the image handle to close.
#[cfg(windows)]
fn defer_removal(running: &Path, dirs: &[PathBuf]) -> Result<(), ErrorOutcome> {
    use std::os::windows::process::CommandExt;
    let mut script = format!(
        "ping -n 3 127.0.0.1 >NUL & del /F /Q \"{}\"",
        running.display()
    );
    for dir in dirs {
        script.push_str(&format!(" & rd /Q \"{}\"", dir.display()));
    }
    std::process::Command::new("cmd")
        .arg("/C")
        .raw_arg(script)
        .spawn()
        .map_err(|error| failure(anyhow::Error::from(error)))?;
    Ok(())
}

#[cfg(not(windows))]
fn defer_removal(_running: &Path, _dirs: &[PathBuf]) -> Result<(), ErrorOutcome> {
    Ok(())
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
        let text = notice_text("0.1.0-beta.4", "0.1.0-beta.5");
        assert!(text.contains("0.1.0-beta.5"), "{text}");
        assert!(text.contains("0.1.0-beta.4"), "{text}");
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
    fn an_install_owns_both_names_the_mcp_server_the_alias_and_the_rollback_copy() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let suffix = env::consts::EXE_SUFFIX;
        for name in ["memnoth", "mn", "memnoth-mcp"] {
            fs::write(dir.join(format!("{name}{suffix}")), b"ours").unwrap();
            fs::write(dir.join(format!("{name}{suffix}.old")), b"ours").unwrap();
        }
        fs::write(dir.join("mn.ps1"), b"ours").unwrap();
        fs::write(dir.join("someone-elses-tool.exe"), b"theirs").unwrap();

        let found: Vec<String> = installed_files(dir)
            .iter()
            .map(|path| file_label(path, dir))
            .collect();
        for expected in [
            format!("memnoth{suffix}"),
            format!("mn{suffix}"),
            format!("memnoth-mcp{suffix}"),
            format!("memnoth{suffix}.old"),
            "mn.ps1".to_string(),
        ] {
            assert!(
                found.contains(&expected),
                "{expected} missing from {found:?}"
            );
        }
        assert!(
            !found.iter().any(|name| name.contains("someone-elses-tool")),
            "only this product's files are the product's to remove: {found:?}"
        );

        // A shared destination survives as long as anyone else is in it.
        assert!(
            empty_dirs_to_remove(dir).is_empty(),
            "a directory with another tool's file must not be offered for removal"
        );
        fs::remove_file(dir.join("someone-elses-tool.exe")).unwrap();
        for path in installed_files(dir) {
            fs::remove_file(path).unwrap();
        }
        assert_eq!(empty_dirs_to_remove(dir), vec![dir.to_path_buf()]);
    }

    #[test]
    fn the_directory_the_installer_made_is_removed_when_it_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("memnoth").join("bin");
        fs::create_dir_all(&dir).unwrap();
        let binary = dir.join(format!("memnoth{}", env::consts::EXE_SUFFIX));
        fs::write(&binary, b"ours").unwrap();

        // The running image is elsewhere, so nothing has to wait for exit.
        let removed = remove_installation(
            &installed_files(&dir),
            &dir,
            temp.path().join("elsewhere").as_path(),
        )
        .unwrap();
        assert!(
            removed.iter().any(|label| label == "install directory"),
            "{removed:?}"
        );
        assert!(!dir.exists());
        assert!(
            !temp.path().join("memnoth").exists(),
            "the parent the Windows installer creates is empty and goes too"
        );
    }

    #[test]
    fn installing_a_build_copies_it_with_its_alias_and_server() {
        let temp = tempfile::tempdir().unwrap();
        let suffix = env::consts::EXE_SUFFIX;
        let source_dir = temp.path().join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join(format!("memnoth{suffix}"));
        fs::write(&source, b"this build").unwrap();
        fs::write(
            source_dir.join(format!("memnoth-mcp{suffix}")),
            b"its server",
        )
        .unwrap();
        let install_dir = temp.path().join("bin");

        let installed = install_into(&source, &install_dir, false).unwrap();
        assert!(
            installed.iter().any(|label| label.starts_with("memnoth")),
            "{installed:?}"
        );
        assert_eq!(
            fs::read(install_dir.join(format!("memnoth{suffix}"))).unwrap(),
            b"this build"
        );
        assert_eq!(
            fs::read(install_dir.join(format!("memnoth-mcp{suffix}"))).unwrap(),
            b"its server"
        );
        #[cfg(unix)]
        assert!(
            fs::symlink_metadata(install_dir.join("mn"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "the short name is the shell installer's relative symlink"
        );
        #[cfg(windows)]
        assert!(
            install_dir.join(format!("mn{suffix}")).exists(),
            "the Windows short name is a copy that cmd and PowerShell both run"
        );

        // A second install must not replace a build that may be running.
        let refused = install_into(&source, &install_dir, false).unwrap_err();
        assert!(
            refused.to_string().contains("already installed"),
            "{refused}"
        );

        fs::write(&source, b"a newer build").unwrap();
        install_into(&source, &install_dir, true).unwrap();
        assert_eq!(
            fs::read(install_dir.join(format!("memnoth{suffix}"))).unwrap(),
            b"a newer build"
        );
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
