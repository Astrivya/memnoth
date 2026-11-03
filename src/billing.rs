//! `mn invoices`, `mn plan`, `mn limits` — billing, plan info, and quotas.

use anyhow::Result;
use serde_json::json;

use crate::AppContext;
use crate::output::Outcome;
use memnoth_cloud_client::{BillingResponse, CloudClient, CreditsResponse};

/// Execute billing-related commands.
pub fn run(ctx: &AppContext, args: &crate::cli::BillingCommands) -> Result<Outcome> {
    match &args.action {
        crate::cli::BillingCommand::Invoices(_) => invoices(ctx),
        crate::cli::BillingCommand::Plan(_) => plan(ctx),
        crate::cli::BillingCommand::Limits(_) => limits(ctx),
    }
}

/// Micros to whole currency units. The cloud bills in millionths, so every
/// human-facing number goes through here rather than being printed raw.
fn units(micros: i64) -> f64 {
    micros as f64 / 1_000_000.0
}

/// Micros per million tokens to "per million" money.
fn per_million(micros: i64) -> String {
    format!("${:.2}", units(micros))
}

/// `mn invoices` — show billing history / invoices link.
fn invoices(ctx: &AppContext) -> Result<Outcome> {
    let client = get_cloud_client(ctx)?;

    // Billing carries the credit balance and the price list; invoices
    // themselves live on the web console.
    let billing = client.billing().ok();

    let text = if let Some(b) = &billing {
        render_invoices(b)
    } else {
        render_invoices_unavailable()
    };

    let data = json!({
        "billing_url": "https://memnoth.astrivya.ai/billing",
        "invoices": billing.map(|b| json!({
            "credits": b.credits,
            "pricing": b.pricing,
        })),
    });

    Ok(Outcome::new("billing.invoices", text, data))
}

fn render_invoices(billing: &BillingResponse) -> String {
    let mut out = String::new();
    out.push_str("Billing & Invoices\n");
    out.push_str("==================\n\n");

    let credits = &billing.credits;
    out.push_str(&format!(
        "Current Balance: {} {:.2}\n",
        credits.currency,
        units(credits.balance_micros)
    ));
    out.push_str(&format!(
        "Auto-recharge: {}\n",
        if credits.auto_recharge {
            "enabled"
        } else {
            "disabled"
        }
    ));
    if let Some(threshold) = credits.recharge_threshold_micros {
        out.push_str(&format!(
            "  Threshold: {} {:.2}\n",
            credits.currency,
            units(threshold)
        ));
    }
    if let Some(amount) = credits.recharge_amount_micros {
        out.push_str(&format!(
            "  Top-up: {} {:.2}\n",
            credits.currency,
            units(amount)
        ));
    }
    if let Some(date) = &credits.next_billing_date {
        out.push_str(&format!("Next billing date: {date}\n"));
    }

    if !billing.pricing.models.is_empty() {
        out.push_str("\nPriced models:\n");
        for pricing in billing.pricing.models.iter().take(10) {
            out.push_str(&format!(
                "  • {} — {} in / {} out per 1M tokens\n",
                pricing.model_id,
                per_million(pricing.input_price_per_million_micros),
                per_million(pricing.output_price_per_million_micros)
            ));
        }
        if billing.pricing.models.len() > 10 {
            out.push_str(&format!(
                "  ... and {} more\n",
                billing.pricing.models.len() - 10
            ));
        }
    }

    out.push_str("\nFull billing history: https://memnoth.astrivya.ai/billing\n");
    out
}

fn render_invoices_unavailable() -> String {
    "Unable to fetch billing info. Check authentication with `mn auth status`.\nFull billing history: https://memnoth.astrivya.ai/billing\n".to_string()
}

/// `mn plan` — show current plan, quotas, and rate limits.
fn plan(ctx: &AppContext) -> Result<Outcome> {
    let client = get_cloud_client(ctx)?;

    // Get credits for plan info
    let credits = client.credits().ok();
    let billing = client.billing().ok();

    let text = if let (Some(c), Some(b)) = (&credits, &billing) {
        render_plan(c, b)
    } else {
        render_plan_unavailable()
    };

    let data = json!({
        "plan": credits.as_ref().map(|c| json!({
            "balance_micros": c.balance_micros,
            "currency": c.currency,
            "auto_recharge": c.auto_recharge,
        })),
        "quotas": billing.as_ref().map(|b| json!({
            "models": b.pricing.models.len(),
        })),
    });

    Ok(Outcome::new("billing.plan", text, data))
}

fn render_plan(credits: &CreditsResponse, billing: &BillingResponse) -> String {
    let mut out = String::new();
    out.push_str("Current Plan\n");
    out.push_str("============\n\n");

    // Determine plan tier from the balance
    let balance = units(credits.balance_micros);
    let plan_name = if balance >= 100.0 {
        "Pro"
    } else if balance > 0.0 {
        "Starter"
    } else {
        "Free"
    };

    out.push_str(&format!("Plan: {plan_name}\n"));
    out.push_str(&format!(
        "Balance: {} {:.2}\n",
        credits.currency, balance
    ));
    out.push_str(&format!(
        "Auto-recharge: {}\n",
        if credits.auto_recharge {
            "enabled"
        } else {
            "disabled"
        }
    ));

    out.push_str("\nQuotas:\n");
    out.push_str("  • Models available: unlimited\n");
    out.push_str("  • Context window: per model\n");
    out.push_str("  • Concurrent sessions: 5\n");

    out.push_str("\nRate Limits:\n");
    out.push_str("  • Requests/minute: 60\n");
    out.push_str("  • Tokens/minute: 100,000\n");

    out.push_str("\nModel Pricing (sample):\n");
    for (i, pricing) in billing.pricing.models.iter().take(5).enumerate() {
        out.push_str(&format!(
            "  {}. {} — {} in, {} out (per 1M)\n",
            i + 1,
            pricing.model_id,
            per_million(pricing.input_price_per_million_micros),
            per_million(pricing.output_price_per_million_micros)
        ));
    }
    if billing.pricing.models.len() > 5 {
        out.push_str(&format!(
            "  ... and {} more\n",
            billing.pricing.models.len() - 5
        ));
    }

    out
}

fn render_plan_unavailable() -> String {
    "Unable to fetch plan info. Check authentication with `mn auth status`.\n".to_string()
}

/// `mn limits` — show detailed rate limits and quotas.
fn limits(ctx: &AppContext) -> Result<Outcome> {
    let client = get_cloud_client(ctx)?;

    let credits = client.credits().ok();
    let billing = client.billing().ok();

    let text = if let (Some(c), Some(b)) = (&credits, &billing) {
        render_limits(c, b)
    } else {
        render_limits_unavailable()
    };

    let data = json!({
        "rate_limits": {
            "requests_per_minute": 60,
            "tokens_per_minute": 100_000,
            "concurrent_sessions": 5,
        },
        "quotas": {
            "models": billing.as_ref().map(|b| b.pricing.models.len()).unwrap_or(0),
        },
    });

    Ok(Outcome::new("billing.limits", text, data))
}

fn render_limits(credits: &CreditsResponse, billing: &BillingResponse) -> String {
    let mut out = String::new();
    out.push_str("Rate Limits & Quotas\n");
    out.push_str("====================\n\n");

    out.push_str("API Rate Limits:\n");
    out.push_str("  • Requests/minute: 60\n");
    out.push_str("  • Tokens/minute: 100,000\n");
    out.push_str("  • Concurrent requests: 10\n\n");

    out.push_str("Session Limits:\n");
    out.push_str("  • Concurrent sessions: 5\n");
    out.push_str("  • Max session duration: 24 hours\n");
    out.push_str("  • Context window: model-dependent\n\n");

    out.push_str("Storage Quotas:\n");
    out.push_str("  • Vault entries: 100,000\n");
    out.push_str("  • Knowledge graph nodes: 500,000\n");
    out.push_str("  • Session history: 1,000 turns\n\n");

    out.push_str("Model Access:\n");
    out.push_str(&format!(
        "  • Available models: {}\n",
        billing.pricing.models.len()
    ));
    out.push_str("  • Custom models: not available\n\n");

    out.push_str(&format!(
        "Current Balance: {} {:.2}\n",
        credits.currency,
        units(credits.balance_micros)
    ));
    out.push_str("Monitor usage: `mn usage`\n");

    out
}

fn render_limits_unavailable() -> String {
    "Unable to fetch limits. Check authentication with `mn auth status`.\n".to_string()
}

fn get_cloud_client(ctx: &AppContext) -> Result<CloudClient> {
    let mut client = CloudClient::new()?;

    if let Some(token) = ctx.auth_token() {
        client.set_auth_token(token);
    }

    Ok(client)
}
