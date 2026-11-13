//! Known model provider registry for memnoth CLI.
//!
//! This crate provides a static registry of all major model providers with their
//! configuration details. Used by the `connect` and `models` commands for provider
//! selection, authentication, and model discovery.

use serde::{Deserialize, Serialize};

/// Authentication header format for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthHeaderFormat {
    /// `Authorization: Bearer <token>`
    Bearer,
    /// `Authorization: Token <token>`
    Token,
    /// `x-api-key: <token>`
    XApiKey,
    /// `api-key: <token>`
    ApiKey,
    /// Custom header name (stored as the header name itself)
    Custom(&'static str),
}

impl AuthHeaderFormat {
    /// Returns the header name for this auth format.
    pub fn header_name(&self) -> &'static str {
        match self {
            AuthHeaderFormat::Bearer => "Authorization",
            AuthHeaderFormat::Token => "Authorization",
            AuthHeaderFormat::XApiKey => "x-api-key",
            AuthHeaderFormat::ApiKey => "api-key",
            AuthHeaderFormat::Custom(name) => name,
        }
    }

    /// Returns the header value prefix (e.g., "Bearer " for Bearer auth).
    pub fn value_prefix(&self) -> &'static str {
        match self {
            AuthHeaderFormat::Bearer => "Bearer ",
            AuthHeaderFormat::Token => "Token ",
            AuthHeaderFormat::XApiKey => "",
            AuthHeaderFormat::ApiKey => "",
            AuthHeaderFormat::Custom(_) => "",
        }
    }
}

/// Provider capability flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Supports chat/completion endpoint
    Chat,
    /// Supports embeddings endpoint
    Embeddings,
    /// Supports image generation
    ImageGeneration,
    /// Supports audio transcription (speech-to-text)
    AudioTranscription,
    /// Supports audio generation (text-to-speech)
    AudioGeneration,
    /// Supports vision/multimodal inputs
    Vision,
    /// Supports function/tool calling
    ToolCalling,
    /// Supports streaming responses
    Streaming,
    /// Supports fine-tuning
    FineTuning,
    /// Provider is an OpenAI-compatible gateway
    OpenAICompatible,
    /// Provider is a proxy/gateway aggregator
    Gateway,
}

/// Known model provider configuration.
///
/// This struct is serialized for introspection but not deserialized —
/// the registry is a compile-time constant.
#[derive(Debug, Clone, Serialize)]
pub struct Provider {
    /// Internal provider name (lowercase, no spaces)
    pub name: &'static str,
    /// Human-readable display name
    pub display_name: &'static str,
    /// Base URL for API requests
    pub base_url: &'static str,
    /// Models list endpoint path (relative to base_url)
    pub models_endpoint: &'static str,
    /// Authentication header format
    pub auth_header_format: AuthHeaderFormat,
    /// Capabilities supported by this provider
    #[serde(serialize_with = "serialize_capabilities")]
    pub capabilities: &'static [Capability],
    /// Default/recommended models for this provider
    #[serde(serialize_with = "serialize_default_models")]
    pub default_models: &'static [&'static str],
    /// URL to pricing page
    pub pricing_url: &'static str,
    /// Whether this provider requires an API key (some local providers don't)
    pub requires_api_key: bool,
    /// Environment variable name for the API key
    pub api_key_env_var: &'static str,
    /// Default model context window in tokens
    pub context_window: u32,
}

fn serialize_capabilities<S>(caps: &&'static [Capability], ser: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let slice: &[Capability] = caps;
    slice.serialize(ser)
}

fn serialize_default_models<S>(models: &&'static [&'static str], ser: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let slice: &[&str] = models;
    slice.serialize(ser)
}

impl Provider {
    /// Returns the full models endpoint URL.
    pub fn models_url(&self) -> String {
        format!(
            "{}{}",
            self.base_url.trim_end_matches('/'),
            self.models_endpoint
        )
    }

    /// Checks if the provider supports a given capability.
    pub fn has_capability(&self, cap: Capability) -> bool {
        self.capabilities.contains(&cap)
    }

    /// Returns the Authorization header value for a given token.
    pub fn auth_header_value(&self, token: &str) -> String {
        format!("{}{}", self.auth_header_format.value_prefix(), token)
    }
}

/// All known model providers.
///
/// This is the single source of truth for provider configuration used by
/// the `connect` command (provider selection, auth setup) and the `models`
/// command (model discovery, filtering).
pub const KNOWN_PROVIDERS: &[Provider] = &[
    Provider {
        name: "openai",
        display_name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::Bearer,
        capabilities: &[
            Capability::Chat,
            Capability::Embeddings,
            Capability::ImageGeneration,
            Capability::AudioTranscription,
            Capability::AudioGeneration,
            Capability::Vision,
            Capability::ToolCalling,
            Capability::Streaming,
            Capability::FineTuning,
        ],
        default_models: &[
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4-turbo",
            "gpt-3.5-turbo",
            "o1-preview",
            "o1-mini",
        ],
        pricing_url: "https://openai.com/pricing",
        requires_api_key: true,
        api_key_env_var: "OPENAI_API_KEY",
        context_window: 128_000,
    },
    Provider {
        name: "anthropic",
        display_name: "Anthropic",
        base_url: "https://api.anthropic.com/v1",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::Bearer,
        capabilities: &[
            Capability::Chat,
            Capability::Vision,
            Capability::ToolCalling,
            Capability::Streaming,
        ],
        default_models: &[
            "claude-3-5-sonnet-20241022",
            "claude-3-5-haiku-20241022",
            "claude-3-opus-20240229",
            "claude-3-sonnet-20240229",
            "claude-3-haiku-20240307",
        ],
        pricing_url: "https://www.anthropic.com/pricing",
        requires_api_key: true,
        api_key_env_var: "ANTHROPIC_API_KEY",
        context_window: 200_000,
    },
    Provider {
        name: "deepseek",
        display_name: "DeepSeek",
        base_url: "https://api.deepseek.com/v1",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::Bearer,
        capabilities: &[
            Capability::Chat,
            Capability::ToolCalling,
            Capability::Streaming,
        ],
        default_models: &["deepseek-chat", "deepseek-coder"],
        pricing_url: "https://www.deepseek.com/pricing",
        requires_api_key: true,
        api_key_env_var: "DEEPSEEK_API_KEY",
        context_window: 64_000,
    },
    Provider {
        name: "kilo",
        display_name: "Kilo Gateway",
        base_url: "https://ai-gateway.kilo.ai/api/v1",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::Bearer,
        capabilities: &[
            Capability::Chat,
            Capability::Embeddings,
            Capability::ImageGeneration,
            Capability::Vision,
            Capability::ToolCalling,
            Capability::Streaming,
            Capability::Gateway,
            Capability::OpenAICompatible,
        ],
        default_models: &[
            "inclusionai/ling-3.0-flash-vl:free",
            "google/gemini-flash-1.5",
            "anthropic/claude-sonnet-4.5",
            "anthropic/claude-3.5-sonnet",
            "openai/gpt-4o",
            "meta-llama/llama-3.1-405b-instruct",
        ],
        pricing_url: "https://kilo.ai/docs/gateway",
        requires_api_key: true,
        api_key_env_var: "KILO_API_KEY",
        context_window: 128_000,
    },
    Provider {
        name: "google",
        display_name: "Google (Vertex AI / AI Studio)",
        base_url: "https://generativelanguage.googleapis.com/v1beta",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::ApiKey,
        capabilities: &[
            Capability::Chat,
            Capability::Embeddings,
            Capability::Vision,
            Capability::ToolCalling,
            Capability::Streaming,
            Capability::AudioTranscription,
            Capability::AudioGeneration,
        ],
        default_models: &[
            "gemini-1.5-pro",
            "gemini-1.5-flash",
            "gemini-1.0-pro",
            "gemini-2.0-flash-exp",
        ],
        pricing_url: "https://ai.google.dev/pricing",
        requires_api_key: true,
        api_key_env_var: "GOOGLE_API_KEY",
        context_window: 1_000_000,
    },
    Provider {
        name: "cohere",
        display_name: "Cohere",
        base_url: "https://api.cohere.ai/v1",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::Bearer,
        capabilities: &[
            Capability::Chat,
            Capability::Embeddings,
            Capability::ToolCalling,
            Capability::Streaming,
            Capability::FineTuning,
        ],
        default_models: &[
            "command-r-plus",
            "command-r",
            "command-light",
            "embed-english-v3.0",
            "embed-multilingual-v3.0",
        ],
        pricing_url: "https://cohere.com/pricing",
        requires_api_key: true,
        api_key_env_var: "COHERE_API_KEY",
        context_window: 128_000,
    },
    Provider {
        name: "mistral",
        display_name: "Mistral AI",
        base_url: "https://api.mistral.ai/v1",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::Bearer,
        capabilities: &[
            Capability::Chat,
            Capability::Embeddings,
            Capability::ToolCalling,
            Capability::Streaming,
            Capability::FineTuning,
        ],
        default_models: &[
            "mistral-large-latest",
            "mistral-small-latest",
            "mistral-embed",
            "pixtral-12b",
            "codestral-latest",
        ],
        pricing_url: "https://mistral.ai/pricing",
        requires_api_key: true,
        api_key_env_var: "MISTRAL_API_KEY",
        context_window: 128_000,
    },
    Provider {
        name: "groq",
        display_name: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::Bearer,
        capabilities: &[
            Capability::Chat,
            Capability::ToolCalling,
            Capability::Streaming,
            Capability::OpenAICompatible,
        ],
        default_models: &[
            "llama-3.3-70b-versatile",
            "llama-3.1-70b-versatile",
            "llama-3.1-8b-instant",
            "mixtral-8x7b-32768",
            "gemma2-9b-it",
        ],
        pricing_url: "https://groq.com/pricing",
        requires_api_key: true,
        api_key_env_var: "GROQ_API_KEY",
        context_window: 131_072,
    },
    Provider {
        name: "together",
        display_name: "Together AI",
        base_url: "https://api.together.xyz/v1",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::Bearer,
        capabilities: &[
            Capability::Chat,
            Capability::Embeddings,
            Capability::ImageGeneration,
            Capability::ToolCalling,
            Capability::Streaming,
            Capability::OpenAICompatible,
            Capability::FineTuning,
        ],
        default_models: &[
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            "meta-llama/Llama-3.1-70B-Instruct-Turbo",
            "meta-llama/Llama-3.1-8B-Instruct-Turbo",
            "mistralai/Mixtral-8x7B-Instruct-v0.1",
            "Qwen/Qwen2.5-72B-Instruct-Turbo",
        ],
        pricing_url: "https://www.together.ai/pricing",
        requires_api_key: true,
        api_key_env_var: "TOGETHER_API_KEY",
        context_window: 128_000,
    },
    Provider {
        name: "perplexity",
        display_name: "Perplexity",
        base_url: "https://api.perplexity.ai",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::Bearer,
        capabilities: &[
            Capability::Chat,
            Capability::Streaming,
            Capability::OpenAICompatible,
        ],
        default_models: &[
            "llama-3.1-sonar-large-128k-online",
            "llama-3.1-sonar-small-128k-online",
            "llama-3.1-sonar-large-128k-chat",
            "llama-3.1-sonar-small-128k-chat",
        ],
        pricing_url: "https://www.perplexity.ai/pricing",
        requires_api_key: true,
        api_key_env_var: "PERPLEXITY_API_KEY",
        context_window: 128_000,
    },
    Provider {
        name: "openrouter",
        display_name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::Bearer,
        capabilities: &[
            Capability::Chat,
            Capability::Embeddings,
            Capability::ImageGeneration,
            Capability::Vision,
            Capability::ToolCalling,
            Capability::Streaming,
            Capability::Gateway,
            Capability::OpenAICompatible,
        ],
        default_models: &[
            "inclusionai/ling-3.0-flash-vl:free",
            "google/gemini-flash-1.5",
            "anthropic/claude-3.5-sonnet",
            "openai/gpt-4o",
            "meta-llama/llama-3.1-405b-instruct",
            "qwen/qwen-2.5-72b-instruct",
        ],
        pricing_url: "https://openrouter.ai/pricing",
        requires_api_key: true,
        api_key_env_var: "OPENROUTER_API_KEY",
        context_window: 128_000,
    },
    Provider {
        name: "fireworks",
        display_name: "Fireworks AI",
        base_url: "https://api.fireworks.ai/inference/v1",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::Bearer,
        capabilities: &[
            Capability::Chat,
            Capability::Embeddings,
            Capability::ImageGeneration,
            Capability::ToolCalling,
            Capability::Streaming,
            Capability::OpenAICompatible,
            Capability::FineTuning,
        ],
        default_models: &[
            "accounts/fireworks/models/llama-v3p3-70b-instruct",
            "accounts/fireworks/models/llama-v3p1-70b-instruct",
            "accounts/fireworks/models/mixtral-8x7b-instruct",
            "accounts/fireworks/models/firefunction-v2",
        ],
        pricing_url: "https://fireworks.ai/pricing",
        requires_api_key: true,
        api_key_env_var: "FIREWORKS_API_KEY",
        context_window: 128_000,
    },
    Provider {
        name: "replicate",
        display_name: "Replicate",
        base_url: "https://api.replicate.com/v1",
        models_endpoint: "/models",
        auth_header_format: AuthHeaderFormat::Bearer,
        capabilities: &[
            Capability::Chat,
            Capability::ImageGeneration,
            Capability::AudioTranscription,
            Capability::AudioGeneration,
            Capability::Gateway,
        ],
        default_models: &[
            "meta/llama-3.1-70b-instruct",
            "meta/llama-3.1-8b-instruct",
            "mistralai/mixtral-8x7b-instruct-v0.1",
            "black-forest-labs/flux-schnell",
            "stability-ai/sdxl",
        ],
        pricing_url: "https://replicate.com/pricing",
        requires_api_key: true,
        api_key_env_var: "REPLICATE_API_KEY",
        context_window: 128_000,
    },
];

/// Returns a provider by its internal name (case-insensitive).
pub fn provider_by_name(name: &str) -> Option<&'static Provider> {
    KNOWN_PROVIDERS
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
}
/// Returns the default context window for a model, if it is listed in any
/// provider's default model list.
pub fn context_window_for(model: &str) -> Option<u32> {
    KNOWN_PROVIDERS
        .iter()
        .find(|p| p.default_models.contains(&model))
        .map(|p| p.context_window)
}

/// Returns all providers that support a given capability.
pub fn providers_with_capability(cap: Capability) -> Vec<&'static Provider> {
    KNOWN_PROVIDERS
        .iter()
        .filter(|p| p.has_capability(cap))
        .collect()
}

/// Returns all provider names as a slice of strings.
pub fn provider_names() -> Vec<&'static str> {
    KNOWN_PROVIDERS.iter().map(|p| p.name).collect()
}

/// Returns all provider display names as a slice of strings.
pub fn provider_display_names() -> Vec<&'static str> {
    KNOWN_PROVIDERS.iter().map(|p| p.display_name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_providers_not_empty() {
        assert!(!KNOWN_PROVIDERS.is_empty());
    }

    #[test]
    fn test_provider_by_name_found() {
        let provider = provider_by_name("openai").expect("openai should exist");
        assert_eq!(provider.name, "openai");
        assert_eq!(provider.display_name, "OpenAI");
    }

    #[test]
    fn test_provider_by_name_case_insensitive() {
        let provider = provider_by_name("OPENAI").expect("openai should exist");
        assert_eq!(provider.name, "openai");
    }

    #[test]
    fn test_provider_by_name_not_found() {
        assert!(provider_by_name("nonexistent").is_none());
    }

    #[test]
    fn test_all_providers_have_unique_names() {
        let mut names = std::collections::HashSet::new();
        for p in KNOWN_PROVIDERS {
            assert!(names.insert(p.name), "duplicate provider name: {}", p.name);
        }
    }

    #[test]
    fn test_all_providers_have_base_url() {
        for p in KNOWN_PROVIDERS {
            assert!(
                !p.base_url.is_empty(),
                "provider {} has empty base_url",
                p.name
            );
            assert!(
                p.base_url.starts_with("https://"),
                "provider {} base_url must be https",
                p.name
            );
        }
    }

    #[test]
    fn test_all_providers_have_models_endpoint() {
        for p in KNOWN_PROVIDERS {
            assert!(
                !p.models_endpoint.is_empty(),
                "provider {} has empty models_endpoint",
                p.name
            );
            assert!(
                p.models_endpoint.starts_with('/'),
                "provider {} models_endpoint must start with /",
                p.name
            );
        }
    }

    #[test]
    fn test_all_providers_have_default_models() {
        for p in KNOWN_PROVIDERS {
            assert!(
                !p.default_models.is_empty(),
                "provider {} has no default models",
                p.name
            );
        }
    }

    #[test]
    fn test_all_providers_have_pricing_url() {
        for p in KNOWN_PROVIDERS {
            assert!(
                !p.pricing_url.is_empty(),
                "provider {} has empty pricing_url",
                p.name
            );
            assert!(
                p.pricing_url.starts_with("https://"),
                "provider {} pricing_url must be https",
                p.name
            );
        }
    }

    #[test]
    fn test_all_providers_have_api_key_env_var() {
        for p in KNOWN_PROVIDERS {
            if p.requires_api_key {
                assert!(
                    !p.api_key_env_var.is_empty(),
                    "provider {} requires API key but has no env var",
                    p.name
                );
            }
        }
    }

    #[test]
    fn test_provider_models_url() {
        let provider = provider_by_name("openai").unwrap();
        assert_eq!(provider.models_url(), "https://api.openai.com/v1/models");
    }

    #[test]
    fn test_provider_has_capability() {
        let provider = provider_by_name("openai").unwrap();
        assert!(provider.has_capability(Capability::Chat));
        assert!(provider.has_capability(Capability::Vision));
        assert!(!provider.has_capability(Capability::Gateway));
    }

    #[test]
    fn test_providers_with_capability() {
        let chat_providers = providers_with_capability(Capability::Chat);
        assert!(!chat_providers.is_empty());
        for p in chat_providers {
            assert!(p.has_capability(Capability::Chat));
        }
    }

    #[test]
    fn test_auth_header_format() {
        let provider = provider_by_name("openai").unwrap();
        assert_eq!(provider.auth_header_format.header_name(), "Authorization");
        assert_eq!(provider.auth_header_format.value_prefix(), "Bearer ");
        assert_eq!(
            provider.auth_header_value("test-token"),
            "Bearer test-token"
        );
    }

    #[test]
    fn test_provider_display_names() {
        let names = provider_display_names();
        assert!(names.contains(&"OpenAI"));
        assert!(names.contains(&"Anthropic"));
        assert!(names.contains(&"DeepSeek"));
    }

    #[test]
    fn test_expected_provider_count() {
        // We expect exactly 13 providers as specified
        assert_eq!(KNOWN_PROVIDERS.len(), 13);
    }

    #[test]
    fn test_serialize_provider() {
        let provider = provider_by_name("openai").unwrap();
        let json = serde_json::to_string(provider).expect("should serialize");
        assert!(json.contains("openai"));
        assert!(json.contains("OpenAI"));
        assert!(json.contains("gpt-4o"));
    }
}
