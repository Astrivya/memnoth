//! Environment-driven configuration. Fail-closed: any malformed or
//! missing required value (when the corresponding mode is enabled) is
//! a hard error at startup, never a silent allow.

use std::env;

use crate::error::{AuthError, AuthFailureReason};
use crate::hs256::Hs256UserJwtVerifier;
use crate::verifier::{MultiAuthenticator, StaticBearerVerifier};
use crate::zitadel::Rs256ZitadelVerifier;
use crate::{AuthMode, SharedAuthenticator};
use std::sync::Arc;

/// Parsed auth configuration. `None` for an unset mode means that mode
/// is disabled. At least one mode must be enabled in production; the
/// `build` helper enforces that.
#[derive(Debug, Default)]
pub struct AuthConfig {
    /// `MEMNOTH_MCP_HTTP_TOKEN` raw value, if set and non-empty.
    pub static_bearer: Option<String>,
    /// `MEMNOTH_MCP_USER_TOKEN_SECRET` raw value (HS256).
    pub hs256_secret: Option<String>,
    /// `MEMNOTH_MCP_AUTH_AUDIENCE` (HS256). Defaults to `memnoth-mcp`.
    pub hs256_audience: Option<String>,
    /// `MEMNOTH_MCP_AUTH_ISSUER` (HS256). Defaults to
    /// `astrivya-memnoth-install`.
    pub hs256_issuer: Option<String>,
    /// `MEMNOTH_MCP_ZITADEL_JWKS_URL` (RS256). Defaults to
    /// `https://auth.astrivya.ai/oauth/v2/keys`.
    pub zitadel_jwks_url: Option<String>,
    /// `MEMNOTH_MCP_ZITADEL_AUDIENCE` (RS256).
    pub zitadel_audience: Option<String>,
    /// `MEMNOTH_MCP_ZITADEL_ISSUER` (RS256). Defaults to
    /// `https://auth.astrivya.ai`.
    pub zitadel_issuer: Option<String>,
}

impl AuthConfig {
    /// Read all `MEMNOTH_MCP_*` env vars. Never panics.
    pub fn from_env() -> Self {
        Self {
            static_bearer: env::var("MEMNOTH_MCP_HTTP_TOKEN")
                .ok()
                .filter(|v| !v.is_empty()),
            hs256_secret: env::var("MEMNOTH_MCP_USER_TOKEN_SECRET")
                .ok()
                .filter(|v| !v.is_empty()),
            hs256_audience: env::var("MEMNOTH_MCP_AUTH_AUDIENCE")
                .ok()
                .filter(|v| !v.is_empty()),
            hs256_issuer: env::var("MEMNOTH_MCP_AUTH_ISSUER")
                .ok()
                .filter(|v| !v.is_empty()),
            zitadel_jwks_url: env::var("MEMNOTH_MCP_ZITADEL_JWKS_URL")
                .ok()
                .filter(|v| !v.is_empty()),
            zitadel_audience: env::var("MEMNOTH_MCP_ZITADEL_AUDIENCE")
                .ok()
                .filter(|v| !v.is_empty()),
            zitadel_issuer: env::var("MEMNOTH_MCP_ZITADEL_ISSUER")
                .ok()
                .filter(|v| !v.is_empty()),
        }
    }

    /// Which modes have enough config to build a verifier. The HTTP
    /// server's loopback-only fallback is the caller's responsibility
    /// when this returns an empty slice.
    pub fn enabled_modes(&self) -> Vec<AuthMode> {
        let mut out = Vec::with_capacity(3);
        if self.static_bearer.is_some() {
            out.push(AuthMode::StaticBearer);
        }
        if self.hs256_secret.is_some() {
            out.push(AuthMode::Hs256UserJwt);
        }
        if self.zitadel_jwks_url.is_some()
            || self.zitadel_audience.is_some()
            || self.zitadel_issuer.is_some()
        {
            out.push(AuthMode::Rs256ZitadelSession);
        }
        out
    }

    /// Build the composed authenticator. Returns `Err(AuthError)` with
    /// `reason = Internal` and a startup-fail message (logged) if zero
    /// modes are enabled and `allow_open` is false.
    pub fn build(self, allow_open: bool) -> Result<SharedAuthenticator, AuthError> {
        let mut verifiers: Vec<Arc<dyn crate::Authenticator>> = Vec::with_capacity(3);
        if let Some(tok) = self.static_bearer.as_ref()
            && let Some(v) = StaticBearerVerifier::new(tok.clone())
        {
            verifiers.push(Arc::new(v));
        }
        if let Some(secret) = self.hs256_secret.as_ref()
            && let Some(v) = Hs256UserJwtVerifier::new(
                secret.as_bytes(),
                self.hs256_audience.clone(),
                self.hs256_issuer.clone(),
            )
        {
            verifiers.push(Arc::new(v));
        }
        if (self.zitadel_jwks_url.is_some()
            || self.zitadel_audience.is_some()
            || self.zitadel_issuer.is_some())
            && let Some(v) = Rs256ZitadelVerifier::new(
                self.zitadel_jwks_url.clone(),
                self.zitadel_audience.clone(),
                self.zitadel_issuer.clone(),
            )
        {
            verifiers.push(Arc::new(v));
        }
        if verifiers.is_empty() && !allow_open {
            return Err(AuthError::new(
                AuthMode::StaticBearer,
                AuthFailureReason::Internal,
            ));
        }
        Ok(Arc::new(MultiAuthenticator::new(verifiers)))
    }
}
