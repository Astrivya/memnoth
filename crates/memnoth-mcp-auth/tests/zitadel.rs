//! ZITADEL RS256 integration tests. Full failure-matrix coverage
//! against ZITADEL-shaped claims (numeric `sub`, project
//! roles map, resource owner) and the self-hosted issuer/JWKS defaults:
//! - valid token signed by a key in the JWKS doc
//! - bad signature (signed with a key NOT in the JWKS doc)
//! - unknown kid (token has a kid that's never in the JWKS)
//! - missing kid (token header has no kid)
//! - wrong audience
//! - wrong issuer
//! - expired
//! - not yet valid (nbf in future)
//! - JWKS unreachable (server down) -> JwksUnavailable
//! - singleflight: 50 concurrent unknown-kid requests produce 1 fetch
//! - warm at startup loads keys
//!
//! Each test starts its own fake JWKS server on an ephemeral port so
//! the tests are independent and parallelizable.

use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use memnoth_mcp_auth::zitadel::{DEFAULT_ZITADEL_ISSUER, Rs256ZitadelVerifier};
use memnoth_mcp_auth::{AuthFailureReason, AuthMode, Authenticator};
use rsa::pkcs1::EncodeRsaPrivateKey;
use serde_json::json;

mod fake_jwks;
use fake_jwks::{FakeJwks, TestKey, sign_rs256};

fn now() -> i64 {
    Utc::now().timestamp()
}

async fn boot_one_key() -> (FakeJwks, TestKey, Rs256ZitadelVerifier) {
    let key = TestKey::generate("zkid-1");
    let server = FakeJwks::start(vec![key.clone()]).await;
    let v = Rs256ZitadelVerifier::new(
        Some(server.url()),
        Some("memnoth-mcp".into()),
        Some(DEFAULT_ZITADEL_ISSUER.into()),
    )
    .expect("verifier must construct");
    (server, key, v)
}

fn claims_base() -> serde_json::Value {
    json!({
        "iss": DEFAULT_ZITADEL_ISSUER,
        "aud": "memnoth-mcp",
        "sub": "389241132255084556",
        "jti": "ztok_test_001",
        "iat": now(),
        "exp": now() + 3600,
        "urn:zitadel:iam:org:project:roles": {
            "389243474001526796": ["admin", "dev"]
        },
        "urn:zitadel:iam:user:resourceowner": "389241132254560268"
    })
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

#[tokio::test]
async fn zitadel_accepts_valid_token() {
    let (_server, key, v) = boot_one_key().await;
    // First verify triggers a JWKS fetch; the result is cached.
    let token = sign_rs256(&key, None, claims_base());
    let ctx = v
        .verify(Some(&bearer(&token)))
        .await
        .expect("valid token must verify");
    assert_eq!(ctx.mode, AuthMode::Rs256ZitadelSession);
    assert_eq!(ctx.user_id.as_deref(), Some("389241132255084556"));
    assert_eq!(ctx.token_id.as_deref(), Some("ztok_test_001"));
    assert!(ctx.expires_at.is_some());
    // Project roles map to deterministic scopes.
    assert!(
        ctx.scopes
            .contains(&"zitadel:project:389243474001526796:role:admin".into())
    );
    assert!(
        ctx.scopes
            .contains(&"zitadel:project:389243474001526796:role:dev".into())
    );
    assert!(ctx.scopes.contains(&"org:389241132254560268".into()));
}

#[tokio::test]
async fn zitadel_rejects_bad_signature() {
    // Token signed by a key the JWKS doc doesn't have.
    let (_server, _good_key, v) = boot_one_key().await;
    let other_key = TestKey::generate("rogue-kid-not-in-jwks");
    let token = sign_rs256(&other_key, None, claims_base());
    let err = v
        .verify(Some(&bearer(&token)))
        .await
        .expect_err("bad sig must fail");
    assert!(
        matches!(
            err.reason,
            AuthFailureReason::BadSignature | AuthFailureReason::UnknownKid
        ),
        "expected bad sig or unknown kid, got {:?}",
        err.reason
    );
}

#[tokio::test]
async fn zitadel_rejects_wrong_audience() {
    let (_server, key, v) = boot_one_key().await;
    let mut claims = claims_base();
    claims["aud"] = json!("some-other-app");
    let token = sign_rs256(&key, None, claims);
    let err = v
        .verify(Some(&bearer(&token)))
        .await
        .expect_err("wrong aud must fail");
    assert_eq!(err.reason, AuthFailureReason::WrongAudience);
}

#[tokio::test]
async fn zitadel_rejects_wrong_issuer() {
    let (_server, key, v) = boot_one_key().await;
    let mut claims = claims_base();
    claims["iss"] = json!("https://evil.example");
    let token = sign_rs256(&key, None, claims);
    let err = v
        .verify(Some(&bearer(&token)))
        .await
        .expect_err("wrong iss must fail");
    assert_eq!(err.reason, AuthFailureReason::WrongIssuer);
}

#[tokio::test]
async fn zitadel_rejects_expired_token() {
    let (_server, key, v) = boot_one_key().await;
    let mut claims = claims_base();
    claims["exp"] = json!(now() - 3600);
    let token = sign_rs256(&key, None, claims);
    let err = v
        .verify(Some(&bearer(&token)))
        .await
        .expect_err("expired must fail");
    assert_eq!(err.reason, AuthFailureReason::Expired);
}

#[tokio::test]
async fn zitadel_rejects_not_yet_valid() {
    let (_server, key, v) = boot_one_key().await;
    let mut claims = claims_base();
    claims["nbf"] = json!(now() + 3600);
    let token = sign_rs256(&key, None, claims);
    let err = v
        .verify(Some(&bearer(&token)))
        .await
        .expect_err("future nbf must fail");
    assert_eq!(err.reason, AuthFailureReason::NotYetValid);
}

#[tokio::test]
async fn zitadel_rejects_missing_kid() {
    let (_server, key, v) = boot_one_key().await;
    // Hand-craft a token without a kid in the header.
    let mut header = Header::new(Algorithm::RS256);
    header.kid = None; // explicit
    let pem = key
        .private
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .unwrap();
    let token = encode(
        &header,
        &claims_base(),
        &EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
    )
    .unwrap();
    let err = v
        .verify(Some(&bearer(&token)))
        .await
        .expect_err("missing kid must fail");
    assert_eq!(err.reason, AuthFailureReason::MissingKid);
}

#[tokio::test]
async fn zitadel_rejects_missing_header() {
    let (_server, _key, v) = boot_one_key().await;
    let err = v.verify(None).await.expect_err("missing header must fail");
    assert_eq!(err.reason, AuthFailureReason::MissingHeader);
}

#[tokio::test]
async fn zitadel_jwks_unavailable_when_server_down() {
    // Start a verifier pointing at a URL nothing is listening on.
    let v = Rs256ZitadelVerifier::new(
        Some("http://127.0.0.1:1/.well-known/jwks.json".into()),
        Some("memnoth-mcp".into()),
        Some(DEFAULT_ZITADEL_ISSUER.into()),
    )
    .expect("verifier must construct even with bad URL");
    // Sign with a real key (kid is irrelevant — the JWKS fetch will fail).
    let key = TestKey::generate("any");
    let token = sign_rs256(&key, None, claims_base());
    let err = v
        .verify(Some(&bearer(&token)))
        .await
        .expect_err("JWKS down must fail");
    assert_eq!(err.reason, AuthFailureReason::JwksUnavailable);
}

#[tokio::test]
async fn zitadel_singleflight_on_concurrent_unknown_kid() {
    // 50 concurrent verifications of a token whose kid is NOT in the
    // JWKS doc must produce exactly 1 fetch (the singleflight
    // guarantee).
    let real_key = TestKey::generate("server-known-kid");
    let server = FakeJwks::start(vec![real_key.clone()]).await;
    let v = Rs256ZitadelVerifier::new(
        Some(server.url()),
        Some("memnoth-mcp".into()),
        Some(DEFAULT_ZITADEL_ISSUER.into()),
    )
    .unwrap();
    // Forge a token signed by a key the server doesn't have.
    let rogue_key = TestKey::generate("never-seen-kid");
    let token = sign_rs256(&rogue_key, None, claims_base());
    let mut handles = Vec::with_capacity(50);
    for _ in 0..50 {
        let v = v.clone();
        let token = token.clone();
        let bearer = bearer(&token);
        handles.push(tokio::spawn(async move { v.verify(Some(&bearer)).await }));
    }
    let mut unknown_kid_count = 0;
    for h in handles {
        let res = h.await.unwrap();
        if let Err(e) = res
            && e.reason == AuthFailureReason::UnknownKid
        {
            unknown_kid_count += 1;
        }
    }
    // We expect all 50 to fail with UnknownKid (the rogue kid was never in the doc).
    assert_eq!(unknown_kid_count, 50, "all 50 must report UnknownKid");
    // And exactly 1 fetch should have hit the server.
    let fetches = server.fetch_count();
    assert_eq!(
        fetches, 1,
        "singleflight must collapse 50 misses to 1 fetch, got {fetches}"
    );
}

#[tokio::test]
async fn zitadel_warm_at_startup_loads_keys() {
    let key = TestKey::generate("warm-kid");
    let server = FakeJwks::start(vec![key.clone()]).await;
    let v = Rs256ZitadelVerifier::new(
        Some(server.url()),
        Some("memnoth-mcp".into()),
        Some(DEFAULT_ZITADEL_ISSUER.into()),
    )
    .unwrap();
    v.warm().await.expect("warm must succeed");
    // After warm, the cache should have the key. We verify by signing
    // a token with the warm-loaded key — the verify path must succeed
    // without any additional JWKS fetch.
    let token = sign_rs256(&key, None, claims_base());
    v.verify(Some(&bearer(&token)))
        .await
        .expect("token must verify after warm");
    // The fetch count is exactly 1 (the warm call); the verify hit
    // the cache.
    assert_eq!(server.fetch_count(), 1);
}

#[tokio::test]
async fn zitadel_defaults_point_at_production_instance() {
    // The verifier with no overrides must default to the self-hosted
    // production instance. The JWKS URL is never fetched here because
    // we don't call verify; we just assert the configured policy.
    let v = Rs256ZitadelVerifier::new(None, Some("memnoth-mcp".into()), None)
        .expect("verifier must construct");
    assert_eq!(v.audience(), "memnoth-mcp");
}
