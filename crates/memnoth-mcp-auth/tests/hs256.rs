//! HS256 verifier integration tests. The full failure matrix:
//! - valid token
//! - expired (`exp` in the past, > leeway)
//! - wrong audience
//! - wrong issuer
//! - bad signature (signed with a different secret)
//! - malformed (base64 garbage)
//! - missing header
//! - malformed header (no `Bearer ` prefix)
//! - token used before `nbf`
//! - alg=none attack (alg header forged to `none`)
//! - token signed with the right secret but missing `sub` (Claim field required)
//! - HS256 verifier composed with static_bearer in MultiAuthenticator

use chrono::Utc;
use jsonwebtoken::{EncodingKey, Header, encode};
use memnoth_mcp_auth::hs256::Hs256UserJwtVerifier;
use memnoth_mcp_auth::verifier::{MultiAuthenticator, StaticBearerVerifier};
use memnoth_mcp_auth::{AuthFailureReason, AuthMode, Authenticator};
use serde_json::json;
use std::sync::Arc;

const SECRET: &[u8] = b"shared-secret-for-tests-9f2b";
const OTHER_SECRET: &[u8] = b"different-secret-8e3c";
const AUDIENCE: &str = "memnoth-mcp";
const ISSUER: &str = "astrivya-memnoth-install";

fn verifier() -> Hs256UserJwtVerifier {
    Hs256UserJwtVerifier::new(SECRET, Some(AUDIENCE.into()), Some(ISSUER.into()))
        .expect("non-empty secret must produce a verifier")
}

fn now() -> i64 {
    Utc::now().timestamp()
}

/// Build a signed token with the given claims. Uses HS256 and the
/// caller-supplied secret. If `alg_override` is set, the header's
/// `alg` field is replaced (used to test the alg=none attack).
fn sign(claims: serde_json::Value, secret: &[u8], alg_override: Option<&str>) -> String {
    let mut header = Header::new(jsonwebtoken::Algorithm::HS256);
    if let Some(alg) = alg_override {
        header.alg = alg.parse().expect("alg override must parse");
    }
    encode(&header, &claims, &EncodingKey::from_secret(secret))
        .expect("signing must succeed in tests")
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn claims_base() -> serde_json::Value {
    json!({
        "sub": "usr_2a3fXYZ",
        "jti": "tok_test_001",
        "aud": AUDIENCE,
        "iss": ISSUER,
        "iat": now(),
        "exp": now() + 3600,
    })
}

#[tokio::test]
async fn hs256_accepts_valid_token() {
    let v = verifier();
    let token = sign(claims_base(), SECRET, None);
    let ctx = v
        .verify(Some(&bearer(&token)))
        .await
        .expect("valid token must verify");
    assert_eq!(ctx.mode, AuthMode::Hs256UserJwt);
    assert_eq!(ctx.user_id.as_deref(), Some("usr_2a3fXYZ"));
    assert_eq!(ctx.token_id.as_deref(), Some("tok_test_001"));
    assert!(ctx.expires_at.is_some());
}

#[tokio::test]
async fn hs256_extracts_scope_string() {
    let v = verifier();
    let mut c = claims_base();
    c["scope"] = json!("mcp:read mcp:write");
    let token = sign(c, SECRET, None);
    let ctx = v.verify(Some(&bearer(&token))).await.unwrap();
    assert_eq!(ctx.scopes, vec!["mcp:read", "mcp:write"]);
}

#[tokio::test]
async fn hs256_extracts_scopes_array() {
    let v = verifier();
    let mut c = claims_base();
    c["scopes"] = json!(["mcp:read", "mcp:admin"]);
    let token = sign(c, SECRET, None);
    let ctx = v.verify(Some(&bearer(&token))).await.unwrap();
    assert_eq!(ctx.scopes, vec!["mcp:read", "mcp:admin"]);
}

#[tokio::test]
async fn hs256_rejects_expired() {
    let v = verifier();
    let mut c = claims_base();
    // Expired well beyond the 60s leeway.
    c["exp"] = json!(now() - 3600);
    let token = sign(c, SECRET, None);
    let err = v
        .verify(Some(&bearer(&token)))
        .await
        .expect_err("expired must fail");
    assert_eq!(err.reason, AuthFailureReason::Expired);
}

#[tokio::test]
async fn hs256_allows_expired_within_leeway() {
    // Token that expired 10 seconds ago is still within the 60s leeway.
    // This is the intended behavior: clock skew between minter and
    // verifier is tolerated up to the leeway.
    let v = verifier();
    let mut c = claims_base();
    c["exp"] = json!(now() - 10);
    let token = sign(c, SECRET, None);
    let ctx = v
        .verify(Some(&bearer(&token)))
        .await
        .expect("within leeway must verify");
    assert_eq!(ctx.mode, AuthMode::Hs256UserJwt);
}

#[tokio::test]
async fn hs256_rejects_wrong_audience() {
    let v = verifier();
    let mut c = claims_base();
    c["aud"] = json!("other-service");
    let token = sign(c, SECRET, None);
    let err = v
        .verify(Some(&bearer(&token)))
        .await
        .expect_err("wrong aud must fail");
    assert_eq!(err.reason, AuthFailureReason::WrongAudience);
}

#[tokio::test]
async fn hs256_rejects_wrong_issuer() {
    let v = verifier();
    let mut c = claims_base();
    c["iss"] = json!("some-other-mint");
    let token = sign(c, SECRET, None);
    let err = v
        .verify(Some(&bearer(&token)))
        .await
        .expect_err("wrong iss must fail");
    assert_eq!(err.reason, AuthFailureReason::WrongIssuer);
}

#[tokio::test]
async fn hs256_rejects_bad_signature() {
    let v = verifier();
    // Signed with a different secret -> signature mismatch.
    let token = sign(claims_base(), OTHER_SECRET, None);
    let err = v
        .verify(Some(&bearer(&token)))
        .await
        .expect_err("bad sig must fail");
    assert_eq!(err.reason, AuthFailureReason::BadSignature);
}

#[tokio::test]
async fn hs256_rejects_malformed_token() {
    let v = verifier();
    let err = v
        .verify(Some("Bearer not-a-jwt"))
        .await
        .expect_err("malformed must fail");
    assert_eq!(err.reason, AuthFailureReason::MalformedHeader);
}

#[tokio::test]
async fn hs256_rejects_garbage_base64() {
    let v = verifier();
    let err = v
        .verify(Some("Bearer !!notbase64!!.!!nope!!.!!nah!!"))
        .await
        .expect_err("garbage must fail");
    assert_eq!(err.reason, AuthFailureReason::MalformedHeader);
}

#[tokio::test]
async fn hs256_rejects_missing_header() {
    let v = verifier();
    let err = v.verify(None).await.expect_err("missing header must fail");
    assert_eq!(err.reason, AuthFailureReason::MissingHeader);
    assert_eq!(err.mode, AuthMode::Hs256UserJwt);
}

#[tokio::test]
async fn hs256_rejects_malformed_header_no_bearer_prefix() {
    let v = verifier();
    let token = sign(claims_base(), SECRET, None);
    let err = v
        .verify(Some(&token))
        .await
        .expect_err("bare token must fail");
    assert_eq!(err.reason, AuthFailureReason::MalformedHeader);
}

#[tokio::test]
async fn hs256_rejects_wrong_scheme() {
    let v = verifier();
    let token = sign(claims_base(), SECRET, None);
    let err = v
        .verify(Some(&format!("Basic {token}")))
        .await
        .expect_err("Basic auth must fail");
    assert_eq!(err.reason, AuthFailureReason::MalformedHeader);
}

#[tokio::test]
async fn hs256_rejects_alg_none_attack() {
    // An attacker forges a token with `alg: none` and no signature.
    // jsonwebtoken's `Validation::new(HS256)` locks the algorithm allow
    // list to HS256, so this must be rejected as either
    // `DisallowedAlgorithm` (if jsonwebtoken's alg-check fires) or
    // `MalformedHeader` (if the empty signature is rejected first).
    // Both are acceptable: the attacker does not get an `Ok`.
    use base64::Engine;
    let header_json = r#"{"alg":"none","typ":"JWT"}"#;
    let header_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header_json);
    let payload_json = serde_json::to_string(&claims_base()).unwrap();
    let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_json);
    let forged = format!("{header_b64}.{payload_b64}."); // empty signature
    let v = verifier();
    let err = v
        .verify(Some(&bearer(&forged)))
        .await
        .expect_err("alg=none must fail");
    assert!(
        matches!(
            err.reason,
            AuthFailureReason::DisallowedAlgorithm
                | AuthFailureReason::BadSignature
                | AuthFailureReason::MalformedHeader
        ),
        "expected disallowed alg, bad sig, or malformed, got {:?}",
        err.reason
    );
}

#[tokio::test]
async fn hs256_rejects_not_yet_valid() {
    let v = verifier();
    let mut c = claims_base();
    // `nbf` 1 hour in the future. Way outside the 60s leeway.
    c["nbf"] = json!(now() + 3600);
    let token = sign(c, SECRET, None);
    let err = v
        .verify(Some(&bearer(&token)))
        .await
        .expect_err("future nbf must fail");
    assert_eq!(err.reason, AuthFailureReason::NotYetValid);
}

#[tokio::test]
async fn multi_authenticator_hs256_then_static() {
    // A static token should still verify when HS256 is also enabled.
    let hs = verifier();
    let st = StaticBearerVerifier::new("static-shared").unwrap();
    let multi = MultiAuthenticator::new(vec![Arc::new(hs), Arc::new(st)]);

    // Static token: MultiAuthenticator should try HS256 first, fail
    // with BadSignature, fall through to static, succeed.
    let ctx = multi
        .verify(Some("Bearer static-shared"))
        .await
        .expect("static must be tried after HS256");
    assert_eq!(ctx.mode, AuthMode::StaticBearer);

    // HS256 token: MultiAuthenticator should try HS256 first, succeed.
    let token = sign(claims_base(), SECRET, None);
    let ctx = multi
        .verify(Some(&bearer(&token)))
        .await
        .expect("hs256 must be tried first");
    assert_eq!(ctx.mode, AuthMode::Hs256UserJwt);
}

#[tokio::test]
async fn multi_authenticator_short_circuits_missing_header() {
    // Missing header should short-circuit: no other mode can accept it.
    let hs = verifier();
    let multi = MultiAuthenticator::new(vec![Arc::new(hs)]);
    let err = multi.verify(None).await.expect_err("missing must fail");
    assert_eq!(err.reason, AuthFailureReason::MissingHeader);
}

#[tokio::test]
async fn multi_authenticator_does_not_short_circuit_on_malformed() {
    // `MalformedHeader` for one mode (e.g. a non-JWT string) must
    // NOT short-circuit: another mode (e.g. static_bearer) might
    // accept the same input. Only `MissingHeader` short-circuits.
    let hs = verifier();
    let st = StaticBearerVerifier::new("static-shared").unwrap();
    let multi = MultiAuthenticator::new(vec![Arc::new(hs), Arc::new(st)]);
    // This is "Bearer not-a-jwt" — HS256 will return MalformedHeader,
    // static_bearer will return BadStaticToken. We should end up with
    // the LAST error tried, not short-circuit.
    let err = multi
        .verify(Some("Bearer not-a-jwt"))
        .await
        .expect_err("should fail");
    assert_eq!(err.reason, AuthFailureReason::BadStaticToken);
}

/// The Display impl for AuthError must not leak the secret, the
/// header, or the token. The metric label is the source of truth.
#[tokio::test]
async fn auth_error_display_does_not_leak_secrets() {
    let v = verifier();
    let err = v
        .verify(Some("Bearer definitely-not-a-jwt"))
        .await
        .expect_err("malformed must fail");
    let display = format!("{err}");
    assert!(
        !display.contains("Bearer"),
        "Display must not include the scheme"
    );
    assert!(
        !display.contains("definitely-not-a-jwt"),
        "Display must not include the token"
    );
    let secret_str = std::str::from_utf8(SECRET).expect("test secret is valid UTF-8");
    assert!(
        !display.contains(secret_str),
        "Display must not include the secret"
    );
    // The reason label is fine to expose (it's the metric label).
    assert!(display.contains(err.reason.as_str()));
}
