//! Provider configuration with secure API key storage for memnoth.
//!
//! This crate provides:
//! - `ProviderConfig` struct with name, base_url, api_key (encrypted), models_cache, enabled
//! - Secure storage via `memnoth-auth` CredentialStore
//! - Known providers list with defaults (base_url, model list endpoint)

use anyhow::{Context, Result};
use chrono::Utc;
use memnoth_auth::{MultiCredentialStore, ensure_config_dir};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A provider configuration entry with secure API key storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Unique identifier for this provider configuration
    pub name: String,
    /// Display name for the provider
    pub display_name: String,
    /// Base URL for the provider's API
    pub base_url: String,
    /// API key reference (stored securely via CredentialStore, not in this struct)
    #[serde(skip)]
    pub api_key: Option<String>,
    /// Cached list of available models
    #[serde(default)]
    pub models_cache: Vec<String>,
    /// When the models cache was last updated (Unix timestamp)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_cache_updated: Option<i64>,
    /// Whether this provider is enabled
    pub enabled: bool,
    /// Provider kind (e.g., "anthropic", "openai", "custom")
    pub kind: String,
    /// Model list endpoint (relative to base_url)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_endpoint: Option<String>,
}

/// Known provider presets with default configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownProvider {
    /// Internal identifier
    pub id: String,
    /// Display name
    pub name: String,
    /// Provider kind
    pub kind: String,
    /// Default base URL
    pub base_url: String,
    /// Model list endpoint (relative to base_url)
    pub models_endpoint: Option<String>,
    /// Whether this is a custom/user-added provider
    #[serde(default)]
    pub is_custom: bool,
}

/// Manager for provider configurations with secure credential storage
pub struct ProviderConfigManager {
    credential_store: MultiCredentialStore,
    config_path: PathBuf,
}

impl ProviderConfigManager {
    /// Create a new provider config manager
    pub async fn new() -> Result<Self> {
        let config_dir = ensure_config_dir()?;
        let config_path = config_dir.join("providers.toml");
        let credential_store = MultiCredentialStore::default().await?;

        Ok(Self {
            credential_store,
            config_path,
        })
    }

    /// Create a new provider config manager with a custom credential store
    pub fn with_store(credential_store: MultiCredentialStore, config_path: PathBuf) -> Self {
        Self {
            credential_store,
            config_path,
        }
    }

    /// Get the credential store
    pub fn credential_store(&self) -> &MultiCredentialStore {
        &self.credential_store
    }

    /// Load all provider configurations from disk
    pub fn load_configs(&self) -> Result<Vec<ProviderConfig>> {
        if !self.config_path.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(&self.config_path)
            .with_context(|| format!("failed to read {}", self.config_path.display()))?;

        let configs: Vec<ProviderConfig> = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", self.config_path.display()))?;

        Ok(configs)
    }

    /// Save all provider configurations to disk
    pub fn save_configs(&self, configs: &[ProviderConfig]) -> Result<()> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }

        let content =
            toml::to_string_pretty(configs).context("cannot serialize provider configs")?;
        std::fs::write(&self.config_path, content)
            .with_context(|| format!("cannot write {}", self.config_path.display()))
    }

    /// Get a provider configuration by name
    pub fn get_config(&self, name: &str) -> Result<Option<ProviderConfig>> {
        let configs = self.load_configs()?;
        Ok(configs.into_iter().find(|c| c.name == name))
    }

    /// Get a provider configuration by name with API key loaded from secure storage
    pub async fn get_config_with_key(&self, name: &str) -> Result<Option<ProviderConfig>> {
        let mut config = self.get_config(name)?;
        if let Some(ref mut c) = config {
            let key_name = format!("provider.{}.api_key", c.name);
            if let Ok(Some(key)) = self.credential_store.get(&key_name).await {
                c.api_key = Some(key);
            }
        }
        Ok(config)
    }

    /// Add or update a provider configuration
    pub async fn upsert_config(&self, mut config: ProviderConfig) -> Result<()> {
        let mut configs = self.load_configs()?;

        // Store API key securely if provided
        if let Some(api_key) = config.api_key.take() {
            let key_name = format!("provider.{}.api_key", config.name);
            self.credential_store
                .set(&key_name, api_key)
                .await
                .with_context(|| format!("failed to store API key for {}", config.name))?;
        }

        // Update or add
        if let Some(existing) = configs.iter_mut().find(|c| c.name == config.name) {
            *existing = config;
        } else {
            configs.push(config);
        }

        self.save_configs(&configs)
    }

    /// Delete a provider configuration and its API key
    pub async fn delete_config(&self, name: &str) -> Result<bool> {
        let mut configs = self.load_configs()?;
        let initial_len = configs.len();
        configs.retain(|c| c.name != name);

        if configs.len() < initial_len {
            let key_name = format!("provider.{}.api_key", name);
            let _ = self.credential_store.delete(&key_name).await;
            self.save_configs(&configs)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// List all provider configurations (without API keys)
    pub fn list_configs(&self) -> Result<Vec<ProviderConfig>> {
        self.load_configs()
    }

    /// Set the API key for a provider
    pub async fn set_api_key(&self, name: &str, api_key: String) -> Result<()> {
        let key_name = format!("provider.{}.api_key", name);
        self.credential_store
            .set(&key_name, api_key)
            .await
            .with_context(|| format!("failed to store API key for {}", name))
    }

    /// Get the API key for a provider
    pub async fn get_api_key(&self, name: &str) -> Result<Option<String>> {
        let key_name = format!("provider.{}.api_key", name);
        self.credential_store
            .get(&key_name)
            .await
            .with_context(|| format!("failed to get API key for {}", name))
    }

    /// Delete the API key for a provider
    pub async fn delete_api_key(&self, name: &str) -> Result<()> {
        let key_name = format!("provider.{}.api_key", name);
        self.credential_store
            .delete(&key_name)
            .await
            .with_context(|| format!("failed to delete API key for {}", name))
    }

    /// Update the models cache for a provider
    pub async fn update_models_cache(&self, name: &str, _models: Vec<String>) -> Result<()> {
        let mut configs = self.load_configs()?;
        if let Some(config) = configs.iter_mut().find(|c| c.name == name) {
            config.models_cache = _models;
            config.models_cache_updated = Some(Utc::now().timestamp());
            self.save_configs(&configs)?;
        }
        Ok(())
    }

    /// Enable or disable a provider
    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        let mut configs = self.load_configs()?;
        if let Some(config) = configs.iter_mut().find(|c| c.name == name) {
            config.enabled = enabled;
            self.save_configs(&configs)?;
        }
        Ok(())
    }
}

/// Get the list of known providers with default configurations
pub fn known_providers() -> Vec<KnownProvider> {
    vec![
        KnownProvider {
            id: "anthropic".to_string(),
            name: "Anthropic".to_string(),
            kind: "anthropic".to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            models_endpoint: Some("/v1/models".to_string()),
            is_custom: false,
        },
        KnownProvider {
            id: "openai".to_string(),
            name: "OpenAI".to_string(),
            kind: "openai".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            models_endpoint: Some("/models".to_string()),
            is_custom: false,
        },
        KnownProvider {
            id: "azure".to_string(),
            name: "Azure OpenAI".to_string(),
            kind: "azure".to_string(),
            base_url: "https://{resource}.openai.azure.com".to_string(),
            models_endpoint: Some("/openai/models?api-version=2024-02-01".to_string()),
            is_custom: false,
        },
        KnownProvider {
            id: "google".to_string(),
            name: "Google AI Studio (Gemini)".to_string(),
            kind: "google".to_string(),
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            models_endpoint: Some("/models".to_string()),
            is_custom: false,
        },
        KnownProvider {
            id: "kilo".to_string(),
            name: "Kilo Code (OpenRouter)".to_string(),
            kind: "openai".to_string(),
            base_url: "https://api.kilo.ai/api/gateway/v1".to_string(),
            models_endpoint: Some("/models".to_string()),
            is_custom: false,
        },
        KnownProvider {
            id: "groq".to_string(),
            name: "Groq".to_string(),
            kind: "openai".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            models_endpoint: Some("/models".to_string()),
            is_custom: false,
        },
        KnownProvider {
            id: "together".to_string(),
            name: "Together AI".to_string(),
            kind: "openai".to_string(),
            base_url: "https://api.together.xyz/v1".to_string(),
            models_endpoint: Some("/models".to_string()),
            is_custom: false,
        },
        KnownProvider {
            id: "deepseek".to_string(),
            name: "DeepSeek".to_string(),
            kind: "openai".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            models_endpoint: Some("/models".to_string()),
            is_custom: false,
        },
        KnownProvider {
            id: "ollama".to_string(),
            name: "Ollama (Local)".to_string(),
            kind: "ollama".to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            models_endpoint: Some("/models".to_string()),
            is_custom: false,
        },
        KnownProvider {
            id: "lmstudio".to_string(),
            name: "LM Studio (Local)".to_string(),
            kind: "openai".to_string(),
            base_url: "http://localhost:1234/v1".to_string(),
            models_endpoint: Some("/models".to_string()),
            is_custom: false,
        },
        KnownProvider {
            id: "custom".to_string(),
            name: "Custom OpenAI-Compatible".to_string(),
            kind: "openai".to_string(),
            base_url: "".to_string(),
            models_endpoint: Some("/models".to_string()),
            is_custom: true,
        },
    ]
}

/// Create a ProviderConfig from a KnownProvider
pub fn create_provider_config(known: &KnownProvider, name: Option<String>) -> ProviderConfig {
    ProviderConfig {
        name: name.unwrap_or_else(|| known.id.clone()),
        display_name: known.name.clone(),
        base_url: known.base_url.clone(),
        api_key: None,
        models_cache: Vec::new(),
        models_cache_updated: None,
        enabled: true,
        kind: known.kind.clone(),
        models_endpoint: known.models_endpoint.clone(),
    }
}

/// Build the full models URL for a provider
pub fn models_url(config: &ProviderConfig) -> Option<String> {
    config.models_endpoint.as_ref().map(|endpoint| {
        let base = config.base_url.trim_end_matches('/');
        let endpoint = endpoint.trim_start_matches('/');
        format!("{}/{}", base, endpoint)
    })
}

/// Build the chat completions URL for a provider
pub fn chat_url(config: &ProviderConfig) -> String {
    let base = config.base_url.trim_end_matches('/');
    match config.kind.as_str() {
        "anthropic" => format!("{}/v1/messages", base),
        "ollama" => format!("{}/api/chat", base),
        _ => format!("{}/chat/completions", base), // OpenAI-compatible
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_provider_config_manager() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("providers.toml");
        let credential_store =
            MultiCredentialStore::new(vec![memnoth_auth::CredentialStore::File(
                memnoth_auth::FileCredentialStore::at_path(temp_dir.path().join("credentials"))
                    .await
                    .unwrap(),
            )]);
        let manager = ProviderConfigManager::with_store(credential_store, config_path);

        let config = ProviderConfig {
            name: "test-provider".to_string(),
            display_name: "Test Provider".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            api_key: None,
            models_cache: vec!["model-1".to_string(), "model-2".to_string()],
            models_cache_updated: Some(Utc::now().timestamp()),
            enabled: true,
            kind: "openai".to_string(),
            models_endpoint: Some("/models".to_string()),
        };

        // Test upsert
        manager.upsert_config(config.clone()).await.unwrap();

        // Test load
        let loaded = manager.get_config("test-provider").unwrap().unwrap();
        assert_eq!(loaded.name, "test-provider");
        assert_eq!(loaded.base_url, "https://api.example.com/v1");
        assert_eq!(loaded.models_cache.len(), 2);

        // Test API key storage
        manager
            .set_api_key("test-provider", "sk-test123".to_string())
            .await
            .unwrap();
        let key = manager.get_api_key("test-provider").await.unwrap();
        assert_eq!(key, Some("sk-test123".to_string()));

        // Test get with key
        let with_key = manager
            .get_config_with_key("test-provider")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(with_key.api_key, Some("sk-test123".to_string()));

        // Test delete
        let deleted = manager.delete_config("test-provider").await.unwrap();
        assert!(deleted);
        assert!(manager.get_config("test-provider").unwrap().is_none());
    }

    #[test]
    fn test_known_providers() {
        let providers = known_providers();
        assert!(!providers.is_empty());

        // Check Anthropic exists
        let anthropic = providers.iter().find(|p| p.id == "anthropic").unwrap();
        assert_eq!(anthropic.kind, "anthropic");
        assert_eq!(anthropic.base_url, "https://api.anthropic.com");

        // Check OpenAI exists
        let openai = providers.iter().find(|p| p.id == "openai").unwrap();
        assert_eq!(openai.kind, "openai");
        assert_eq!(openai.base_url, "https://api.openai.com/v1");

        // Check custom provider
        let custom = providers.iter().find(|p| p.id == "custom").unwrap();
        assert!(custom.is_custom);
    }

    #[test]
    fn test_create_provider_config() {
        let known = &known_providers()[0]; // Anthropic
        let config = create_provider_config(known, Some("my-anthropic".to_string()));
        assert_eq!(config.name, "my-anthropic");
        assert_eq!(config.display_name, "Anthropic");
        assert_eq!(config.kind, "anthropic");
        assert!(config.enabled);
    }

    #[test]
    fn test_urls() {
        let config = ProviderConfig {
            name: "test".to_string(),
            display_name: "Test".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            api_key: None,
            models_cache: vec![],
            models_cache_updated: None,
            enabled: true,
            kind: "openai".to_string(),
            models_endpoint: Some("/models".to_string()),
        };

        assert_eq!(
            models_url(&config),
            Some("https://api.example.com/v1/models".to_string())
        );
        assert_eq!(
            chat_url(&config),
            "https://api.example.com/v1/chat/completions".to_string()
        );

        // Anthropic
        let mut anthro = config.clone();
        anthro.kind = "anthropic".to_string();
        assert_eq!(
            chat_url(&anthro),
            "https://api.example.com/v1/v1/messages".to_string()
        );

        // Ollama
        let mut ollama = config.clone();
        ollama.kind = "ollama".to_string();
        ollama.base_url = "http://localhost:11434".to_string();
        assert_eq!(
            chat_url(&ollama),
            "http://localhost:11434/api/chat".to_string()
        );
    }
}
