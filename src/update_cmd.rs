//! Update, install, and uninstall commands for memnoth.

use anyhow::{Result, anyhow};
use memnoth_update::{Channel, UpdateConfig, check_for_update, detect_managed_install};
use std::env;
use std::fs;
use std::path::Path;

use crate::cli::{InstallArgs, UninstallArgs, UpdateArgs};
use crate::output::{ErrorOutcome, ExitCode, Outcome};

/// Map an engine or I/O failure into the CLI's error envelope.
///
/// The update engine returns its own error type and the download path returns
/// `std::io::Error`; both reach the user as the message they carry rather than
/// collapsing into a generic failure.
fn failure(error: impl std::fmt::Display) -> ErrorOutcome {
    ErrorOutcome::new(ExitCode::Unexpected, error.to_string())
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

    let current_version = env!("CARGO_PKG_VERSION");
    let config = UpdateConfig {
        channel,
        current_version: current_version.to_string(),
        // A key in the environment lets a tester verify a local build without
        // rebuilding the binary; otherwise the compiled-in key is the one that
        // counts, because a key read from the release host proves nothing.
        minisign_pubkey: std::env::var("MEMNOTH_MINISIGN_PUBKEY")
            .ok()
            .filter(|key| !key.trim().is_empty())
            .or_else(memnoth_update::compiled_in_pubkey),
        ..Default::default()
    };

    if args.check {
        return check_update(&config);
    }

    if let Some(version) = args.version.as_deref() {
        return install_specific_version(&config, version, args.no_verify);
    }

    // Full update: check and apply
    match check_for_update(&config) {
        Ok(result) => {
            if result.update_available {
                apply_update_with_config(&config, &result.manifest, args.no_verify, false)
            } else {
                Ok(Outcome::text(format!(
                    "memnoth {} is up to date (channel: {})",
                    current_version,
                    channel.dist_subdir()
                )))
            }
        }
        Err(e) => Err(failure(e)),
    }
}

/// Execute install verb.
pub fn execute_install(
    _ctx: &crate::context::AppContext,
    args: &InstallArgs,
) -> Result<Outcome, ErrorOutcome> {
    // On Windows, we can't easily install from here; show the install script URL
    // On Unix, the install.sh script handles this
    #[cfg(windows)]
    {
        if !args.force && is_installed() {
            return Ok(Outcome::text(
                "memnoth is already installed. Use --force to reinstall.\n\
                 Run the PowerShell installer:\n\
                 irm https://memnoth.astrivya.ai/install.ps1 | iex",
            ));
        }
        Ok(Outcome::text(
            "To install memnoth on Windows, run the PowerShell installer:\n\
             irm https://memnoth.astrivya.ai/install.ps1 | iex\n\
             \n\
             Or download from: https://github.com/astrivya/memnoth/releases",
        ))
    }
    #[cfg(not(windows))]
    {
        if !args.force && is_installed() {
            return Ok(Outcome::text(
                "memnoth is already installed. Use --force to reinstall.\n\
                 Run the installer:\n\
                 curl -fsSL https://memnoth.astrivya.ai/install.sh | sh",
            ));
        }
        Ok(Outcome::text(
            "To install memnoth, run the installer:\n\
             curl -fsSL https://memnoth.astrivya.ai/install.sh | sh\n\
             \n\
             Or download from: https://github.com/astrivya/memnoth/releases",
        ))
    }
}

/// Execute uninstall verb.
pub fn execute_uninstall(
    _ctx: &crate::context::AppContext,
    args: &UninstallArgs,
) -> Result<Outcome, ErrorOutcome> {
    let current_exe = env::current_exe().map_err(failure)?;

    // Check for managed install
    if let Some(manager) = detect_managed_install() {
        return Err(ErrorOutcome::usage(format!(
            "memnoth is managed by {}. Use {} to uninstall.",
            manager, manager
        )));
    }

    let exe_path = current_exe.clone();
    let install_dir = exe_path
        .parent()
        .ok_or_else(|| ErrorOutcome::usage("Could not determine install directory"))?;

    // Remove the binary
    let memnoth_bin = install_dir.join("memnoth");
    let mn_bin = install_dir.join("mn");

    let mut removed = Vec::new();
    if memnoth_bin.exists() {
        fs::remove_file(&memnoth_bin).map_err(failure)?;
        removed.push("memnoth");
    }
    if mn_bin.exists() {
        fs::remove_file(&mn_bin).map_err(failure)?;
        removed.push("mn");
    }

    // Remove state unless --keep-state
    if !args.keep_state {
        let state_dir = dirs::data_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))
            .map(|d| d.join("memnoth"));

        if let Some(state_dir) = state_dir
            && state_dir.exists()
        {
            fs::remove_dir_all(&state_dir).map_err(failure)?;
            removed.push("state directory");
        }

        let config_dir = dirs::config_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .map(|d| d.join("memnoth"));

        if let Some(config_dir) = config_dir
            && config_dir.exists()
        {
            fs::remove_dir_all(&config_dir).map_err(failure)?;
            removed.push("config directory");
        }
    }

    Ok(Outcome::text(format!(
        "Uninstalled: {}\nInstall directory: {}",
        removed.join(", "),
        install_dir.display()
    )))
}

fn check_update(config: &UpdateConfig) -> Result<Outcome, ErrorOutcome> {
    match check_for_update(config) {
        Ok(result) => {
            if result.update_available {
                let manifest = &result.manifest;
                Ok(Outcome::text(format!(
                    "Update available: {} → {} (channel: {})\nPublished: {}\n{}",
                    config.current_version,
                    manifest.version,
                    config.channel.dist_subdir(),
                    manifest.published_at.format("%Y-%m-%d %H:%M UTC"),
                    manifest.notes.as_deref().unwrap_or("No release notes")
                )))
            } else {
                Ok(Outcome::text(format!(
                    "memnoth {} is up to date (channel: {})",
                    config.current_version,
                    config.channel.dist_subdir()
                )))
            }
        }
        Err(e) => Err(failure(e)),
    }
}

fn install_specific_version(
    config: &UpdateConfig,
    version: &str,
    no_verify: bool,
) -> Result<Outcome, ErrorOutcome> {
    // Fetch manifest for specific version
    let dist_url = format!(
        "{}/dist/{}/{}",
        config.dist_base_url,
        config.channel.dist_subdir(),
        version
    );
    let manifest_url = format!("{}/manifest.json", dist_url);

    let manifest: memnoth_update::Manifest = reqwest::blocking::get(&manifest_url)
        .map_err(|e| failure(anyhow!("Failed to fetch manifest: {}", e)))?
        .json()
        .map_err(|e| failure(anyhow!("Failed to parse manifest: {}", e)))?;

    if manifest.version != version {
        return Err(ErrorOutcome::usage(format!(
            "Version mismatch: manifest has {}, requested {}",
            manifest.version, version
        )));
    }

    // Naming the version is what makes a downgrade deliberate. The config keeps
    // the version actually running: rewriting `current_version` to the target
    // made every downgrade look like an upgrade and skipped the policy check
    // below.
    println!(
        "Installing {} (running {})",
        version, config.current_version
    );
    apply_update_with_config(config, &manifest, no_verify, true)
}

fn apply_update_with_config(
    config: &UpdateConfig,
    manifest: &memnoth_update::Manifest,
    no_verify: bool,
    explicit_version: bool,
) -> Result<Outcome, ErrorOutcome> {
    // Check for managed install
    if let Some(manager) = detect_managed_install()
        && !no_verify
    {
        return Err(ErrorOutcome::usage(format!(
            "memnoth is managed by {}. Use {} to update, or pass --no-verify to override.",
            manager, manager
        )));
    }

    // A manifest that points at an older version is refused unless the version
    // was named on the command line. Closing a security hole by downgrading is
    // a decision, not something an update does on its own.
    memnoth_update::check_version_policy(
        &config.current_version,
        &manifest.version,
        explicit_version,
    )
    .map_err(failure)?;

    let current_exe = env::current_exe().map_err(failure)?;

    // Find matching target for current platform
    let target_triple = format!("{}-{}", env::consts::ARCH, env::consts::OS);
    let target = manifest
        .targets
        .iter()
        .find(|t| t.triple == target_triple)
        .ok_or_else(|| {
            ErrorOutcome::usage(format!("No release artifact for target: {}", target_triple))
        })?;

    // Download to temp file
    let temp_dir = env::temp_dir();
    let temp_exe = temp_dir.join(format!("memnoth-update-{}.tmp", std::process::id()));
    let download_url = format!(
        "{}/dist/{}/{}",
        config.dist_base_url,
        config.channel.dist_subdir(),
        target.filename
    );

    // Signatures first: the checksum below comes from the manifest, and a
    // manifest is not a signature. This is also the order in which a refusal
    // costs nothing — an unsigned release is rejected before the binary is
    // fetched, not after.
    if !no_verify {
        println!("Verifying signature...");
        let sums_url = format!(
            "{}/dist/{}/SHA256SUMS",
            config.dist_base_url,
            config.channel.dist_subdir()
        );
        let sums_path = temp_dir.join(format!("memnoth-update-{}.SHA256SUMS", std::process::id()));
        let verified = download_file_with_progress(&sums_url, &sums_path)
            .and_then(|()| memnoth_update::verify_signatures(&sums_path, manifest, config));
        let _ = fs::remove_file(&sums_path);
        match verified {
            Ok(()) => println!("Signature OK"),
            Err(error) => {
                return Err(failure(anyhow!(
                    "{error}\n\nNothing was changed. If this machine is meant to install an \
                     unsigned build, say so with --no-verify."
                )));
            }
        }
    } else {
        println!("WARNING: Skipping signature and checksum verification (--no-verify)");
    }

    // Download with progress
    println!("Downloading {}...", download_url);
    download_file_with_progress(&download_url, &temp_exe).map_err(failure)?;

    if !no_verify {
        println!("Verifying checksum...");
        if let Err(error) = memnoth_update::verify_checksum(&temp_exe, &manifest.checksums.sha256) {
            let _ = fs::remove_file(&temp_exe);
            return Err(failure(error));
        }
        println!("Checksum OK");
    }

    // Prove the artifact runs and is the version promised, before it replaces
    // the working binary. After the swap on Windows there is no way back: a
    // running executable cannot be renamed.
    println!("Checking the downloaded binary...");
    if let Err(error) = memnoth_update::verify_new_binary(&temp_exe, &manifest.version) {
        let _ = fs::remove_file(&temp_exe);
        return Err(failure(anyhow!("{error}\n\nNothing was changed.")));
    }

    // Atomic swap
    println!("Applying update...");
    memnoth_update::atomic_swap(&current_exe, &temp_exe)
        .map_err(|e| failure(anyhow!("Update failed: {}", e)))?;

    Ok(Outcome::text(format!(
        "Successfully updated to {}\nRun `memnoth --version` to verify.",
        manifest.version
    )))
}

fn download_file_with_progress(url: &str, dest: &Path) -> Result<()> {
    let client = reqwest::blocking::Client::new();
    let mut response = client.get(url).send()?;

    let total_size = response.content_length().unwrap_or(0);
    let mut downloaded = 0u64;
    let mut file = fs::File::create(dest)?;

    let mut buffer = [0u8; 8192];
    use std::io::{Read, Write};

    while let Ok(bytes_read) = response.read(&mut buffer) {
        if bytes_read == 0 {
            break;
        }
        file.write_all(&buffer[..bytes_read])?;
        downloaded += bytes_read as u64;

        if let Some(percent) = (downloaded * 100).checked_div(total_size) {
            print!(
                "\rProgress: {}% ({}/{} bytes)",
                percent, downloaded, total_size
            );
            std::io::stdout().flush()?;
        }
    }

    if total_size > 0 {
        println!();
    }

    Ok(())
}

fn is_installed() -> bool {
    // Check if memnoth is in PATH or common install locations
    let current_exe = env::current_exe().ok();
    current_exe.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_parse() {
        assert!(Channel::parse("stable").is_ok());
        assert!(Channel::parse("beta").is_ok());
        assert!(Channel::parse("nightly").is_ok());
        assert!(Channel::parse("unknown").is_err());
    }
}
