//! Token and identity caching with refresh logic and TTL.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::credentials::CredentialStore;

/// Information about a stored access token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenInfo {
    pub access_token: String,
    /// Optional refresh token for obtaining new access tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Token expiry time (UTC)
    #[serde(with = "chrono::serde::ts_seconds_option")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Token type (usually "Bearer")
    #[serde(default = "default_token_type")]
    pub token_type: String,
    /// Organization/context this token belongs to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    /// Organization name for display
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
    /// Scopes granted by this token
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// When this token was cached
    #[serde(with = "chrono::serde::ts_seconds")]
    pub cached_at: DateTime<Utc>,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

impl TokenInfo {
    /// Create a new token info with just an access token.
    pub fn new(access_token: String) -> Self {
        Self {
            access_token,
            refresh_token: None,
            expires_at: None,
            token_type: default_token_type(),
            org_id: None,
            org_name: None,
            scopes: Vec::new(),
            cached_at: Utc::now(),
        }
    }

    /// Create from an OAuth token response.
    pub fn from_oauth(
        access_token: String,
        refresh_token: Option<String>,
        expires_in: Option<u64>,
        token_type: Option<String>,
        org_id: Option<String>,
        org_name: Option<String>,
        scopes: Vec<String>,
    ) -> Self {
        let expires_at = expires_in.map(|secs| Utc::now() + Duration::seconds(secs as i64));
        Self {
            access_token,
            refresh_token,
            expires_at,
            token_type: token_type.unwrap_or_else(default_token_type),
            org_id,
            org_name,
            scopes,
            cached_at: Utc::now(),
        }
    }

    /// Check if the token is expired (with optional buffer).
    pub fn is_expired(&self, buffer: Duration) -> bool {
        match self.expires_at {
            Some(expires) => Utc::now() + buffer >= expires,
            None => false, // No expiry = treat as non-expiring
        }
    }

    /// Check if token is expiring soon (within buffer).
    pub fn is_expiring_soon(&self, buffer: Duration) -> bool {
        match self.expires_at {
            Some(expires) => Utc::now() + buffer >= expires,
            None => false,
        }
    }

    /// Get the Authorization header value.
    pub fn auth_header(&self) -> String {
        format!("{} {}", self.token_type, self.access_token)
    }

    /// Get a redacted version for logging.
    pub fn redacted(&self) -> String {
        let token_preview = if self.access_token.len() > 8 {
            format!("{}***", &self.access_token[..4])
        } else {
            "***".to_string()
        };
        format!(
            "TokenInfo {{ token: {}, org: {:?}, expires: {:?} }}",
            token_preview, self.org_id, self.expires_at
        )
    }
}

/// Cached whoami/user identity information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WhoamiInfo {
    /// User ID
    pub user_id: String,
    /// User email
    pub email: String,
    /// User display name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Organization memberships
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub organizations: Vec<OrgMembership>,
    /// When this info was cached
    #[serde(with = "chrono::serde::ts_seconds")]
    pub cached_at: DateTime<Utc>,
    /// When this cache entry expires
    #[serde(with = "chrono::serde::ts_seconds")]
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrgMembership {
    pub org_id: String,
    pub org_name: String,
    pub role: String,
}

impl WhoamiInfo {
    /// Create new whoami info with a TTL.
    pub fn new(
        user_id: String,
        email: String,
        name: Option<String>,
        organizations: Vec<OrgMembership>,
        ttl: Duration,
    ) -> Self {
        let now = Utc::now();
        Self {
            user_id,
            email,
            name,
            organizations,
            cached_at: now,
            expires_at: now + ttl,
        }
    }

    /// Check if the cache entry is expired.
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at
    }

    /// Get a redacted version for logging.
    pub fn redacted(&self) -> String {
        format!(
            "WhoamiInfo {{ user: {}, email: {}, orgs: {} }}",
            self.user_id,
            self.email,
            self.organizations.len()
        )
    }
}

/// Token cache with automatic refresh logic.
///
/// Stores tokens per organization/context and handles refresh when needed.
pub struct TokenCache {
    /// In-memory cache: org_id -> TokenInfo (None = default/global)
    cache: Arc<RwLock<HashMap<Option<String>, TokenInfo>>>,
    /// Credential store for persistence
    store: Arc<CredentialStore>,
    /// Refresh callback: (refresh_token, org_id) -> new TokenInfo
    refresh_fn: Option<Arc<dyn Fn(String, Option<String>) -> Result<TokenInfo> + Send + Sync>>,
}

impl TokenCache {
    /// Create a new token cache with a credential store.
    pub fn new(store: Arc<CredentialStore>) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            store,
            refresh_fn: None,
        }
    }

    /// Set a custom refresh function.
    pub fn with_refresh<F>(mut self, refresh_fn: F) -> Self
    where
        F: Fn(String, Option<String>) -> Result<TokenInfo> + Send + Sync + 'static,
    {
        self.refresh_fn = Some(Arc::new(refresh_fn));
        self
    }

    /// Load all tokens from the credential store.
    pub async fn load(&self) -> Result<()> {
        let keys = self.store.list_keys().await?;
        let mut cache = self.cache.write().await;

        for key in keys {
            if key.starts_with("token:") {
                let org_id = key.strip_prefix("token:");
                let org_key = org_id.map(|s| s.to_string());

                if let Some(value) = self.store.get(&key).await? {
                    if let Ok(token_info) = serde_json::from_str::<TokenInfo>(&value) {
                        cache.insert(org_key, token_info);
                    }
                }
            }
        }
        Ok(())
    }

    /// Get a token for the given org (None = default).
    pub async fn get(&self, org_id: Option<&str>) -> Result<Option<TokenInfo>> {
        let cache = self.cache.read().await;
        Ok(cache.get(&org_id.map(String::from)).cloned())
    }

    /// Get a valid (non-expired) token, refreshing if necessary.
    pub async fn get_valid(
        &self,
        org_id: Option<&str>,
        refresh_buffer: Duration,
    ) -> Result<Option<TokenInfo>> {
        let org_key = org_id.map(String::from);

        // First check cache
        let cached_token = {
            let cache = self.cache.read().await;
            cache.get(&org_key).cloned()
        };

        if let Some(token) = &cached_token {
            if !token.is_expired(refresh_buffer) {
                return Ok(Some(token.clone()));
            }
            // Token expired but we have a refresh token - try to refresh
            if token.refresh_token.is_some() && self.refresh_fn.is_some() {
                return self
                    .refresh_token(org_id, token.refresh_token.as_deref())
                    .await;
            }
        }

        // Try to load from store if not in cache
        let store_key = format!("token:{}", org_id.unwrap_or("default"));
        if let Some(value) = self.store.get(&store_key).await? {
            if let Ok(token_info) = serde_json::from_str::<TokenInfo>(&value) {
                if !token_info.is_expired(refresh_buffer) {
                    // Cache it
                    let cache_key = org_key.clone();
                    self.cache
                        .write()
                        .await
                        .insert(cache_key, token_info.clone());
                    return Ok(Some(token_info));
                }
                // Try refresh
                if token_info.refresh_token.is_some() && self.refresh_fn.is_some() {
                    return self
                        .refresh_token(org_id, token_info.refresh_token.as_deref())
                        .await;
                }
            }
        }

        Ok(None)
    }

    /// Refresh a token using the refresh function.
    async fn refresh_token(
        &self,
        org_id: Option<&str>,
        refresh_token: Option<&str>,
    ) -> Result<Option<TokenInfo>> {
        let refresh_token =
            refresh_token.ok_or_else(|| anyhow::anyhow!("no refresh token available"))?;
        let refresh_fn = self
            .refresh_fn
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no refresh function configured"))?;

        let org_key = org_id.map(String::from);
        let new_token = refresh_fn(refresh_token.to_string(), org_key.clone())?;

        // Cache and persist the new token
        self.set(org_id, new_token.clone()).await?;
        Ok(Some(new_token))
    }

    /// Store a token.
    pub async fn set(&self, org_id: Option<&str>, token: TokenInfo) -> Result<()> {
        let org_key = org_id.map(String::from);
        {
            let mut cache = self.cache.write().await;
            cache.insert(org_key.clone(), token.clone());
        }

        // Persist to credential store
        let store_key = format!("token:{}", org_id.unwrap_or("default"));
        let json = serde_json::to_string(&token).context("failed to serialize token")?;
        self.store.set(&store_key, json).await
    }

    /// Delete a token.
    pub async fn delete(&self, org_id: Option<&str>) -> Result<()> {
        let org_key = org_id.map(String::from);
        {
            let mut cache = self.cache.write().await;
            cache.remove(&org_key);
        }

        let store_key = format!("token:{}", org_id.unwrap_or("default"));
        self.store.delete(&store_key).await
    }

    /// List all cached org IDs.
    pub async fn list_orgs(&self) -> Vec<Option<String>> {
        let cache = self.cache.read().await;
        cache.keys().cloned().collect()
    }

    /// Clear all tokens.
    pub async fn clear(&self) -> Result<()> {
        let cache = self.cache.read().await;
        let keys: Vec<_> = cache.keys().cloned().collect();
        drop(cache);

        for key in keys {
            let store_key = format!("token:{}", key.as_deref().unwrap_or("default"));
            let _ = self.store.delete(&store_key).await;
        }

        self.cache.write().await.clear();
        Ok(())
    }
}

/// Whoami cache with TTL-based expiration.
pub struct WhoamiCache {
    cache: Arc<RwLock<Option<WhoamiInfo>>>,
    store: Arc<CredentialStore>,
    #[allow(dead_code)]
    default_ttl: Duration,
}

impl WhoamiCache {
    /// Create a new whoami cache.
    pub fn new(store: Arc<CredentialStore>, default_ttl: Duration) -> Self {
        Self {
            cache: Arc::new(RwLock::new(None)),
            store,
            default_ttl,
        }
    }

    /// Load whoami from store.
    pub async fn load(&self) -> Result<()> {
        if let Some(value) = self.store.get("whoami").await? {
            if let Ok(info) = serde_json::from_str::<WhoamiInfo>(&value) {
                if !info.is_expired() {
                    *self.cache.write().await = Some(info);
                }
            }
        }
        Ok(())
    }

    /// Get cached whoami info.
    pub async fn get(&self) -> Option<WhoamiInfo> {
        let cache = self.cache.read().await;
        cache.as_ref().filter(|i| !i.is_expired()).cloned()
    }

    /// Get valid whoami, or None if expired/not cached.
    pub async fn get_valid(&self) -> Option<WhoamiInfo> {
        self.get().await
    }

    /// Set whoami info.
    pub async fn set(&self, info: WhoamiInfo) -> Result<()> {
        *self.cache.write().await = Some(info.clone());
        let json = serde_json::to_string(&info).context("failed to serialize whoami")?;
        self.store.set("whoami", json).await
    }

    /// Clear the cache.
    pub async fn clear(&self) -> Result<()> {
        *self.cache.write().await = None;
        self.store.delete("whoami").await
    }

    /// Check if we have a valid cached whoami.
    pub async fn has_valid(&self) -> bool {
        self.get().await.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::{CredentialStore, FileCredentialStore};
    use chrono::Duration;
    use tempfile::tempdir;

    #[tokio::test]
    async fn token_cache_basic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("credentials");
        let store = Arc::new(CredentialStore::File(
            FileCredentialStore::at_path(&path).await.unwrap(),
        ));
        let cache = TokenCache::new(store);

        let token = TokenInfo::new("test_token".to_string());
        cache.set(None, token.clone()).await.unwrap();

        let retrieved = cache.get(None).await.unwrap();
        assert_eq!(retrieved, Some(token));
    }

    #[tokio::test]
    async fn token_cache_expiry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("credentials");
        let store = Arc::new(CredentialStore::File(
            FileCredentialStore::at_path(&path).await.unwrap(),
        ));
        let cache = TokenCache::new(store);

        let mut token = TokenInfo::new("test_token".to_string());
        token.expires_at = Some(Utc::now() - Duration::seconds(10)); // Expired

        cache.set(None, token).await.unwrap();

        let retrieved = cache.get_valid(None, Duration::seconds(0)).await.unwrap();
        assert_eq!(retrieved, None); // Should be None because expired
    }

    #[tokio::test]
    async fn whoami_cache_ttl() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("credentials");
        let store = Arc::new(CredentialStore::File(
            FileCredentialStore::at_path(&path).await.unwrap(),
        ));
        let cache = WhoamiCache::new(store, Duration::hours(1));

        let info = WhoamiInfo::new(
            "user123".to_string(),
            "test@example.com".to_string(),
            Some("Test User".to_string()),
            vec![],
            Duration::hours(1),
        );

        cache.set(info.clone()).await.unwrap();
        let retrieved = cache.get().await.unwrap();
        assert_eq!(retrieved.user_id, "user123");

        // Expire it
        let mut expired = info;
        expired.expires_at = Utc::now() - Duration::seconds(10);
        cache.set(expired).await.unwrap();

        let retrieved = cache.get().await;
        assert_eq!(retrieved, None);
    }

    #[tokio::test]
    async fn token_info_redacted() {
        let token = TokenInfo::new("sk-1234567890abcdef".to_string());
        let redacted = token.redacted();
        assert!(redacted.contains("sk-1"));
        assert!(redacted.contains("***"));
        assert!(!redacted.contains("abcdef"));
    }

    #[tokio::test]
    async fn whoami_info_redacted() {
        let info = WhoamiInfo::new(
            "user123".to_string(),
            "test@example.com".to_string(),
            Some("Test User".to_string()),
            vec![],
            Duration::hours(1),
        );
        let redacted = info.redacted();
        assert!(redacted.contains("user123"));
        assert!(redacted.contains("test@example.com"));
        assert!(redacted.contains("orgs: 0"));
    }
}
