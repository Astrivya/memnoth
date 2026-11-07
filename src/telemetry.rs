//! `mn telemetry` — show what's collected and toggle telemetry.

use anyhow::Result;
use serde_json::json;
use std::fs;

use crate::AppContext;
use crate::context::Config;
use crate::output::Outcome;
/// Execute `mn telemetry`.
pub fn run(ctx: &AppContext, args: &crate::cli::TelemetryArgs) -> Result<Outcome> {
    match &args.action {
        crate::cli::TelemetryCommand::Status(_) => telemetry_status(ctx),
        crate::cli::TelemetryCommand::On(_) => telemetry_on(ctx),
        crate::cli::TelemetryCommand::Off(_) => telemetry_off(ctx),
    }
}

/// `mn telemetry status` — show what telemetry is collected and current state.
fn telemetry_status(ctx: &AppContext) -> Result<Outcome> {
    // Get local config
    let config = ctx.config();
    let telemetry_enabled = config
        .privacy
        .as_ref()
        .and_then(|p| p.telemetry_enabled)
        .unwrap_or(true); // default: enabled

    let text = if telemetry_enabled {
        render_telemetry_enabled()
    } else {
        render_telemetry_disabled()
    };

    let data = json!({
        "enabled": telemetry_enabled,
        "categories": get_telemetry_categories(),
    });

    Ok(Outcome::new("telemetry.status", text, data))
}

fn render_telemetry_enabled() -> String {
    let mut out = String::new();
    out.push_str("Telemetry: ENABLED\n\n");
    out.push_str("What is collected:\n");
    for (category, desc) in get_telemetry_categories() {
        out.push_str(&format!("  • {category}: {desc}\n"));
    }
    out.push_str("\nTo disable: `mn telemetry off`\n");
    out
}

fn render_telemetry_disabled() -> String {
    let mut out = String::new();
    out.push_str("Telemetry: DISABLED\n\n");
    out.push_str("No telemetry data is being collected.\n\n");
    out.push_str("What would be collected if enabled:\n");
    for (category, desc) in get_telemetry_categories() {
        out.push_str(&format!("  • {category}: {desc}\n"));
    }
    out.push_str("\nTo enable: `mn telemetry on`\n");
    out
}

fn get_telemetry_categories() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "command_usage",
            "Which commands are run and their success/failure",
        ),
        ("performance", "Command latency and memory usage"),
        ("errors", "Error types and stack traces (sanitized)"),
        (
            "model_usage",
            "Model IDs, token counts, latency (no prompt content)",
        ),
        ("feature_flags", "Which experimental features are enabled"),
    ]
}

/// `mn telemetry on` — enable telemetry collection.
fn telemetry_on(ctx: &AppContext) -> Result<Outcome> {
    set_telemetry(ctx, true)
}

/// `mn telemetry off` — disable telemetry collection.
fn telemetry_off(ctx: &AppContext) -> Result<Outcome> {
    set_telemetry(ctx, false)
}

fn set_telemetry(ctx: &AppContext, enabled: bool) -> Result<Outcome> {
    let config_path = ctx.config_path();
    let mut config: Config = if config_path.exists() {
        let content = fs::read_to_string(config_path)?;
        toml::from_str(&content)?
    } else {
        Config::default()
    };

    let privacy = config.privacy.get_or_insert_default();
    privacy.telemetry_enabled = Some(enabled);

    let content = toml::to_string_pretty(&config)?;
    fs::write(config_path, content)?;

    // Nothing is pushed to the control plane: it has no telemetry-preference
    // endpoint, and the local `config.toml` is what `mn sync` reads before it
    // sends anything. A call here used to 404 on every deployment and was
    // swallowed, which made the toggle look synced when it never was.

    let text = if enabled {
        "Telemetry enabled."
    } else {
        "Telemetry disabled."
    };

    let data = json!({ "enabled": enabled });
    Ok(Outcome::new("telemetry.toggle", text.to_string(), data))
}
