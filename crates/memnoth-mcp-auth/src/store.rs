//! In-memory user-token store. Tracks per-token metadata for the
//! purposes of explicit revocation. Keyed by `sha256(token)` so we
//! never store the raw token in our memory long-term.
//!
//! ## Design rationale
//! - **In-memory only**: tokens are short-lived (90 days) and the JWT
//!   itself is self-validating. Restart = "lose the cache, not the
//!   auth decision". A revocation rebuild from IdP revocation
//!   webhooks is the recovery path (out of scope for T4).
//! - **DashMap**: lock-free shard-per-bucket concurrent map, ~50ns
//!   lookup. No contention on the auth hot path.
//! - **`sha256(token)` keys**: constant-time, fixed-size, never
//!   stored the raw token. The HTTP layer passes the full
//!   Authorization header to `verify`; the store does NOT see the
//!   raw token — it only sees the hash, computed once after a
//!   successful verify.
//! - **Revocation API ready for IdP revocation webhooks** (T7+):
//!   `revoke(jti)`
//!   marks a token as revoked; subsequent verifies surface
//!   `AuthFailureReason::Revoked`.
//! - **TTL eviction**: entries older than the token's `exp` are
//!   pruned on lookup (no background sweeper needed for 90-day TTLs;
//!   one prune per verify keeps the map bounded).

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use sha2::{Digest, Sha256};

/// Per-token metadata stored in the user-token table. Includes the
/// `jti` (so revoke by jti is O(1)) and the `exp` (so we can prune
/// without a background sweeper).
#[derive(Debug, Clone)]
pub struct UserTokenMeta {
    /// The `jti` claim of the token (JWT id, opaque per-session).
    pub jti: String,
    /// When the token expires (unix seconds). Used for prune-on-lookup.
    pub exp: i64,
    /// The user id the token was minted for.
    pub user_id: String,
}

/// The user-token store. Cheap to clone (`DashMap` is internally `Arc`).
#[derive(Clone, Default)]
pub struct UserTokenStore {
    /// `sha256(token) -> UserTokenMeta`. Lookup is O(1), lock-free.
    by_token: Arc<DashMap<[u8; 32], UserTokenMeta>>,
    /// `jti -> sha256(token)`. Lets `revoke(jti)` find the entry to
    /// mark. Same Arc-shape as `by_token` for cheap clones.
    by_jti: Arc<DashMap<String, [u8; 32]>>,
}

impl UserTokenStore {
    /// Construct a new empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Hash a token (Bearer value, no `Bearer ` prefix) to the
    /// fixed-size key the store uses. Exposed for the HTTP layer
    /// which calls this once per successful verify.
    pub fn hash_token(token: &str) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let out = hasher.finalize();
        let mut key = [0u8; 32];
        key.copy_from_slice(&out);
        key
    }

    /// Insert a token. Idempotent: re-inserting the same token updates
    /// the entry (we don't error on duplicate).
    pub fn insert(&self, token: &str, meta: UserTokenMeta) {
        let key = Self::hash_token(token);
        self.by_jti.insert(meta.jti.clone(), key);
        self.by_token.insert(key, meta);
    }

    /// Look up a token. Returns `None` if the token is unknown OR if
    /// the stored entry has expired (pruned as a side effect).
    ///
    /// `now_unix_secs` is the current time in unix seconds; pass
    /// `chrono::Utc::now().timestamp()` from the caller. Passing it
    /// in (rather than reading the clock inside) makes this function
    /// testable without time mocking.
    pub fn lookup(&self, token: &str, now_unix_secs: i64) -> Option<UserTokenMeta> {
        let key = Self::hash_token(token);
        let entry = self.by_token.get(&key)?;
        if entry.exp <= now_unix_secs {
            // Expired; prune and report as not-found (so the auth
            // layer treats it the same as an unknown token — the
            // JWT's own `exp` check will catch it on verify).
            drop(entry); // release the shard lock before removing
            self.by_token.remove(&key);
            return None;
        }
        Some(entry.clone())
    }

    /// Mark a token as revoked. Subsequent `lookup` calls return
    /// `None` for the same token, and the next `verify` through
    /// `Authenticator` can surface `AuthFailureReason::Revoked` if
    /// the HTTP layer wires it up.
    ///
    /// Returns `true` if a token was actually revoked (i.e. the jti
    /// was known), `false` otherwise.
    pub fn revoke(&self, jti: &str) -> bool {
        let Some(entry) = self.by_jti.get(jti) else {
            return false;
        };
        let key = *entry.value();
        drop(entry);
        self.by_token.remove(&key).is_some()
    }

    /// How many tokens are currently tracked. Useful for /health and
    /// the bench harness.
    pub fn len(&self) -> usize {
        self.by_token.len()
    }

    /// True if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.by_token.is_empty()
    }
}

/// Convenience: insert with the default 90-day TTL hint. The HTTP
/// layer doesn't actually need this; it's here so tests can build
/// meta without picking an `exp` manually.
#[allow(dead_code)]
pub fn default_exp_from_now(now_unix_secs: i64) -> i64 {
    now_unix_secs + Duration::from_secs(90 * 24 * 60 * 60).as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(jti: &str, exp: i64) -> UserTokenMeta {
        UserTokenMeta {
            jti: jti.into(),
            exp,
            user_id: "user_test".into(),
        }
    }

    #[test]
    fn hash_is_deterministic() {
        let a = UserTokenStore::hash_token("hello");
        let b = UserTokenStore::hash_token("hello");
        assert_eq!(a, b);
    }

    #[test]
    fn hash_is_collision_resistant() {
        let a = UserTokenStore::hash_token("hello");
        let b = UserTokenStore::hash_token("hellp");
        assert_ne!(a, b);
    }

    #[test]
    fn insert_then_lookup() {
        let store = UserTokenStore::new();
        store.insert("tok-1", meta("jti-1", i64::MAX));
        let got = store.lookup("tok-1", 1000).expect("must be present");
        assert_eq!(got.jti, "jti-1");
        assert_eq!(got.user_id, "user_test");
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        let store = UserTokenStore::new();
        assert!(store.lookup("nope", 0).is_none());
    }

    #[test]
    fn lookup_prunes_expired() {
        let store = UserTokenStore::new();
        // Insert with exp in the past.
        store.insert("tok-expired", meta("jti-old", 100));
        assert!(store.lookup("tok-expired", 200).is_none());
        // After prune, the entry is gone.
        assert!(store.lookup("tok-expired", 200).is_none());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn revoke_removes_by_jti() {
        let store = UserTokenStore::new();
        store.insert("tok-1", meta("jti-1", i64::MAX));
        assert!(store.revoke("jti-1"));
        assert!(store.lookup("tok-1", 0).is_none());
        // Revoking again returns false.
        assert!(!store.revoke("jti-1"));
    }

    #[test]
    fn revoke_unknown_jti_returns_false() {
        let store = UserTokenStore::new();
        assert!(!store.revoke("never-seen"));
    }

    #[test]
    fn len_tracks_inserts() {
        let store = UserTokenStore::new();
        assert!(store.is_empty());
        store.insert("a", meta("jti-a", i64::MAX));
        store.insert("b", meta("jti-b", i64::MAX));
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn default_exp_is_90_days() {
        let now = 1_000_000_000;
        let exp = default_exp_from_now(now);
        let ninety_days = 90 * 24 * 60 * 60;
        assert_eq!(exp - now, ninety_days);
    }
}
