//! Hot-path benchmarks for the auth crate.
//!
//! Targets (from `D:\xclaw\memnoth-findings.md` §12):
//! - cached HS256 verify: **<10μs p99**
//! - cached RS256 verify: **<50μs p99**
//!
//! Run with:
//! ```bash
//! $env:CARGO_TARGET_DIR='D:\xclaw\memnoth\target\verify3'
//! cd D:\xclaw\memnoth
//! cargo bench -p memnoth-mcp-auth
//! ```
//!
//! The criterion output is written to
//! `target/verify3/criterion/` with an HTML report at
//! `target/verify3/criterion/report/index.html`.

use std::sync::Arc;

use chrono::Utc;
use criterion::{Criterion, criterion_group, criterion_main};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use memnoth_mcp_auth::Authenticator;
use memnoth_mcp_auth::hs256::Hs256UserJwtVerifier;
use memnoth_mcp_auth::zitadel::{DEFAULT_ZITADEL_ISSUER, Rs256ZitadelVerifier};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use serde_json::json;

const HS_SECRET: &[u8] = b"bench-secret-not-real-9f2b-7c3a";
const HS_AUD: &str = "memnoth-mcp";
const HS_ISS: &str = "astrivya-memnoth-install";

fn hs256_token() -> String {
    let now = Utc::now().timestamp();
    let claims = json!({
        "iss": HS_ISS,
        "aud": HS_AUD,
        "sub": "bench-user-1",
        "jti": "tok_bench_001",
        "iat": now,
        "exp": now + 3600,
        "scope": "mcp:read mcp:write",
    });
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(HS_SECRET),
    )
    .expect("sign must succeed")
}

fn bench_hs256(c: &mut Criterion) {
    let v = Arc::new(
        Hs256UserJwtVerifier::new(HS_SECRET, Some(HS_AUD.into()), Some(HS_ISS.into()))
            .expect("verifier"),
    );
    let token = hs256_token();
    let header = format!("Bearer {token}");

    c.bench_function("hs256_verify_cached", move |b| {
        let v = v.clone();
        let header = header.clone();
        b.to_async(criterion::async_executor::FuturesExecutor)
            .iter(move || {
                let v = v.clone();
                let header = header.clone();
                async move { v.verify(Some(&header)).await }
            });
    });
}

fn bench_static(c: &mut Criterion) {
    use memnoth_mcp_auth::verifier::StaticBearerVerifier;
    let v = Arc::new(StaticBearerVerifier::new("static-bench-token").expect("verifier"));
    let header = "Bearer static-bench-token";

    c.bench_function("static_bearer_verify", move |b| {
        let v = v.clone();
        b.to_async(criterion::async_executor::FuturesExecutor)
            .iter(move || {
                let v = v.clone();
                async move { v.verify(Some(header)).await }
            });
    });
}

// RS256 bench: needs a pre-built `DecodingKey` (cache hit) and a real
// verifier. We pre-fetch the JWKS once (via warm) so subsequent
// verifies are pure cache hits.
fn bench_rs256(c: &mut Criterion) {
    use rsa::RsaPrivateKey;
    use tokio::runtime::Runtime;
    let rt = Runtime::new().expect("runtime");

    let key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).expect("rsa keygen");
    let n_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.n().to_bytes_be());
    let e_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.e().to_bytes_be());
    let kid = "bench-kid-1";

    // Hand-craft a JWKS doc JSON. We use a custom, ephemeral JWKS
    // "server" in the form of a verifier pointed at a data: URL... but
    // reqwest doesn't support data: URLs. Instead we use a static file
    // approach: write the JWKS to a temp file and use a file:// URL.
    // But reqwest also doesn't support file:// by default in newer
    // versions. The simplest path: spin up a hyper server on an
    // ephemeral port (like the integration test does) but inline so
    // the bench is self-contained.
    use base64::Engine;
    use http_body_util::Full;
    use hyper::body::{Bytes, Incoming};
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper::{Request, Response};
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    let jwks_body = format!(
        r#"{{"keys":[{{"kid":"{kid}","kty":"RSA","alg":"RS256","use":"sig","n":"{n_b64}","e":"{e_b64}"}}]}}"#
    );

    let listener = rt.block_on(async { TcpListener::bind("127.0.0.1:0").await.expect("bind") });
    let addr: SocketAddr = rt.block_on(async { listener.local_addr().expect("local_addr") });
    rt.spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let body = jwks_body.clone();
            let io = TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |_req: Request<Incoming>| {
                            let body = body.clone();
                            async move {
                                Ok::<_, Infallible>(
                                    Response::builder()
                                        .header("content-type", "application/json")
                                        .body(Full::new(Bytes::from(body)))
                                        .unwrap(),
                                )
                            }
                        }),
                    )
                    .await;
            });
        }
    });

    let url = format!("http://{addr}/.well-known/jwks.json");
    let v = rt.block_on(async {
        let v = Rs256ZitadelVerifier::new(
            Some(url),
            Some("memnoth-mcp".into()),
            Some(DEFAULT_ZITADEL_ISSUER.into()),
        )
        .expect("verifier");
        v.warm().await.expect("warm");
        Arc::new(v)
    });

    // Sign a token with the test key.
    let now = Utc::now().timestamp();
    let claims = json!({
        "iss": DEFAULT_ZITADEL_ISSUER,
        "aud": "memnoth-mcp",
        "sub": "389241132255084556",
        "jti": "tok_bench_rs256_001",
        "iat": now,
        "exp": now + 3600,
    });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.into());
    let pem = key.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).expect("pem");
    let token = encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_pem(pem.as_bytes()).expect("key from pem"),
    )
    .expect("sign");
    let bearer = format!("Bearer {token}");

    c.bench_function("rs256_verify_cached", move |b| {
        let v = v.clone();
        let bearer = bearer.clone();
        b.to_async(criterion::async_executor::FuturesExecutor)
            .iter(move || {
                let v = v.clone();
                let bearer = bearer.clone();
                async move { v.verify(Some(&bearer)).await }
            });
    });
}

criterion_group!(benches, bench_static, bench_hs256, bench_rs256);
criterion_main!(benches);
