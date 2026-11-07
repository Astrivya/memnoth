//! `mn keys` — the org's API keys.
//!
//! The console's `/app/api-keys` page and this verb are the same three routes:
//! list, mint, revoke (`revoke-all` is the console's panic button). What matters
//! here is what a key *is* on the wire: the server stores only a hash, so the
//! secret exists exactly once — in the response that minted it. That is why
//! `create` prints the token with a warning instead of quietly putting it in the
//! table, and why `list` can never show one: a list that could re-print a live
//! credential would make every `mn keys list | tee` a key leak.
//!
//! Revoked keys stay in the list on the server (a dead credential is a fact), so
//! they are rendered under their own heading rather than dropped or counted as
//! live.

use anyhow::Result;
use memnoth_cloud_client::{ApiKeyInfo, CreateKeyRequest, CreateKeyResponse};
use serde_json::{Value, json};

use crate::AppContext;
use crate::output::{Outcome, confirm};

/// Execute a `mn keys` subcommand.
pub fn run(ctx: &AppContext, args: &crate::cli::KeysArgs) -> Result<Outcome> {
    match &args.action {
        crate::cli::KeysCommand::List(_) => list(ctx),
        crate::cli::KeysCommand::Create(create) => create_key(ctx, create),
        crate::cli::KeysCommand::Revoke(revoke) => revoke_key(ctx, revoke),
        crate::cli::KeysCommand::RevokeAll(all) => revoke_all(ctx, all),
    }
}

/// `mn keys list` — live keys, then the revoked ones.
fn list(ctx: &AppContext) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;
    let response = client.api_keys()?;

    let data = json!({
        "keys": response.keys.iter().map(key_json).collect::<Vec<_>>(),
    });

    Ok(Outcome::new("keys.list", render_list(&response.keys), data))
}

/// `mn keys create` — mint a key and show its secret once.
fn create_key(ctx: &AppContext, args: &crate::cli::KeysCreateArgs) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;

    // Every optional field is passed through as it arrived. Substituting a
    // name or a scope here would mint a key with authority the caller never
    // asked for, and the server has a considered default for each.
    let response = client.create_key(CreateKeyRequest {
        name: args.name.clone(),
        scope: args.scope.clone(),
        project_id: args.project.clone(),
        expires_in_days: args.expires_in_days,
    })?;

    let data = json!({
        "key": key_json(&response.key),
        "token": response.token,
    });

    Ok(Outcome::new("keys.create", render_created(&response), data))
}

/// `mn keys revoke <id>` — one key, now.
fn revoke_key(ctx: &AppContext, args: &crate::cli::KeysRevokeArgs) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;
    let response = client.revoke_key(&args.id)?;

    Ok(Outcome::new(
        "keys.revoke",
        render_revoked(&args.id, response.revoked),
        json!({ "id": args.id, "revoked": response.revoked }),
    ))
}

/// `mn keys revoke-all` — every live key in the org.
fn revoke_all(ctx: &AppContext, args: &crate::cli::KeysRevokeAllArgs) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;

    // Asked before the call, not after: the server has no undo, and this ends
    // every CI job and every machine's session in one request.
    let question = "Revoke every live key in this org? Every process using one loses access \
                    immediately, with no undo.";
    if !confirm(question, args.yes) {
        return Ok(Outcome::new(
            "keys.revoke_all",
            "Cancelled; nothing was revoked.\n",
            json!({ "cancelled": true, "revoked": 0 }),
        ));
    }

    let response = client.revoke_all_keys()?;

    Ok(Outcome::new(
        "keys.revoke_all",
        render_revoked_all(response.revoked),
        json!({ "cancelled": false, "revoked": response.revoked }),
    ))
}

/// The list view: two headings, because a revoked key is not an active one.
fn render_list(keys: &[ApiKeyInfo]) -> String {
    if keys.is_empty() {
        return format!(
            "No API keys in this org yet.\n\nMint one: `mn keys create --name \"laptop\"`\n\
             Manage them in the console: {}\n",
            crate::context::console_url("api-keys")
        );
    }

    let (revoked, live): (Vec<&ApiKeyInfo>, Vec<&ApiKeyInfo>) =
        keys.iter().partition(|key| key.revoked_at.is_some());

    let mut out = String::from("API keys\n========\n");

    out.push_str(&format!("\nLive ({})\n", live.len()));
    if live.is_empty() {
        out.push_str("  none\n");
    }
    for key in live {
        out.push_str(&format!("  {}\n", key_line(key)));
    }

    if !revoked.is_empty() {
        out.push_str(&format!("\nRevoked ({})\n", revoked.len()));
        for key in revoked {
            out.push_str(&format!("  {}\n", key_line(key)));
        }
    }

    out.push_str(&format!(
        "\nManage keys in the console: {}\n",
        crate::context::console_url("api-keys")
    ));
    out
}

/// One key on one line: what it is, where it is bound, and whether it is dead.
fn key_line(key: &ApiKeyInfo) -> String {
    let mut parts = vec![key.prefix.clone()];

    if !key.name.is_empty() {
        parts.push(key.name.clone());
    }
    if !key.scope.is_empty() {
        parts.push(format!("scope {}", key.scope));
    }
    parts.push(project_line(key));

    let created = date(&key.created_at);
    if !created.is_empty() {
        parts.push(format!("created {created}"));
    }
    parts.push(match key.last_used_at.as_deref() {
        Some(at) if !at.is_empty() => format!("last used {}", date(at)),
        _ => "never used".to_string(),
    });
    if let Some(expires) = key.expires_at.as_deref().filter(|value| !value.is_empty()) {
        parts.push(format!("expires {}", date(expires)));
    }
    if key.revoked_at.is_some() {
        parts.push("revoked".to_string());
    }

    format!("{}   [{}]", parts.join(" · "), key.id)
}

/// An org-wide key versus one bound to a project.
fn project_line(key: &ApiKeyInfo) -> String {
    match key.project_id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => format!("project {id}"),
        None => "org-wide".to_string(),
    }
}

/// The date part of an RFC3339 timestamp.
///
/// The list is scanned, not read: the time of day of a key's creation answers no
/// question anyone asks, while the day does. An unparseable value is shown as it
/// arrived rather than dropped.
fn date(timestamp: &str) -> String {
    match timestamp.split_once('T') {
        Some((day, _)) => day.to_string(),
        None => timestamp.to_string(),
    }
}

/// The mint view: the secret, once, and what it is attached to.
fn render_created(response: &CreateKeyResponse) -> String {
    let key = &response.key;

    let mut out = String::from("API key created\n===============\n\n");
    out.push_str(&format!("{}\n\n", response.token));
    out.push_str(
        "This is the only time the token is shown: the server keeps a hash, not the key, so \
         it cannot be recovered — save it now, or revoke it and mint another.\n\n",
    );
    out.push_str(&format!("  id      {}\n", key.id));
    out.push_str(&format!("  prefix  {}\n", key.prefix));
    if !key.name.is_empty() {
        out.push_str(&format!("  name    {}\n", key.name));
    }
    if !key.scope.is_empty() {
        out.push_str(&format!("  scope   {}\n", key.scope));
    }
    out.push_str(&format!("  reaches {}\n", project_line(key)));
    if let Some(expires) = key.expires_at.as_deref().filter(|value| !value.is_empty()) {
        out.push_str(&format!("  expires {}\n", date(expires)));
    }

    out
}

/// What one revoke did, including the case where nothing was live.
fn render_revoked(id: &str, revoked: u64) -> String {
    let head = if revoked == 0 {
        format!("Nothing revoked: {id} is not a live key in this org.\n")
    } else {
        format!("Revoked {id} ({revoked} key).\n")
    };

    format!("{head}\nAny process still holding that key loses access immediately.\n")
}

/// What `revoke-all` did.
fn render_revoked_all(revoked: u64) -> String {
    if revoked == 0 {
        return "No live keys to revoke.\n".to_string();
    }

    format!(
        "Revoked {revoked} live key(s) in this org.\n\nEvery process using one loses access \
         immediately. `mn auth login` mints a fresh key for this machine.\n"
    )
}

/// One key as JSON. The secret is never part of it: the server does not send it.
fn key_json(key: &ApiKeyInfo) -> Value {
    json!({
        "id": key.id,
        "name": key.name,
        "prefix": key.prefix,
        "scope": key.scope,
        "project_id": key.project_id,
        "created_at": key.created_at,
        "last_used_at": key.last_used_at,
        "revoked_at": key.revoked_at,
        "expires_at": key.expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str, prefix: &str, revoked: Option<&str>) -> ApiKeyInfo {
        ApiKeyInfo {
            id: id.into(),
            name: "deploy".into(),
            prefix: prefix.into(),
            scope: "write".into(),
            project_id: None,
            created_at: "2026-10-01T09:12:00Z".into(),
            last_used_at: Some("2026-11-02T18:30:00Z".into()),
            revoked_at: revoked.map(str::to_string),
            expires_at: None,
        }
    }

    #[test]
    fn an_empty_list_points_at_create_and_the_console() {
        let text = render_list(&[]);
        assert!(text.contains("No API keys"), "{text}");
        assert!(text.contains("mn keys create"), "{text}");
        assert!(text.contains("/app/api-keys"), "{text}");
    }

    #[test]
    fn a_revoked_key_is_shown_under_revoked_and_not_counted_as_live() {
        let keys = vec![
            key("a1", "mn_live_aaaa", None),
            key("b2", "mn_live_bbbb", Some("2026-10-20T00:00:00Z")),
        ];
        let text = render_list(&keys);

        assert!(text.contains("Live (1)"), "{text}");
        assert!(text.contains("Revoked (1)"), "{text}");
        // The dead key's line says so, and appears after the heading.
        let revoked_at = text.find("Revoked (1)").expect("heading");
        let dead = text.find("mn_live_bbbb").expect("dead key row");
        assert!(dead > revoked_at, "{text}");
        assert!(
            text.contains("mn_live_bbbb · deploy · scope write · org-wide"),
            "{text}"
        );
    }

    #[test]
    fn a_key_with_no_project_is_org_wide() {
        let text = render_list(&[key("a1", "mn_live_aaaa", None)]);
        assert!(text.contains("org-wide"), "{text}");
    }

    #[test]
    fn a_project_bound_key_names_the_project() {
        let mut bound = key("a1", "mn_live_aaaa", None);
        bound.project_id = Some("4f2a".into());
        let text = render_list(&[bound]);
        assert!(text.contains("project 4f2a"), "{text}");
        assert!(!text.contains("org-wide"), "{text}");
    }

    #[test]
    fn the_mint_view_prints_the_token_with_its_consequence() {
        let response = CreateKeyResponse {
            key: key("a1", "mn_live_aaaa", None),
            token: "mn_live_aaaa_secret".into(),
        };
        let text = render_created(&response);

        assert!(text.contains("mn_live_aaaa_secret"), "{text}");
        assert!(text.contains("only time the token is shown"), "{text}");
        assert!(text.contains("id      a1"), "{text}");
    }

    #[test]
    fn revoking_a_key_that_was_not_live_says_so_rather_than_claiming_success() {
        let text = render_revoked("a1", 0);
        assert!(text.contains("Nothing revoked"), "{text}");
        assert!(!text.contains("Revoked a1"), "{text}");
    }

    #[test]
    fn revoking_reports_the_server_count() {
        let text = render_revoked("a1", 1);
        assert!(text.contains("Revoked a1 (1 key)"), "{text}");
        assert!(text.contains("loses access immediately"), "{text}");
    }

    #[test]
    fn revoke_all_with_nothing_live_does_not_invent_a_count() {
        let text = render_revoked_all(0);
        assert_eq!(text, "No live keys to revoke.\n");
    }

    #[test]
    fn revoke_all_reports_how_many_died() {
        let text = render_revoked_all(3);
        assert!(text.contains("Revoked 3 live key(s)"), "{text}");
        assert!(text.contains("mn auth login"), "{text}");
    }
}
