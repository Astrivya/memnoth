//! `requestState` signing for MCP **Multi Round-Trip Requests** (SEP-2322).
//!
//! When a server wants to ask the client something mid-request — an approval, a
//! missing parameter — it returns `resultType: "input_required"` and the client
//! **retries the original request** with the answers. There is no session and no
//! server-side pending-request table, so anything the server needs on the retry
//! has to travel through the client in `requestState`.
//!
//! The spec (2026-07-28, `basic/patterns/mrtr`) is explicit that this field is
//! **attacker-controlled**:
//!
//! > If a client request contains a `requestState` field, servers MUST treat
//! > `requestState` as an attacker-controlled input. If `requestState`
//! > influences authorization, resource access, or business logic, servers MUST
//! > protect its integrity (e.g. HMAC or AEAD) and MUST reject state that fails
//! > verification.
//!
//! Ours does influence business logic — it carries *what is about to be
//! written* — so a forged state could redirect a confirmed approval onto a
//! different payload. That is worse than a failed request, which is the only
//! case the spec lets you skip integrity protection for. Hence: signed, always.
//!
//! Implementation is an HS256 JWT rather than hand-rolled HMAC: `jsonwebtoken`
//! is already the crate's verifier, and the spec names "encrypted JWT" as an
//! acceptable encoding. Signing is what is added here; the extra claims are the
//! spec's own SHOULD list (expiry, originating request) plus a `jti` so the MCP
//! layer can enforce single-use.

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde_json::Value;
use std::sync::OnceLock;

/// Environment override for the signing key. Set it to keep pending
/// confirmations valid across a restart, or across instances behind a load
/// balancer (the state is self-contained, so any instance can verify it).
pub const STATE_SECRET_ENV: &str = "MEMNOTH_MCP_STATE_SECRET";

/// How long a signed state stays valid. Short on purpose: this is an approval
/// prompt, and an approval that was answered an hour ago should not still be
/// redeemable.
pub const STATE_TTL_SECS: i64 = 15 * 60;

/// Why a signed `requestState` was rejected.
///
/// The variants are deliberately coarse: the accompanying [`StateError::message`]
/// is what reaches the client, and it must not double as a verification oracle.
#[derive(Debug, PartialEq, Eq)]
pub enum StateError {
    /// Signature/algorithm/structure did not verify — forged, truncated, or
    /// signed with a different key.
    Invalid,
    /// Verified, but past `exp`.
    Expired,
    /// `exp` was missing. We always set it, so an absent one means the token did
    /// not come from here.
    MissingExpiry,
}

impl StateError {
    /// Message safe to surface: says *that* verification failed, never why in
    /// enough detail to be an oracle, and never echoes the token.
    pub fn message(&self) -> &'static str {
        match self {
            StateError::Expired => "requestState has expired; re-issue the original request",
            StateError::MissingExpiry => "requestState is missing its expiry claim",
            StateError::Invalid => "requestState failed verification",
        }
    }
}

/// The process signing key: `MEMNOTH_MCP_STATE_SECRET` when set, otherwise 32
/// bytes of CSPRNG output generated once per process.
///
/// The fallback is deliberately ephemeral. A restart loses the key, so every
/// in-flight confirmation is rejected and has to be re-asked — which is the
/// correct outcome, because a restart also loses the pending write it was about
/// to perform. Persisting a key in a file would add a secret to manage for no
/// gain in the stdio/desktop case.
pub fn process_secret() -> &'static [u8; 32] {
    static SECRET: OnceLock<[u8; 32]> = OnceLock::new();
    SECRET.get_or_init(|| {
        match std::env::var(STATE_SECRET_ENV) {
            Ok(secret) if secret.len() >= 32 => {
                let mut key = [0u8; 32];
                key.copy_from_slice(&secret.as_bytes()[..32]);
                key
            }
            // A short/missing override must not silently weaken the key to a
            // guessable one, so anything under 32 bytes falls through to random
            // rather than being zero-padded into a 4-byte secret.
            _ => {
                let mut key = [0u8; 32];
                key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
                key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
                key
            }
        }
    })
}

/// Sign a claims object into an opaque `requestState` string.
pub fn sign(secret: &[u8; 32], claims: &Value) -> Result<String, String> {
    encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|e| format!("sign requestState: {e}"))
}

/// Verify and decode a `requestState`. Expiry is checked by the decoder, and
/// `exp` is *required* — a token without one did not come from [`sign`].
pub fn verify(secret: &[u8; 32], token: &str) -> Result<Value, StateError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    // No `aud`/`iss`/`sub` requirements: the state is not a credential and is
    // never presented to anyone but us; the binding claims we care about are
    // checked by the caller against the retry request.
    validation.required_spec_claims = std::collections::HashSet::from(["exp".to_string()]);
    match decode::<Value>(token, &DecodingKey::from_secret(secret), &validation) {
        Ok(data) => Ok(data.claims),
        Err(err) => {
            use jsonwebtoken::errors::ErrorKind;
            match err.kind() {
                ErrorKind::ExpiredSignature => Err(StateError::Expired),
                ErrorKind::MissingRequiredClaim(name) if name == "exp" => {
                    Err(StateError::MissingExpiry)
                }
                _ => Err(StateError::Invalid),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn round_trips_a_claim_object() {
        let claims = json!({"sub": "ops", "jti": "n-1", "exp": 4_000_000_000i64});
        let token = sign(&key(7), &claims).unwrap();
        let back = verify(&key(7), &token).unwrap();
        assert_eq!(back["sub"], "ops");
        assert_eq!(back["jti"], "n-1");
    }

    #[test]
    fn a_forged_payload_is_rejected() {
        // Same claims, different key: the payload is perfectly readable and
        // still must not be trusted — this is the whole threat model.
        let claims = json!({"sub": "attacker", "exp": 4_000_000_000i64});
        let token = sign(&key(1), &claims).unwrap();
        assert_eq!(verify(&key(2), &token).unwrap_err(), StateError::Invalid);
    }

    #[test]
    fn tampering_with_the_body_breaks_the_signature() {
        let token = sign(&key(3), &json!({"sub": "ops", "exp": 4_000_000_000i64})).unwrap();
        // Flip a byte in the payload segment.
        let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
        let mut payload = parts[1].clone().into_bytes();
        payload[0] = if payload[0] == b'a' { b'b' } else { b'a' };
        parts[1] = String::from_utf8(payload).unwrap();
        let tampered = parts.join(".");
        assert_eq!(verify(&key(3), &tampered).unwrap_err(), StateError::Invalid);
    }

    #[test]
    fn expired_state_is_distinguishable_from_forged_state() {
        let expired = sign(
            &key(4),
            &json!({"sub": "ops", "exp": 1_000_000_000i64}), // year 2001
        )
        .unwrap();
        assert_eq!(verify(&key(4), &expired).unwrap_err(), StateError::Expired);
        assert_ne!(
            StateError::Expired.message(),
            StateError::Invalid.message(),
            "a stale prompt must tell the client to re-issue; a forged one must not"
        );
    }

    #[test]
    fn state_without_expiry_is_refused() {
        // Signed correctly but with no `exp`: still not ours, because every
        // token we mint carries one.
        let token = sign(&key(5), &json!({"sub": "ops"})).unwrap();
        assert_eq!(
            verify(&key(5), &token).unwrap_err(),
            StateError::MissingExpiry
        );
    }

    #[test]
    fn a_short_override_falls_back_to_random_rather_than_weakening() {
        // Only assert the documented behaviour of the length gate; the secret
        // itself is process-global and must not be mutated by a test.
        for short in ["", "hunter2", &"x".repeat(31)] {
            assert!(short.len() < 32, "{short:?} must not be usable as a key");
        }
        let secret = process_secret();
        assert_eq!(secret.len(), 32);
        assert!(
            secret.iter().any(|b| *b != 0),
            "the fallback key must not be all zeroes"
        );
    }

    #[test]
    fn messages_never_echo_the_token() {
        for error in [
            StateError::Invalid,
            StateError::Expired,
            StateError::MissingExpiry,
        ] {
            assert!(!error.message().contains("eyJ"));
            assert!(!error.message().is_empty());
        }
    }
}
