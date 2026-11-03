//! Cloud commands: sync, profile, billing, credits.

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::cli::{
    BillingArgs, CloudArgs, CloudCommand, CreditsArgs, ProfileArgs, ProfileCommand, ProfileSetArgs,
    ProfileShowArgs, SyncArgs,
};
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};
use memnoth_cloud_client::{
    BatchIngestRequest, BatchIngestResponse, BillingResponse, CloudClient, CreditsResponse,
    IngestRequest, ModelPricing, PricingResponse,
};

/// Run the cloud command.
pub fn run(ctx: &AppContext, args: &CloudArgs) -> Result<Outcome> {
    match &args.action {
        CloudCommand::Sync(args) => sync(ctx, args),
        CloudCommand::Profile(args) => profile(ctx, args),
        CloudCommand::Billing(args) => billing(ctx, args),
        CloudCommand::Credits(args) => credits(ctx, args),
    }
}

/// Get authenticated cloud client.
fn get_cloud_client(ctx: &AppContext) -> Result<CloudClient> {
    let mut client = CloudClient::new().context("Failed to create cloud client")?;

    // Try to get token from environment
    if let Ok(token) = std::env::var("MEMNOTH_API_KEY") {
        client.set_auth_token(token);
    } else if let Ok(token) = std::env::var("MEMNOTH_MCP_TOKEN") {
        client.set_auth_token(token);
    } else {
        // Try to load from credential store
        if let Ok(store) = crate::auth::create_credential_store() {
            if let Ok(Some(token)) = store.get() {
                client.set_auth_token(token);
            }
        }
    }

    Ok(client)
}

/// `mn cloud sync` - POST /api/v1/ingest/batch
pub fn sync(ctx: &AppContext, args: &SyncArgs) -> Result<Outcome> {
    let client = get_cloud_client(ctx)?;

    // Build batch ingest request from local data
    // For now, create a dry-run or empty request structure
    let request = BatchIngestRequest {
        items: vec![], // TODO: collect actual session data to sync
        dry_run: if args.dry_run { Some(true) } else { None },
    };

    let response: BatchIngestResponse = if args.dry_run {
        // For dry run, just validate without sending
        BatchIngestResponse {
            accepted: true,
            batch_id: Some("dry-run".to_string()),
            accepted_count: 0,
            rejected_count: 0,
            results: Some(vec![]),
        }
    } else {
        client
            .ingest_batch(request)
            .context("Failed to sync with cloud")?
    };

    let data = json!({
        "accepted": response.accepted,
        "batch_id": response.batch_id,
        "accepted_count": response.accepted_count,
        "rejected_count": response.rejected_count,
        "dry_run": args.dry_run,
    });

    let text = if args.dry_run {
        "Dry run: no data sent to cloud".to_string()
    } else if response.accepted {
        format!(
            "Synced {} items to cloud (batch: {})",
            response.accepted_count,
            response.batch_id.as_deref().unwrap_or("unknown")
        )
    } else {
        format!("Sync failed: {} items rejected", response.rejected_count)
    };

    Ok(Outcome::new("cloud.sync", text, data))
}

/// `mn cloud profile` - manage cloud profile in config.toml
fn profile(ctx: &AppContext, args: &ProfileArgs) -> Result<Outcome> {
    match &args.action {
        ProfileCommand::Show(args) => profile_show(ctx, args),
        ProfileCommand::Set(args) => profile_set(ctx, args),
    }
}

/// `mn cloud profile show`
fn profile_show(ctx: &AppContext, _args: &ProfileShowArgs) -> Result<Outcome> {
    let config = &ctx.config;

    let data = json!({
        "mcp_url": config.mcp_url(),
        "profile": config.profile(),
        "vault": config.vault.as_ref().map(|v| v.as_str()),
        "provider": config.provider.as_ref().map(|p| json!({
            "endpoint": p.endpoint,
            "model": p.model,
        })),
    });

    let text = format!(
        "Cloud profile:\n  mcp_url: {}\n  profile: {}\n  vault: {}\n  provider: {}",
        config.mcp_url(),
        config.profile(),
        config.vault.as_deref().unwrap_or("default"),
        config
            .provider
            .as_ref()
            .map(|p| format!("{} ({})", p.endpoint, p.model))
            .unwrap_or_else(|| "not configured".to_string())
    );

    Ok(Outcome::new("cloud.profile.show", text, data))
}

/// `mn cloud profile set`
fn profile_set(ctx: &AppContext, args: &ProfileSetArgs) -> Result<Outcome> {
    let mut config = ctx.config.clone();

    match args.key.as_str() {
        "mcp_url" => config.mcp_url = Some(args.value.clone()),
        "profile" => config.profile = Some(args.value.clone()),
        "vault" => config.vault = Some(args.value.clone()),
        _ => {
            return Err(anyhow::anyhow!(
                "Unknown profile key: {}. Valid keys: mcp_url, profile, vault",
                args.key
            ));
        }
    }

    config
        .save(&ctx.config_path)
        .context("Failed to save config")?;

    let data = json!({
        "key": args.key,
        "value": args.value,
    });

    let text = format!("Set {} = {}", args.key, args.value);

    Ok(Outcome::new("cloud.profile.set", text, data))
}

/// `mn cloud billing` - GET /api/v1/billing
fn billing(ctx: &AppContext, _args: &BillingArgs) -> Result<Outcome> {
    let client = get_cloud_client(ctx)?;

    let response: BillingResponse = client.billing().context("Failed to fetch billing info")?;

    let data = json!({
        "credits": {
            "balance_micros": response.credits.balance_micros,
            "currency": response.credits.currency,
            "auto_recharge": response.credits.auto_recharge,
            "recharge_threshold_micros": response.credits.recharge_threshold_micros,
            "recharge_amount_micros": response.credits.recharge_amount_micros,
            "next_billing_date": response.credits.next_billing_date,
            "grants": response.credits.grants.map(|g| g.iter().map(|grant| json!({
                "id": grant.id,
                "amount_micros": grant.amount_micros,
                "reason": grant.reason,
                "created_at": grant.created_at,
                "expires_at": grant.expires_at,
            })).collect::<Vec<_>>()),
            "expiry": response.credits.expiry.map(|e| json!({
                "expires_at": e.expires_at,
                "amount_micros": e.amount_micros,
            })),
        },
        "pricing": {
            "currency": response.pricing.currency,
            "models": response.pricing.models.iter().map(|m| json!({
                "model_id": m.model_id,
                "input_price_per_million_micros": m.input_price_per_million_micros,
                "output_price_per_million_micros": m.output_price_per_million_micros,
                "currency": m.currency,
            })).collect::<Vec<_>>(),
        },
    });

    let balance_usd = response.credits.balance_micros as f64 / 1_000_000.0;
    let text = format!(
        "Billing:\n  Credits: ${:.4} {}\n  Auto-recharge: {}\n  Models with pricing: {}",
        balance_usd,
        response.credits.currency,
        if response.credits.auto_recharge {
            "enabled"
        } else {
            "disabled"
        },
        response.pricing.models.len()
    );

    Ok(Outcome::new("cloud.billing", text, data))
}

/// `mn cloud credits` - GET /api/v1/credits
fn credits(ctx: &AppContext, _args: &CreditsArgs) -> Result<Outcome> {
    let client = get_cloud_client(ctx)?;

    let response: CreditsResponse = client.credits().context("Failed to fetch credits info")?;

    let data = json!({
        "balance_micros": response.balance_micros,
        "currency": response.currency,
        "auto_recharge": response.auto_recharge,
        "recharge_threshold_micros": response.recharge_threshold_micros,
        "recharge_amount_micros": response.recharge_amount_micros,
        "next_billing_date": response.next_billing_date,
        "grants": response.grants.map(|g| g.iter().map(|grant| json!({
            "id": grant.id,
            "amount_micros": grant.amount_micros,
            "reason": grant.reason,
            "created_at": grant.created_at,
            "expires_at": grant.expires_at,
        })).collect::<Vec<_>>()),
        "expiry": response.expiry.map(|e| json!({
            "expires_at": e.expires_at,
            "amount_micros": e.amount_micros,
        })),
    });

    let balance_usd = response.balance_micros as f64 / 1_000_000.0;
    let mut text = format!(
        "Credits: ${:.4} {}\n  Auto-recharge: {}",
        balance_usd,
        response.currency,
        if response.auto_recharge {
            "enabled"
        } else {
            "disabled"
        }
    );

    if let Some(threshold) = response.recharge_threshold_micros {
        text.push_str(&format!(
            "\n  Recharge threshold: ${:.4}",
            threshold as f64 / 1_000_000.0
        ));
    }
    if let Some(amount) = response.recharge_amount_micros {
        text.push_str(&format!(
            "\n  Recharge amount: ${:.4}",
            amount as f64 / 1_000_000.0
        ));
    }
    if let Some(date) = &response.next_billing_date {
        text.push_str(&format!("\n  Next billing: {}", date));
    }
    if let Some(grants) = &response.grants {
        text.push_str(&format!("\n  Grants: {}", grants.len()));
    }
    if let Some(expiry) = &response.expiry {
        text.push_str(&format!(
            "\n  Expiring: ${:.4} at {}",
            expiry.amount_micros as f64 / 1_000_000.0,
            expiry.expires_at
        ));
    }

    Ok(Outcome::new("cloud.credits", text, data))
}
