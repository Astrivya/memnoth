//! Fake JWKS server for integration tests. A `hyper` 1.x HTTP/1
//! listener that serves:
//! - `GET /.well-known/jwks.json` — returns the current key set
//! - any other path — 404
//!
//! Tracks request count via an `Arc<AtomicU64>` so the singleflight
//! test can assert that 100 concurrent requests for the same unknown
//! `kid` produce exactly 1 (or 2: initial miss + refresh) fetches.
//!
//! Inspired by the style in `memnoth-mcp/src/http.rs` — hand-rolled
//! HTTP, no framework dependency, only the test fixture pulls in
//! `hyper` (and only in dev-dependencies).

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use rsa::RsaPrivateKey;
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use tokio::net::TcpListener;

/// An RSA keypair paired with its base64url-encoded JWK components.
#[derive(Clone)]
pub struct TestKey {
    pub kid: String,
    pub n_b64: String,
    pub e_b64: String,
    pub private: RsaPrivateKey,
}

impl TestKey {
    /// Generate a fresh 2048-bit RSA keypair.
    pub fn generate(kid: impl Into<String>) -> Self {
        use rand::rngs::OsRng;
        let mut rng = OsRng;
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("RSA keygen");
        let n_b64 =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(private.n().to_bytes_be());
        let e_b64 =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(private.e().to_bytes_be());
        Self {
            kid: kid.into(),
            n_b64,
            e_b64,
            private,
        }
    }

    /// The JWK JSON object for this key (no wrapping).
    pub fn jwk_json(&self) -> String {
        format!(
            r#"{{"kid":"{}","kty":"RSA","alg":"RS256","use":"sig","n":"{}","e":"{}"}}"#,
            self.kid, self.n_b64, self.e_b64
        )
    }
}

/// Handle to a running fake JWKS server. Drop it to shut down.
pub struct FakeJwks {
    pub addr: SocketAddr,
    // Retained so the fixture owns the key material for its whole
    // lifetime (the serving task clones it); tests that want to
    // inspect keys can read this field.
    #[allow(dead_code)]
    pub keys: Vec<TestKey>,
    pub request_count: Arc<AtomicU64>,
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

impl FakeJwks {
    /// Start the server on an ephemeral port. The `request_count`
    /// tracks every GET to `/.well-known/jwks.json`.
    pub async fn start(keys: Vec<TestKey>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let request_count = Arc::new(AtomicU64::new(0));
        let request_count_clone = request_count.clone();
        let keys_clone = keys.clone();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accept = listener.accept() => {
                        let (stream, _peer) = match accept {
                            Ok(s) => s,
                            Err(_) => continue,
                        };
                        let keys = keys_clone.clone();
                        let counter = request_count_clone.clone();
                        let io = TokioIo::new(stream);
                        tokio::spawn(async move {
                            let _ = http1::Builder::new()
                                .serve_connection(
                                    io,
                                    service_fn(move |req: Request<Incoming>| {
                                        let keys = keys.clone();
                                        let counter = counter.clone();
                                        async move { handle(req, keys, counter).await }
                                    }),
                                )
                                .await;
                        });
                    }
                }
            }
        });
        Self {
            addr,
            keys,
            request_count,
            _shutdown: tx,
        }
    }

    /// The JWKS URL to give to `JwksCache::new`.
    pub fn url(&self) -> String {
        format!("http://{}/.well-known/jwks.json", self.addr)
    }

    /// How many times the JWKS endpoint was hit.
    pub fn fetch_count(&self) -> u64 {
        self.request_count.load(Ordering::Relaxed)
    }
}

async fn handle(
    req: Request<Incoming>,
    keys: Vec<TestKey>,
    counter: Arc<AtomicU64>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if req.method() == hyper::Method::GET && req.uri().path() == "/.well-known/jwks.json" {
        counter.fetch_add(1, Ordering::Relaxed);
        let keys_json: Vec<String> = keys.iter().map(|k| k.jwk_json()).collect();
        let body = format!(r#"{{"keys":[{}]}}"#, keys_json.join(","));
        Ok(Response::builder()
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .unwrap())
    } else {
        Ok(Response::builder()
            .status(404)
            .body(Full::new(Bytes::from("not found")))
            .unwrap())
    }
}

/// Sign a JWT with a `TestKey`. Returns the token string (no `Bearer `
/// prefix).
pub fn sign_rs256(
    key: &TestKey,
    header_extra: Option<serde_json::Value>,
    claims: serde_json::Value,
) -> String {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(key.kid.clone());
    if let Some(extra) = header_extra
        && let Some(obj) = extra.as_object()
    {
        for (k, v) in obj {
            if k == "kid" || k == "alg" {
                continue;
            }
            // We don't model every header field; this helper is
            // only for ad-hoc test variations.
            eprintln!("ignoring extra header field {k}={v}");
        }
    }
    let pem = key
        .private
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .expect("PEM encode");
    let key_pem = pem.as_bytes();
    let encoding = EncodingKey::from_rsa_pem(key_pem).expect("EncodingKey from PEM");
    encode(&header, &claims, &encoding).expect("sign")
}
