use anyhow::{Context, Result};
use serde_json::json;
use std::collections::HashSet;
use std::fs;

use crate::AppContext;
use crate::context::Config;
use crate::output::Outcome;
use memnoth_auth::get_credential_sync;
use memnoth_cloud_client::CloudClient;

/// Execute `mn models` or `mn model`.
pub fn run(ctx: &AppContext, args: &crate::cli::ModelsArgs) -> Result<Outcome> {
    match &args.action {
        crate::cli::ModelsCommand::List(args) => models_list(ctx, args),
        crate::cli::ModelsCommand::Use(args) => model_use(ctx, args),
    }
}

/// `mn models` — list available models from the cloud.
fn models_list(ctx: &AppContext, args: &crate::cli::ModelsListArgs) -> Result<Outcome> {
    let client = CloudClient::new()?;
    let list = client.models()?;

    // Load config to check for connected providers
    let config_path = ctx.config_path()?;
    let config: Config = if config_path.exists() {
        let content = fs::read_to_string(&config_path)?;
        toml::from_str(&content).context("Failed to parse config.toml")?
    } else {
        Config::default()
    };

    // Determine which providers are "connected" (have API keys configured)
    let connected_providers = get_connected_providers(&config);

    // Filter models
    let mut models: Vec<_> = list.data.into_iter().collect();

    // Filter by provider name if specified
    if let Some(provider_filter) = &args.provider {
        models.retain(|m| {
            m.provider.eq_ignore_ascii_case(provider_filter)
                || m.id.to_lowercase().contains(&provider_filter.to_lowercase())
        });
    }

    // Filter by connected providers if --connected flag is set
    if args.connected {
        models.retain(|m| connected_providers.contains(&m.provider.to_lowercase()));
    }

    // Handle compare mode
    if let Some(compare) = &args.compare {
        let ids: Vec<&str> = compare.split(',').map(|s| s.trim()).collect();
        if ids.len() != 2 {
            anyhow::bail!("--compare requires exactly two model IDs separated by comma");
        }
        return render_compare(&models, ids[0], ids[1], args.json);
    }

    // Render based on format
    let text = match args.format {
        crate::cli::ModelsFormat::Table => render_models_table(&models),
        crate::cli::ModelsFormat::Wide => render_models_wide(&models),
        crate::cli::ModelsFormat::Json => {
            // For JSON format, we'll output full metadata via the data field
            // and keep text minimal
            "Use --json for full metadata output".to_string()
        }
    };

    // Build JSON data with full metadata
    let data = json!({
        "models": models.iter().map(|m| json!({
            "id": m.id,
            "name": m.name,
            "provider": m.provider,
            "description": m.description,
        })).collect::<Vec<_>>(),
        "format": format!("{:?}", args.format).to_lowercase(),
        "connected_providers": connected_providers,
    });

    Ok(Outcome::new("models.list", text, data))
}

/// Get providers that have API keys configured (are "connected").
fn get_connected_providers(config: &Config) -> HashSet<String> {
    let mut providers = HashSet::new();

    // Check config for provider settings
    if let Some(provider_config) = &config.provider {
        if provider_config.kind.is_some() {
            providers.insert(provider_config.kind.as_ref().unwrap().to_lowercase());
        }
    }

    // Check for API keys in environment/credential store
    // Check MEMNOTH_API_KEY
    if std::env::var("MEMNOTH_API_KEY").is_ok() {
        // Try to determine provider from config or default to "memnoth"
        if let Some(provider_config) = &config.provider {
            if let Some(kind) = &provider_config.kind {
                providers.insert(kind.to_lowercase());
            } else {
                providers.insert("memnoth".to_string());
            }
        } else {
            providers.insert("memnoth".to_string());
        }
    }

    // Check credential store for stored keys
    if let Ok(Some(key)) = get_credential_sync("memnoth") {
        if !key.is_empty() {
            if let Some(provider_config) = &config.provider {
                if let Some(kind) = &provider_config.kind {
                    providers.insert(kind.to_lowercase());
                } else {
                    providers.insert("memnoth".to_string());
                }
            } else {
                providers.insert("memnoth".to_string());
            }
        }
    }

    // Also check for known provider-specific env vars
    for (key, _) in std::env::vars() {
        let key_lower = key.to_lowercase();
        if key_lower.ends_with("_api_key") || key_lower.ends_with("_apikey") {
            let provider = key_lower
                .strip_suffix("_api_key")
                .or_else(|| key_lower.strip_suffix("_apikey"))
                .unwrap_or("unknown");
            providers.insert(provider.to_string());
        }
    }

    providers
}

/// Render models in wide format (all columns).
fn render_models_wide(models: &[memnoth_cloud_client::ModelEntry]) -> String {
    if models.is_empty() {
        return "No models available.".to_string();
    }

    let mut rows = Vec::new();
    rows.push(
        "ID                              PROVIDER        NAME                    CONTEXT   MAX OUT  PRICING (IN/OUT)      CAPABILITIES".to_string(),
    );
    rows.push(
        "────────────────────────────────  ──────────────  ──────────────────────  ───────  ──────  ───────────────────  ──────────────────────"
            .to_string(),
    );

    for m in models {
        let (provider, context, pricing, caps) = parse_model_info(&m.id, &m.description);

        // Extract pricing from description more thoroughly
        let pricing_detail = extract_pricing_detail(&m.description);

        rows.push(format!(
            "{:<34} {:<14} {:<22} {:>7}  {:>6}  {:<19}  {}",
            truncate(&m.id, 32),
            truncate(&provider, 12),
            truncate(&m.name, 20),
            context,
            format!("{}", m.max_output_tokens),
            pricing_detail,
            truncate(&caps, 50)
        ));
    }

    rows.join("\n")
}

/// Extract detailed pricing from description.
fn extract_pricing_detail(desc: &str) -> String {
    let desc_lower = desc.to_lowercase();
    if desc_lower.contains("free") {
        return "free".to_string();
    }

    // Try to extract input/output pricing
    let mut in_price = String::new();
    let mut out_price = String::new();

    for word in desc.split_whitespace() {
        if word.starts_with('$') && word.contains("/1m") {
            if word.contains("in") || word.contains("input") {
                in_price = word.trim_start_matches('$').replace("/1m", "/M").replace("/1M", "/M");
            } else if word.contains("out") || word.contains("output") {
                out_price = word.trim_start_matches('$').replace("/1m", "/M").replace("/1M", "/M");
            }
        } else if word.starts_with('$') && word.len() > 1 {
            // Generic price
            if in_price.is_empty() {
                in_price = word.trim_start_matches('$').to_string();
            } else if out_price.is_empty() {
                out_price = word.trim_start_matches('$').to_string();
            }
        }
    }

    if !in_price.is_empty() || !out_price.is_empty() {
        format!("${}/{}", in_price, out_price)
    } else if desc_lower.contains("paid") || desc_lower.contains("premium") {
        "paid".to_string()
    } else {
        "?".to_string()
    }
}

/// Render side-by-side comparison of two models.
fn render_compare(
    models: &[memnoth_cloud_client::ModelEntry],
    id1: &str,
    id2: &str,
    json_output: bool,
) -> Result<Outcome> {
    let model1 = models.iter().find(|m| m.id == id1);
    let model2 = models.iter().find(|m| m.id == id2);

    if model1.is_none() {
        anyhow::bail!("Model '{}' not found", id1);
    }
    if model2.is_none() {
        anyhow::bail!("Model '{}' not found", id2);
    }

    let m1 = model1.unwrap();
    let m2 = model2.unwrap();

    if json_output {
        let data = json!({
            "comparison": {
                "model1": model_to_json(m1),
                "model2": model_to_json(m2),
            }
        });
        let text = format!("Comparison: {} vs {}\nUse --json for full details", id1, id2);
        return Ok(Outcome::new("models.compare", text, data));
    }

    // Text comparison
    let (p1, c1, pr1, cap1) = parse_model_info(&m1.id, &m1.description);
    let (p2, c2, pr2, cap2) = parse_model_info(&m2.id, &m2.description);
    let prd1 = extract_pricing_detail(&m1.description);
    let prd2 = extract_pricing_detail(&m2.description);

    let mut rows = Vec::new();
    rows.push(format!("══════════════════════════════════════════════════════════════"));
    rows.push(format!("           MODEL COMPARISON: {} vs {}", m1.id, m2.id));
    rows.push(format!("══════════════════════════════════════════════════════════════"));
    rows.push(format!(""));
    rows.push(format!("{:<25} {:<35} {:<35}", "ATTRIBUTE", m1.id, m2.id));
    rows.push(format!("{:<25} {:<35} {:<35}", "─────────", "────────────────", "────────────────"));
    rows.push(format!("{:<25} {:<35} {:<35}", "Name", truncate(&m1.name, 33), truncate(&m2.name, 33)));
    rows.push(format!("{:<25} {:<35} {:<35}", "Provider", p1, p2));
    rows.push(format!("{:<25} {:<35} {:<35}", "Context Window", c1, c2));
    rows.push(format!("{:<25} {:<35} {:<35}", "Max Output Tokens", m1.max_output_tokens.to_string(), m2.max_output_tokens.to_string()));
    rows.push(format!("{:<25} {:<35} {:<35}", "Pricing (in/out)", prd1, prd2));
    rows.push(format!("{:<25} {:<35} {:<35}", "Capabilities", truncate(&cap1, 33), truncate(&cap2, 33)));
    rows.push(format!(""));
    rows.push(format!("Description:"));
    rows.push(format!("  {}: {}", m1.id, truncate(&m1.description, 100)));
    rows.push(format!("  {}: {}", m2.id, truncate(&m2.description, 100)));

    let text = rows.join("\n");

    let data = json!({
        "comparison": {
            "model1": model_to_json(m1),
            "model2": model_to_json(m2),
        }
    });

    Ok(Outcome::new("models.compare", text, data))
}

/// Convert model to full JSON representation.
fn model_to_json(m: &memnoth_cloud_client::ModelEntry) -> serde_json::Value {
    let (provider, context, pricing, caps) = parse_model_info(&m.id, &m.description);
    let pricing_detail = extract_pricing_detail(&m.description);

    json!({
        "id": m.id,
        "name": m.name,
        "provider": provider,
        "description": m.description,
        "context_window": context,
        "max_output_tokens": m.max_output_tokens,
        "pricing": pricing_detail,
        "capabilities": caps,
    })
}

/// `mn model use <id>` — set default model (persisted to config).
fn model_use(ctx: &AppContext, args: &crate::cli::ModelUseArgs) -> Result<Outcome> {
    let client = CloudClient::new()?;
    let list = client.models()?;

    // Verify model exists
    let model = list.data.iter().find(|m| m.id == args.model_id);
    if model.is_none() {
        anyhow::bail!("Model '{}' not found. Run `mn models` to see available models.", args.model_id);
    }
    let model = model.unwrap();

    // Update config
    let config_path = ctx.config_path()?;
    let mut config: Config = if config_path.exists() {
        let content = fs::read_to_string(&config_path)?;
        toml::from_str(&content).context("Failed to parse config.toml")?
    } else {
        Config::default()
    };

    // Ensure provider config exists
    let provider = config.provider.get_or_insert_default();
    provider.model = Some(args.model_id.clone());

    // Write back
    let content = toml::to_string_pretty(&config).context("Failed to serialize config")?;
    fs::write(&config_path, content).context("Failed to write config.toml")?;

    let text = format!(
        "Default model set to: {} ({})\nConfig updated: {}",
        model.id, model.provider, config_path.display()
    );

    let data = json!({
        "model_id": model.id,
        "provider": model.provider,
        "context_window": model.context_window,
        "config_path": config_path.to_string_lossy(),
    });

    Ok(Outcome::new("model.use", text, data))
}

/// Render models as a table for human consumption.
fn render_models_table(models: &[memnoth_cloud_client::ModelEntry]) -> String {
    if models.is_empty() {
        return "No models available.".to_string();
    }

    let mut rows = Vec::new();
    rows.push(
        "ID                          PROVIDER       CONTEXT    PRICING  CAPABILITIES".to_string(),
    );
    rows.push(
        "────────────────────────────  ──────────────  ─────────  ───────  ────────────"
            .to_string(),
    );

    for m in models {
        let (provider, context, pricing, caps) = parse_model_info(&m.id, &m.description);
        rows.push(format!(
            "{:<30} {:<14} {:>9}  {:<7}  {}",
            truncate(&m.id, 28),
            truncate(&provider, 12),
            context,
            pricing,
            truncate(&caps, 40)
        ));
    }

    rows.join("\n")
}

/// Parse model info from id and description.
fn parse_model_info(id: &str, description: &str) -> (String, String, String, String) {
    let provider = if id.contains("gpt") || id.contains("o1") || id.contains("o3") {
        "OpenAI"
    } else if id.contains("claude") {
        "Anthropic"
    } else if id.contains("gemini") {
        "Google"
    } else if id.contains("llama") || id.contains("Llama") {
        "Meta"
    } else if id.contains("mistral") || id.contains("Mixtral") {
        "Mistral"
    } else if id.contains("command") {
        "Cohere"
    } else {
        "Other"
    };

    // Extract context window from description
    let context = extract_context_window(description).unwrap_or_else(|| "?".to_string());

    // Extract pricing hint
    let pricing = extract_pricing_hint(description).unwrap_or_else(|| "?".to_string());

    // Extract capabilities
    let caps = extract_capabilities(description).unwrap_or_else(|| "text".to_string());

    (provider.to_string(), context, pricing, caps)
}

fn extract_context_window(desc: &str) -> Option<String> {
    let desc_lower = desc.to_lowercase();
    if desc_lower.contains("128k") || desc_lower.contains("128 k") {
        Some("128k".to_string())
    } else if desc_lower.contains("200k") || desc_lower.contains("200 k") {
        Some("200k".to_string())
    } else if desc_lower.contains("1m") || desc_lower.contains("1 m") || desc_lower.contains("1000k") {
        Some("1M".to_string())
    } else if desc_lower.contains("32k") || desc_lower.contains("32 k") {
        Some("32k".to_string())
    } else if desc_lower.contains("8k") || desc_lower.contains("8 k") {
        Some("8k".to_string())
    } else if desc_lower.contains("4k") || desc_lower.contains("4 k") {
        Some("4k".to_string())
    } else {
        None
    }
}

fn extract_pricing_hint(desc: &str) -> Option<String> {
    let desc_lower = desc.to_lowercase();
    if desc_lower.contains("free") {
        Some("free".to_string())
    } else if desc_lower.contains("$") {
        // Try to extract a price pattern
        for word in desc.split_whitespace() {
            if word.starts_with('$') && word.len() > 1 {
                return Some(word.to_string());
            }
        }
        Some("paid".to_string())
    } else {
        None
    }
}

fn extract_capabilities(desc: &str) -> Option<String> {
    let mut caps = Vec::new();
    let desc_lower = desc.to_lowercase();
    if desc_lower.contains("vision") || desc_lower.contains("image") {
        caps.push("vision");
    }
    if desc_lower.contains("tool") || desc_lower.contains("function") {
        caps.push("tools");
    }
    if desc_lower.contains("json") {
        caps.push("json");
    }
    if desc_lower.contains("audio") || desc_lower.contains("speech") {
        caps.push("audio");
    }
    if desc_lower.contains("reasoning") || desc_lower.contains("thinking") {
        caps.push("reasoning");
    }
    if caps.is_empty() {
        Some("text".to_string())
    } else {
        Some(caps.join(", "))
    }
}

/// Truncate string to max length.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_model_info() {
        let (provider, context, pricing, caps) = parse_model_info(
            "gpt-4o",
            "GPT-4o 128k context, vision, tools, $5/1M in $15/1M out",
        );
        assert_eq!(provider, "OpenAI");
        assert_eq!(context, "128k");
        assert!(pricing.contains('$'));
        assert!(caps.contains("vision"));
        assert!(caps.contains("tools"));
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello wo…");
    }
}