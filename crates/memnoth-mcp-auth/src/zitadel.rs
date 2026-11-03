//! ZITADEL session JWT verifier (`rs256_zitadel_session` mode).
//!
//! Validates a ZITADEL JWT **access token** (JWKS-signed) against the
//! self-hosted instance at `https://auth.astrivya.ai/oauth/v2/keys`.
//! This is the production auth mode for memnoth-mcp: the MCP gateway
//! accepts the same ZITADEL access tokens the Astrivya app issues (jose/JWKS verification,
//! audience = the OIDC client id, issuer = `https://auth.astrivya.ai`).
//!
//! Token shape (ZITADEL JWT access token, `AccessTokenType: JWT`):
//! ```json
//! {
//!   "iss": "https://auth.astrivya.ai",
//!   "sub": "389241132255084556",        // ZITADEL user id
//!   "aud": "<client-id>",               // or array; the memnoth client
//!   "client_id": "<client-id>",
//!   "exp": <unix-seconds>,
//!   "iat": <unix-seconds>,
//!   "jti": "<opaque token id>",
//!   "urn:zitadel:iam:org:project:roles": {
//!     "<project-id>": ["role-a", "role-b"]
//!   },
//!   "urn:zitadel:iam:user:resourceowner": "<org-id>",
//!   "urn:zitadel:iam:user:metadata": { ... }
//! }
//! ```
//!
//! Note: ZITADEL **OIDC apps** can be configured to mint JWT access
//! tokens (`AccessTokenType: JWT`) which this verifier checks against
//! the instance JWKS. **API apps** (machine client-credentials) mint
//! opaque tokens that CANNOT be JWKS-verified — for machine traffic use
//! a machine-user PAT (still JWKS-verifiable) or token introspection.

use serde::{Deserialize, Serialize};

use crate::AuthMode;
use crate::rs256::{Rs256Claims, Rs256JwtVerifier};

/// Default ZITADEL JWKS URL (self-hosted instance).
pub const DEFAULT_ZITADEL_JWKS_URL: &str = "https://auth.astrivya.ai/oauth/v2/keys";

/// Default issuer (ZITADEL `ExternalDomain`).
pub const DEFAULT_ZITADEL_ISSUER: &str = "https://auth.astrivya.ai";

/// Subset of ZITADEL's access-token claims we care about. Forward-compat:
/// unknown fields are ignored by serde.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZitadelClaims {
    /// Subject (ZITADEL user id, a numeric string).
    sub: String,
    /// JWT id (opaque per-token). ZITADEL access tokens carry it.
    jti: Option<String>,
    /// Expiry (unix seconds).
    exp: i64,
    /// Not-before (unix seconds, optional).
    #[serde(default)]
    nbf: Option<i64>,
    /// Issued-at (unix seconds, optional).
    #[serde(default)]
    iat: Option<i64>,
    /// Authorized party (client id).
    #[serde(default)]
    azp: Option<String>,
    /// Project → roles map. ZITADEL encodes project grants here.
    #[serde(default, rename = "urn:zitadel:iam:org:project:roles")]
    project_roles: Option<std::collections::HashMap<String, Vec<String>>>,
    /// Owning organization id.
    #[serde(default, rename = "urn:zitadel:iam:user:resourceowner")]
    resource_owner: Option<String>,
}

impl ZitadelClaims {
    /// Read the `sub` claim (used by tests + diagnostics).
    pub fn subject(&self) -> &str {
        &self.sub
    }
}

impl Rs256Claims for ZitadelClaims {
    fn mode() -> AuthMode {
        AuthMode::Rs256ZitadelSession
    }

    fn default_jwks_url() -> &'static str {
        DEFAULT_ZITADEL_JWKS_URL
    }

    fn default_issuer() -> &'static str {
        DEFAULT_ZITADEL_ISSUER
    }

    fn user_id(&self) -> String {
        self.sub.clone()
    }

    fn token_id(&self) -> Option<String> {
        self.jti.clone()
    }

    fn expires_at(&self) -> i64 {
        self.exp
    }

    fn normalized_scopes(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(org) = &self.resource_owner {
            out.push(format!("org:{org}"));
        }
        if let Some(roles) = &self.project_roles {
            let mut projects: Vec<_> = roles.iter().collect();
            projects.sort();
            for (project, role_list) in projects {
                let mut roles = role_list.clone();
                roles.sort();
                for role in roles {
                    out.push(format!("zitadel:project:{project}:role:{role}"));
                }
            }
        }
        out
    }
}

/// The ZITADEL RS256 verifier. Construct with
/// `MEMNOTH_MCP_ZITADEL_JWKS_URL` / `_ISSUER` / `_AUDIENCE` overrides,
/// or defaults from the production instance.
pub type Rs256ZitadelVerifier = Rs256JwtVerifier<ZitadelClaims>;

#[cfg(test)]
mod tests {
    use super::*;

    fn claims_with_roles() -> ZitadelClaims {
        ZitadelClaims {
            sub: "389241132255084556".into(),
            jti: Some("tok_1".into()),
            exp: 0,
            nbf: None,
            iat: None,
            azp: Some("memnoth-mcp".into()),
            project_roles: Some(
                [
                    (
                        "proj-a".to_string(),
                        vec!["admin".to_string(), "dev".to_string()],
                    ),
                    ("proj-b".to_string(), vec!["viewer".to_string()]),
                ]
                .into_iter()
                .collect(),
            ),
            resource_owner: Some("389241132254560268".into()),
        }
    }

    #[test]
    fn zitadel_claims_normalizes_project_roles() {
        let c = claims_with_roles();
        let scopes = c.normalized_scopes();
        assert!(scopes.contains(&"org:389241132254560268".to_string()));
        assert!(scopes.contains(&"zitadel:project:proj-a:role:admin".to_string()));
        assert!(scopes.contains(&"zitadel:project:proj-a:role:dev".to_string()));
        assert!(scopes.contains(&"zitadel:project:proj-b:role:viewer".to_string()));
    }

    #[test]
    fn zitadel_claims_empty_scopes_when_absent() {
        let c = ZitadelClaims {
            sub: "u".into(),
            jti: None,
            exp: 0,
            nbf: None,
            iat: None,
            azp: None,
            project_roles: None,
            resource_owner: None,
        };
        assert!(c.normalized_scopes().is_empty());
    }

    #[test]
    fn default_endpoints_point_at_self_hosted_instance() {
        assert_eq!(
            DEFAULT_ZITADEL_JWKS_URL,
            "https://auth.astrivya.ai/oauth/v2/keys"
        );
        assert_eq!(DEFAULT_ZITADEL_ISSUER, "https://auth.astrivya.ai");
    }
}
