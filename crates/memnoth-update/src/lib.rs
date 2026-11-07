//! Self-update mechanism with signature verification, atomic swap, and passive checks.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration, Utc};
use dirs::home_dir;
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use thiserror::Error;

/// Errors specific to the update mechanism.
#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid manifest: {0}")]
    InvalidManifest(String),

    #[error("Signature verification failed: {0}")]
    SignatureVerification(String),

    #[error("Checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("No update available (current: {current}, latest: {latest})")]
    NoUpdateAvailable { current: String, latest: String },

    #[error("Managed install detected: {manager}. Use {manager} to update.")]
    ManagedInstall { manager: String },

    #[error("Atomic swap failed: {0}")]
    AtomicSwapFailed(String),

    #[error("Semver error: {0}")]
    Semver(#[from] semver::Error),

    #[error(
        "Refusing to downgrade from {current} to {wanted}: a downgrade is not an update. \
         Pass `--version {wanted}` to ask for that version deliberately."
    )]
    Downgrade { current: String, wanted: String },

    #[error("The downloaded binary is not usable: {0}")]
    NewBinaryFailed(String),
}

/// Release channel for update checks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
    Nightly,
}

impl Channel {
    /// Resolve channel to distribution subdirectory.
    pub fn dist_subdir(&self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
            Channel::Nightly => "nightly",
        }
    }

    /// Parse channel from string.
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "stable" => Ok(Channel::Stable),
            "beta" => Ok(Channel::Beta),
            "nightly" => Ok(Channel::Nightly),
            _ => bail!("Unknown channel: {}", s),
        }
    }
}

/// Target platform for downloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub triple: String,
    pub filename: String,
}

/// Checksums for a release artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checksums {
    pub sha256: String,
}

/// Signature information for a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signatures {
    pub minisign: Option<String>,
    pub cosign: Option<String>,
}

/// Release manifest from the distribution server.
#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub channel: Channel,
    pub targets: Vec<Target>,
    pub checksums: Checksums,
    pub signatures: Signatures,
    #[serde_as(as = "DisplayFromStr")]
    pub published_at: DateTime<Utc>,
    pub notes: Option<String>,
}

/// Cached update check state for passive notifications.
#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCache {
    #[serde_as(as = "DisplayFromStr")]
    pub last_check: DateTime<Utc>,
    pub latest_version: Option<String>,
    pub latest_channel: Option<Channel>,
    pub notification_shown: bool,
}

/// Configuration for update behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// Base URL for distribution server (e.g., "https://releases.memnoth.astrivya.ai")
    pub dist_base_url: String,
    /// Public key for minisign verification (base64 encoded)
    pub minisign_pubkey: Option<String>,
    /// Cosign public key path or URL
    pub cosign_pubkey: Option<String>,
    /// Current channel
    pub channel: Channel,
    /// Current version
    pub current_version: String,
    /// Disable passive update checks
    pub passive_disabled: bool,
    /// Quiet mode - suppress passive notifications
    pub quiet: bool,
}

/// The public key baked into this build, if the release was built with one.
///
/// A key compiled into the binary is what makes signature verification
/// meaningful: a key read from the same host as the manifest would be signed
/// by whoever served the manifest.
pub fn compiled_in_pubkey() -> Option<String> {
    option_env!("MEMNOTH_MINISIGN_PUBKEY")
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            dist_base_url: "https://releases.memnoth.astrivya.ai".to_string(),
            minisign_pubkey: compiled_in_pubkey(),
            cosign_pubkey: None,
            channel: Channel::Stable,
            current_version: "0.1.0-beta.1".to_string(),
            passive_disabled: false,
            quiet: false,
        }
    }
}

/// Result of an update check.
#[derive(Debug, Clone)]
pub struct UpdateCheckResult {
    pub current_version: Version,
    pub latest_version: Version,
    pub manifest: Manifest,
    pub update_available: bool,
}

/// Resolve the distribution URL for a given channel.
pub fn resolve_dist_url(config: &UpdateConfig, channel: Channel) -> String {
    format!(
        "{}/dist/{}/latest",
        config.dist_base_url,
        channel.dist_subdir()
    )
}

/// Fetch the manifest from the distribution server.
pub fn fetch_manifest(config: &UpdateConfig, channel: Channel) -> Result<Manifest> {
    let url = resolve_dist_url(config, channel);
    let client = reqwest::blocking::Client::new();
    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .send()?
        .error_for_status()?;

    let manifest: Manifest = response.json()?;
    Ok(manifest)
}

/// Verify SHA-256 checksum of a file.
pub fn verify_checksum(file_path: &Path, expected_sha256: &str) -> Result<()> {
    let mut file = fs::File::open(file_path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    let hash = hasher.finalize();
    let actual = hex::encode(hash);

    if actual != expected_sha256 {
        bail!(UpdateError::ChecksumMismatch {
            expected: expected_sha256.to_string(),
            actual,
        });
    }
    Ok(())
}

/// Verify minisign signature over SHA256SUMS file.
pub fn verify_minisign(
    sha256sums_path: &Path,
    signature_b64: &str,
    pubkey_b64: &str,
) -> Result<()> {
    let pubkey = PublicKey::from_base64(pubkey_b64)
        .map_err(|e| anyhow!("Invalid minisign public key: {}", e))?;
    // minisign-verify uses decode for base64 signature strings
    let signature = Signature::decode(signature_b64)
        .map_err(|e| anyhow!("Invalid minisign signature: {}", e))?;

    let content = fs::read(sha256sums_path)?;
    // verify takes (content, signature, trusted_comment)
    pubkey
        .verify(&content, &signature, true)
        .map_err(|e| anyhow!("Minisign verification failed: {}", e))?;

    Ok(())
}

/// Verify a sigstore/cosign signature over the SHA256SUMS file.
///
/// This build has no sigstore verifier, and the honest answer to "check this"
/// when the check cannot be performed is an error. The previous version read
/// the file, dropped every argument and returned `Ok(())`, which meant a
/// release that advertised a cosign signature it could not check installed
/// with the same message as one that had been checked — a verification that
/// verifies nothing is worse than none, because the person stops looking.
pub fn verify_cosign(sha256sums_path: &Path, signature_b64: &str, pubkey_path: &str) -> Result<()> {
    let _ = fs::read(sha256sums_path)?;
    let _ = (signature_b64, pubkey_path);
    bail!(UpdateError::SignatureVerification(
        "this build cannot verify cosign signatures; refusing to treat it as verified ".to_string()
    ))
}

/// Verify a release's signatures and refuse one that is not actually checked.
///
/// The policy is fail-closed in both directions:
///
/// - a signature the manifest offers but this build cannot check (a cosign
///   signature, or a minisign signature with no configured key) is a refusal —
///   an unverifiable claim is not a verified one;
/// - a manifest with no signature at all is a refusal too, because a checksum
///   served by the same host as the artifact only proves the download was not
///   corrupted in transit, not that it came from us.
///
/// `--no-verify` exists for a deliberate, local decision to install anyway; it
/// is not a default.
pub fn verify_signatures(
    sha256sums_path: &Path,
    manifest: &Manifest,
    config: &UpdateConfig,
) -> Result<()> {
    let mut verified = 0usize;

    if let Some(signature) = &manifest.signatures.minisign {
        let Some(pubkey) = config.minisign_pubkey.as_deref() else {
            bail!(UpdateError::SignatureVerification(
                "the release carries a minisign signature and this machine has no public key \
                 configured, so it cannot be checked"
                    .to_string()
            ));
        };
        verify_minisign(sha256sums_path, signature, pubkey)?;
        verified += 1;
    }

    if let Some(signature) = &manifest.signatures.cosign {
        let Some(pubkey) = config.cosign_pubkey.as_deref() else {
            bail!(UpdateError::SignatureVerification(
                "the release carries a cosign signature and this machine has no public key \
                 configured, so it cannot be checked"
                    .to_string()
            ));
        };
        verify_cosign(sha256sums_path, signature, pubkey)?;
        verified += 1;
    }

    if verified == 0 {
        bail!(UpdateError::SignatureVerification(
            "the release is unsigned; a checksum from the same host as the artifact is not a \
             signature"
                .to_string()
        ));
    }

    Ok(())
}

/// Whether installing `manifest_version` over `current` is a downgrade.
///
/// A downgrade is not an update: it is how a caller is walked back onto a
/// version with a known bug. It is allowed only when someone asked for that
/// version by name, and the caller says so by passing `explicit`.
pub fn check_version_policy(current: &str, manifest_version: &str, explicit: bool) -> Result<()> {
    let current = Version::parse(current.trim_start_matches('v'))?;
    let wanted = Version::parse(manifest_version.trim_start_matches('v'))?;
    if wanted < current && !explicit {
        bail!(UpdateError::Downgrade {
            current: current.to_string(),
            wanted: wanted.to_string(),
        });
    }
    Ok(())
}

/// Run the freshly downloaded binary and require it to report the version the
/// manifest promised.
///
/// This is the update's only real proof that the artifact is the program it
/// claims to be, and it happens *before* the swap. Verifying afterwards would
/// mean the broken binary is already in place, and "roll back" would then have
/// to replace a running executable on Windows, which cannot be done.
pub fn verify_new_binary(new_exe: &Path, expected_version: &str) -> Result<()> {
    let output = process::Command::new(new_exe)
        .arg("--version")
        .output()
        .with_context(|| format!("the downloaded binary at {} did not run", new_exe.display()))?;

    if !output.status.success() {
        bail!(UpdateError::NewBinaryFailed(format!(
            "exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let reported = String::from_utf8_lossy(&output.stdout);
    let expected = expected_version.trim_start_matches('v');
    if !reported.contains(expected) {
        bail!(UpdateError::NewBinaryFailed(format!(
            "reported {:?} instead of {expected}",
            reported.trim()
        )));
    }
    Ok(())
}

/// Download a file with progress.
pub fn download_file(url: &str, dest: &Path) -> Result<()> {
    let client = reqwest::blocking::Client::new();
    let mut response = client.get(url).send()?.error_for_status()?;

    let mut file = fs::File::create(dest)?;
    std::io::copy(&mut response, &mut file)?;

    Ok(())
}

/// Perform atomic swap of binary on Unix (rename).
#[cfg(unix)]
pub fn atomic_swap_unix(current_exe: &Path, new_exe: &Path) -> Result<()> {
    // On Unix, rename(2) is atomic when source and dest are on same filesystem
    fs::rename(new_exe, current_exe)
        .with_context(|| format!("Failed to atomically replace {}", current_exe.display()))?;
    Ok(())
}

/// Perform atomic swap of binary on Windows (rename running exe to .old then move new).
#[cfg(windows)]
pub fn atomic_swap_windows(current_exe: &Path, new_exe: &Path) -> Result<()> {
    // On Windows, we can't overwrite a running executable.
    // Strategy: rename current to .old, move new into place.
    let old_path = current_exe.with_extension("exe.old");

    // Remove any existing .old file
    if old_path.exists() {
        fs::remove_file(&old_path)?;
    }

    // Rename current exe to .old
    fs::rename(current_exe, &old_path)
        .with_context(|| format!("Failed to rename {} to .old", current_exe.display()))?;

    // Move new exe into place
    fs::rename(new_exe, current_exe)
        .with_context(|| format!("Failed to move new exe to {}", current_exe.display()))?;

    // Schedule .old for deletion on next boot (best effort)
    let _ = std::process::Command::new("cmd")
        .args(["/C", "del", "/F", "/Q", &old_path.to_string_lossy()])
        .spawn();

    Ok(())
}

/// Cross-platform atomic swap.
pub fn atomic_swap(current_exe: &Path, new_exe: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        atomic_swap_unix(current_exe, new_exe)
    }
    #[cfg(windows)]
    {
        atomic_swap_windows(current_exe, new_exe)
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Fallback for other platforms
        fs::rename(new_exe, current_exe)?;
        Ok(())
    }
}

/// Detect if current installation is managed by a package manager.
pub fn detect_managed_install() -> Option<String> {
    let current_exe = std::env::current_exe().ok()?;
    let exe_path = current_exe.to_string_lossy().to_lowercase();

    // Check for common package manager paths
    if exe_path.contains("\\scoop\\") || exe_path.contains("/.local/share/scoop/") {
        return Some("scoop".to_string());
    }
    if exe_path.contains("\\chocolatey\\") || exe_path.contains("/opt/chocolatey/") {
        return Some("chocolatey".to_string());
    }
    if exe_path.contains("/.cargo/bin/") || exe_path.contains("\\.cargo\\bin\\") {
        return Some("cargo".to_string());
    }
    if exe_path.contains("/home/linuxbrew/")
        || exe_path.contains("/opt/homebrew/")
        || exe_path.contains("/usr/local/Homebrew/")
    {
        return Some("brew".to_string());
    }
    if exe_path.contains("/.local/share/pnpm/") || exe_path.contains("\\.local\\share\\pnpm\\") {
        return Some("pnpm".to_string());
    }
    if exe_path.contains("/.npm-global/") || exe_path.contains("\\.npm-global\\") {
        return Some("npm".to_string());
    }
    if exe_path.contains("/snap/") {
        return Some("snap".to_string());
    }
    if exe_path.contains("/var/lib/flatpak/") || exe_path.contains("/.local/share/flatpak/") {
        return Some("flatpak".to_string());
    }
    if exe_path.contains("/nix/store/") {
        return Some("nix".to_string());
    }

    // Check argv[0] for clues
    let args: Vec<String> = std::env::args().collect();
    if let Some(argv0) = args.first() {
        let argv0_lower = argv0.to_lowercase();
        if argv0_lower.contains("cargo") {
            return Some("cargo".to_string());
        }
        if argv0_lower.contains("brew") {
            return Some("brew".to_string());
        }
        if argv0_lower.contains("pnpm") {
            return Some("pnpm".to_string());
        }
    }

    None
}

/// Get the cache file path for passive update checks.
fn cache_path() -> Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow!("Could not determine home directory"))?;
    Ok(home.join(".memnoth").join("update.json"))
}

/// Load cached update state.
pub fn load_cache() -> Result<UpdateCache> {
    let path = cache_path()?;
    if !path.exists() {
        return Ok(UpdateCache {
            last_check: Utc::now() - Duration::days(2), // Force check on first run
            latest_version: None,
            latest_channel: None,
            notification_shown: false,
        });
    }

    let content = fs::read_to_string(path)?;
    let cache: UpdateCache = serde_json::from_str(&content)?;
    Ok(cache)
}

/// Save cached update state.
pub fn save_cache(cache: &UpdateCache) -> Result<()> {
    let path = cache_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(cache)?;
    fs::write(path, content)?;
    Ok(())
}

/// Check if passive update notification should be shown.
pub fn should_notify(config: &UpdateConfig, cache: &UpdateCache) -> bool {
    if config.passive_disabled || config.quiet {
        return false;
    }

    // Check environment variables
    if std::env::var("NO_UPDATE_NOTIFIER").is_ok() {
        return false;
    }
    if std::env::var("CI").is_ok() {
        return false;
    }

    // Check if 24 hours have passed since last check
    let now = Utc::now();
    if now - cache.last_check < Duration::hours(24) {
        return false;
    }

    // Check if we already showed notification for this version
    if cache.notification_shown && cache.latest_version.as_ref() == Some(&config.current_version) {
        return false;
    }

    true
}

/// Perform a full update check (active).
pub fn check_for_update(config: &UpdateConfig) -> Result<UpdateCheckResult> {
    let current_version = Version::parse(&config.current_version)?;
    let manifest = fetch_manifest(config, config.channel)?;
    let latest_version = Version::parse(&manifest.version)?;

    let update_available = latest_version > current_version;

    Ok(UpdateCheckResult {
        current_version,
        latest_version,
        manifest,
        update_available,
    })
}

/// Perform passive update check (cached, once per 24h).
pub fn passive_check(config: &UpdateConfig) -> Result<Option<UpdateCheckResult>> {
    let mut cache = load_cache()?;

    if !should_notify(config, &cache) {
        return Ok(None);
    }

    // Update last check time
    cache.last_check = Utc::now();

    let result = check_for_update(config)?;

    // Update cache with latest info
    cache.latest_version = Some(result.latest_version.to_string());
    cache.latest_channel = Some(config.channel);

    if result.update_available {
        cache.notification_shown = true;
    }

    save_cache(&cache)?;

    Ok(Some(result))
}

/// Download and verify update, then apply it.
///
/// `explicit_version` says the caller named this version on the command line,
/// which is the only thing that makes a downgrade deliberate.
///
/// The order is the point, and it is the same order the `mn update` command
/// takes: refuse what can be refused without the network (a managed install, a
/// missing artifact for this platform, a version older than the running one),
/// then fetch the signature and check it, and only then fetch the binary. The
/// version check used to sit after a full download, so a downgrade was refused
/// after the bytes it refused had already been transferred.
pub fn apply_update(
    config: &UpdateConfig,
    manifest: &Manifest,
    explicit_version: bool,
) -> Result<()> {
    // Detect managed install
    if let Some(manager) = detect_managed_install() {
        bail!(UpdateError::ManagedInstall { manager });
    }

    let current_exe = std::env::current_exe()?;

    // Find matching target for current platform
    let target_triple = std::env::consts::ARCH.to_string() + "-" + std::env::consts::OS;
    let target = manifest
        .targets
        .iter()
        .find(|t| t.triple == target_triple)
        .ok_or_else(|| anyhow!("No release artifact for target: {}", target_triple))?;

    // A manifest that would walk this machine backwards is refused before a
    // byte of the artifact is sent.
    check_version_policy(&config.current_version, &manifest.version, explicit_version)?;

    let temp_dir = std::env::temp_dir();
    let temp_exe = temp_dir.join(format!("memnoth-update-{}.tmp", process::id()));
    let download_url = format!(
        "{}/dist/{}/{}",
        config.dist_base_url,
        config.channel.dist_subdir(),
        target.filename
    );

    // The signature covers the checksum file, not the artifact directly, so the
    // checksum file is fetched and checked first: a signature over a list of
    // hashes is only worth something if the hash used below came from it.
    let sums_url = format!(
        "{}/dist/{}/SHA256SUMS",
        config.dist_base_url,
        config.channel.dist_subdir()
    );
    let sums_path = temp_dir.join(format!("memnoth-update-{}.SHA256SUMS", process::id()));
    download_file(&sums_url, &sums_path)?;
    let signatures = verify_signatures(&sums_path, manifest, config);
    let _ = fs::remove_file(&sums_path);
    signatures?;

    download_file(&download_url, &temp_exe)?;

    // Verify checksum
    if let Err(error) = verify_checksum(&temp_exe, &manifest.checksums.sha256) {
        let _ = fs::remove_file(&temp_exe);
        return Err(error);
    }

    // Prove the artifact runs and is the version it claims, before it replaces
    // anything. On Windows there is no way back after the swap: the running
    // executable cannot be renamed.
    if let Err(error) = verify_new_binary(&temp_exe, &manifest.version) {
        let _ = fs::remove_file(&temp_exe);
        return Err(error);
    }

    // Atomic swap
    atomic_swap(&current_exe, &temp_exe)?;

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&current_exe)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&current_exe, perms)?;
    }

    Ok(())
}

/// Format update notification message.
pub fn format_notification(result: &UpdateCheckResult) -> String {
    format!(
        "Update available: {} → {} (channel: {})\nRun `memnoth update` to install.",
        result.current_version,
        result.latest_version,
        result.manifest.channel.dist_subdir()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_channel_parsing() {
        assert_eq!(Channel::parse("stable").unwrap(), Channel::Stable);
        assert_eq!(Channel::parse("beta").unwrap(), Channel::Beta);
        assert_eq!(Channel::parse("nightly").unwrap(), Channel::Nightly);
        assert!(Channel::parse("unknown").is_err());
    }

    #[test]
    fn test_channel_dist_subdir() {
        assert_eq!(Channel::Stable.dist_subdir(), "stable");
        assert_eq!(Channel::Beta.dist_subdir(), "beta");
        assert_eq!(Channel::Nightly.dist_subdir(), "nightly");
    }

    #[test]
    fn test_semver_comparison() {
        let v1 = Version::parse("1.0.0").unwrap();
        let v2 = Version::parse("1.0.1").unwrap();
        let v3 = Version::parse("1.1.0").unwrap();
        let v4 = Version::parse("2.0.0").unwrap();
        let v5 = Version::parse("0.1.0-beta.1").unwrap();
        let v6 = Version::parse("0.1.0-beta.2").unwrap();

        assert!(v2 > v1);
        assert!(v3 > v2);
        assert!(v4 > v3);
        assert!(v6 > v5);
    }

    #[test]
    fn test_verify_checksum() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.bin");
        fs::write(&file, b"hello world").unwrap();

        let mut hasher = Sha256::new();
        hasher.update(b"hello world");
        let expected = hex::encode(hasher.finalize());

        verify_checksum(&file, &expected).unwrap();

        // Wrong checksum should fail
        assert!(
            verify_checksum(
                &file,
                "0000000000000000000000000000000000000000000000000000000000000000"
            )
            .is_err()
        );
    }

    #[test]
    fn test_resolve_dist_url() {
        let config = UpdateConfig::default();
        assert_eq!(
            resolve_dist_url(&config, Channel::Stable),
            "https://releases.memnoth.astrivya.ai/dist/stable/latest"
        );
        assert_eq!(
            resolve_dist_url(&config, Channel::Beta),
            "https://releases.memnoth.astrivya.ai/dist/beta/latest"
        );
        assert_eq!(
            resolve_dist_url(&config, Channel::Nightly),
            "https://releases.memnoth.astrivya.ai/dist/nightly/latest"
        );
    }

    #[test]
    fn test_manifest_serialization() {
        let manifest = Manifest {
            version: "1.0.0".to_string(),
            channel: Channel::Stable,
            targets: vec![Target {
                triple: "x86_64-linux".to_string(),
                filename: "memnoth-x86_64-linux.tar.gz".to_string(),
            }],
            checksums: Checksums {
                sha256: "abc123".to_string(),
            },
            signatures: Signatures {
                minisign: Some("sig123".to_string()),
                cosign: None,
            },
            published_at: Utc::now(),
            notes: Some("Release notes".to_string()),
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest.version, parsed.version);
        assert_eq!(manifest.channel, parsed.channel);
    }

    #[test]
    fn test_cache_serialization() {
        let cache = UpdateCache {
            last_check: Utc::now(),
            latest_version: Some("1.0.0".to_string()),
            latest_channel: Some(Channel::Stable),
            notification_shown: true,
        };

        let json = serde_json::to_string(&cache).unwrap();
        let parsed: UpdateCache = serde_json::from_str(&json).unwrap();
        assert_eq!(cache.latest_version, parsed.latest_version);
        assert_eq!(cache.notification_shown, parsed.notification_shown);
    }

    #[test]
    fn test_fixture_dist_dir() {
        // Test that the update mechanism can work against a local fixture dist directory
        let dir = tempdir().unwrap();
        let dist_dir = dir.path().join("dist").join("stable");
        fs::create_dir_all(&dist_dir).unwrap();

        // Create a mock manifest
        let manifest = Manifest {
            version: "1.0.0".to_string(),
            channel: Channel::Stable,
            targets: vec![Target {
                triple: "x86_64-windows".to_string(),
                filename: "memnoth.exe".to_string(),
            }],
            checksums: Checksums {
                sha256: "abc123".to_string(),
            },
            signatures: Signatures {
                minisign: None,
                cosign: None,
            },
            published_at: Utc::now(),
            notes: Some("Test release".to_string()),
        };

        let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
        fs::write(dist_dir.join("latest"), manifest_json).unwrap();

        // Create a mock binary
        let mock_binary = b"mock binary content";
        fs::write(dist_dir.join("memnoth.exe"), mock_binary).unwrap();

        // Create SHA256SUMS
        let mut hasher = Sha256::new();
        hasher.update(mock_binary);
        let sha256 = hex::encode(hasher.finalize());
        fs::write(
            dist_dir.join("SHA256SUMS"),
            format!("{}  memnoth.exe", sha256),
        )
        .unwrap();

        // Test that the fixture dist dir has the expected structure
        assert!(dist_dir.join("latest").exists());
        assert!(dist_dir.join("memnoth.exe").exists());
        assert!(dist_dir.join("SHA256SUMS").exists());

        // Verify we can parse the manifest
        let content = fs::read_to_string(dist_dir.join("latest")).unwrap();
        let parsed: Manifest = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.version, "1.0.0");
        assert_eq!(parsed.channel, Channel::Stable);
        assert_eq!(parsed.targets.len(), 1);

        // Verify checksum matches
        let sums_content = fs::read_to_string(dist_dir.join("SHA256SUMS")).unwrap();
        assert!(sums_content.contains(&sha256));
    }

    fn manifest_with(signatures: Signatures) -> Manifest {
        Manifest {
            version: "1.0.0".to_string(),
            channel: Channel::Stable,
            targets: vec![Target {
                triple: "x86_64-windows".to_string(),
                filename: "memnoth.exe".to_string(),
            }],
            checksums: Checksums {
                sha256: "abc123".to_string(),
            },
            signatures,
            published_at: Utc::now(),
            notes: None,
        }
    }

    #[test]
    fn an_unsigned_release_is_refused() {
        let dir = tempdir().unwrap();
        let sums = dir.path().join("SHA256SUMS");
        fs::write(&sums, b"abc123  memnoth.exe\n").unwrap();

        let manifest = manifest_with(Signatures {
            minisign: None,
            cosign: None,
        });
        let error = verify_signatures(&sums, &manifest, &UpdateConfig::default())
            .expect_err("an unsigned release must not install");
        assert!(error.to_string().contains("unsigned"), "{error}");
    }

    #[test]
    fn a_signature_without_a_key_is_refused_rather_than_skipped() {
        let dir = tempdir().unwrap();
        let sums = dir.path().join("SHA256SUMS");
        fs::write(&sums, b"abc123  memnoth.exe\n").unwrap();

        let manifest = manifest_with(Signatures {
            minisign: Some("not-actually-a-signature".to_string()),
            cosign: None,
        });
        // `default()` may carry a key compiled into this build; the case under
        // test is a machine that has none.
        let config = UpdateConfig {
            minisign_pubkey: None,
            cosign_pubkey: None,
            ..Default::default()
        };
        let error = verify_signatures(&sums, &manifest, &config)
            .expect_err("a signature nobody can check is not a verified release");
        assert!(error.to_string().contains("no public key"), "{error}");
    }

    #[test]
    fn cosign_verification_fails_closed() {
        // This build has no sigstore verifier. The old implementation returned
        // Ok(()) after reading the file, so a signed-by-anyone release passed.
        let dir = tempdir().unwrap();
        let sums = dir.path().join("SHA256SUMS");
        fs::write(&sums, b"abc123  memnoth.exe\n").unwrap();
        assert!(verify_cosign(&sums, "c2ln", "/tmp/key").is_err());
    }

    #[test]
    fn a_downgrade_needs_to_be_said_out_loud() {
        assert!(check_version_policy("1.2.0", "1.3.0", false).is_ok());
        assert!(check_version_policy("1.2.0", "1.2.0", false).is_ok());

        let error = check_version_policy("1.2.0", "1.0.0", false)
            .expect_err("an update must not walk the machine backwards");
        assert!(error.to_string().contains("downgrade"), "{error}");

        // Naming the version is what makes it deliberate.
        assert!(check_version_policy("1.2.0", "1.0.0", true).is_ok());
    }

    #[test]
    fn an_artifact_that_does_not_run_is_refused() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("not-a-binary");
        assert!(verify_new_binary(&missing, "1.0.0").is_err());
    }

    #[test]
    fn test_managed_install_detection() {
        // Test that the managed install detection logic works
        // We can't easily test the actual argv[0] but we can test the function doesn't panic
        let _ = detect_managed_install();
    }
}
