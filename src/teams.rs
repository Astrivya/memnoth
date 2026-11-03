//! Team commands: join an organization via invite code.

use anyhow::{Context, Result};

use crate::context::AppContext;
use crate::output::Outcome;
use memnoth_auth::get_credential_sync;
use memnoth_cloud_client::{AcceptInviteResponse, CloudClient};
use serde_json::json;

/// Run the `mn teams join` command.
pub fn join(ctx: &AppContext, args: &crate::cli::TeamsJoinArgs) -> Result<Outcome> {
    let base_url = get_base_url(ctx)?;
    let mut client =
        CloudClient::with_base_url(&base_url).context("failed to create cloud client")?;

    // Get auth token from credential store (synchronous)
    let token = get_credential_sync("api_key")?
        .ok_or_else(|| anyhow::anyhow!("not authenticated; run `mn auth login` first"))?;

    client.set_auth_token(token);

    let response: AcceptInviteResponse = client
        .accept_invite(&args.code)
        .context("failed to accept invite")?;

    let text = format!(
        "Joined organization {} as {}",
        response.org_id, response.role
    );
    let data = json!({
        "org_id": response.org_id,
        "role": response.role,
    });

    Ok(Outcome::new("teams.join", text, data))
}

/// Get the base URL for the cloud API.
fn get_base_url(_ctx: &AppContext) -> Result<String> {
    Ok(std::env::var("MEMNOTH_CLOUD_URL")
        .unwrap_or_else(|_| "https://memnoth.astrivya.ai/api/v1".to_string()))
}
