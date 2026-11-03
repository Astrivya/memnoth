//! `memnoth-mcp-auth` — authentication verifier for the `memnoth-mcp` HTTP
//! transport. Three modes (static bearer, HS256 user JWT, ZITADEL RS256)
//! behind one trait, all in-process, fail-closed.
//!
//! See `D:\xclaw\memnoth-findings.md` §12 for the design doc.
//!
//! ## Hot path
//! `Authenticator::verify(&authorization_header) -> Result<AuthContext, AuthError>`
//! is the only thing every MCP HTTP request calls. It must be allocation-free
//! beyond what `jsonwebtoken` does internally, and the static-bearer path must
//! be a single `constant_time_eq` call away from returning a verdict.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod hs256;
pub mod jwks;
pub mod metrics;
pub mod rs256;
pub mod state;
pub mod store;
pub mod verifier;
pub mod zitadel;

// Re-export the ZITADEL verifier so callers can construct it without
// reaching into `rs256::` internals.
pub use crate::zitadel::Rs256ZitadelVerifier;

// Re-exports so callers can `use memnoth_mcp_auth::AuthError` instead
// of `use memnoth_mcp_auth::error::AuthError`. The module is still
// `pub` for direct navigation in docs.
pub use crate::error::{AuthError, AuthFailureReason};
pub use crate::verifier::{MultiAuthenticator, StaticBearerVerifier};

use std::sync::Arc;

/// What the verifier returns on success. The HTTP layer threads this into
/// the request context so downstream handlers know who is calling.
///
/// `user_id` and `token_id` are both optional because the `static_bearer`
/// mode does not have a per-user identity (it's a shared service secret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    /// Which mode authenticated this request. Used for metrics + audit log.
    pub mode: AuthMode,
    /// The external user id (`sub` claim) for `hs256_user_jwt` and
    /// `rs256_zitadel_session`. `None` for `static_bearer`.
    pub user_id: Option<String>,
    /// The token's `jti` claim if present, used to revoke individual
    /// install tokens via `store.revoke`. `None` for `static_bearer`.
    pub token_id: Option<String>,
    /// Scopes encoded in the token (empty for `static_bearer`).
    pub scopes: Vec<String>,
    /// When the token expires. `None` for `static_bearer`.
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl AuthContext {
    /// True if the verifier can identify a specific human user (matters for
    /// audit logging and per-user rate limits).
    pub fn is_per_user(&self) -> bool {
        self.user_id.is_some()
    }
}

/// Which auth mode authenticated a request. This is the `mode` label on the
/// `memnoth_mcp_auth_total` Prometheus counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthMode {
    /// `Authorization: Bearer <MEMNOTH_MCP_HTTP_TOKEN>`
    StaticBearer,
    /// HS256 user install token from the Hono sidecar.
    Hs256UserJwt,
    /// RS256 ZITADEL access token, verified against the self-hosted
    /// ZITADEL JWKS (`https://auth.astrivya.ai/oauth/v2/keys`).
    Rs256ZitadelSession,
}

impl AuthMode {
    /// Stable label used in metrics + structured logs. NEVER change the
    /// string values without updating the Grafana dashboards that match.
    pub fn as_str(self) -> &'static str {
        match self {
            AuthMode::StaticBearer => "static",
            AuthMode::Hs256UserJwt => "hs256",
            AuthMode::Rs256ZitadelSession => "rs256_zitadel",
        }
    }
}

/// The core trait. Implementations must be:
/// - **Send + Sync** (shared via `Arc` across all HTTP request tasks)
/// - **stateless across calls** (any per-instance state must be safe to
///   read concurrently; the trait gives `&self`)
/// - **fail-closed** (any internal error must surface as `Err(AuthError)`,
///   never silently `Ok`)
///
/// The HTTP layer will call `verify` with the raw value of the
/// `Authorization` request header (the full `Bearer <token>` string, or
/// `None` if the header is missing).
#[async_trait::async_trait]
pub trait Authenticator: Send + Sync {
    /// Verify the request. Returns the auth context on success, or a
    /// specific `AuthError` on failure. The HTTP layer will always
    /// translate any `AuthError` into a 401 with body `unauthorized`
    /// (no detail leak); the typed error is for metrics and logs.
    async fn verify(&self, authorization_header: Option<&str>) -> Result<AuthContext, AuthError>;

    /// Which modes this authenticator accepts. The HTTP layer uses this
    /// to know which metrics labels to register and which `verify`
    /// branches to consider.
    fn modes(&self) -> &[AuthMode];

    /// Number of JWKS keys currently cached, for modes that verify
    /// against a remote JWKS (RS256). `None` for modes without a JWKS
    /// cache (static bearer, HS256). The HTTP layer refreshes the
    /// `memnoth_mcp_auth_jwks_cache_entries` gauge from this on each
    /// `/metrics` scrape.
    fn jwks_cache_entries(&self) -> Option<u64> {
        None
    }
}

/// Convenience: shared, cheap-to-clone handle.
pub type SharedAuthenticator = Arc<dyn Authenticator>;
