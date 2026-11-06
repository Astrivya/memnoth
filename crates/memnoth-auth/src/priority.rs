//! Token source priority resolution.
//!
//! Priority order (CLI wins, never written to config):
//! 1. `--token` flag (highest, CLI only)
//! 2. `MEMNOTH_API_KEY` environment variable
//! 3. OS keychain (keyring)
//! 4. File fallback (~/.memnoth/credentials, 0600)
//!
//! No token in config.toml (context.rs already asserts this).

use anyhow::Result;
use std::path::PathBuf;

use crate::credentials::{
    CredentialStore, CredentialStoreTrait, FileCredentialStore, KeyringCredentialStore,
    MultiCredentialStore,
};

/// Source of a resolved token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// Explicit CLI flag (--token)
    CliFlag,
    /// MEMNOTH_API_KEY environment variable
    EnvVar,
    /// OS keychain/credential manager
    Keyring,
    /// File-based credential store
    File,
    /// No token found
    None,
}

impl TokenSource {
    /// Get the priority (lower = higher priority).
    pub fn priority(&self) -> u8 {
        match self {
            TokenSource::CliFlag => 0,
            TokenSource::EnvVar => 1,
            TokenSource::Keyring => 2,
            TokenSource::File => 3,
            TokenSource::None => 255,
        }
    }

    /// Human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            TokenSource::CliFlag => "CLI --token flag",
            TokenSource::EnvVar => "MEMNOTH_API_KEY environment variable",
            TokenSource::Keyring => "OS keychain (keyring)",
            TokenSource::File => "~/.memnoth/credentials file",
            TokenSource::None => "no token found",
        }
    }
}

/// Resolved token with its source.
#[derive(Debug, Clone)]
pub struct ResolvedToken {
    pub token: String,
    pub source: TokenSource,
}

impl ResolvedToken {
    pub fn new(token: String, source: TokenSource) -> Self {
        Self { token, source }
    }

    /// Get Authorization header value.
    pub fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

/// Resolve a token using the priority chain.
///
/// Arguments:
/// - `cli_token`: Token from --token flag (if provided)
/// - `store`: Optional multi-store for keyring + file fallback
///
/// Returns the highest-priority available token.
pub async fn resolve_token(
    cli_token: Option<String>,
    store: Option<&MultiCredentialStore>,
) -> Result<Option<ResolvedToken>> {
    // 1. CLI flag (highest priority)
    if let Some(token) = cli_token
        && !token.trim().is_empty()
    {
        return Ok(Some(ResolvedToken::new(token, TokenSource::CliFlag)));
    }

    // 2. Environment variable
    if let Ok(token) = std::env::var("MEMNOTH_API_KEY")
        && !token.trim().is_empty()
    {
        return Ok(Some(ResolvedToken::new(token, TokenSource::EnvVar)));
    }

    // 3 & 4. Keychain + File fallback (via store)
    if let Some(store) = store {
        // Check if keyring store exists and try it
        let has_keyring = store
            .stores()
            .iter()
            .any(|s| matches!(s, CredentialStore::Keyring(_)));
        let has_file = store
            .stores()
            .iter()
            .any(|s| matches!(s, CredentialStore::File(_)));

        if has_keyring && let CredentialStore::Keyring(keyring_store) = &store.stores()[0] {
            let exists = keyring_store.exists("api_key").await.unwrap_or(false);
            if exists
                && let Some(token) = keyring_store.get("api_key").await?
                && !token.trim().is_empty()
            {
                return Ok(Some(ResolvedToken::new(token, TokenSource::Keyring)));
            }
        }

        if has_file
            && let CredentialStore::File(file_store) = store
                .stores()
                .get(1)
                .unwrap_or(&CredentialStore::Keyring(KeyringCredentialStore::memnoth()))
            && let Some(token) = file_store.get("api_key").await?
            && !token.trim().is_empty()
        {
            return Ok(Some(ResolvedToken::new(token, TokenSource::File)));
        }
    }

    Ok(None)
}

/// Resolve token with explicit store separation for testing/control.
pub async fn resolve_token_explicit(
    cli_token: Option<String>,
    keyring: Option<&KeyringCredentialStore>,
    file: Option<&FileCredentialStore>,
) -> Result<Option<ResolvedToken>> {
    // 1. CLI flag
    if let Some(token) = cli_token
        && !token.trim().is_empty()
    {
        return Ok(Some(ResolvedToken::new(token, TokenSource::CliFlag)));
    }

    // 2. Environment variable
    if let Ok(token) = std::env::var("MEMNOTH_API_KEY")
        && !token.trim().is_empty()
    {
        return Ok(Some(ResolvedToken::new(token, TokenSource::EnvVar)));
    }

    // 3. Keyring
    if let Some(keyring) = keyring
        && let Some(token) = keyring.get("api_key").await?
        && !token.trim().is_empty()
    {
        return Ok(Some(ResolvedToken::new(token, TokenSource::Keyring)));
    }

    // 4. File
    if let Some(file) = file
        && let Some(token) = file.get("api_key").await?
        && !token.trim().is_empty()
    {
        return Ok(Some(ResolvedToken::new(token, TokenSource::File)));
    }

    Ok(None)
}

/// Store a token to the default location (keyring + file).
pub async fn store_token(
    token: String,
    keyring: Option<&KeyringCredentialStore>,
    file: Option<&FileCredentialStore>,
) -> Result<()> {
    if let Some(keyring) = keyring {
        keyring.set("api_key", token.clone()).await?;
    }
    if let Some(file) = file {
        file.set("api_key", token).await?;
    }
    Ok(())
}

/// Delete token from all stores.
pub async fn delete_token(
    keyring: Option<&KeyringCredentialStore>,
    file: Option<&FileCredentialStore>,
) -> Result<()> {
    if let Some(keyring) = keyring {
        let _ = keyring.delete("api_key").await;
    }
    if let Some(file) = file {
        let _ = file.delete("api_key").await;
    }
    Ok(())
}

/// Get the default credentials file path for display.
pub fn default_credentials_file() -> PathBuf {
    crate::default_credentials_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::{CredentialStore, FileCredentialStore};
    use tempfile::tempdir;

    // Both tests below mutate the same process-wide variable. Without this lock
    // they race when the harness runs them in parallel: one removes the value
    // the other is about to read, so the test that fails depends on scheduling.
    // A tokio mutex, not a std one: these tests hold the guard across an
    // `.await`, which a std guard must not span.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn resolve_cli_priority() {
        let _guard = ENV_LOCK.lock().await;
        unsafe { std::env::set_var("MEMNOTH_API_KEY", "env_token") };

        let result = resolve_token(Some("cli_token".to_string()), None)
            .await
            .unwrap();
        assert!(result.is_some());
        let resolved = result.clone().unwrap();
        assert_eq!(resolved.source, TokenSource::CliFlag);

        unsafe { std::env::remove_var("MEMNOTH_API_KEY") };
    }

    #[tokio::test]
    async fn resolve_env_priority() {
        let _guard = ENV_LOCK.lock().await;
        unsafe { std::env::set_var("MEMNOTH_API_KEY", "env_token") };

        let result = resolve_token(None, None).await.unwrap();
        assert!(result.is_some());
        let resolved = result.clone().unwrap();
        assert_eq!(resolved.source, TokenSource::EnvVar);
        assert_eq!(resolved.token, "env_token");

        unsafe { std::env::remove_var("MEMNOTH_API_KEY") };
    }

    #[tokio::test]
    async fn resolve_file_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("credentials");
        let file = CredentialStore::File(FileCredentialStore::at_path(&path).await.unwrap());
        file.set("api_key", "file_token".to_string()).await.unwrap();

        let result = resolve_token_explicit(None, None, None).await.unwrap();
        assert!(result.is_none()); // No explicit file store passed

        // Now pass the file store explicitly
        if let CredentialStore::File(f) = &file {
            let result = resolve_token_explicit(None, None, Some(f)).await.unwrap();
            assert!(result.is_some());
            let resolved = result.unwrap();
            assert_eq!(resolved.source, TokenSource::File);
            assert_eq!(resolved.token, "file_token");
        }
    }

    #[tokio::test]
    async fn resolve_none_when_no_token() {
        // Same variable, same lock: this test removes what the two above set.
        let _guard = ENV_LOCK.lock().await;
        unsafe { std::env::remove_var("MEMNOTH_API_KEY") };

        let result = resolve_token(None, None).await.unwrap();
        assert!(result.is_none());
    }
    #[tokio::test]
    async fn token_source_priority_order() {
        assert_eq!(TokenSource::CliFlag.priority(), 0);
        assert_eq!(TokenSource::EnvVar.priority(), 1);
        assert_eq!(TokenSource::Keyring.priority(), 2);
        assert_eq!(TokenSource::File.priority(), 3);
        assert_eq!(TokenSource::None.priority(), 255);
    }
}
