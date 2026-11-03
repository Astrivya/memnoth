//! Configuration commands: get, set, list, edit.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

use crate::context::Config;
use crate::output::{Outcome, human_count};

/// Known configuration keys with their descriptions and value types.
#[derive(Debug, Clone)]
struct ConfigKey {
    key: &'static str,
    description: &'static str,
    value_type: &'static str,
    env_var: Option<&'static str>,
}

const CONFIG_KEYS: &[ConfigKey] = &[
    ConfigKey {
        key: "mcp_url",
        description: "MCP server endpoint URL",
        value_type: "string (URL)",
        env_var: None,
    },
    ConfigKey {
        key: "profile",
        description: "Default tool profile (micro, memory, coord, code, full, agent)",
        value_type: "string",
        env_var: None,
    },
    ConfigKey {
        key: "vault",
        description: "Default vault file path",
        value_type: "string (path)",
        env_var: Some("MEMNOTH_VAULT"),
    },
    ConfigKey {
        key: "provider.kind",
        description: "Provider kind (anthropic, openai)",
        value_type: "string",
        env_var: None,
    },
    ConfigKey {
        key: "provider.model",
        description: "Model ID to use",
        value_type: "string",
        env_var: None,
    },
    ConfigKey {
        key: "provider.base_url",
        description: "Provider base URL (for gateways/local servers)",
        value_type: "string (URL)",
        env_var: None,
    },
    ConfigKey {
        key: "provider.max_tokens",
        description: "Maximum tokens per response",
        value_type: "integer",
        env_var: None,
    },
    ConfigKey {
        key: "provider.input_per_mtok",
        description: "Input cost per million tokens (USD)",
        value_type: "float",
        env_var: None,
    },
    ConfigKey {
        key: "provider.output_per_mtok",
        description: "Output cost per million tokens (USD)",
        value_type: "float",
        env_var: None,
    },
];

/// Validate that a key is known.
fn validate_key(key: &str) -> Result<&ConfigKey> {
    CONFIG_KEYS.iter().find(|k| k.key == key).ok_or_else(|| {
        bail!("unknown configuration key: {key}\nRun `mn config list` to see available keys")
    })
}

/// Get the effective value of a config key, considering env vars and defaults.
fn get_effective_value(config: &Config, key: &str) -> (Value, &'static str) {
    match key {
        "mcp_url" => (
            json!(
                config
                    .mcp_url
                    .clone()
                    .unwrap_or_else(|| crate::DEFAULT_MCP_URL.to_string())
            ),
            if config.mcp_url.is_some() {
                "config"
            } else {
                "default"
            },
        ),
        "profile" => (
            json!(
                config
                    .profile
                    .clone()
                    .unwrap_or_else(|| crate::DEFAULT_PROFILE.to_string())
            ),
            if config.profile.is_some() {
                "config"
            } else {
                "default"
            },
        ),
        "vault" => {
            if let Ok(v) = std::env::var("MEMNOTH_VAULT") {
                (json!(v), "env")
            } else if let Some(v) = config.vault.clone() {
                (json!(v), "config")
            } else {
                let home = crate::context::memnoth_home().unwrap_or_default();
                (
                    json!(home.join("vault.db").to_string_lossy().to_string()),
                    "default",
                )
            }
        }
        "provider.kind" => {
            if let Some(ref p) = config.provider {
                if let Some(ref v) = p.kind {
                    return (json!(v.clone()), "config");
                }
            }
            (json!(Value::Null), "default")
        }
        "provider.model" => {
            if let Some(ref p) = config.provider {
                if let Some(ref v) = p.model {
                    return (json!(v.clone()), "config");
                }
            }
            (json!(Value::Null), "default")
        }
        "provider.base_url" => {
            if let Some(ref p) = config.provider {
                if let Some(ref v) = p.base_url {
                    return (json!(v.clone()), "config");
                }
            }
            (json!(Value::Null), "default")
        }
        "provider.max_tokens" => {
            if let Some(ref p) = config.provider {
                if let Some(v) = p.max_tokens {
                    return (json!(v), "config");
                }
            }
            (json!(Value::Null), "default")
        }
        "provider.input_per_mtok" => {
            if let Some(ref p) = config.provider {
                if let Some(v) = p.input_per_mtok {
                    return (json!(v), "config");
                }
            }
            (json!(Value::Null), "default")
        }
        "provider.output_per_mtok" => {
            if let Some(ref p) = config.provider {
                if let Some(v) = p.output_per_mtok {
                    return (json!(v), "config");
                }
            }
            (json!(Value::Null), "default")
        }
        _ => (json!(Value::Null), "unknown"),
    }
}

/// Get the value explicitly set in config file (not env or default).
fn get_config_file_value(config: &Config, key: &str) -> Option<Value> {
    match key {
        "mcp_url" => config.mcp_url.clone().map(|v| json!(v)),
        "profile" => config.profile.clone().map(|v| json!(v)),
        "vault" => config.vault.clone().map(|v| json!(v)),
        "provider.kind" => config.provider.as_ref()?.kind.clone().map(|v| json!(v)),
        "provider.model" => config.provider.as_ref()?.model.clone().map(|v| json!(v)),
        "provider.base_url" => config.provider.as_ref()?.base_url.clone().map(|v| json!(v)),
        "provider.max_tokens" => config.provider.as_ref()?.max_tokens.map(|v| json!(v)),
        "provider.input_per_mtok" => config.provider.as_ref()?.input_per_mtok.map(|v| json!(v)),
        "provider.output_per_mtok" => config.provider.as_ref()?.output_per_mtok.map(|v| json!(v)),
        _ => None,
    }
}

/// `mn config get <key>`
pub fn get(ctx: &crate::context::AppContext, args: &crate::cli::ConfigGetArgs) -> Result<Outcome> {
    let key_info = validate_key(&args.key)?;

    let (value, source) = get_effective_value(&ctx.config, &args.key);
    let config_file_value = get_config_file_value(&ctx.config, &args.key);

    let data = json!({
        "key": args.key,
        "value": value,
        "source": source,
        "description": key_info.description,
        "type": key_info.value_type,
        "config_file_value": config_file_value,
    });

    let text = if args.json {
        serde_json::to_string_pretty(&data)?
    } else {
        format!(
            "{} = {}\n  source: {}\n  type: {}\n  description: {}",
            args.key,
            serde_json::to_string(&value)?,
            source,
            key_info.value_type,
            key_info.description
        )
    };

    Ok(Outcome { text, data })
}

/// `mn config set <key> <value>`
pub fn set(ctx: &crate::context::AppContext, args: &crate::cli::ConfigSetArgs) -> Result<Outcome> {
    let key_info = validate_key(&args.key)?;

    // Parse the value based on the expected type
    let parsed_value: Value = if key_info.key.contains("max_tokens") {
        json!(
            args.value
                .parse::<u32>()
                .context("max_tokens must be an integer")?
        )
    } else if key_info.key.contains("per_mtok") {
        json!(
            args.value
                .parse::<f64>()
                .context("per_mtok values must be a number")?
        )
    } else {
        json!(args.value)
    };

    // Load current config
    let mut config = Config::load(&ctx.config_path)?;

    // Set the value
    match args.key.as_str() {
        "mcp_url" => config.mcp_url = Some(args.value.clone()),
        "profile" => {
            // Validate profile
            let valid_profiles = ["micro", "memory", "coord", "code", "full", "agent"];
            if !valid_profiles.contains(&args.value.as_str()) {
                bail!(
                    "invalid profile '{}'; must be one of: {}",
                    args.value,
                    valid_profiles.join(", ")
                );
            }
            config.profile = Some(args.value.clone());
        }
        "vault" => config.vault = Some(args.value.clone()),
        "provider.kind" => {
            let valid_kinds = ["anthropic", "openai"];
            if !valid_kinds.contains(&args.value.as_str()) {
                bail!(
                    "invalid provider kind '{}'; must be one of: {}",
                    args.value,
                    valid_kinds.join(", ")
                );
            }
            config.provider.get_or_insert_default().kind = Some(args.value.clone());
        }
        "provider.model" => {
            config.provider.get_or_insert_default().model = Some(args.value.clone())
        }
        "provider.base_url" => {
            config.provider.get_or_insert_default().base_url = Some(args.value.clone())
        }
        "provider.max_tokens" => {
            let val = args
                .value
                .parse::<u32>()
                .context("max_tokens must be an integer")?;
            config.provider.get_or_insert_default().max_tokens = Some(val);
        }
        "provider.input_per_mtok" => {
            let val = args
                .value
                .parse::<f64>()
                .context("input_per_mtok must be a number")?;
            config.provider.get_or_insert_default().input_per_mtok = Some(val);
        }
        "provider.output_per_mtok" => {
            let val = args
                .value
                .parse::<f64>()
                .context("output_per_mtok must be a number")?;
            config.provider.get_or_insert_default().output_per_mtok = Some(val);
        }
        _ => bail!("unknown configuration key: {}", args.key),
    }

    // Save config
    config
        .save(&ctx.config_path)
        .context("failed to save config")?;

    let data = json!({
        "key": args.key,
        "value": parsed_value,
        "config_path": ctx.config_path.to_string_lossy().to_string(),
    });

    let text = if args.json {
        serde_json::to_string_pretty(&data)?
    } else {
        format!(
            "Set {} = {} in {}",
            args.key,
            serde_json::to_string(&parsed_value)?,
            ctx.config_path.display()
        )
    };

    Ok(Outcome { text, data })
}

/// `mn config list [--json]`
pub fn list(
    ctx: &crate::context::AppContext,
    args: &crate::cli::ConfigListArgs,
) -> Result<Outcome> {
    let mut entries = Vec::new();

    for key_info in CONFIG_KEYS {
        let (value, source) = get_effective_value(&ctx.config, key_info.key);
        let config_file_value = get_config_file_value(&ctx.config, key_info.key);

        entries.push(json!({
            "key": key_info.key,
            "value": value,
            "source": source,
            "description": key_info.description,
            "type": key_info.value_type,
            "config_file_value": config_file_value,
        }));
    }

    let data = json!({
        "config_path": ctx.config_path.to_string_lossy().to_string(),
        "entries": entries,
    });

    let text = if args.json {
        serde_json::to_string_pretty(&data)?
    } else {
        let mut lines = vec![
            format!("Configuration file: {}", ctx.config_path.display()),
            "".to_string(),
        ];
        for entry in &entries {
            let key = entry["key"].as_str().unwrap();
            let value = &entry["value"];
            let source = entry["source"].as_str().unwrap();
            let desc = entry["description"].as_str().unwrap();
            let value_str = if value.is_null() {
                "(not set)".to_string()
            } else {
                serde_json::to_string(value).unwrap()
            };
            lines.push(format!("{} = {}  [{}]", key, value_str, source));
            lines.push(format!("  {}", desc));
            lines.push("".to_string());
        }
        lines.join("\n")
    };

    Ok(Outcome { text, data })
}

/// `mn config edit [--json]`
pub fn edit(
    ctx: &crate::context::AppContext,
    args: &crate::cli::ConfigEditArgs,
) -> Result<Outcome> {
    // Ensure config file exists
    if !ctx.config_path.exists() {
        Config::default()
            .save(&ctx.config_path)
            .context("failed to create default config")?;
    }

    if args.json {
        let data = json!({
            "config_path": ctx.config_path.to_string_lossy().to_string(),
        });
        return Ok(Outcome {
            text: serde_json::to_string_pretty(&data)?,
            data,
        });
    }

    // Get editor from environment
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| {
            if cfg!(windows) {
                "notepad".to_string()
            } else {
                "vi".to_string()
            }
        });

    let status = Command::new(&editor)
        .arg(&ctx.config_path)
        .status()
        .with_context(|| format!("failed to launch editor '{}'", editor))?;

    if !status.success() {
        bail!("editor exited with status: {}", status);
    }

    // Reload and validate config
    let config = Config::load(&ctx.config_path).context("failed to reload config after edit")?;

    let data = json!({
        "config_path": ctx.config_path.to_string_lossy().to_string(),
        "reloaded": true,
    });

    let text = format!(
        "Edited and reloaded config from {}",
        ctx.config_path.display()
    );

    Ok(Outcome { text, data })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Config;

    #[test]
    fn test_validate_key() {
        assert!(validate_key("mcp_url").is_ok());
        assert!(validate_key("provider.model").is_ok());
        assert!(validate_key("unknown_key").is_err());
    }

    #[test]
    fn test_get_effective_value() {
        let config = Config {
            mcp_url: Some("http://custom:8788/mcp".into()),
            profile: Some("code".into()),
            vault: None,
            provider: None,
        };

        let (value, source) = get_effective_value(&config, "mcp_url");
        assert_eq!(value, "http://custom:8788/mcp");
        assert_eq!(source, "config");

        let (value, source) = get_effective_value(&config, "profile");
        assert_eq!(value, "code");
        assert_eq!(source, "config");

        let config = Config::default();
        let (value, source) = get_effective_value(&config, "mcp_url");
        assert_eq!(value, crate::DEFAULT_MCP_URL);
        assert_eq!(source, "default");
    }

    #[test]
    fn test_get_config_file_value() {
        let config = Config {
            mcp_url: Some("http://custom:8788/mcp".into()),
            profile: Some("code".into()),
            vault: Some("/custom/vault.db".into()),
            provider: Some(crate::provider::ProviderConfig {
                kind: Some("anthropic".into()),
                model: Some("claude-3".into()),
                base_url: None,
                max_tokens: Some(2000),
                input_per_mtok: Some(3.0),
                output_per_mtok: Some(15.0),
            }),
        };

        assert_eq!(
            get_config_file_value(&config, "mcp_url"),
            Some(json!("http://custom:8788/mcp"))
        );
        assert_eq!(
            get_config_file_value(&config, "provider.kind"),
            Some(json!("anthropic"))
        );
        assert_eq!(
            get_config_file_value(&config, "provider.max_tokens"),
            Some(json!(2000))
        );
        assert_eq!(get_config_file_value(&config, "unknown"), None);
    }
}
