//! Cloud commands: sync, profile, billing, credits.

use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::{
    BillingArgs, CloudArgs, CloudCommand, CreditsArgs, ProfileArgs, ProfileCommand, ProfileSetArgs,
    ProfileShowArgs, SyncArgs,
};
use crate::context::AppContext;
use crate::output::Outcome;
use memnoth_cloud_client::{CloudClient, CreditsResponse};

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
/// The client every command that *requires* an account uses.
///
/// Same token sources as [`get_cloud_client`] — an exported key wins over the
/// stored one — but a machine with no credential is refused here rather than
/// answering 401 on the first request. A write command that cannot say "sign
/// in first" is a command that looks broken.
pub fn signed_in_client(ctx: &AppContext) -> Result<CloudClient> {
    let mut client = CloudClient::new().context("Failed to create cloud client")?;

    let token = std::env::var("MEMNOTH_API_KEY")
        .ok()
        .or_else(|| std::env::var("MEMNOTH_MCP_TOKEN").ok())
        .or_else(|| ctx.auth_token())
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| {
            // These verbs answer as `/projects` and friends inside the session, so
            // the sign-in they name has to be the one that exists where the person is.
            anyhow::anyhow!(
                "not signed in. Run `/login` in the session, `mn auth login` in a shell, \
                 or set MEMNOTH_API_KEY"
            )
        })?;

    client.set_auth_token(token);
    Ok(client)
}

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

/// `mn cloud sync` — the same sync `mn sync` runs.
///
/// This used to build an empty batch and post it, so every run answered
/// "Synced 0 items to cloud" against a server that was never asked to do
/// anything. The collection, the privacy gate, the retry queue and the
/// heartbeat all live in [`crate::sync`]; delegating is what makes the number a
/// real number instead of a hard-coded zero.
pub fn sync(ctx: &AppContext, args: &SyncArgs) -> Result<Outcome> {
    crate::sync::run_sync(ctx, args)
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
        "vault": config.vault.as_deref(),
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

/// `mn cloud billing` — the credit summary and the org it belongs to.
///
/// There is no `/billing` route: this view is `GET /credits` plus `GET /org`,
/// the two reads the control plane actually serves.
fn billing(ctx: &AppContext, _args: &BillingArgs) -> Result<Outcome> {
    let client = get_cloud_client(ctx)?;

    let credits = client.credits().context("Failed to fetch credits info")?;
    // The org is context, not the subject: a failure to read it must not hide
    // the balance, which is what the command was asked for.
    let org = client.org().ok();

    let data = json!({
        "org": org.as_ref().map(|org| json!({
            "slug": org.slug,
            "name": org.name,
            "plan": org.plan,
        })),
        "credits": {
            "balance_micros": credits.balance_micros,
            "burned_micros": credits.burned_micros,
            "window_days": credits.window_days,
            "expiring_micros": credits.expiring_micros,
            "next_expiry_at": credits.next_expiry_at,
            "demo": credits.demo,
        },
    });

    let mut text = format!(
        "Billing:\n  Credits: ${:.4} USD",
        credits.balance_micros as f64 / 1_000_000.0
    );
    if let Some(org) = &org {
        text.push_str(&format!(
            "\n  Organization: {} (plan {})",
            org.name, org.plan
        ));
    }
    if credits.window_days > 0 {
        text.push_str(&format!(
            "\n  Spent (last {} days): ${:.4}",
            credits.window_days,
            credits.burned_micros as f64 / 1_000_000.0
        ));
    }
    if credits.expiring_micros > 0 {
        text.push_str(&format!(
            "\n  Expiring within {} days: ${:.4} across {} grant(s)",
            credits.expiry_warning_days,
            credits.expiring_micros as f64 / 1_000_000.0,
            credits.expiring_grants
        ));
    }

    Ok(Outcome::new("cloud.billing", text, data))
}

/// `mn cloud credits` - GET /api/v1/credits
fn credits(ctx: &AppContext, _args: &CreditsArgs) -> Result<Outcome> {
    let client = get_cloud_client(ctx)?;

    let response: CreditsResponse = client.credits().context("Failed to fetch credits info")?;

    let data = json!({
        "balance_micros": response.balance_micros,
        "burned_micros": response.burned_micros,
        "window_days": response.window_days,
        "expiring_micros": response.expiring_micros,
        "expiring_grants": response.expiring_grants,
        "next_expiry_at": response.next_expiry_at,
        "expiry_warning_days": response.expiry_warning_days,
        "grants_total": response.grants_total,
        "ledger_total": response.ledger_total,
        "unpriced_sessions": response.unpriced_sessions,
        "demo": response.demo,
    });

    let mut text = format!(
        "Credits: ${:.4} USD",
        response.balance_micros as f64 / 1_000_000.0
    );
    if response.window_days > 0 {
        text.push_str(&format!(
            "\n  Spent (last {} days): ${:.4}",
            response.window_days,
            response.burned_micros as f64 / 1_000_000.0
        ));
    }
    if response.expiring_micros > 0 {
        text.push_str(&format!(
            "\n  Expiring within {} days: ${:.4} across {} grant(s)",
            response.expiry_warning_days,
            response.expiring_micros as f64 / 1_000_000.0,
            response.expiring_grants
        ));
    }
    if let Some(date) = &response.next_expiry_at {
        text.push_str(&format!("\n  Next expiry: {date}"));
    }
    text.push_str(&format!(
        "\n  Grants: {}   Ledger rows: {}",
        response.grants_total, response.ledger_total
    ));

    Ok(Outcome::new("cloud.credits", text, data))
}
