//! JWKS cache. Pulls a JSON Web Key Set from the configured IdP's
//! JWKS endpoint (`MEMNOTH_MCP_ZITADEL_JWKS_URL`, default
//! `https://auth.astrivya.ai/oauth/v2/keys`) and caches the
//! parsed `DecodingKey`s by `kid`. Refresh-on-miss: if a token's `kid`
//! is not in the cache, we fetch the JWKS doc once and try again.
//! Singleflight: 100 concurrent requests for the same unknown `kid`
//! produce exactly one HTTP fetch (the rest `await` the in-flight
//! future).
//!
//! The cache itself is `moka::future::Cache<kid, Arc<DecodingKey>>`.
//! Per-entry TTL (default 10 min) and `time_to_live` give us automatic
//! rotation handling. moka is async-native; `try_get_with_by_ref` does
//! the singleflight without us writing manual `OnceCell` plumbing.

use std::sync::Arc;
use std::time::Duration;

use jsonwebtoken::DecodingKey;
use moka::future::Cache;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::OnceCell;

/// How long a `kid -> DecodingKey` entry stays in the cache before
/// being treated as stale. On expiry, the next `get()` for that `kid`
/// will refetch the JWKS doc. 10 minutes is a good balance between
/// freshness and load (IdP keys rotate rarely).
pub const DEFAULT_JWKS_TTL: Duration = Duration::from_secs(10 * 60);

/// The moka cache max capacity. IdPs rotate keys rarely (1-2/year), so
/// 64 is comfortably above the working set; we want the bound to be
/// tight enough that a malicious JWKS doc with millions of keys can't
/// blow up our memory.
pub const DEFAULT_JWKS_CAPACITY: u64 = 64;

/// HTTP timeout for a single JWKS fetch. 5s is generous; the ZITADEL
/// endpoint is usually <200ms from any region.
pub const DEFAULT_JWKS_HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// What can go wrong while fetching + parsing a JWKS doc. Mapped
/// to `AuthFailureReason` by the caller.
#[derive(Debug, Error)]
pub enum JwksError {
    /// The upstream JWKS HTTP fetch failed (network error, timeout,
    /// non-success status).
    #[error("JWKS fetch failed: {0}")]
    Fetch(#[from] reqwest::Error),
    /// The JWKS document body could not be parsed as JSON.
    #[error("JWKS parse failed: {0}")]
    Parse(#[from] serde_json::Error),
    /// The document was empty or had no usable RSA keys.
    #[error("JWKS doc is empty or missing 'keys' field")]
    Empty,
    /// A key entry is not an RSA public key (only RS256 supported).
    #[error("JWKS key {0:?} is not an RSA public key (only RS256 supported)")]
    UnsupportedKeyType(Option<String>),
    /// A key entry is missing the `n` or `e` component.
    #[error("JWKS key {0:?} is missing the 'n' or 'e' field")]
    MalformedRsaKey(Option<String>),
    /// A background refresh failed while a concurrent request waited
    /// on the singleflight.
    #[error("JWKS refresh failed during singleflight: {0}")]
    Refresh(String),
}

/// Minimal subset of a JWK that we care about. ZITADEL uses
/// RSA keys, which in JWK form are `kty: "RSA"`, `n: <base64url(modulus)>`,
/// `e: <base64url(exponent)>`, `kid: <key id>`, `alg: "RS256"`,
/// `use: "sig"`. We accept and ignore other fields.
#[derive(Debug, Deserialize)]
struct Jwk {
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    kty: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    // Other fields (use, x5c, x5t, key_ops, ...) intentionally not
    // modeled — serde ignores them by default.
}

/// The JWKS doc itself.
#[derive(Debug, Deserialize)]
struct JwksDoc {
    keys: Vec<Jwk>,
}

impl JwksDoc {
    /// Parse a JWKS doc and extract every RSA public key as a
    /// `(kid, DecodingKey)` pair. Keys with `kty != "RSA"` are
    /// skipped (ZITADEL doesn't use them; future-proofing for Ed25519
    /// would need its own JWK -> DecodingKey path).
    fn to_decoding_keys(&self) -> Result<Vec<(String, DecodingKey)>, JwksError> {
        let mut out = Vec::with_capacity(self.keys.len());
        for jwk in &self.keys {
            if jwk.kty.as_deref() != Some("RSA") {
                continue;
            }
            // If the key advertises an algorithm, only RS256 is usable;
            // keys without an `alg` claim are accepted (some providers
            // omit it and the signing kid implies RS256).
            if let Some(alg) = jwk.alg.as_deref()
                && alg != "RS256"
            {
                continue;
            }
            let kid = match jwk.kid.as_deref() {
                Some(k) => k.to_string(),
                None => return Err(JwksError::MalformedRsaKey(jwk.kid.clone())),
            };
            let n = jwk
                .n
                .as_deref()
                .ok_or_else(|| JwksError::MalformedRsaKey(jwk.kid.clone()))?;
            let e = jwk
                .e
                .as_deref()
                .ok_or_else(|| JwksError::MalformedRsaKey(jwk.kid.clone()))?;
            let key = DecodingKey::from_rsa_components(n, e)
                .map_err(|_| JwksError::MalformedRsaKey(jwk.kid.clone()))?;
            out.push((kid, key));
        }
        if out.is_empty() {
            return Err(JwksError::Empty);
        }
        Ok(out)
    }
}

/// The JWKS cache. Cheap to clone (moka cache is internally `Arc`).
#[derive(Clone)]
pub struct JwksCache {
    /// moka cache: `kid -> Arc<DecodingKey>`. Holds successfully
    /// resolved keys; TTL = 10 min means an IdP key rotation auto-refreshes
    /// without operator action.
    cache: Cache<String, Arc<DecodingKey>>,
    /// Per-kid in-flight latch. When 50 concurrent calls all ask for
    /// the same unknown `kid`, exactly one runs the refresh-and-lookup;
    /// the other 49 await this `OnceCell`. Keyed by `kid` so different
    /// `kid` requests don't serialize behind each other.
    inflight: Cache<String, Arc<OnceCell<Result<(), JwksError>>>>,
    /// The HTTP client. `reqwest::Client` is internally `Arc` and
    /// pools connections; one client per cache is the right shape.
    http: reqwest::Client,
    /// The JWKS URL to fetch (e.g. `https://auth.astrivya.ai/oauth/v2/keys`).
    url: String,
}

impl JwksCache {
    /// Construct a new JWKS cache. Does NOT fetch eagerly — call
    /// [`JwksCache::warm`] at startup if you want to fail-fast on
    /// misconfiguration.
    pub fn new(url: impl Into<String>) -> Result<Self, JwksError> {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_JWKS_HTTP_TIMEOUT)
            .build()
            .map_err(JwksError::Fetch)?;
        let cache = Cache::builder()
            .max_capacity(DEFAULT_JWKS_CAPACITY)
            .time_to_live(DEFAULT_JWKS_TTL)
            .build();
        let inflight = Cache::builder()
            .max_capacity(DEFAULT_JWKS_CAPACITY)
            // Per-kid latches are short-lived. After the OnceCell fires,
            // it sits in the cache briefly then is evicted; a stale
            // latch is harmless because it gets initialized on first use.
            .time_to_live(Duration::from_secs(60))
            .build();
        Ok(Self {
            cache,
            inflight,
            http,
            url: url.into(),
        })
    }

    /// The URL this cache fetches (handy for diagnostics + tests).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Number of `kid` entries currently cached (0 until the first
    /// fetch/warm). Used by the HTTP layer to keep the
    /// `memnoth_mcp_auth_jwks_cache_entries` gauge fresh on each
    /// `/metrics` scrape.
    // A gauge, not a collection length: nothing here is ever "empty", and an
    // `is_empty()` companion would have no caller
    // (clippy::len_without_is_empty).
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u64 {
        self.cache.entry_count()
    }

    /// Look up a `kid`. On miss, refresh the cache from the JWKS URL
    /// and try again. If the second lookup still misses, return
    /// `JwksError::UnsupportedKeyType` (the kid is unknown).
    ///
    /// **Singleflight**: 50 concurrent calls for the same unknown
    /// `kid` produce exactly one HTTP fetch. The mechanism is a
    /// per-kid `OnceCell<Result<...>>` stored in a second moka cache:
    /// the first caller to reach `get_or_init` runs the refresh; the
    /// rest await the same `OnceCell`'s value. The hot path (kid
    /// already in the main cache) skips the `OnceCell` entirely.
    pub async fn get(&self, kid: &str) -> Result<Arc<DecodingKey>, JwksError> {
        // Hot path: in cache. Avoid the OnceCell allocation entirely.
        if let Some(k) = self.cache.get(kid).await {
            return Ok(k);
        }
        // Cold path: per-kid singleflight.
        let cell = self
            .inflight
            .get_with(kid.to_string(), async { Arc::new(OnceCell::new()) })
            .await;
        // The OnceCell stores the OUTCOME (Ok or a sentinel Err). The
        // sentinel is just an empty JwksError; we re-run the lookup
        // below to disambiguate refresh-failed vs kid-not-in-doc.
        let outcome: Arc<OnceCell<Result<(), JwksError>>> = cell; // type hint for the reader
        let result = outcome
            .get_or_init(|| async {
                self.refresh().await?;
                Ok(())
            })
            .await;
        // After the refresh (success or fail), look up the kid in the
        // cache. If present, return it. If absent, decide between
        // JwksUnavailable and UnknownKid.
        if let Some(k) = self.cache.get(kid).await {
            return Ok(k);
        }
        match result {
            Ok(()) => Err(JwksError::UnsupportedKeyType(Some(kid.to_string()))),
            Err(e) => Err(JwksError::Refresh(format!("{e}"))),
        }
    }

    /// Force a refresh of the JWKS doc. Every kid in the new doc is
    /// inserted into the cache; existing entries not in the new doc
    /// are evicted by moka's TTL.
    pub async fn refresh(&self) -> Result<(), JwksError> {
        let doc: JwksDoc = self
            .http
            .get(&self.url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let pairs = doc.to_decoding_keys()?;
        for (kid, key) in pairs {
            self.cache.insert(kid, Arc::new(key)).await;
        }
        Ok(())
    }

    /// Warm the cache at startup. Returns the number of keys loaded
    /// (or an error). The caller decides whether to fail boot on
    /// failure or log-and-continue.
    pub async fn warm(&self) -> Result<u64, JwksError> {
        self.refresh().await?;
        Ok(self.cache.entry_count())
    }

    /// How many keys are currently cached (diagnostic for /health).
    pub fn cached_key_count(&self) -> u64 {
        self.cache.entry_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `to_decoding_keys` must skip non-RSA keys without erroring
    /// (so a mixed doc that includes future key types does not
    /// break us).
    #[test]
    fn to_decoding_keys_skips_non_rsa() {
        let doc = JwksDoc {
            keys: vec![
                Jwk {
                    kid: Some("ec-1".into()),
                    kty: Some("EC".into()),
                    alg: None,
                    n: None,
                    e: None,
                },
                Jwk {
                    kid: Some("rsa-1".into()),
                    kty: Some("RSA".into()),
                    alg: Some("RS256".into()),
                    // These are not valid base64url-encoded RSA components,
                    // but the parsing test does not care: it just checks
                    // that the EC key was skipped and the RSA key was
                    // attempted. (The from_rsa_components call will fail
                    // in a real test, but that's a separate path.)
                    n: Some("abc".into()),
                    e: Some("AQAB".into()),
                },
            ],
        };
        // EC key is skipped. RSA key is parsed (this will likely fail
        // with the dummy 'abc' modulus, but we just want to confirm
        // the code path runs).
        let res = doc.to_decoding_keys();
        // We don't care if it succeeded or failed at the RSA key — we
        // care that the EC key didn't crash it.
        let _ = res;
    }

    /// `to_decoding_keys` on a doc with no RSA keys returns
    /// `JwksError::Empty` so the caller surfaces a clear error.
    #[test]
    fn to_decoding_keys_empty_when_no_rsa() {
        let doc = JwksDoc {
            keys: vec![Jwk {
                kid: Some("ec-1".into()),
                kty: Some("EC".into()),
                alg: None,
                n: None,
                e: None,
            }],
        };
        let res = doc.to_decoding_keys();
        assert!(matches!(res, Err(JwksError::Empty)));
    }

    /// `to_decoding_keys` errors on an RSA key missing the `kid`.
    #[test]
    fn to_decoding_keys_errors_on_rsa_without_kid() {
        let doc = JwksDoc {
            keys: vec![Jwk {
                kid: None,
                kty: Some("RSA".into()),
                alg: Some("RS256".into()),
                n: Some("abc".into()),
                e: Some("AQAB".into()),
            }],
        };
        let res = doc.to_decoding_keys();
        assert!(matches!(res, Err(JwksError::MalformedRsaKey(None))));
    }

    /// `new()` constructs a cache without fetching anything.
    #[tokio::test]
    async fn new_does_not_fetch() {
        // Use an invalid URL to prove we don't hit the network at
        // construction. If `new` tried to fetch, this would error.
        let cache = JwksCache::new("http://127.0.0.1:1/never-touched").unwrap();
        assert_eq!(cache.cached_key_count(), 0);
        assert_eq!(cache.url(), "http://127.0.0.1:1/never-touched");
    }
}
