//! Authentication commands: login, logout, auth status, whoami.
//!
//! Implements the device authorization flow (RFC 8628) against the memnoth cloud API.

use anyhow::{Context, Result};
use std::io::IsTerminal;
use std::thread;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

/// How often a wait reports in, and how long a rate-limited poll may push the
/// next check out. Both are presentation bounds, not protocol ones: RFC 8628
/// fixes the cadence at the interval the server named.
const HEARTBEAT: Duration = Duration::from_secs(5);
const MAX_POLL_WAIT: Duration = Duration::from_secs(60);

use crate::cli::{AuthLoginArgs, AuthLogoutArgs, AuthStatusArgs, WhoamiArgs};
use crate::context::AppContext;
use crate::output::Outcome;
use memnoth_auth::{
    CredentialStore, FileCredentialStore, KeyringCredentialStore, MultiCredentialStore,
};
use memnoth_cloud_client::{
    ApprovedAccountKey, CloudClient, DeviceAuthRequest, DeviceAuthResponse,
};
pub fn login(ctx: &AppContext, args: &AuthLoginArgs) -> Result<Outcome> {
    // Handle --with-token flag for CI/paste path
    if let Some(token) = &args.with_token {
        return login_with_token(ctx, token);
    }

    // Start device authorization. The request and the wait are shared with the
    // session screen; everything printed here is this command's presentation,
    // so the TUI can show the same phases without a `println!` landing in the
    // middle of a frame.
    let client_name = args
        .client_name
        .clone()
        .unwrap_or_else(|| "memnoth-cli".to_string());

    if !ctx.json {
        println!("memnoth sign-in");
        println!("  asking memnoth cloud for a one-time code…");
    }
    let (client, auth) = begin_device_login(&client_name)?;
    let url = auth.verification_uri_complete.clone();

    if ctx.json {
        // A JSON caller still has to show someone the code, and stdout belongs
        // to the one envelope at the end: the grant goes to stderr as its own
        // line, so a wrapper can render it and a parser never sees it.
        eprintln!(
            "{}",
            serde_json::json!({
                "event": "device_code",
                "user_code": auth.user_code,
                "verification_url": url,
                "verification_uri": auth.verification_uri,
                "expires_in": auth.expires_in,
            })
        );
    } else {
        println!();
        println!("  code     {}", auth.user_code);
        println!("  link     {url}");
        println!(
            "  expires  in {}",
            human_duration(Duration::from_secs(auth.expires_in))
        );
    }

    // The link is shown, never opened on its own. A browser that appears
    // unbidden is a surprise on a desk and a hard failure on a headless box, so
    // the person decides: Enter opens it, `c` copies it, `q` stops.
    //
    // Keys are read only when there is a terminal to read them from: a pipe, a
    // `--json` caller, or `--no-browser` all fall back to printing the link and
    // waiting, which is what a CI job needs.
    let terminal = !args.no_browser && !ctx.json && std::io::stdin().is_terminal();
    let raw = if terminal { RawKeys::enable() } else { None };
    // Keys are promised only when raw mode actually took: a terminal that
    // refuses it cannot deliver a keystroke, and a hint that does nothing is
    // worse than no hint at all.
    let interactive = terminal && raw.is_some();
    if interactive {
        say(interactive, "");
        say(
            interactive,
            "  press Enter to open it in your browser · c to copy the link · q to cancel",
        );
        say(interactive, "");
    }

    let mut waiter = |wait: Duration| -> bool {
        if !interactive {
            thread::sleep(wait);
            return true;
        }
        // Sliced so a keypress is answered now rather than after the next poll
        // interval: the wait is the whole flow, so it cannot be unresponsive.
        let step = Duration::from_millis(100);
        let mut left = wait;
        while left > Duration::ZERO {
            while let Some(key) = read_key() {
                match key {
                    Key::Open => match open_browser(&url) {
                        Ok(()) => say(interactive, &format!("  opened {url}")),
                        Err(error) => say(
                            interactive,
                            &format!("  could not open a browser ({error}); open the link above"),
                        ),
                    },
                    Key::Copy => match copy_to_clipboard(&url) {
                        Ok(()) => say(interactive, "  copied the link to the clipboard"),
                        Err(error) => say(
                            interactive,
                            &format!("  could not copy ({error}); select the link above"),
                        ),
                    },
                    Key::Cancel => return false,
                    Key::Ignore => {}
                }
            }
            let slice = left.min(step);
            thread::sleep(slice);
            left -= slice;
        }
        true
    };

    // Starts already due, so the first "waiting" line is printed rather than
    // delayed by one heartbeat: the wait itself is the news.
    let mut reported = Instant::now()
        .checked_sub(HEARTBEAT)
        .unwrap_or_else(Instant::now);
    let mut on_phase = |phase: LoginPhase| {
        if ctx.json {
            return;
        }
        if let LoginPhase::Awaiting { attempt, elapsed } = phase
            && reported.elapsed() >= HEARTBEAT
        {
            // A heartbeat rather than a spinner: logs and journals keep every
            // line, and a repainted line is unreadable in a pipe.
            say(
                interactive,
                &format!(
                    "  waiting for approval… {} (check {attempt})",
                    human_duration(elapsed)
                ),
            );
            reported = Instant::now();
        }
    };

    // Poll until the browser side approves. The minted key carries the tier the
    // server actually granted; `--tier` can only narrow what this machine asks
    // for, so it wins when given.
    let approved = await_device_login(&client, &auth, &mut on_phase, &mut waiter)?;
    // The wait is over: give the terminal back before printing the result, so
    // the lines below end with a normal newline.
    drop(raw);
    let Some(token) = approved else {
        anyhow::bail!("sign-in cancelled — nothing was stored")
    };

    // Storing is local and instant, so the person is told who they are before
    // the two enrichment reads that used to sit in front of this line.
    if !ctx.json {
        println!();
        println!("  approved in the browser — saving credentials…");
    }
    let signed_in = store_login(&token, args.tier.as_deref())?;
    if !ctx.json {
        println!("signed in as {}", signed_in.email);
    }

    // The organisation's name, this account's role, and the key's deadline are
    // three more round trips. They improve the answer; they do not decide
    // whether the sign-in worked, so a network that cannot serve them leaves a
    // working credential and one honest line.
    let mut done = signed_in.clone();
    match enrich_login(&token, args.tier.as_deref()) {
        Ok(enriched) => {
            done = enriched;
            if !ctx.json {
                println!("  organization  {}", done.org_name);
                println!("  tier          {}", done.tier);
            }
        }
        Err(error) => {
            if !ctx.json {
                println!("  organization  unavailable right now ({error:#})");
            }
        }
    }

    Ok(Outcome::new(
        "auth.login",
        String::new(),
        serde_json::json!({
            "account_id": done.account_id,
            "email": done.email,
            "org_name": done.org_name,
            "tier": done.tier,
            "status": done.status,
        }),
    ))
}

/// `15m`, `45s`, `1m 05s`, `2h 09m`, `29d 04h`.
///
/// Shared so every surface counts a span the same way: the wait for a code, the
/// footer's count, and how long a key has left. The unit is the largest one that
/// still says something — a key with 29 days left is not usefully "41760m".
pub(crate) fn human_duration(wait: Duration) -> String {
    let secs = wait.as_secs();
    if secs >= 86_400 {
        format!("{}d {:02}h", secs / 86_400, (secs % 86_400) / 3_600)
    } else if secs >= 3_600 {
        format!("{}h {:02}m", secs / 3_600, (secs % 3_600) / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Login with a pre-provided token (CI/paste path).
fn login_with_token(ctx: &AppContext, token: &str) -> Result<Outcome> {
    let mut client = CloudClient::from_env().context("failed to create cloud client")?;
    client.set_auth_token(token.to_string());

    // Verify the token works by fetching who it is. `/me` is a token's view of
    // itself — subject, org scope, roles — and carries no email or profile, so
    // the subject is the only stable name this path can show.
    let me = client.me().context("invalid token")?;
    let org = client.org().context("failed to fetch org")?;
    let account_id = me
        .sub
        .clone()
        .or_else(|| me.org.clone())
        .unwrap_or_else(|| "unknown".to_string());
    // Store the token
    let stored_token = StoredToken {
        api_key: token.to_string(),
        account_id: account_id.clone(),
        email: account_id.clone(),
        org_id: org.slug.clone(),
        org_name: org.name.clone(),
        tier: "unknown".to_string(),
        status: "active".to_string(),
        expires_at: key_expiry_ms(&client, token),
    };
    store_token(&stored_token)?;

    let whoami = StoredWhoami {
        account_id: account_id.clone(),
        email: account_id,
        name: None,
        org_id: org.slug,
        org_name: org.name,
        role: org.role,
    };
    cache_whoami_direct(&whoami)?;
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

/// What a completed sign-in leaves behind.
#[derive(Debug, Clone)]
pub struct SignedIn {
    pub account_id: String,
    pub email: String,
    pub org_name: String,
    /// The tier asked for, or `unknown` when the flow did not say.
    pub tier: String,
    pub status: String,
}

/// Start the device authorization request. Prints nothing, opens nothing.
///
/// The caller owns the presentation: `mn auth login` prints the code, the
/// session screen draws it in a frame. Sharing only the request is what keeps
/// one flow behind both surfaces.
pub fn begin_device_login(client_name: &str) -> Result<(CloudClient, DeviceAuthResponse)> {
    let client = CloudClient::from_env().context("failed to create cloud client")?;
    let mut auth = client
        .device_auth(DeviceAuthRequest {
            client_name: client_name.to_string(),
        })
        .context("failed to start device authorization")?;

    // The server names the approval page. A deployment whose public origin is a
    // bare host answers `/api-keys`, where the console bundle is not served, so
    // the one-time link 404s and the sign-in can never be approved. Normalised
    // here, once, because both surfaces — `mn auth login` and `/login` in the
    // session screen — print what this returns and open it unchanged.
    auth.verification_uri = crate::context::mounted_console_page(&auth.verification_uri);
    auth.verification_uri_complete =
        crate::context::mounted_console_page(&auth.verification_uri_complete);

    Ok((client, auth))
}

/// What one poll came back as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Poll {
    /// `428 authorization_pending`: nobody has approved yet.
    Pending,
    /// The server asked us to slow down; carry its penalty.
    SlowDown(Duration),
    /// `400 access_denied`: a person said no, and that is final.
    Denied,
    /// `400 expired_token`: the grant is gone; a new flow is the only way in.
    Expired,
}

/// The pause before the next poll.
///
/// RFC 8628 fixes the cadence at the interval the server named, and only a
/// `slow_down` may lengthen it. This used to double on *every* pending answer —
/// 3s, 6s, 12s, 24s, 30s — so once a flow had been waiting half a minute an
/// approval in the browser sat unnoticed for up to another thirty seconds. The
/// interval is the contract; waiting is not a backoff problem.
fn next_wait(interval: Duration, poll: Poll) -> Duration {
    match poll {
        Poll::SlowDown(penalty) => (interval + penalty).min(MAX_POLL_WAIT),
        _ => interval,
    }
}

/// Read a poll failure as a protocol state, or `None` when it is a real error.
fn classify_poll(error: &memnoth_cloud_client::CloudError) -> Option<Poll> {
    use memnoth_cloud_client::CloudError;
    match error {
        CloudError::ClientError { status: 428, .. } => Some(Poll::Pending),
        CloudError::ClientError {
            status: 400,
            message,
        } if message.contains("denied") => Some(Poll::Denied),
        CloudError::ClientError {
            status: 400,
            message,
        } if message.contains("expired") => Some(Poll::Expired),
        CloudError::RateLimited {
            retry_after_secs, ..
        } => Some(Poll::SlowDown(Duration::from_secs(
            (*retry_after_secs).clamp(1, 60),
        ))),
        _ => None,
    }
}

/// A stage of the wait, reported so both surfaces can show the same thing.
#[derive(Debug, Clone, Copy)]
pub enum LoginPhase {
    /// A poll came back "not yet": `attempt` polls done, `elapsed` since the wait
    /// began.
    Awaiting { attempt: u32, elapsed: Duration },
}

/// Block until the browser side approves, is denied, or the code expires.
///
/// Polling is the RFC 8628 contract: a 428 means "not yet", a 400 that says
/// denied or expired is final, and anything else is a real failure. `on_phase`
/// is told about each wait so a surface can report progress; `waiter` owns the
/// sleeping and returns `false` when the person cancelled.
///
/// `Ok(None)` is that cancellation: nothing was approved and nothing stored.
pub fn await_device_login(
    client: &CloudClient,
    auth: &DeviceAuthResponse,
    on_phase: &mut dyn FnMut(LoginPhase),
    waiter: &mut dyn FnMut(Duration) -> bool,
) -> Result<Option<ApprovedAccountKey>> {
    let start = Instant::now();
    let timeout = Duration::from_secs(auth.expires_in);
    // The server's own cadence, floored at a second: a zero interval would spin.
    let interval = Duration::from_secs(auth.interval.max(1)).min(MAX_POLL_WAIT);
    let mut attempt = 0u32;

    loop {
        if start.elapsed() >= timeout {
            anyhow::bail!(
                "the one-time code expired after {} without an approval — run `memnoth auth login` again",
                human_duration(timeout)
            );
        }

        attempt += 1;
        let wait = match client.device_token(&auth.device_code) {
            Ok(key) => {
                on_phase(LoginPhase::Awaiting {
                    attempt,
                    elapsed: start.elapsed(),
                });
                return Ok(Some(key));
            }
            Err(error) => match classify_poll(&error) {
                Some(Poll::Denied) => {
                    anyhow::bail!("the sign-in was denied on the web page — nothing was stored")
                }
                Some(Poll::Expired) => {
                    anyhow::bail!("the one-time code expired — run `memnoth auth login` again")
                }
                Some(poll) => {
                    on_phase(LoginPhase::Awaiting {
                        attempt,
                        elapsed: start.elapsed(),
                    });
                    next_wait(interval, poll)
                }
                None => return Err(error).context("the sign-in poll failed"),
            },
        };

        if !waiter(wait) {
            return Ok(None);
        }
    }
}

/// Turn an approved key into a stored credential, with no network at all.
///
/// The token response *is* the identity — `api_key`, `account_id`, `email`,
/// `tier`, `status` — because the cloud mints an API key rather than an OAuth
/// token. That means the person can be told who they are the instant the poll
/// returns, instead of after the org read the old flow insisted on first.
///
/// The organisation is left blank here on purpose: `enrich_login` fills it in,
/// and an empty org is the same thing `signed_in_identity` reports for an
/// account that has none.
pub fn store_login(token: &ApprovedAccountKey, tier: Option<&str>) -> Result<SignedIn> {
    let whoami = StoredWhoami {
        account_id: token.account_id.clone(),
        email: token.email.clone(),
        name: None,
        org_id: String::new(),
        org_name: String::new(),
        role: String::new(),
    };

    let stored_token = StoredToken {
        api_key: token.api_key.clone(),
        account_id: whoami.account_id.clone(),
        email: whoami.email.clone(),
        org_id: whoami.org_id.clone(),
        org_name: whoami.org_name.clone(),
        tier: tier.unwrap_or(&token.tier).to_string(),
        status: token.status.clone(),
        expires_at: None,
    };
    store_token(&stored_token)?;
    cache_whoami_direct(&whoami)?;

    Ok(SignedIn {
        account_id: whoami.account_id,
        email: whoami.email,
        org_name: whoami.org_name,
        tier: stored_token.tier,
        status: stored_token.status,
    })
}

/// Fill in the organisation and the key's deadline behind an already-stored
/// credential.
///
/// `/org` is where the org's name and the caller's role are reported, and the
/// deadline comes from the org's key list; both are enrichment reads that a
/// sign-in survives without. The caller keeps the credential it stored and
/// prints whatever error this returns as one line.
pub fn enrich_login(token: &ApprovedAccountKey, tier: Option<&str>) -> Result<SignedIn> {
    let mut authed = CloudClient::from_env().context("failed to create cloud client")?;
    authed.set_auth_token(token.api_key.clone());
    let org = authed.org().context("failed to fetch the organization")?;

    let whoami = StoredWhoami {
        account_id: token.account_id.clone(),
        email: token.email.clone(),
        name: None,
        org_id: org.slug.clone(),
        org_name: org.name.clone(),
        role: org.role.clone(),
    };
    let tier = tier.unwrap_or(&token.tier).to_string();
    let stored_token = StoredToken {
        api_key: token.api_key.clone(),
        account_id: whoami.account_id.clone(),
        email: whoami.email.clone(),
        org_id: whoami.org_id.clone(),
        org_name: whoami.org_name.clone(),
        tier: tier.clone(),
        status: token.status.clone(),
        expires_at: key_expiry_ms(&authed, &token.api_key),
    };
    store_token(&stored_token)?;
    cache_whoami_direct(&whoami)?;

    Ok(SignedIn {
        account_id: whoami.account_id,
        email: whoami.email,
        org_name: whoami.org_name,
        tier,
        status: stored_token.status,
    })
}

/// Who this machine is signed in as, including the team it belongs to.
///
/// The team is not a mode the CLI switches into: it is the org the account is
/// already in, cached beside the email by the last `whoami`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The account's email.
    pub email: String,
    /// The organisation's display name, empty when the account has none.
    pub org_name: String,
    /// This account's role in that organisation (`member`, `admin`, …).
    pub role: String,
}

impl Identity {
    /// Whether the account belongs to a named organisation.
    pub fn in_team(&self) -> bool {
        !self.org_name.trim().is_empty()
    }
}

/// The cached identity, if this machine has one.
///
/// Never fails: a store that cannot be read is the same answer as no store,
/// because a status line must not be able to break a frame or a command.
pub fn signed_in_identity() -> Option<Identity> {
    let store = create_credential_store().ok()?;
    let whoami: StoredWhoami = store
        .get_sync("whoami")
        .ok()?
        .and_then(|serialized| serde_json::from_str(&serialized).ok())?;
    Some(Identity {
        email: whoami.email,
        org_name: whoami.org_name,
        role: whoami.role,
    })
}

/// The cached signed-in email, if this machine has one.
pub fn signed_in_account() -> Option<String> {
    signed_in_identity().map(|identity| identity.email)
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
    /// When the server says this key stops working, in unix milliseconds.
    ///
    /// `None` is not "no expiry" and not "unknown because nobody looked": it is
    /// the server reporting no deadline, or the key list being unreadable at the
    /// moment the key was stored. Both are stated as unknown rather than turned
    /// into a date nobody promised.
    #[serde(default)]
    expires_at: Option<i64>,
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

/// The deadline the server reports for the key a login just minted.
///
/// Neither the device-approval nor the `/me` response carries one for the key
/// itself, so it is read back from the org's key list and matched by prefix —
/// still the server's own answer, never a lifetime this file made up. A list
/// that cannot be read leaves the deadline unknown, which the status line says.
fn key_expiry_ms(client: &CloudClient, api_key: &str) -> Option<i64> {
    let keys = client.api_keys().ok()?;
    keys.keys
        .iter()
        .find(|key| !key.prefix.is_empty() && api_key.starts_with(&key.prefix))
        .and_then(|key| key.expires_at.as_deref())
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.timestamp_millis())
}

/// One stored token's deadline, and whether it is already behind us.
fn expiry_state(token: Option<&StoredToken>) -> (Option<i64>, bool) {
    let expires_at = token.and_then(|token| token.expires_at);
    let expired = expires_at.is_some_and(|at| at <= crate::session::now_ms());
    (expires_at, expired)
}

/// The status line for a deadline: the date and the time left, or the reason
/// there is none.
///
/// One phrasing behind `auth status`, `whoami` and the session screen, so a
/// credential cannot be described two ways on one machine. The date is the
/// server's; the span is this machine's clock against it, which is why it is
/// rounded down rather than presented as a promise.
fn expiry_text(token: Option<&StoredToken>) -> String {
    let (expires_at, expired) = expiry_state(token);
    let Some(at) = expires_at else {
        return "unknown (the server reported no deadline)".to_string();
    };
    let stamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at)
        .map(|at| at.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| at.to_string());
    if expired {
        return format!("{stamp} — expired, run `mn auth login`");
    }
    let left = at.saturating_sub(crate::session::now_ms()).max(0) as u64;
    format!(
        "{stamp} (in {})",
        human_duration(Duration::from_millis(left))
    )
}

/// Store the API token in the credential store.
fn store_token(stored_token: &StoredToken) -> Result<()> {
    let store = create_credential_store()?;

    // Store API key
    store.set_sync("api_key", stored_token.api_key.clone())?;

    // Store token metadata as JSON
    let metadata = serde_json::to_string(stored_token)?;
    store.set_sync("token_metadata", metadata)?;

    Ok(())
}

/// Cache whoami info directly.
fn cache_whoami_direct(whoami: &StoredWhoami) -> Result<()> {
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
        let mut client = CloudClient::from_env().context("failed to create cloud client")?;
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

    // The plan and seat counts are the server's, not the cached token's: a
    // status line that reports yesterday's plan is worse than one that says the
    // live read failed. This is best effort and never makes the command fail.
    let live = store
        .get_sync("api_key")
        .ok()
        .flatten()
        .and_then(|api_key| {
            let mut client = CloudClient::from_env().ok()?;
            client.set_auth_token(api_key);
            Some((client.org().ok(), client.entitlements().ok()))
        });
    let (live_org, live_limits) = live.unwrap_or((None, None));

    let authenticated = whoami.is_some();
    let (_expires_at, _expired) = expiry_state(token.as_ref());

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
        "plan": live_org.as_ref().map(|org| org.plan.clone()),
        "seats": live_org.as_ref().map(|org| org.seats),
        "seats_used": live_org.as_ref().map(|org| org.seats_used),
        "member_count": live_org.as_ref().map(|org| org.member_count),
        "limits": live_limits.as_ref().map(|limits| serde_json::json!({
            "workspaces": limits.workspaces,
            "history_days": limits.history_days,
            "audit_log": limits.audit_log,
            "sso": limits.sso,
            "scim": limits.scim,
        })),
        "checkout_url": crate::context::console_url("billing/checkout"),
        "portal_url": crate::context::console_url("billing/portal"),
        // The page that shows the plan and sells seats. Named here so a script
        // never has to reconstruct the console mount by hand.
        "billing_url": crate::context::console_url("org"),
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
            if let Some(org) = &live_org {
                println!("Plan:          {}", org.plan);
                println!("Seats:         {} / {}", org.seats_used, org.seats);
            }
            if let Some(limits) = &live_limits {
                println!(
                    "Limits:        workspaces {}, history {}",
                    limit_label(limits.workspaces),
                    history_label(limits.history_days)
                );
            }
            println!(
                "Checkout:      {}",
                crate::context::console_url("billing/checkout")
            );
            println!(
                "Portal:        {}",
                crate::context::console_url("billing/portal")
            );
            // Printed even when unknown: a person checking a credential needs
            // to know whether the deadline is missing or merely far away, and a
            // missing line reads as "no expiry at all".
            println!("Expires:       {}", expiry_text(token.as_ref()));
            println!("Billing:       {}", crate::context::console_url("org"));
        } else {
            println!("Authenticated: no");
        }
    }

    Ok(Outcome::new("auth.status", String::new(), data))
}

fn limit_label(limit: Option<i64>) -> String {
    limit
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unlimited".to_string())
}

fn history_label(days: Option<i64>) -> String {
    days.map(|value| format!("{value} days"))
        .unwrap_or_else(|| "unlimited".to_string())
}

/// Run the `mn auth whoami` command.
pub fn whoami(ctx: &AppContext, _args: &WhoamiArgs) -> Result<Outcome> {
    let store = create_credential_store()?;
    let whoami: Option<StoredWhoami> = store
        .get_sync("whoami")?
        .and_then(|s| serde_json::from_str(&s).ok());
    // The key's deadline lives beside the token, not in `whoami`, so it is read
    // here for the same reason `auth status` reads it: "who am I" on a machine
    // that asks you to sign in again tomorrow is two facts, not one.
    let token: Option<StoredToken> = store
        .get_sync("token_metadata")?
        .and_then(|s| serde_json::from_str(&s).ok());
    let (expires_at, expired) = expiry_state(token.as_ref());
    let expires = expiry_text(token.as_ref());

    let data = if let Some(w) = whoami {
        if !ctx.json {
            println!("{} ({})", w.email, w.account_id);
            println!("expires {expires}");
        }
        serde_json::json!({
            "account_id": w.account_id,
            "email": w.email,
            "name": w.name,
            "org_id": w.org_id,
            "org_name": w.org_name,
            "role": w.role,
            "expires_at": expires_at,
            "expired": expired,
            "expires": expires,
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
            "expires_at": null,
            "expired": false,
            "expires": null,
        })
    };

    Ok(Outcome::new("auth.whoami", String::new(), data))
}
/// Get stored API token from credential store.
pub fn get_stored_token(_ctx: &AppContext) -> Result<Option<String>> {
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
///
/// Shared with the session screen's sign-in worker, which needs the same
/// platform switch and nothing else.
pub(crate) fn open_browser(url: &str) -> Result<()> {
    // One statement per platform and a single `Ok(())` at the end: a `return`
    // inside the block that happens to be last on this host is a needless
    // return there, and clippy says so on that host only.
    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(url).spawn()?;
    #[cfg(target_os = "windows")]
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open").arg(url).spawn()?;
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        anyhow::bail!("unsupported platform for opening browser");
    }
    Ok(())
}

/// Print one line while the terminal may be in raw mode.
///
/// Raw mode turns output post-processing off, so a bare `\n` moves down without
/// returning the carriage and each line starts one column further right. The
/// carriage is written here instead of trusting the terminal to add it.
fn say(raw: bool, text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let written = if raw {
        write!(out, "{text}\r\n")
    } else {
        writeln!(out, "{text}")
    };
    let _ = written.and_then(|()| out.flush());
}

/// One key the sign-in wait answers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Open,
    Copy,
    Cancel,
    Ignore,
}

/// Raw mode for as long as the wait needs single keystrokes.
///
/// A guard rather than a paired call: the wait has several exits, and one of
/// them is an error, and leaving a terminal in raw mode without echo is worse
/// than the slow sign-in this replaces.
struct RawKeys;

impl RawKeys {
    fn enable() -> Option<Self> {
        crossterm::terminal::enable_raw_mode().ok().map(|_| Self)
    }
}

impl Drop for RawKeys {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// The next keypress, or `None` when nothing is waiting.
///
/// Non-blocking on purpose: the caller slices its sleep so a keypress is
/// answered within a tick instead of after the next poll interval.
fn read_key() -> Option<Key> {
    use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    loop {
        if !event::poll(Duration::ZERO).ok()? {
            return None;
        }
        let Ok(event) = event::read() else {
            return None;
        };
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) = event
        else {
            continue;
        };
        if kind == KeyEventKind::Release {
            continue;
        }
        return Some(if modifiers.contains(KeyModifiers::CONTROL) {
            match code {
                KeyCode::Char('c') => Key::Cancel,
                _ => Key::Ignore,
            }
        } else {
            match code {
                KeyCode::Enter => Key::Open,
                KeyCode::Char('c') | KeyCode::Char('C') => Key::Copy,
                KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => Key::Cancel,
                _ => Key::Ignore,
            }
        });
    }
}

/// Put the link on the system clipboard.
///
/// No clipboard crate: each platform already ships the tool, and a dependency
/// that duplicates three command lines is one more thing to keep patched.
fn copy_to_clipboard(text: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    return feed_clipboard(clipboard_tool("cmd", &["/C", "clip"])?, text);
    #[cfg(target_os = "macos")]
    return feed_clipboard(clipboard_tool("pbcopy", &[])?, text);
    #[cfg(target_os = "linux")]
    {
        // Wayland first, then the two X11 tools: whichever one exists is the
        // one the session actually uses.
        for (program, args) in [
            ("wl-copy", &[][..]),
            ("xclip", &["-selection", "clipboard"][..]),
            ("xsel", &["--clipboard", "--input"][..]),
        ] {
            if let Ok(child) = clipboard_tool(program, args) {
                return feed_clipboard(child, text);
            }
        }
        anyhow::bail!("no clipboard tool found (wl-copy, xclip, xsel) — copy the link by hand")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = text;
        anyhow::bail!("no clipboard on this platform — copy the link by hand")
    }
}

fn clipboard_tool(program: &str, args: &[&str]) -> Result<std::process::Child> {
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("cannot run `{program}`"))
}

fn feed_clipboard(mut child: std::process::Child, text: &str) -> Result<()> {
    use std::io::Write;
    let mut stdin = child
        .stdin
        .take()
        .context("the clipboard tool took no input")?;
    stdin
        .write_all(text.as_bytes())
        .context("clipboard write failed")?;
    drop(stdin);
    let status = child.wait().context("the clipboard tool did not exit")?;
    if !status.success() {
        anyhow::bail!("the clipboard tool exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(expires_at: Option<i64>) -> StoredToken {
        StoredToken {
            api_key: "mn_live_test".into(),
            account_id: "acct".into(),
            email: "a@example.com".into(),
            org_id: "acme".into(),
            org_name: "Acme".into(),
            tier: "pro".into(),
            status: "active".into(),
            expires_at,
        }
    }

    #[test]
    fn a_token_stored_before_expiry_tracking_still_loads() {
        // The field was added later, so every credential already on disk lacks
        // it. Refusing those would sign people out on upgrade.
        let legacy = r#"{"api_key":"mn_live_test","account_id":"acct",
            "email":"a@example.com","org_id":"acme","org_name":"Acme",
            "tier":"pro","status":"active"}"#;
        let token: StoredToken = serde_json::from_str(legacy).expect("legacy token loads");
        assert_eq!(token.expires_at, None);
        assert_eq!(expiry_state(Some(&token)), (None, false));
        assert!(expiry_text(Some(&token)).contains("unknown"));
    }

    #[test]
    fn a_pending_poll_never_stretches_the_servers_interval() {
        // The bug this replaces doubled the interval on every pending answer, so
        // after half a minute of waiting an approval took up to thirty seconds
        // more to notice. The interval the server named is the cadence, full
        // stop; only `slow_down` may lengthen it.
        let interval = Duration::from_secs(3);
        for _ in 0..30 {
            assert_eq!(
                next_wait(interval, Poll::Pending),
                interval,
                "a pending answer moved the cadence"
            );
        }
    }

    #[test]
    fn a_rate_limited_poll_lengthens_the_wait_and_the_cap_holds() {
        let interval = Duration::from_secs(3);
        assert_eq!(
            next_wait(interval, Poll::SlowDown(Duration::from_secs(5))),
            Duration::from_secs(8)
        );
        assert_eq!(
            next_wait(interval, Poll::SlowDown(Duration::from_secs(60))),
            MAX_POLL_WAIT,
            "a penalty past the cap is still bounded by the cap"
        );
    }

    #[test]
    fn the_servers_statuses_are_read_as_protocol_states() {
        use memnoth_cloud_client::CloudError;
        assert_eq!(
            classify_poll(&CloudError::ClientError {
                status: 428,
                message: "authorization_pending".into(),
            }),
            Some(Poll::Pending)
        );
        assert_eq!(
            classify_poll(&CloudError::ClientError {
                status: 400,
                message: "access_denied".into(),
            }),
            Some(Poll::Denied)
        );
        assert_eq!(
            classify_poll(&CloudError::ClientError {
                status: 400,
                message: "expired_token".into(),
            }),
            Some(Poll::Expired)
        );
        assert_eq!(
            classify_poll(&CloudError::RateLimited {
                retry_after_secs: 7,
                message: "slow down".into(),
            }),
            Some(Poll::SlowDown(Duration::from_secs(7)))
        );
        // A real failure is not a state to keep waiting in.
        assert_eq!(
            classify_poll(&CloudError::NetworkError("offline".into())),
            None
        );
    }

    #[test]
    fn a_deadline_in_the_past_is_reported_as_expired_not_hidden() {
        let past = crate::session::now_ms() - 60_000;
        let (at, expired) = expiry_state(Some(&stored(Some(past))));
        assert_eq!(at, Some(past));
        assert!(expired, "a deadline behind us is an expired credential");
        let line = expiry_text(Some(&stored(Some(past))));
        assert!(line.contains("expired"), "{line}");
        assert!(line.contains("mn auth login"), "{line}");

        let future = crate::session::now_ms() + 3_600_000;
        let (_, expired) = expiry_state(Some(&stored(Some(future))));
        assert!(!expired);
        assert!(!expiry_text(Some(&stored(Some(future)))).contains("expired"));
    }
}
