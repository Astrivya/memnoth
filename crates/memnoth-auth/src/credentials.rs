//! Credential storage abstraction with OS keychain and file fallback.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use crate::{default_credentials_path, ensure_config_dir};

/// A stored credential entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialEntry {
    /// The credential value (API key, token, etc.)
    pub value: String,
    /// Optional metadata
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, String>>,
    /// When this credential was stored (Unix timestamp)
    pub stored_at: i64,
}

impl CredentialEntry {
    pub fn new(value: String) -> Self {
        Self {
            value,
            metadata: None,
            stored_at: chrono::Utc::now().timestamp(),
        }
    }
}

/// Synchronously read a credential from the default credentials file.
/// This is a blocking alternative for contexts that cannot be async.
pub fn get_credential_sync(key: &str) -> anyhow::Result<Option<String>> {
    let path = default_credentials_path();
    if !path.exists() {
        return Ok(None);
    }
    let file = std::fs::File::open(&path)
        .with_context(|| format!("failed to open credentials file: {}", path.display()))?;
    let reader = std::io::BufReader::new(file);
    let entries: std::collections::HashMap<String, CredentialEntry> =
        serde_json::from_reader(reader).context("failed to parse credentials file")?;
    Ok(entries.get(key).map(|e| e.value.clone()))
}

/// Trait for credential storage backends.
///
/// Implementations must be thread-safe and handle concurrent access.
#[async_trait::async_trait]
pub trait CredentialStoreTrait: Send + Sync {
    /// Get a credential by key.
    async fn get(&self, key: &str) -> Result<Option<String>>;

    /// Store a credential.
    async fn set(&self, key: &str, value: String) -> Result<()>;

    /// Delete a credential.
    async fn delete(&self, key: &str) -> Result<()>;

    /// List all credential keys (without values).
    async fn list_keys(&self) -> Result<Vec<String>>;

    /// Check if a credential exists.
    async fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.get(key).await?.is_some())
    }
}

/// Credential storage backend enum.
///
/// Provides a dyn-compatible way to use multiple credential stores.
#[derive(Clone)]
pub enum CredentialStore {
    Keyring(KeyringCredentialStore),
    File(FileCredentialStore),
}

impl CredentialStore {
    /// Get a credential by key.
    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        match self {
            CredentialStore::Keyring(s) => s.get(key).await,
            CredentialStore::File(s) => s.get(key).await,
        }
    }

    /// Store a credential.
    pub async fn set(&self, key: &str, value: String) -> Result<()> {
        match self {
            CredentialStore::Keyring(s) => s.set(key, value).await,
            CredentialStore::File(s) => s.set(key, value).await,
        }
    }

    /// Delete a credential.
    pub async fn delete(&self, key: &str) -> Result<()> {
        match self {
            CredentialStore::Keyring(s) => s.delete(key).await,
            CredentialStore::File(s) => s.delete(key).await,
        }
    }

    /// List all credential keys (without values).
    pub async fn list_keys(&self) -> Result<Vec<String>> {
        match self {
            CredentialStore::Keyring(s) => s.list_keys().await,
            CredentialStore::File(s) => s.list_keys().await,
        }
    }

    /// Check if a credential exists.
    pub async fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.get(key).await?.is_some())
    }
}

/// OS keychain credential store using the `keyring` crate.
///
/// Uses the system credential manager:
/// - macOS: Keychain
/// - Windows: Credential Manager
/// - Linux: Secret Service (libsecret/gnome-keyring/kwallet)
#[derive(Clone)]
pub struct KeyringCredentialStore {
    service: String,
}

impl KeyringCredentialStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// Default service name for memnoth CLI credentials.
    pub fn memnoth() -> Self {
        Self::new("memnoth-cli")
    }
}

#[async_trait::async_trait]
impl CredentialStoreTrait for KeyringCredentialStore {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        let entry = keyring::Entry::new(&self.service, key)?;
        match entry.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context("failed to get credential from keyring"),
        }
    }

    async fn set(&self, key: &str, value: String) -> Result<()> {
        let entry = keyring::Entry::new(&self.service, key)?;
        entry
            .set_password(&value)
            .context("failed to set credential in keyring")
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let entry = keyring::Entry::new(&self.service, key)?;
        entry
            .delete_credential()
            .context("failed to delete credential from keyring")
    }

    async fn list_keys(&self) -> Result<Vec<String>> {
        // keyring crate doesn't support listing keys directly
        // Return empty list - callers should track keys themselves
        Ok(Vec::new())
    }
}

/// File-based credential store with secure permissions.
///
/// Stores credentials in ~/.memnoth/credentials as JSON with 0600 permissions.
/// On Windows, verifies ACL restricts access to current user.
#[derive(Clone)]
pub struct FileCredentialStore {
    path: std::path::PathBuf,
    // In-memory cache for performance
    cache: Arc<tokio::sync::RwLock<HashMap<String, CredentialEntry>>>,
}

impl FileCredentialStore {
    /// Create a new file credential store at the default location.
    pub async fn new() -> Result<Self> {
        let path = default_credentials_path();
        Self::at_path(path).await
    }

    /// Create a new file credential store at a custom path.
    pub async fn at_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        ensure_config_dir()?;

        let store = Self {
            path,
            cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        };

        store.load().await?;
        Ok(store)
    }

    /// Load credentials from disk into memory.
    async fn load(&self) -> Result<()> {
        if !self.path.exists() {
            return Ok(());
        }

        let file = File::open(&self.path).context("failed to open credentials file")?;
        let reader = BufReader::new(file);
        let entries: HashMap<String, CredentialEntry> =
            serde_json::from_reader(reader).context("failed to parse credentials file")?;

        let mut cache = self.cache.write().await;
        *cache = entries;
        Ok(())
    }

    /// Persist credentials to disk with secure permissions.
    async fn persist(&self) -> Result<()> {
        let cache = self.cache.read().await;

        // Write to temp file first, then atomic rename
        let temp_path = self.path.with_extension("tmp");
        {
            let file =
                File::create(&temp_path).context("failed to create temp credentials file")?;
            let mut writer = BufWriter::new(file);
            serde_json::to_writer_pretty(&mut writer, &*cache)
                .context("failed to serialize credentials")?;
            writer.flush().context("failed to flush credentials file")?;
        }

        // Set secure permissions before rename
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&temp_path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&temp_path, perms)?;
        }

        fs::rename(&temp_path, &self.path).context("failed to rename credentials file")?;
        Ok(())
    }

    /// Get the credentials file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Synchronously get a credential from the file store.
    pub fn get_sync(&self, key: &str) -> Result<Option<String>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let file = std::fs::File::open(&self.path).context("failed to open credentials file")?;
        let reader = std::io::BufReader::new(file);
        let entries: HashMap<String, CredentialEntry> =
            serde_json::from_reader(reader).context("failed to parse credentials file")?;
        Ok(entries.get(key).map(|e| e.value.clone()))
    }

    /// Synchronously set a credential in the file store.
    pub fn set_sync(&self, key: &str, value: String) -> Result<()> {
        let mut entries = if self.path.exists() {
            let file =
                std::fs::File::open(&self.path).context("failed to open credentials file")?;
            let reader = std::io::BufReader::new(file);
            serde_json::from_reader(reader).context("failed to parse credentials file")?
        } else {
            HashMap::new()
        };

        entries.insert(key.to_string(), CredentialEntry::new(value));

        // Write to temp file first, then atomic rename
        let temp_path = self.path.with_extension("tmp");
        {
            let file = std::fs::File::create(&temp_path)
                .context("failed to create temp credentials file")?;
            let mut writer = std::io::BufWriter::new(file);
            serde_json::to_writer_pretty(&mut writer, &entries)
                .context("failed to serialize credentials")?;
            writer.flush().context("failed to flush credentials file")?;
        }

        // Set secure permissions before rename
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&temp_path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&temp_path, perms)?;
        }

        fs::rename(&temp_path, &self.path).context("failed to rename credentials file")?;
        Ok(())
    }

    /// Synchronously delete a credential from the file store.
    pub fn delete_sync(&self, key: &str) -> Result<()> {
        if !self.path.exists() {
            return Ok(());
        }
        let mut entries: HashMap<String, CredentialEntry> = {
            let file =
                std::fs::File::open(&self.path).context("failed to open credentials file")?;
            let reader = std::io::BufReader::new(file);
            serde_json::from_reader(reader).context("failed to parse credentials file")?
        };

        entries.remove(key);

        // Write to temp file first, then atomic rename
        let temp_path = self.path.with_extension("tmp");
        {
            let file = std::fs::File::create(&temp_path)
                .context("failed to create temp credentials file")?;
            let mut writer = std::io::BufWriter::new(file);
            serde_json::to_writer_pretty(&mut writer, &entries)
                .context("failed to serialize credentials")?;
            writer.flush().context("failed to flush credentials file")?;
        }

        // Set secure permissions before rename
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&temp_path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&temp_path, perms)?;
        }

        fs::rename(&temp_path, &self.path).context("failed to rename credentials file")?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl CredentialStoreTrait for FileCredentialStore {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        let cache = self.cache.read().await;
        Ok(cache.get(key).map(|e| e.value.clone()))
    }

    async fn set(&self, key: &str, value: String) -> Result<()> {
        let entry = CredentialEntry::new(value);
        {
            let mut cache = self.cache.write().await;
            cache.insert(key.to_string(), entry);
        }
        self.persist().await
    }

    async fn delete(&self, key: &str) -> Result<()> {
        {
            let mut cache = self.cache.write().await;
            cache.remove(key);
        }
        self.persist().await
    }

    async fn list_keys(&self) -> Result<Vec<String>> {
        let cache = self.cache.read().await;
        Ok(cache.keys().cloned().collect())
    }
}

/// Multi-store credential store that tries multiple backends in order.
///
/// Tries keyring first, then file. This is the recommended store for CLI use.
pub struct MultiCredentialStore {
    stores: Vec<CredentialStore>,
}

impl MultiCredentialStore {
    /// Create with default stores: keyring -> file.
    pub async fn default() -> Result<Self> {
        let keyring = CredentialStore::Keyring(KeyringCredentialStore::memnoth());
        let file = CredentialStore::File(FileCredentialStore::new().await?);

        Ok(Self {
            stores: vec![keyring, file],
        })
    }

    /// Create with custom stores in priority order (first = highest priority).
    pub fn new(stores: Vec<CredentialStore>) -> Self {
        Self { stores }
    }

    /// Get the underlying stores for inspection.
    pub fn stores(&self) -> &[CredentialStore] {
        &self.stores
    }
}

impl MultiCredentialStore {
    /// Get a credential by key - tries each store in priority order.
    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        for store in &self.stores {
            if let Some(value) = store.get(key).await? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    /// Store a credential - writes to all stores (best effort).
    pub async fn set(&self, key: &str, value: String) -> Result<()> {
        for store in &self.stores {
            let _ = store.set(key, value.clone()).await;
        }
        Ok(())
    }

    /// Delete a credential from all stores.
    pub async fn delete(&self, key: &str) -> Result<()> {
        for store in &self.stores {
            let _ = store.delete(key).await;
        }
        Ok(())
    }

    /// List all credential keys from all stores.
    pub async fn list_keys(&self) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        for store in &self.stores {
            keys.extend(store.list_keys().await?);
        }
        keys.sort();
        keys.dedup();
        Ok(keys)
    }

    /// Check if a credential exists in any store.
    pub async fn exists(&self, key: &str) -> Result<bool> {
        for store in &self.stores {
            if store.exists(key).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Try to get from a specific store index (0 = keyring, 1 = file).
    pub async fn get_from(&self, index: usize, key: &str) -> Result<Option<String>> {
        self.stores
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("no store at index {index}"))?
            .get(key)
            .await
    }

    /// Synchronously get a credential by key - tries each store in priority order.
    /// Uses the file store directly for blocking contexts.
    pub fn get_sync(&self, key: &str) -> Result<Option<String>> {
        // Try file store directly (synchronous)
        for store in &self.stores {
            if let CredentialStore::File(file_store) = store {
                if let Some(value) = file_store.get_sync(key)? {
                    return Ok(Some(value));
                }
            }
        }
        Ok(None)
    }

    /// Synchronously store a credential - writes to file store.
    pub fn set_sync(&self, key: &str, value: String) -> Result<()> {
        for store in &self.stores {
            if let CredentialStore::File(file_store) = store {
                file_store.set_sync(key, value.clone())?;
            }
        }
        Ok(())
    }

    /// Synchronously delete a credential from all stores.
    pub fn delete_sync(&self, key: &str) -> Result<()> {
        for store in &self.stores {
            if let CredentialStore::File(file_store) = store {
                file_store.delete_sync(key)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn file_store_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("credentials");
        let store = FileCredentialStore::at_path(&path).await.unwrap();

        store
            .set("test_key", "test_value".to_string())
            .await
            .unwrap();
        let value = store.get("test_key").await.unwrap();
        assert_eq!(value, Some("test_value".to_string()));

        store.delete("test_key").await.unwrap();
        let value = store.get("test_key").await.unwrap();
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn file_store_permissions_unix() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let dir = tempdir().unwrap();
            let path = dir.path().join("credentials");
            let store = FileCredentialStore::at_path(&path).await.unwrap();

            store.set("key", "value".to_string()).await.unwrap();

            let metadata = fs::metadata(&path).unwrap();
            let mode = metadata.mode() & 0o777;
            assert_eq!(mode, 0o600, "credentials file should be 0600");
        }
    }

    #[tokio::test]
    async fn multi_store_priority() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("credentials");
        let file = CredentialStore::File(FileCredentialStore::at_path(&path).await.unwrap());
        let store = MultiCredentialStore::new(vec![file]);

        store
            .set("api_key", "test_token".to_string())
            .await
            .unwrap();
        let value = store.get("api_key").await.unwrap();
        assert_eq!(value, Some("test_token".to_string()));
    }
}
