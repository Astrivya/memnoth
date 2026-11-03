//! Cloud commands: sync, profile, billing, credits.

use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::{
    BillingArgs, CloudArgs, CloudCommand, CreditsArgs, ProfileArgs, ProfileCommand, ProfileSetArgs,
    ProfileShowArgs, SyncArgs,
};
use crate::context::AppContext;
use crate::output::Outcome;
use memnoth_cloud_client::{
    BatchIngestRequest, BatchIngestResponse, BillingResponse, CloudClient, CreditsResponse,
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

    // Environment first, so an exported key wins over the stored one.
    if let Ok(token) = std::env::var("MEMNOTH_API_KEY") {
        client.set_auth_token(token);
    } else if let Ok(token) = std::env::var("MEMNOTH_MCP_TOKEN") {
        client.set_auth_token(token);
    } else if let Some(token) = ctx.auth_token() {
        client.set_auth_token(token);
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

    // `mcp_url` and `profile` are resolved the same way every other verb
    // resolves them, so this view never disagrees with `mcp add`.
    let mcp_url = ctx.mcp_url();
    let profile = ctx.profile();

    let data = json!({
        "mcp_url": mcp_url,
        "profile": profile,
        "vault": config.vault.as_ref().map(|v| v.as_str()),
        "provider": config.provider.as_ref().map(|p| json!({
            "base_url": p.base_url,
            "model": p.model,
            "kind": p.kind,
        })),
    });

    let text = format!(
        "Cloud profile:\n  mcp_url: {}\n  profile: {}\n  vault: {}\n  provider: {}",
        mcp_url,
        profile,
        config.vault.as_deref().unwrap_or("default"),
        config
            .provider
            .as_ref()
            .map(|p| format!(
                "{} ({})",
                p.base_url.as_deref().unwrap_or("default endpoint"),
                p.model.as_deref().unwrap_or("default model")
            ))
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

    Ok(Outcome::new("cloud.credits", text, data))
}
