use anyhow::{Context, Result};
use serde_json::json;
use std::collections::HashSet;
use std::fs;

use crate::output::Outcome;
use crate::{AppContext, Config};
use memnoth_auth::get_credential_sync;
use memnoth_cloud_client::CloudClient;

/// The provider a catalog entry belongs to.
///
/// A catalog entry carries an id, name and description only, so the provider is
/// read from the id exactly the way the tables read it.
fn model_provider(m: &memnoth_cloud_client::ModelEntry) -> String {
    parse_model_info(&m.id, &m.description).0
}

/// Execute `mn models` or `mn model`.
pub fn run(ctx: &AppContext, args: &crate::cli::ModelsArgs) -> Result<Outcome> {
    match &args.action {
        crate::cli::ModelsCommand::List(args) => models_list(ctx, args),
        crate::cli::ModelsCommand::Use(args) => model_use(ctx, args),
    }
}

/// `mn models` — list available models from the cloud.
fn models_list(ctx: &AppContext, args: &crate::cli::ModelsListArgs) -> Result<Outcome> {
    let client = CloudClient::from_env()?;
    let list = client.models()?;

    // The context already loaded config.toml for this invocation.
    let config = &ctx.config;

    // Determine which providers are "connected" (have API keys configured)
    let connected_providers = get_connected_providers(config);

    // Filter models
    let mut models: Vec<_> = list.data.into_iter().collect();

    // Filter by provider name if specified
    if let Some(provider_filter) = &args.provider {
        models.retain(|m| {
            let provider = model_provider(m);
            provider.eq_ignore_ascii_case(provider_filter)
                || m.id
                    .to_lowercase()
                    .contains(&provider_filter.to_lowercase())
        });
    }

    // Filter by connected providers if --connected flag is set
    if args.connected {
        models.retain(|m| connected_providers.contains(&model_provider(m).to_lowercase()));
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
            "provider": model_provider(m),
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
    if let Some(provider_config) = &config.provider
        && provider_config.kind.is_some()
    {
        providers.insert(provider_config.kind.as_ref().unwrap().to_lowercase());
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
    if let Ok(Some(key)) = get_credential_sync("memnoth")
        && !key.is_empty()
    {
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
        "ID                              PROVIDER        NAME                    CONTEXT  PRICING (IN/OUT)      CAPABILITIES".to_string(),
    );
    rows.push(
        "────────────────────────────────  ──────────────  ──────────────────────  ───────  ───────────────────  ──────────────────────"
            .to_string(),
    );

    for m in models {
        let (provider, context, _pricing, caps) = parse_model_info(&m.id, &m.description);

        // Extract pricing from description more thoroughly
        let pricing_detail = extract_pricing_detail(&m.description);

        rows.push(format!(
            "{:<34} {:<14} {:<22} {:>7}  {:<19}  {}",
            truncate(&m.id, 32),
            truncate(&provider, 12),
            truncate(&m.name, 20),
            context,
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
                in_price = word
                    .trim_start_matches('$')
                    .replace("/1m", "/M")
                    .replace("/1M", "/M");
            } else if word.contains("out") || word.contains("output") {
                out_price = word
                    .trim_start_matches('$')
                    .replace("/1m", "/M")
                    .replace("/1M", "/M");
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
        let text = format!(
            "Comparison: {} vs {}\nUse --json for full details",
            id1, id2
        );
        return Ok(Outcome::new("models.compare", text, data));
    }

    // Text comparison
    let (p1, c1, _pr1, cap1) = parse_model_info(&m1.id, &m1.description);
    let (p2, c2, _pr2, cap2) = parse_model_info(&m2.id, &m2.description);
    let prd1 = extract_pricing_detail(&m1.description);
    let prd2 = extract_pricing_detail(&m2.description);

    let mut rows = Vec::new();
    rows.push("══════════════════════════════════════════════════════════════".to_string());
    rows.push(format!(
        "           MODEL COMPARISON: {} vs {}",
        m1.id, m2.id
    ));
    rows.push("══════════════════════════════════════════════════════════════".to_string());
    rows.push(String::new());
    rows.push(format!("{:<25} {:<35} {:<35}", "ATTRIBUTE", m1.id, m2.id));
    rows.push(format!(
        "{:<25} {:<35} {:<35}",
        "─────────", "────────────────", "────────────────"
    ));
    rows.push(format!(
        "{:<25} {:<35} {:<35}",
        "Name",
        truncate(&m1.name, 33),
        truncate(&m2.name, 33)
    ));
    rows.push(format!("{:<25} {:<35} {:<35}", "Provider", p1, p2));
    rows.push(format!("{:<25} {:<35} {:<35}", "Context Window", c1, c2));
    rows.push(format!(
        "{:<25} {:<35} {:<35}",
        "Max Output Tokens", "unreported", "unreported"
    ));
    rows.push(format!(
        "{:<25} {:<35} {:<35}",
        "Pricing (in/out)", prd1, prd2
    ));
    rows.push(format!(
        "{:<25} {:<35} {:<35}",
        "Capabilities",
        truncate(&cap1, 33),
        truncate(&cap2, 33)
    ));
    rows.push(String::new());
    rows.push("Description:".to_string());
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
    let (provider, context, _pricing, caps) = parse_model_info(&m.id, &m.description);
    let pricing_detail = extract_pricing_detail(&m.description);

    json!({
        "id": m.id,
        "name": m.name,
        "provider": provider,
        "description": m.description,
        "context_window": context,
        "pricing": pricing_detail,
        "capabilities": caps,
    })
}

/// `mn model use <id>` — set default model (persisted to config).
fn model_use(ctx: &AppContext, args: &crate::cli::ModelUseArgs) -> Result<Outcome> {
    let client = CloudClient::from_env()?;
    let list = client.models()?;

    // Verify model exists
    let Some(model) = list.data.iter().find(|m| m.id == args.model_id) else {
        anyhow::bail!(
            "Model '{}' not found. Run `mn models` to see available models.",
            args.model_id
        );
    };

    // Update config
    let config_path = &ctx.config_path;
    let mut config = ctx.config.clone();

    // Ensure the `[provider]` table exists. Every field is optional, so this is
    // an empty table, not a guessed endpoint.
    if config.provider.is_none() {
        config.provider = Some(crate::provider::ProviderConfig {
            kind: None,
            model: None,
            base_url: None,
            max_tokens: None,
            input_per_mtok: None,
            output_per_mtok: None,
            ..Default::default()
        });
    }
    let provider_name = model_provider(model);

    // A model id is only meaningful on the provider that serves it, so the
    // preset is switched with it — otherwise `mn model use claude-…` leaves the
    // config pointed at an OpenAI endpoint that will answer 404.
    let preset = crate::connect::known_providers()
        .into_iter()
        .find(|info| info.name.eq_ignore_ascii_case(&provider_name));

    if let Some(provider) = config.provider.as_mut() {
        provider.model = Some(args.model_id.clone());
        if let Some(info) = preset {
            let changed = provider.kind.as_deref() != Some(info.kind.name());
            provider.kind = Some(info.kind.name().to_string());
            provider.name = Some(info.name.to_string());
            provider.base_url = Some(info.default_base_url.to_string());
            if changed {
                provider.input_per_mtok = None;
                provider.output_per_mtok = None;
            }
        }
    }

    // Write back
    let content = toml::to_string_pretty(&config).context("Failed to serialize config")?;
    fs::write(config_path, content).context("Failed to write config.toml")?;

    let (_, context_window, _, _) = parse_model_info(&model.id, &model.description);

    let text = format!(
        "Default model set to: {} ({})\nConfig updated: {}",
        model.id,
        provider_name,
        config_path.display()
    );

    let data = json!({
        "model_id": model.id,
        "provider": provider_name,
        "context_window": context_window,
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
    } else if desc_lower.contains("1m")
        || desc_lower.contains("1 m")
        || desc_lower.contains("1000k")
    {
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
/// Truncate to `max` display columns, ending in `…` when it had to cut.
///
/// The ellipsis is inside the budget, so a truncated value never overflows the
/// column it is drawn in, and cutting on a character boundary cannot panic on
/// multi-byte text.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
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
        assert_eq!(truncate("hello world", 8), "hello w…");
    }
}
