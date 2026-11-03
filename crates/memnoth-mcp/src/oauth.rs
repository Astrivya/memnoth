//! OAuth 2.0 Protected Resource Metadata (RFC 9728) for the HTTP transport.
//!
//! The 2026-07-28 authorization spec leaves no discretion about this one:
//!
//! > MCP servers MUST implement OAuth 2.0 Protected Resource Metadata (RFC9728).
//! > MCP clients MUST use OAuth 2.0 Protected Resource Metadata for
//! > authorization server discovery.
//!
//! A host that has a token to present still has to *find out where to get one*
//! and *what to ask for*, and the only address it knows is ours. With no
//! metadata document, that host gets a 401 with nothing to act on, and the
//! installation stops there — which is why this is worth doing before the
//! fancier auth work.
//!
//! Two things are deliberately absent, because emitting them would be a lie:
//!
//! * **`scopes_supported`** unless `MEMNOTH_MCP_SCOPES` names scopes. The spec
//!   says the field "is intended to represent the minimal set of scopes
//!   necessary for basic functionality", and it explicitly permits omitting the
//!   `scope` parameter when `scopes_supported` is undefined.
//! * **A `scope` in the 401 challenge.** Servers SHOULD include the scopes
//!   needed *for the operation being attempted*; no tool here declares a
//!   required scope yet, so any set we named would be a guess. Add real scopes
//!   first, then the challenge (tracked as `L-MCP-22`).

use memnoth_mcp_auth::config::AuthConfig;
use serde_json::{Value, json};

/// RFC 9728 fixed path. Served unauthenticated: it is the document that tells a
/// client how to authenticate, so requiring authentication to read it would be
/// a loop.
pub const METADATA_PATH: &str = "/.well-known/oauth-protected-resource";

/// RFC 9728 also defines a path-suffixed location when the resource has a path.
/// Our endpoint is `POST /mcp`, and clients are not consistent about which form
/// they probe, so both are served with the same document.
pub const METADATA_PATH_SUFFIXED: &str = "/.well-known/oauth-protected-resource/mcp";

/// The MCP endpoint path, which is part of this server's canonical URI.
pub const ENDPOINT_PATH: &str = "/mcp";

/// `MEMNOTH_MCP_PUBLIC_URL` — the externally visible base URL.
pub const PUBLIC_URL_ENV: &str = "MEMNOTH_MCP_PUBLIC_URL";

/// `MEMNOTH_MCP_SCOPES` — comma/space separated scopes this resource accepts.
pub const SCOPES_ENV: &str = "MEMNOTH_MCP_SCOPES";

/// The canonical URI of this MCP server (RFC 9728 / RFC 8707 §2). Lowercase
/// scheme and host, no trailing slash, endpoint path included.
///
/// `Host` is attacker-controllable, and a poisoned canonical URI is worse than
/// useless: it would tell a client to request a token **for someone else's
/// resource**. So an operator-declared public URL always wins, and the header is
/// only a fallback for the loopback/desktop case where the bind is already
/// restricted to 127.0.0.1.
pub fn canonical_resource(public_url: Option<&str>, host: &str) -> String {
    let base = match public_url.map(str::trim).filter(|url| !url.is_empty()) {
        Some(url) => url.trim_end_matches('/').to_string(),
        None => format!("http://{}", host.trim_end_matches('/')),
    };
    let mut canonical = base.to_ascii_lowercase();
    if canonical.ends_with(ENDPOINT_PATH) {
        return canonical;
    }
    canonical.push_str(ENDPOINT_PATH);
    canonical
}

/// The URL of the metadata document itself, for `resource_metadata` in a
/// `WWW-Authenticate` challenge. Note this is a *different* URL from
/// `public fn canonical_resource`: the challenge points at the document, the
/// document names the resource.
///
/// Derived by stripping the endpoint path, so the two can never drift apart.
pub fn metadata_url(resource: &str) -> String {
    let base = resource
        .strip_suffix(ENDPOINT_PATH)
        .unwrap_or(resource)
        .trim_end_matches('/');
    format!("{base}{METADATA_PATH}")
}

/// The issuer the RS256 mode defaults to when Zitadel is configured without an
/// explicit `MEMNOTH_MCP_ZITADEL_ISSUER`.
pub const DEFAULT_ZITADEL_ISSUER: &str = "https://auth.astrivya.ai";

/// Issuer identifiers that belong in `authorization_servers`.
///
/// Two filters, both deliberate:
///
/// * **Only absolute `http(s)` issuers qualify.** The field is a list of issuer
///   *URLs* (RFC 9728). The HS256 default issuer (`astrivya-memnoth-install`) is
///   an internal id, not a URL — advertising it would send a client to fetch
///   metadata from a host that does not exist.
/// * **Zitadel is named only when it is actually configured.** The default
///   issuer is the default *for that mode*, not a fact about every deployment.
///   A static-bearer-only install has no authorization server at all, and says
///   so by omitting the field rather than pointing clients at someone else's.
pub fn authorization_servers(config: &AuthConfig) -> Vec<String> {
    let mut servers: Vec<String> = Vec::new();
    let zitadel_configured = config.zitadel_issuer.is_some()
        || config.zitadel_jwks_url.is_some()
        || config.zitadel_audience.is_some();
    if zitadel_configured {
        servers.push(
            config
                .zitadel_issuer
                .clone()
                .unwrap_or_else(|| DEFAULT_ZITADEL_ISSUER.to_string()),
        );
    }
    if let Some(issuer) = config
        .hs256_issuer
        .as_deref()
        .filter(|issuer| issuer.starts_with("https://") || issuer.starts_with("http://"))
    {
        servers.push(issuer.to_string());
    }
    servers = servers
        .into_iter()
        .map(|issuer| issuer.trim_end_matches('/').to_string())
        .collect();
    servers.sort();
    servers.dedup();
    servers
}

fn configured_scopes() -> Vec<String> {
    let mut scopes: Vec<String> = std::env::var(SCOPES_ENV)
        .unwrap_or_default()
        .split([',', ' '])
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(str::to_string)
        .collect();
    scopes.sort();
    scopes.dedup();
    scopes
}

/// The protected resource metadata document.
pub fn metadata(config: &AuthConfig, resource: &str) -> Value {
    let mut document = json!({
        "resource": resource,
        // Query-string tokens are forbidden by the authorization spec
        // ("Access tokens MUST NOT be included in the URI query string"), so
        // advertise only the header method.
        "bearer_methods_supported": ["header"],
        "resource_name": "Memnoth MCP Server",
        "resource_documentation": "https://github.com/amruthjakku/memnoth/blob/master/docs/MCP.md",
    });
    let servers = authorization_servers(config);
    if !servers.is_empty() {
        document["authorization_servers"] = json!(servers);
    }
    let scopes = configured_scopes();
    if !scopes.is_empty() {
        document["scopes_supported"] = json!(scopes);
    }
    document
}

/// The `WWW-Authenticate` challenge for a 401.
///
/// RFC 6750 §3 shape, with the MCP-required `resource_metadata` parameter that
/// turns a dead end into a discovery step. `error` is `invalid_token` when a
/// token was presented and rejected, and absent when none was presented —
/// claiming `invalid_token` for a missing token is a different (and wrong)
/// message to the client.
pub fn www_authenticate(metadata_url: &str, presented_a_token: bool) -> String {
    if presented_a_token {
        format!("Bearer error=\"invalid_token\", resource_metadata=\"{metadata_url}\"")
    } else {
        format!("Bearer resource_metadata=\"{metadata_url}\"")
    }
}

/// The 403 challenge for an operation needing scopes the token lacks.
///
/// Present, tested, and **not yet reachable**: no tool declares a required
/// scope. It exists in `L-MCP-22` form rather than as a half-wired caller,
/// because a challenge naming scopes nothing enforces teaches hosts to ignore
/// challenges.
///
/// Allowed rather than gated: this one is *deliberate* shipped surface, not a test
/// fixture. `#[cfg(test)]` would delete the deliverable from the release build, and
/// the point of writing it in final form is that wiring it later is calling it.
#[allow(dead_code)]
pub fn insufficient_scope(metadata_url: &str, required: &[String], description: &str) -> String {
    let scopes = required.join(" ");
    format!(
        "Bearer error=\"insufficient_scope\", scope=\"{scopes}\", \
         resource_metadata=\"{metadata_url}\", error_description=\"{description}\""
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(zitadel_issuer: Option<&str>, hs256_issuer: Option<&str>) -> AuthConfig {
        AuthConfig {
            static_bearer: None,
            hs256_secret: None,
            hs256_audience: None,
            hs256_issuer: hs256_issuer.map(str::to_string),
            zitadel_jwks_url: None,
            zitadel_audience: None,
            zitadel_issuer: zitadel_issuer.map(str::to_string),
        }
    }

    #[test]
    fn canonical_uri_is_lowercase_path_suffixed_and_slash_free() {
        assert_eq!(
            canonical_resource(Some("https://MCP.Example.COM/"), "ignored:443"),
            "https://mcp.example.com/mcp"
        );
        assert_eq!(
            canonical_resource(None, "127.0.0.1:7842"),
            "http://127.0.0.1:7842/mcp"
        );
        // Already suffixed: do not append the path twice.
        assert_eq!(
            canonical_resource(Some("https://mcp.example.com/mcp"), "ignored"),
            "https://mcp.example.com/mcp"
        );
    }

    #[test]
    fn a_declared_public_url_beats_a_spoofed_host_header() {
        // The whole point: the canonical URI decides which resource a client
        // asks its authorization server for, so it must not follow `Host`.
        let canonical = canonical_resource(Some("https://mcp.astrivya.ai"), "evil.example.com");
        assert_eq!(canonical, "https://mcp.astrivya.ai/mcp");
        assert!(!canonical.contains("evil.example.com"));
    }

    #[test]
    fn a_static_bearer_only_deployment_advertises_no_authorization_server() {
        // No Zitadel anywhere, and the HS256 issuer default is an id, not a
        // URL: the honest answer is an empty list, not a guess at a URL.
        let servers = authorization_servers(&config(None, None));
        assert!(servers.is_empty(), "{servers:?}");
        let with_id_issuer = authorization_servers(&config(None, Some("astrivya-memnoth-install")));
        assert!(
            with_id_issuer.is_empty(),
            "an issuer id must never be advertised as a URL: {with_id_issuer:?}"
        );
    }

    #[test]
    fn zitadel_is_advertised_only_when_it_is_configured() {
        // `zitadel_issuer` set explicitly.
        assert_eq!(
            authorization_servers(&config(Some("https://custom.example.com/"), None)),
            vec!["https://custom.example.com"]
        );
        // Configured *without* an explicit issuer (e.g. only a JWKS URL): the
        // mode default applies, because the mode is what is in use.
        let mut implicit = config(None, None);
        implicit.zitadel_jwks_url = Some("https://auth.astrivya.ai/oauth/v2/keys".to_string());
        assert_eq!(
            authorization_servers(&implicit),
            vec![DEFAULT_ZITADEL_ISSUER]
        );
    }

    #[test]
    fn issuers_are_deduplicated_and_sorted() {
        let servers = authorization_servers(&config(
            Some("https://auth.astrivya.ai/"),
            Some("https://custom.example.com"),
        ));
        assert_eq!(
            servers,
            vec!["https://auth.astrivya.ai", "https://custom.example.com"]
        );
    }

    #[test]
    fn the_document_omits_what_it_cannot_honestly_claim() {
        let document = metadata(
            &config(Some("https://auth.example.com"), None),
            "https://x/mcp",
        );
        assert_eq!(document["resource"], "https://x/mcp");
        assert_eq!(document["bearer_methods_supported"][0], "header");
        assert_eq!(
            document["authorization_servers"][0],
            "https://auth.example.com"
        );
        assert!(document.get("scopes_supported").is_none());
        // We do not accept query-string tokens, so we must not advertise them.
        assert_eq!(
            document["bearer_methods_supported"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn challenges_distinguish_missing_from_rejected_tokens() {
        let missing = www_authenticate("https://x/.well-known/oauth-protected-resource", false);
        assert!(!missing.contains("invalid_token"), "{missing}");
        assert!(
            missing.contains("resource_metadata=\"https://x/"),
            "{missing}"
        );
        let rejected = www_authenticate("https://x/.well-known/oauth-protected-resource", true);
        assert!(rejected.contains("error=\"invalid_token\""), "{rejected}");
    }

    #[test]
    fn insufficient_scope_names_every_required_scope_at_once() {
        // The spec: challenge all required scopes together, never incrementally.
        let challenge = insufficient_scope(
            "https://x/.well-known/oauth-protected-resource",
            &["vault:write".to_string(), "kg:index".to_string()],
            "write access required",
        );
        assert!(
            challenge.contains("error=\"insufficient_scope\""),
            "{challenge}"
        );
        assert!(
            challenge.contains("scope=\"vault:write kg:index\""),
            "{challenge}"
        );
        assert!(challenge.contains("resource_metadata="), "{challenge}");
    }

    #[test]
    fn the_challenge_url_is_derived_from_the_resource_not_duplicated() {
        let resource = canonical_resource(Some("https://mcp.example.com"), "ignored");
        assert_eq!(resource, "https://mcp.example.com/mcp");
        assert_eq!(
            metadata_url(&resource),
            "https://mcp.example.com/.well-known/oauth-protected-resource"
        );
        // No endpoint path to strip (e.g. a resource configured at the root)
        // still yields a valid metadata URL.
        assert_eq!(
            metadata_url("https://mcp.example.com"),
            "https://mcp.example.com/.well-known/oauth-protected-resource"
        );
    }

    #[test]
    fn the_metadata_paths_are_the_ones_clients_probe() {
        assert_eq!(METADATA_PATH, "/.well-known/oauth-protected-resource");
        assert!(METADATA_PATH_SUFFIXED.starts_with(METADATA_PATH));
        assert!(METADATA_PATH_SUFFIXED.ends_with(ENDPOINT_PATH));
    }
}
