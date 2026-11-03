//! HS256 user-JWT verifier. This is the path the Hono sidecar
//! (`@astrivya/memnoth-mcp-install`) hits when it mints a per-user
//! install token. The token shape is:
//!
//! ```json
//! {
//!   "iss": "astrivya-memnoth-install",
//!   "aud": "memnoth-mcp",
//!   "sub": "usr_2a3f...",
//!   "jti": "<uuid>",
//!   "exp": <unix-seconds, now + 90d>,
//!   "iat": <unix-seconds>,
//!   "scope": "mcp:read mcp:write"
//! }
//! ```
//!
//! The verifier:
//! 1. Strips `Bearer ` from the Authorization header (or returns
//!    `MalformedHeader`).
//! 2. Calls `jsonwebtoken::decode` with a pre-built `DecodingKey`
//!    (built ONCE at construction time, cached in the verifier).
//! 3. Maps each `jsonwebtoken::errors::ErrorKind` to the corresponding
//!    `AuthFailureReason` so the metric labels are stable.
//! 4. Returns an `AuthContext` with the user_id, token_id (jti),
//!    scopes, and exp.

use std::sync::Arc;

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::{Deserialize, Serialize};

use crate::error::{AuthError, AuthFailureReason};
use crate::{AuthContext, AuthMode, Authenticator};

/// Clock skew tolerance. 60s is a balance between replay safety
/// (`exp - 60s` still valid) and VM clock drift (NTP usually keeps
/// within a few seconds). The HS256 path uses the same constant the
/// RS256 path will, so the two stay consistent.
pub const CLOCK_SKEW_LEEWAY_SECS: u64 = 60;

/// Default audience (aud) and issuer (iss) expected on user install
/// tokens. Override via `AuthConfig` env vars if you ever rotate.
pub const DEFAULT_AUDIENCE: &str = "memnoth-mcp";
/// Default JWT issuer; the Hono sidecar mints with this `iss`.
pub const DEFAULT_ISSUER: &str = "astrivya-memnoth-install";

/// The subset of JWT claims we care about. Unknown extra claims are
/// ignored (forward-compatible with new fields from the sidecar).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Claims {
    /// Subject — the authenticated user id the sidecar minted the
    /// token for (opaque, provider-specific format).
    sub: String,
    /// JWT id — unique per token, used for revocation via
    /// `store.revoke(jti)`.
    jti: String,
    /// Expiry (unix seconds). `jsonwebtoken` validates this against
    /// `now + leeway`.
    exp: i64,
    /// Issued-at (unix seconds). Optional in some flows; we don't
    /// require it but parse it if present for `AuthContext`.
    #[serde(default)]
    iat: Option<i64>,
    /// Not-before (unix seconds). Optional.
    #[serde(default)]
    nbf: Option<i64>,
    /// Space-separated scope string. Per RFC 8693, scopes can be a
    /// string or an array; we accept both.
    #[serde(default)]
    scope: Option<String>,
    /// Some issuers send scopes as an array. We accept that too.
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

impl Claims {
    /// Convert the claim's `scope` / `scopes` fields into a normalized
    /// `Vec<String>`. Order is preserved (scope first, then scopes,
    /// deduped). Empty fields produce an empty vec.
    fn normalized_scopes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(s) = &self.scope {
            for tok in s.split_whitespace() {
                if !out.iter().any(|existing| existing == tok) {
                    out.push(tok.to_string());
                }
            }
        }
        if let Some(arr) = &self.scopes {
            for tok in arr {
                if !out.iter().any(|existing| existing == tok) {
                    out.push(tok.clone());
                }
            }
        }
        out
    }
}

/// HS256 user-JWT verifier. Construction is **not** cheap (it builds a
/// `DecodingKey` from the secret), so build ONE and share it across
/// all HTTP request tasks. `Clone` is cheap because the key is in an
/// `Arc` internally.
#[derive(Clone)]
pub struct Hs256UserJwtVerifier {
    /// Pre-built decoding key. `jsonwebtoken` keeps the HMAC state
    /// inside, so this is one allocation shared across all verify
    /// calls.
    key: Arc<DecodingKey>,
    /// The `Validation` configuration: algorithm allow-list, audience,
    /// issuer, leeway. Cloned per call (cheap: small struct).
    validation: Validation,
    /// Cached copy of the expected audience for `AuthContext` reporting.
    audience: String,
}

impl Hs256UserJwtVerifier {
    /// Construct a new HS256 verifier.
    ///
    /// - `secret`: the raw shared secret bytes. Must be non-empty.
    /// - `audience`: expected `aud` claim. Defaults to `memnoth-mcp`.
    /// - `issuer`: expected `iss` claim. Defaults to `astrivya-memnoth-install`.
    pub fn new(
        secret: impl AsRef<[u8]>,
        audience: Option<String>,
        issuer: Option<String>,
    ) -> Option<Self> {
        let secret = secret.as_ref();
        if secret.is_empty() {
            return None;
        }
        let audience = audience.unwrap_or_else(|| DEFAULT_AUDIENCE.to_string());
        let issuer = issuer.unwrap_or_else(|| DEFAULT_ISSUER.to_string());

        let key = DecodingKey::from_secret(secret);
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&[&audience]);
        validation.set_issuer(&[&issuer]);
        validation.leeway = CLOCK_SKEW_LEEWAY_SECS;
        // We do our own required-claims enforcement in the Claims struct
        // (sub, jti, exp are non-Option, so `serde` rejects them
        //  missing). Disable jsonwebtoken's `required_spec_claims` so
        //  it doesn't also require `nbf` / `iat` and fail before our
        //  mapping runs. We DO want nbf validated when present, so
        //  explicitly enable that.
        validation.required_spec_claims.remove("nbf");
        validation.required_spec_claims.remove("iat");
        validation.validate_nbf = true;

        Some(Self {
            key: Arc::new(key),
            validation,
            audience,
        })
    }

    /// The expected audience (handy for tests + diagnostics).
    pub fn audience(&self) -> &str {
        &self.audience
    }

    /// Map a `jsonwebtoken::errors::Error` to the corresponding
    /// `AuthFailureReason`. Stable, exhaustively matched.
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
            // MissingRequiredClaims shouldn't fire because we disabled
            // jsonwebtoken's check, but if it does we treat it as
            // malformed (the token is missing fields we care about).
            ErrorKind::MissingRequiredClaim(_) => AuthFailureReason::MalformedHeader,
            // Anything else is a true internal error.
            _ => AuthFailureReason::Internal,
        }
    }
}

#[async_trait]
impl Authenticator for Hs256UserJwtVerifier {
    async fn verify(&self, authorization_header: Option<&str>) -> Result<AuthContext, AuthError> {
        let Some(header) = authorization_header else {
            return Err(AuthError::new(
                AuthMode::Hs256UserJwt,
                AuthFailureReason::MissingHeader,
            ));
        };
        let Some(token) = header.strip_prefix("Bearer ") else {
            return Err(AuthError::new(
                AuthMode::Hs256UserJwt,
                AuthFailureReason::MalformedHeader,
            ));
        };
        // `jsonwebtoken::decode` is sync and CPU-bound. It's fast
        // (<5μs for HS256), so we don't move it to a blocking thread.
        // If benchmarks show contention we can revisit with
        // `tokio::task::spawn_blocking`.
        let token_data = match decode::<Claims>(token, &self.key, &self.validation) {
            Ok(td) => td,
            Err(e) => {
                return Err(AuthError::new(
                    AuthMode::Hs256UserJwt,
                    Self::map_jwt_error(&e),
                ));
            }
        };
        let claims = token_data.claims;
        let expires_at = chrono::DateTime::<chrono::Utc>::from_timestamp(claims.exp, 0);
        // Compute scopes BEFORE moving `claims.sub` / `claims.jti` out
        // (Rust will not let us partially-move a field and then borrow
        // the same struct afterwards).
        let scopes = claims.normalized_scopes();
        let user_id = claims.sub;
        let token_id = claims.jti;
        Ok(AuthContext {
            mode: AuthMode::Hs256UserJwt,
            user_id: Some(user_id),
            token_id: Some(token_id),
            scopes,
            expires_at,
        })
    }

    fn modes(&self) -> &[AuthMode] {
        &[AuthMode::Hs256UserJwt]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty secret must not produce a verifier.
    #[test]
    fn new_rejects_empty_secret() {
        assert!(Hs256UserJwtVerifier::new(b"", None, None).is_none());
        assert!(Hs256UserJwtVerifier::new("", None, None).is_none());
    }

    /// Defaults: audience and issuer should be the documented ones.
    #[test]
    fn new_uses_documented_defaults() {
        let v = Hs256UserJwtVerifier::new(b"secret", None, None).unwrap();
        assert_eq!(v.audience(), "memnoth-mcp");
    }

    /// Scope normalization handles both `scope` (space-separated) and
    /// `scopes` (array) fields, dedupes, preserves order.
    #[test]
    fn claims_normalizes_scopes() {
        let c = Claims {
            sub: "u".into(),
            jti: "j".into(),
            exp: 0,
            iat: None,
            nbf: None,
            scope: Some("mcp:read mcp:write".into()),
            scopes: Some(vec!["mcp:write".into(), "mcp:admin".into()]),
        };
        let s = c.normalized_scopes();
        assert_eq!(s, vec!["mcp:read", "mcp:write", "mcp:admin"]);
    }

    /// Empty Claims.normalized_scopes returns empty vec.
    #[test]
    fn claims_empty_scopes() {
        let c = Claims {
            sub: "u".into(),
            jti: "j".into(),
            exp: 0,
            iat: None,
            nbf: None,
            scope: None,
            scopes: None,
        };
        assert!(c.normalized_scopes().is_empty());
    }

    /// The 60s leeway is a u64 constant — surface it for runtime
    /// diagnostics. (Internal `Validation` field is not pub.)
    #[test]
    fn leeway_is_one_minute() {
        assert_eq!(CLOCK_SKEW_LEEWAY_SECS, 60);
    }
}
