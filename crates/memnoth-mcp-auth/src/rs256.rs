//! RS256 JWT verifier against a remote JWKS. Provider-agnostic: the
//! same verification core serves the self-hosted ZITADEL mode (and any
//! future RS256 IdP). The claims type (`Rs256Claims`) supplies
//! provider-specific defaults (JWKS URL, issuer) and the `AuthContext`
//! mapping (user id, scopes, token id).
//!
//! The verification flow is identical for every provider:
//! 1. Decode the JWT header to extract the `kid`.
//! 2. Look the `kid` up in the JWKS cache (singleflight refresh on miss).
//! 3. `jsonwebtoken::decode` with `aud` / `iss` / `exp` / `nbf`
//!    validation against the configured policy.
//!
//! Per `D:\xclaw\memnoth-findings.md` §12, the verifier is fail-closed
//! and never falls through on JWKS unavailability.

use std::marker::PhantomData;

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, Validation, decode, decode_header};
use serde::de::DeserializeOwned;

use crate::error::{AuthError, AuthFailureReason};
use crate::jwks::{JwksCache, JwksError};
use crate::{AuthContext, AuthMode, Authenticator};

/// Provider-specific claims + defaults for an RS256 JWT mode.
///
/// Implementations are plain `Deserialize` structs (the subset of
/// claims the deployment cares about); unknown JWT claims are ignored
/// by serde.
pub trait Rs256Claims: DeserializeOwned + Send + Sync {
    /// Which `AuthMode` this claims type authenticates.
    fn mode() -> AuthMode;

    /// Default JWKS URL for the production instance. The concrete
    /// verifier may override it via `MEMNOTH_MCP_*_JWKS_URL`.
    fn default_jwks_url() -> &'static str;

    /// Default issuer. The concrete verifier may override it via
    /// `MEMNOTH_MCP_*_ISSUER`.
    fn default_issuer() -> &'static str;

    /// Subject user id (`sub` claim).
    fn user_id(&self) -> String;

    /// Token id for revocation (`jti` / `sid`), if the provider issues one.
    fn token_id(&self) -> Option<String>;

    /// Expiry as unix seconds (`exp` claim).
    fn expires_at(&self) -> i64;

    /// Normalized scope strings derived from provider-specific claims
    /// (org membership, project roles, ...). Empty when not applicable.
    fn normalized_scopes(&self) -> Vec<String>;
}

/// RS256 JWT verifier against a remote JWKS. `Clone` is cheap (cache +
/// config are internally `Arc`).
#[derive(Clone)]
pub struct Rs256JwtVerifier<C: Rs256Claims> {
    /// JWKS cache (provider URL, singleflight refresh on kid miss).
    jwks: JwksCache,
    /// Pre-built `Validation` with the configured `aud` / `iss` /
    /// `leeway`. The `audiences` and `issuers` fields are owned so
    /// we can hand `&str` slices to `set_audience` / `set_issuer` on
    /// the `Validation` we keep around.
    validation: Validation,
    /// The expected audience (kept for `audience()` diagnostics).
    audience: String,
    /// Marker for the claims type.
    _claims: PhantomData<C>,
}

impl<C: Rs256Claims> Rs256JwtVerifier<C> {
    /// Construct a new RS256 verifier.
    ///
    /// - `jwks_url`: where to fetch the JWKS doc. `None` falls back to
    ///   the provider default.
    /// - `audience`: expected `aud` claim. Optional — if `None`, no
    ///   audience check is performed (not recommended).
    /// - `issuer`: expected `iss` claim. `None` falls back to the
    ///   provider default.
    pub fn new(
        jwks_url: Option<String>,
        audience: Option<String>,
        issuer: Option<String>,
    ) -> Option<Self> {
        let jwks_url = jwks_url.unwrap_or_else(|| C::default_jwks_url().to_string());
        let jwks = match JwksCache::new(&jwks_url) {
            Ok(c) => c,
            Err(_) => return None, // HTTP client build failed (impossible in practice)
        };
        let issuer = issuer.unwrap_or_else(|| C::default_issuer().to_string());
        let mut validation = Validation::new(Algorithm::RS256);
        if let Some(aud) = &audience {
            validation.set_audience(&[aud.as_str()]);
        } else {
            // If no audience is configured we MUST disable jsonwebtoken's
            // required-claim check, otherwise decoding fails on tokens
            // with no `aud` at all.
            validation.validate_aud = false;
        }
        validation.set_issuer(&[&issuer]);
        validation.leeway = crate::hs256::CLOCK_SKEW_LEEWAY_SECS;
        validation.validate_nbf = true;
        Some(Self {
            jwks,
            validation,
            audience: audience.unwrap_or_default(),
            _claims: PhantomData,
        })
    }

    /// The expected audience (handy for tests + diagnostics).
    pub fn audience(&self) -> &str {
        &self.audience
    }

    /// Trigger a JWKS refresh. Call this at startup (the `serve`
    /// command can `await` it before opening the listener) so that
    /// the first user request doesn't pay the fetch cost.
    pub async fn warm(&self) -> Result<u64, JwksError> {
        self.jwks.warm().await
    }

    /// Number of keys currently in the JWKS cache (0 until the first
    /// fetch or [`Self::warm`]). Consumed by the HTTP layer's
    /// `/metrics` gauge via [`crate::Authenticator::jwks_cache_entries`].
    pub fn cached_keys(&self) -> u64 {
        self.jwks.len()
    }

    /// Map a `jsonwebtoken::errors::Error` to `AuthFailureReason`.
    fn map_jwt_error(err: &jsonwebtoken::errors::Error) -> AuthFailureReason {
        use jsonwebtoken::errors::ErrorKind;
        match err.kind() {
            ErrorKind::InvalidToken => AuthFailureReason::MalformedHeader,
            ErrorKind::InvalidSignature => AuthFailureReason::BadSignature,
            ErrorKind::ExpiredSignature => AuthFailureReason::Expired,
            ErrorKind::ImmatureSignature => AuthFailureReason::NotYetValid,
            ErrorKind::InvalidAudience => AuthFailureReason::WrongAudience,
            ErrorKind::InvalidIssuer => AuthFailureReason::WrongIssuer,
            ErrorKind::InvalidAlgorithm => AuthFailureReason::DisallowedAlgorithm,
            ErrorKind::Base64(_) | ErrorKind::Json(_) | ErrorKind::Utf8(_) => {
                AuthFailureReason::MalformedHeader
            }
            ErrorKind::MissingRequiredClaim(_) => AuthFailureReason::MalformedHeader,
            _ => AuthFailureReason::Internal,
        }
    }

    /// Map a `JwksError` to `AuthFailureReason`.
    fn map_jwks_error(err: &JwksError) -> AuthFailureReason {
        match err {
            JwksError::Fetch(_) | JwksError::Refresh(_) => AuthFailureReason::JwksUnavailable,
            JwksError::Parse(_) | JwksError::Empty | JwksError::MalformedRsaKey(_) => {
                AuthFailureReason::JwksUnavailable
            }
            // The kid is not in the JWKS doc even after a refresh.
            JwksError::UnsupportedKeyType(_) => AuthFailureReason::UnknownKid,
        }
    }
}

#[async_trait]
impl<C: Rs256Claims> Authenticator for Rs256JwtVerifier<C> {
    async fn verify(&self, authorization_header: Option<&str>) -> Result<AuthContext, AuthError> {
        let Some(header) = authorization_header else {
            return Err(AuthError::new(C::mode(), AuthFailureReason::MissingHeader));
        };
        let Some(token) = header.strip_prefix("Bearer ") else {
            return Err(AuthError::new(
                C::mode(),
                AuthFailureReason::MalformedHeader,
            ));
        };
        // 1. Decode the header to extract the `kid`. Fail closed if
        //    misconfiguration results in no `kid`.
        let header = match decode_header(token) {
            Ok(h) => h,
            Err(e) => {
                return Err(AuthError::new(C::mode(), Self::map_jwt_error(&e)));
            }
        };
        let kid = match header.kid {
            Some(k) => k,
            None => {
                return Err(AuthError::new(C::mode(), AuthFailureReason::MissingKid));
            }
        };
        // 2. Look up the `kid` in the JWKS cache (singleflight refresh).
        let decoding_key = match self.jwks.get(&kid).await {
            Ok(k) => k,
            Err(e) => {
                return Err(AuthError::new(C::mode(), Self::map_jwks_error(&e)));
            }
        };
        // 3. Verify the signature + claims.
        let token_data = match decode::<C>(token, &decoding_key, &self.validation) {
            Ok(td) => td,
            Err(e) => {
                return Err(AuthError::new(C::mode(), Self::map_jwt_error(&e)));
            }
        };
        let claims = token_data.claims;
        let expires_at = chrono::DateTime::<chrono::Utc>::from_timestamp(claims.expires_at(), 0);
        let scopes = claims.normalized_scopes();
        let user_id = claims.user_id();
        let token_id = claims.token_id();
        Ok(AuthContext {
            mode: C::mode(),
            user_id: Some(user_id),
            token_id,
            scopes,
            expires_at,
        })
    }

    fn jwks_cache_entries(&self) -> Option<u64> {
        Some(self.jwks.len())
    }

    fn modes(&self) -> &[AuthMode] {
        // Static slices so the borrow is `'static` (matches the trait
        // signature). `C::mode()` returns a Copy value; we can't return
        // a reference to a temporary, so this mirrors the concrete
        // per-mode implementations with a match on the mode.
        match C::mode() {
            AuthMode::StaticBearer => &[AuthMode::StaticBearer],
            AuthMode::Hs256UserJwt => &[AuthMode::Hs256UserJwt],
            AuthMode::Rs256ZitadelSession => &[AuthMode::Rs256ZitadelSession],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generic verifier must disable the audience check when no
    /// audience is configured (jsonwebtoken requires it as a claim
    /// otherwise; tokens without `aud` would fail to decode).
    #[test]
    fn generic_core_disables_audience_when_unset() {
        let v = Rs256JwtVerifier::<crate::zitadel::ZitadelClaims>::new(None, None, None);
        assert!(v.is_some());
        assert_eq!(v.unwrap().audience(), "");
    }
}
