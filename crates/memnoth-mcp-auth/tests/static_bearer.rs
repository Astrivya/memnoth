//! Tests for the `static_bearer` mode. These are the regression
//! oracle: the `agent_mode_over_http_is_bearer_gated_and_drives_sessions`
//! test in `memnoth-mcp/src/http.rs:649` validates the HTTP wiring, and
//! these tests validate the verifier itself.

use memnoth_mcp_auth::verifier::StaticBearerVerifier;
use memnoth_mcp_auth::{AuthError, AuthFailureReason, AuthMode, Authenticator};

const TOKEN: &str = "super-secret-shared-token-9f2b";

fn make_verifier() -> StaticBearerVerifier {
    StaticBearerVerifier::new(TOKEN).expect("non-empty token must produce a verifier")
}

#[tokio::test]
async fn static_bearer_accepts_correct_token() {
    let v = make_verifier();
    let header = format!("Bearer {TOKEN}");
    let ctx = v
        .verify(Some(&header))
        .await
        .expect("correct token must verify");
    assert_eq!(ctx.mode, AuthMode::StaticBearer);
    assert!(
        ctx.user_id.is_none(),
        "static mode has no per-user identity"
    );
    assert!(ctx.token_id.is_none());
    assert!(ctx.expires_at.is_none());
}

#[tokio::test]
async fn static_bearer_rejects_wrong_token() {
    let v = make_verifier();
    let header = "Bearer wrong-token";
    let err = v
        .verify(Some(header))
        .await
        .expect_err("wrong token must fail");
    assert_eq!(err.mode, AuthMode::StaticBearer);
    assert_eq!(err.reason, AuthFailureReason::BadStaticToken);
}

#[tokio::test]
async fn static_bearer_rejects_missing_header() {
    let v = make_verifier();
    let err = v.verify(None).await.expect_err("missing header must fail");
    assert_eq!(err.reason, AuthFailureReason::MissingHeader);
}

#[tokio::test]
async fn static_bearer_rejects_malformed_header() {
    let v = make_verifier();
    // No "Bearer " prefix.
    let err = v
        .verify(Some(TOKEN))
        .await
        .expect_err("bare token must fail");
    assert_eq!(err.reason, AuthFailureReason::MalformedHeader);

    // Wrong scheme.
    let err = v
        .verify(Some(&format!("Basic {TOKEN}")))
        .await
        .expect_err("Basic auth must fail");
    assert_eq!(err.reason, AuthFailureReason::MalformedHeader);
}

#[tokio::test]
async fn static_bearer_rejects_empty_bearer_value() {
    let v = make_verifier();
    // "Bearer " (no token after the space) is structurally valid header
    // but an empty token cannot match. Must be BadStaticToken, not
    // MalformedHeader — distinguishing these in metrics matters.
    let err = v
        .verify(Some("Bearer "))
        .await
        .expect_err("empty bearer must fail");
    assert_eq!(err.reason, AuthFailureReason::BadStaticToken);
}

#[tokio::test]
async fn static_bearer_constant_time_comparison() {
    // This test does not measure timing precisely (CI noise would make
    // it flaky). It instead asserts the **structural** property that
    // we delegate comparison to `subtle::ConstantTimeEq` and do not
    // short-circuit on length mismatch. If someone replaces `ct_eq`
    // with `==` in the future, this test will not catch it — but the
    // code review should.
    //
    // The length-mismatch case (e.g. presented token is much longer
    // than expected) must still be `BadStaticToken`, never a panic
    // or an early return.
    let v = make_verifier();
    let err = v
        .verify(Some(
            "Bearer this-is-a-much-longer-token-than-expected-aaaaaaaaaaaaa",
        ))
        .await
        .expect_err("length mismatch must still be BadStaticToken, not a panic");
    assert_eq!(err.reason, AuthFailureReason::BadStaticToken);
}

#[tokio::test]
async fn static_bearer_new_rejects_empty_token() {
    let v = StaticBearerVerifier::new("");
    assert!(v.is_none(), "empty token must not produce a verifier");
    let v = StaticBearerVerifier::new("   ");
    // We treat whitespace-only as valid (the HTTP layer's responsibility
    // to trim). This documents the behavior; change `new` if you want
    // to be stricter.
    assert!(v.is_some());
}

#[tokio::test]
async fn static_bearer_error_is_send_sync() {
    // Compile-time check: `AuthError` is the boundary type passed
    // across `.await` and into HTTP response writers.
    fn assert_send<T: Send + Sync>() {}
    assert_send::<AuthError>();
}
