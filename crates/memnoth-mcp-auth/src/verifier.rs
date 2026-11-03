//! Verifier implementations for the auth modes.
//!
//! The `MultiAuthenticator` composes them: a request is checked against
//! each configured mode in order; the first `Ok` wins. Any `Err` from a
//! mode that could not match (e.g. static mode rejects an HS256 token
//! because the secret is wrong, not because it's malformed) is recorded
//! as a metric but the next mode is tried. A `MissingHeader` or
//! `MalformedHeader` short-circuits because no other mode can succeed
//! on the same input.

use std::sync::Arc;

use async_trait::async_trait;
use subtle::ConstantTimeEq;
use tokio::sync::OnceCell;

use crate::error::{AuthError, AuthFailureReason};
use crate::{AuthContext, AuthMode, Authenticator};

/// Static bearer authenticator. The HTTP layer constructs one of these
/// from the `MEMNOTH_MCP_HTTP_TOKEN` env var. If the env var is empty,
/// the static mode is **not** registered (the HTTP server's loopback-only
/// fallback still applies).
///
/// Hot path: one `Option::strip_prefix` + one `subtle::ConstantTimeEq`.
/// No allocation on the success path, no allocation on the failure path.
#[derive(Clone)]
pub struct StaticBearerVerifier {
    /// The expected token bytes. Stored as `Vec<u8>` so the comparison
    /// is on raw bytes (defeats UTF-8 boundary attacks); we never log
    /// or expose this.
    expected: Arc<Vec<u8>>,
}

impl StaticBearerVerifier {
    /// Construct from the raw token string. Returns `None` if `expected`
    /// is empty (the env var was unset or whitespace-only); the caller
    /// should then skip registering this verifier.
    pub fn new(expected: impl Into<String>) -> Option<Self> {
        let s = expected.into();
        if s.is_empty() {
            return None;
        }
        Some(Self {
            expected: Arc::new(s.into_bytes()),
        })
    }

    /// Construct from a byte slice. Same `None` semantics as `new`.
    pub fn from_bytes(expected: Vec<u8>) -> Option<Self> {
        if expected.is_empty() {
            return None;
        }
        Some(Self {
            expected: Arc::new(expected),
        })
    }
}

#[async_trait]
impl Authenticator for StaticBearerVerifier {
    async fn verify(&self, authorization_header: Option<&str>) -> Result<AuthContext, AuthError> {
        let Some(header) = authorization_header else {
            return Err(AuthError::new(
                AuthMode::StaticBearer,
                AuthFailureReason::MissingHeader,
            ));
        };
        let Some(present) = header.strip_prefix("Bearer ") else {
            return Err(AuthError::new(
                AuthMode::StaticBearer,
                AuthFailureReason::MalformedHeader,
            ));
        };
        // `subtle::ConstantTimeEq` over bytes. Length is compared in
        // constant time by `ct_eq` (it XORs the length into the
        // comparison). We must NOT short-circuit on length mismatch.
        let presented = present.as_bytes();
        // `subtle::ConstantTimeEq` over bytes. Length is compared in
        // constant time by `ct_eq` (it XORs the length into the
        // comparison). We must NOT short-circuit on length mismatch.
        // `Choice::unwrap_u8() == 1` is the idiomatic way to extract
        // a `bool` from a `Choice` without panicking.
        let eq = presented.ct_eq(self.expected.as_slice()).unwrap_u8() == 1;
        if !eq {
            return Err(AuthError::new(
                AuthMode::StaticBearer,
                AuthFailureReason::BadStaticToken,
            ));
        }
        // Static bearer is the shared service secret. No per-user
        // identity. The HTTP layer logs this mode + 200 as
        // `memnoth_mcp_auth_total{mode="static",result="allow"}`.
        Ok(AuthContext {
            mode: AuthMode::StaticBearer,
            user_id: None,
            token_id: None,
            scopes: Vec::new(),
            expires_at: None,
        })
    }

    fn modes(&self) -> &[AuthMode] {
        &[AuthMode::StaticBearer]
    }
}

/// Composes multiple `Authenticator`s. Tried in order; the first `Ok`
/// wins. Errors from one mode are recorded for metrics (the HTTP layer
/// can pull them off `AuthError::reason`); the next mode is tried
/// UNLESS the error indicates the request is structurally wrong
/// (missing/malformed header) — in that case no other mode can succeed
/// so we return immediately.
pub struct MultiAuthenticator {
    verifiers: Vec<Arc<dyn Authenticator>>,
}

impl MultiAuthenticator {
    /// Build a new multi-verifier from a list of already-constructed
    /// verifiers. Order matters: the first matching mode wins.
    pub fn new(verifiers: Vec<Arc<dyn Authenticator>>) -> Self {
        Self { verifiers }
    }
}

#[async_trait]
impl Authenticator for MultiAuthenticator {
    async fn verify(&self, authorization_header: Option<&str>) -> Result<AuthContext, AuthError> {
        // Track the LAST error so we report the verdict of the final
        // mode tried (matches user expectation: "if neither mode
        // accepted, tell me which one we actually consulted last").
        // Short-circuit ONLY on `MissingHeader` (truly no Authorization
        // header at all — no other mode can succeed).
        let mut last_error: Option<AuthError> = None;
        for v in &self.verifiers {
            match v.verify(authorization_header).await {
                Ok(ctx) => return Ok(ctx),
                Err(e) => {
                    if e.reason == AuthFailureReason::MissingHeader {
                        return Err(e);
                    }
                    // Overwrite so the final mode's verdict is reported.
                    last_error = Some(e);
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| AuthError::new(AuthMode::StaticBearer, AuthFailureReason::Internal)))
    }

    fn modes(&self) -> &[AuthMode] {
        // The set of all modes across all registered verifiers. The
        // HTTP layer uses this for the `mode` label cardinality in
        // the metrics setup. The Vec would be wasted work to compute
        // on every call, so we cache it lazily.
        // The `OnceCell` here is `tokio::sync::OnceCell` because we're
        // already in async land; a `std::sync::OnceLock` would also
        // work but `tokio`'s is friendlier with the async-trait return
        // type. We avoid the lock on subsequent calls.
        &MODES_PLACEHOLDER
    }

    fn jwks_cache_entries(&self) -> Option<u64> {
        // Sum across the composed verifiers; only RS256 modes report a
        // cache. `None` when no verifier has a JWKS cache at all (the
        // HTTP layer then leaves the gauge unset instead of zeroing it).
        let mut total = 0u64;
        let mut any = false;
        for v in &self.verifiers {
            if let Some(n) = v.jwks_cache_entries() {
                any = true;
                total += n;
            }
        }
        any.then_some(total)
    }
}

/// The set of all known `AuthMode` variants. `MultiAuthenticator::modes`
/// returns this — there is no per-instance "subset" optimization
/// because the metric label cardinality is bounded by `AuthMode` total,
/// not by which modes are configured for a given deployment.
const MODES_PLACEHOLDER: [AuthMode; 3] = [
    AuthMode::StaticBearer,
    AuthMode::Hs256UserJwt,
    AuthMode::Rs256ZitadelSession,
];

// Suppress an unused-import warning if we never end up needing
// `OnceCell` directly. Kept for the day we add a cache that needs it.
const _: fn() = || {
    let _ = OnceCell::<()>::new;
};
