//! All possible failure modes. These are **internal** — the HTTP layer
//! must never leak the `Display` text to the client. They flow into
//! `memnoth_mcp_auth_total{mode, result}` metric labels and into the
//! `tracing` log only.

use std::fmt;

use crate::AuthMode;

/// Typed auth failure. The `reason` is stable for use in metric labels
/// (no PII, no secret material).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthError {
    /// Which mode was being attempted. Even if the header is missing,
    /// the caller may still want to know which mode "would have" matched
    /// (used to count mode-specific miss rates).
    pub mode: AuthMode,
    /// The stable, lowercase, machine-readable reason. Used in metric
    /// labels. Must be a small fixed set (see [`AuthFailureReason`]).
    pub reason: AuthFailureReason,
}

impl AuthError {
    /// Construct a new typed error.
    pub fn new(mode: AuthMode, reason: AuthFailureReason) -> Self {
        Self { mode, reason }
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Deliberately terse. NEVER include the offending token, header,
        // or anything that could help an attacker tune a probe.
        write!(
            f,
            "unauthorized (mode={}, reason={})",
            self.mode.as_str(),
            self.reason.as_str()
        )
    }
}

impl std::error::Error for AuthError {}

/// Machine-readable reason. Every variant must have a stable `as_str()`
/// because it ends up in Prometheus label values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthFailureReason {
    /// No `Authorization` header on the request.
    MissingHeader,
    /// Header present but not in `Bearer <token>` form.
    MalformedHeader,
    /// The `static_bearer` token did not match.
    BadStaticToken,
    /// JWT signature did not verify against the configured key.
    BadSignature,
    /// JWT `exp` claim is in the past (with 60s clock skew tolerance).
    Expired,
    /// JWT `nbf` claim is in the future.
    NotYetValid,
    /// JWT `aud` claim did not match the expected audience.
    WrongAudience,
    /// JWT `iss` claim did not match the expected issuer.
    WrongIssuer,
    /// JWT was signed with an algorithm we did not allow-list.
    DisallowedAlgorithm,
    /// JWT's `kid` header is missing.
    MissingKid,
    /// JWKS lookup was needed but failed (network or 4xx/5xx), AND the
    /// `kid` is not in the cache. Fail-closed: deny.
    JwksUnavailable,
    /// `kid` is not in the JWKS doc and a refresh did not produce it.
    UnknownKid,
    /// The token was explicitly revoked via `store.revoke()`.
    Revoked,
    /// Catch-all for unexpected internal failures (e.g. a claim type was
    /// not what we expected). Kept distinct so it shows up as a different
    /// metric label from the named variants.
    Internal,
}

impl AuthFailureReason {
    /// Stable, lowercase, no whitespace. Used in metric labels.
    pub fn as_str(self) -> &'static str {
        match self {
            AuthFailureReason::MissingHeader => "missing_header",
            AuthFailureReason::MalformedHeader => "malformed_header",
            AuthFailureReason::BadStaticToken => "bad_static_token",
            AuthFailureReason::BadSignature => "bad_signature",
            AuthFailureReason::Expired => "expired",
            AuthFailureReason::NotYetValid => "not_yet_valid",
            AuthFailureReason::WrongAudience => "wrong_audience",
            AuthFailureReason::WrongIssuer => "wrong_issuer",
            AuthFailureReason::DisallowedAlgorithm => "disallowed_algorithm",
            AuthFailureReason::MissingKid => "missing_kid",
            AuthFailureReason::JwksUnavailable => "jwks_unavailable",
            AuthFailureReason::UnknownKid => "unknown_kid",
            AuthFailureReason::Revoked => "revoked",
            AuthFailureReason::Internal => "internal",
        }
    }
}
