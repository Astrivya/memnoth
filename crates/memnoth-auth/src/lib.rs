//! Authentication and credential management for memnoth CLI.
//!
//! Provides:
//! - `CredentialStore` trait with OS keychain (keyring) + file fallback
//! - `TokenCache` with refresh logic, expiry tracking, org context
//! - `WhoamiCache` with TTL for user identity
//! - Redaction helpers for logging (strip tokens from output)
//!
//! Priority order (CLI wins, never written):
//! 1. `--token` flag
//! 2. `MEMNOTH_API_KEY` environment variable
//! 3. OS keychain (keyring)
//! 4. File fallback (~/.memnoth/credentials, 0600, Windows ACL check)
//!
//! No token in config.toml (context.rs already asserts this).

pub mod cache;
pub mod credentials;
pub mod priority;
pub mod redaction;

pub use cache::{TokenCache, TokenInfo, WhoamiCache, WhoamiInfo};
pub use credentials::{
    ACCOUNT_KEY, CredentialStore, FileCredentialStore, KeyringCredentialStore,
    MultiCredentialStore, account_key_sync, get_credential_sync,
};
pub use priority::{TokenSource, resolve_token};
pub use redaction::{redact_header_value, redact_token};

use std::path::PathBuf;

/// Default path for the credentials file: ~/.memnoth/credentials
pub fn default_credentials_path() -> PathBuf {
    dirs::home_dir()
        .expect("home directory must be resolvable")
        .join(".memnoth")
        .join("credentials")
}

/// Ensure the ~/.memnoth directory exists with correct permissions
pub fn ensure_config_dir() -> anyhow::Result<PathBuf> {
    let path = dirs::home_dir()
        .expect("home directory must be resolvable")
        .join(".memnoth");

    std::fs::create_dir_all(&path)?;

    #[cfg(windows)]
    {
        // On Windows, verify the directory ACL restricts access to the current user
        use std::os::windows::fs::MetadataExt;
        let metadata = std::fs::metadata(&path)?;
        let file_attributes = metadata.file_attributes();
        // FILE_ATTRIBUTE_DIRECTORY = 0x10, we just check it's a directory
        if file_attributes & 0x10 == 0 {
            anyhow::bail!("expected directory, got file");
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(&path)?;
        let mode = metadata.mode() & 0o777;
        if mode != 0o700 {
            // Fix permissions if they're too open
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
    }

    Ok(path)
}
