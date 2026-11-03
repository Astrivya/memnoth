//! Authentication commands: login, logout, auth status, whoami.
//!
//! Implements the device authorization flow (RFC 8628) against the memnoth cloud API.

use anyhow::{Context, Result};
use std::thread;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

use crate::cli::{AuthLoginArgs, AuthLogoutArgs, AuthStatusArgs, WhoamiArgs};
use crate::context::AppContext;
use crate::output::Outcome;
use memnoth_auth::{
    CredentialStore, FileCredentialStore, KeyringCredentialStore, MultiCredentialStore,
};
use memnoth_cloud_client::{CloudClient, DeviceAuthRequest, DeviceAuthResponse, TokenResponse};
pub fn login(ctx: &AppContext, args: &AuthLoginArgs) -> Result<Outcome> {
    // Handle --with-token flag for CI/paste path
    if let Some(token) = &args.with_token {
        return login_with_token(ctx, token);
    }

    // Create cloud client without auth token
    let client = CloudClient::new().context("failed to create cloud client")?;

    // Start device authorization
    let client_name = args
        .client_name
        .clone()
        .unwrap_or_else(|| format!("memnoth-cli"));

    let request = DeviceAuthRequest { client_name };

    let auth = client
        .device_auth(request)
        .context("failed to start device authorization")?;

    // Print user code and verification URI
    println!("User code:      {}", auth.user_code);
    println!("Verification URL: {}", auth.verification_uri);
    println!();

    // Open browser unless --no-browser
    if !args.no_browser {
        if let Err(e) = open_browser(&auth.verification_uri_complete) {
            eprintln!("Failed to open browser: {e}");
            println!(
                "Please open this URL manually: {}",
                auth.verification_uri_complete
            );
        }
    } else {
        println!(
            "Open this URL in your browser: {}",
            auth.verification_uri_complete
        );
    }

    // Poll for token with backoff
    let token = poll_for_token(&client, &auth)?;

    // The token response carries no identity, so identity is fetched with the
    // token itself: the account and its default organization are what
    // `whoami` and `auth status` read from the cache.
    let mut authed = CloudClient::new().context("failed to create cloud client")?;
    authed.set_auth_token(token.access_token.clone());
    let me = authed.me().context("failed to fetch the account")?;
    let org = authed.org().context("failed to fetch the organization")?;

    let role = me
        .organizations
        .as_ref()
        .and_then(|organizations| organizations.first())
        .map(|organization| organization.role.clone())
        .unwrap_or_else(|| org.role.clone());

    let whoami = StoredWhoami {
        account_id: me.id.clone(),
        email: me.email.clone(),
        name: me.name.clone(),
        org_id: org.id.clone(),
        org_name: org.name.clone(),
        role,
    };

    let stored_token = StoredToken {
        api_key: token.access_token.clone(),
        account_id: whoami.account_id.clone(),
        email: whoami.email.clone(),
        org_id: whoami.org_id.clone(),
        org_name: whoami.org_name.clone(),
        // The device flow does not report the tier it granted; the tier asked
        // for is the closest true answer, and "unknown" is honest otherwise.
        tier: args.tier.clone().unwrap_or_else(|| "unknown".to_string()),
        status: "active".to_string(),
    };
    store_token(ctx, &stored_token)?;
    cache_whoami_direct(ctx, &whoami)?;

    if !ctx.json {
        println!("Login successful! Welcome, {}", whoami.email);
    }

    Ok(Outcome::new(
        "auth.login",
        String::new(),
        serde_json::json!({
            "account_id": whoami.account_id,
            "email": whoami.email,
            "tier": stored_token.tier,
            "status": stored_token.status,
        }),
    ))
}

/// Login with a pre-provided token (CI/paste path).
fn login_with_token(ctx: &AppContext, token: &str) -> Result<Outcome> {
    let mut client = CloudClient::new().context("failed to create cloud client")?;
    client.set_auth_token(token.to_string());

    // Verify the token works by fetching user info
    let me = client.me().context("invalid token")?;
    let org = client.org().context("failed to fetch org")?;
    // Store the token
    let stored_token = StoredToken {
        api_key: token.to_string(),
        account_id: me.id.clone(),
        email: me.email.clone(),
        org_id: org.id.clone(),
        org_name: org.name.clone(),
        tier: "unknown".to_string(),
        status: "active".to_string(),
    };
    store_token(ctx, &stored_token)?;

    let whoami = StoredWhoami {
        account_id: me.id,
        email: me.email,
        name: me.name,
        org_id: org.id,
        org_name: org.name,
        role: org.role,
    };
    cache_whoami_direct(ctx, &whoami)?;
    if !ctx.json {
        println!("Login successful! Welcome, {}", whoami.email);
    }

    Ok(Outcome::new(
        "auth.login",
        String::new(),
        serde_json::json!({
            "account_id": whoami.account_id,
            "email": whoami.email,
            "tier": "unknown",
            "status": "active",
        }),
    ))
}

/// Poll for device authorization token with exponential backoff.
fn poll_for_token(client: &CloudClient, auth: &DeviceAuthResponse) -> Result<TokenResponse> {
    let start = Instant::now();
    let timeout = Duration::from_secs(auth.expires_in);
    let mut interval = Duration::from_secs(auth.interval);

    loop {
        // Check timeout
        if start.elapsed() >= timeout {
            anyhow::bail!(
                "device authorization timed out after {} seconds",
                auth.expires_in
            );
        }

        // Poll
        match client.device_token(&auth.device_code) {
            Ok(key) => return Ok(key),
            Err(e) => {
                // Check if it's a "pending" error (428)
                let is_pending = matches!(&e, memnoth_cloud_client::CloudError::ClientError { status, .. } if *status == 428);
                let is_denied = matches!(&e, memnoth_cloud_client::CloudError::ClientError { status, message } if *status == 400 && message.contains("denied"));

                if is_denied {
                    anyhow::bail!("device authorization was denied");
                } else if !is_pending {
                    return Err(e).context("device token poll failed");
                }

                // Wait before next poll
                thread::sleep(interval);
                // Exponential backoff, capped at 30 seconds
                interval = std::cmp::min(interval * 2, Duration::from_secs(30));
            }
        }
    }
}

/// Stored token information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StoredToken {
    api_key: String,
    account_id: String,
    email: String,
    org_id: String,
    org_name: String,
    tier: String,
    status: String,
}

/// Stored whoami information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StoredWhoami {
    account_id: String,
    email: String,
    name: Option<String>,
    org_id: String,
    org_name: String,
    role: String,
}

/// Store the API token in the credential store.
fn store_token(_ctx: &AppContext, stored_token: &StoredToken) -> Result<()> {
    let store = create_credential_store()?;

    // Store API key
    store.set_sync("api_key", stored_token.api_key.clone())?;

    // Store token metadata as JSON
    let metadata = serde_json::to_string(stored_token)?;
    store.set_sync("token_metadata", metadata)?;

    Ok(())
}

/// Cache whoami info directly.
fn cache_whoami_direct(_ctx: &AppContext, whoami: &StoredWhoami) -> Result<()> {
    let store = create_credential_store()?;
    let metadata = serde_json::to_string(whoami)?;
    store.set_sync("whoami", metadata)?;
    Ok(())
}

/// Run the `mn auth logout` command.
pub fn logout(ctx: &AppContext, _args: &AuthLogoutArgs) -> Result<Outcome> {
    // Get stored token
    let store = create_credential_store()?;

    if let Some(token) = store.get_sync("api_key")? {
        // Try to revoke it on the server
        let mut client = CloudClient::new().context("failed to create cloud client")?;
        client.set_auth_token(token);
        let _ = client.revoke_current_key(); // Best effort
    }

    // Clear local credentials
    let store = create_credential_store()?;
    store.delete_sync("api_key")?;
    store.delete_sync("token_metadata")?;
    store.delete_sync("whoami")?;

    if !ctx.json {
        println!("Logged out successfully");
    }

    Ok(Outcome::new(
        "auth.logout",
        String::new(),
        serde_json::json!({}),
    ))
}

/// Run the `mn auth status` command.
pub fn auth_status(ctx: &AppContext, _args: &AuthStatusArgs) -> Result<Outcome> {
    let store = create_credential_store()?;

    // Get cached whoami
    let whoami: Option<StoredWhoami> = store
        .get_sync("whoami")?
        .and_then(|s| serde_json::from_str(&s).ok());

    // Get token metadata
    let token: Option<StoredToken> = store
        .get_sync("token_metadata")?
        .and_then(|s| serde_json::from_str(&s).ok());

    let authenticated = whoami.is_some();

    let data = serde_json::json!({
        "authenticated": authenticated,
        "account_id": whoami.as_ref().map(|w| w.account_id.clone()),
        "email": whoami.as_ref().map(|w| w.email.clone()),
        "name": whoami.as_ref().and_then(|w| w.name.clone()),
        "org_id": whoami.as_ref().map(|w| w.org_id.clone()),
        "org_name": whoami.as_ref().map(|w| w.org_name.clone()),
        "role": whoami.as_ref().map(|w| w.role.clone()),
        "tier": token.as_ref().map(|t| t.tier.clone()),
        "status": token.as_ref().map(|t| t.status.clone()),
        "expires_at": null, // TODO: add expiry tracking
    });

    if !ctx.json {
        if let Some(w) = &whoami {
            println!("Authenticated: yes");
            println!("Account:       {} ({})", w.email, w.account_id);
            if let Some(name) = &w.name {
                println!("Name:          {}", name);
            }
            println!("Organization:  {} ({})", w.org_name, w.org_id);
            println!("Role:          {}", w.role);
            if let Some(t) = &token {
                println!("Tier:          {}", t.tier);
                println!("Status:        {}", t.status);
            }
        } else {
            println!("Authenticated: no");
        }
    }

    Ok(Outcome::new("auth.status", String::new(), data))
}

/// Run the `mn auth whoami` command.
pub fn whoami(ctx: &AppContext, _args: &WhoamiArgs) -> Result<Outcome> {
    let store = create_credential_store()?;
    let whoami: Option<StoredWhoami> = store
        .get_sync("whoami")?
        .and_then(|s| serde_json::from_str(&s).ok());

    let data = if let Some(w) = whoami {
        if !ctx.json {
            println!("{} ({})", w.email, w.account_id);
        }
        serde_json::json!({
            "account_id": w.account_id,
            "email": w.email,
            "name": w.name,
            "org_id": w.org_id,
            "org_name": w.org_name,
            "role": w.role,
        })
    } else {
        if !ctx.json {
            println!("not authenticated");
        }
        serde_json::json!({
            "account_id": null,
            "email": null,
            "name": null,
            "org_id": null,
            "org_name": null,
            "role": null,
        })
    };

    Ok(Outcome::new("auth.whoami", String::new(), data))
}
/// Get stored API token from credential store.
pub fn get_stored_token(ctx: &AppContext) -> Result<Option<String>> {
    let store = create_credential_store()?;
    let token: Option<StoredToken> = store
        .get_sync("token_metadata")?
        .and_then(|s| serde_json::from_str(&s).ok());
    Ok(token.map(|t| t.api_key))
}

/// Create the credential store (tries keyring first, falls back to file).
pub fn create_credential_store() -> Result<MultiCredentialStore> {
    let keyring = KeyringCredentialStore::new("memnoth");
    let rt = Runtime::new()?;
    let file = rt.block_on(FileCredentialStore::new())?;
    Ok(MultiCredentialStore::new(vec![
        CredentialStore::Keyring(keyring),
        CredentialStore::File(file),
    ]))
}

/// Open a URL in the default browser.
fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        anyhow::bail!("unsupported platform for opening browser");
    }
}
