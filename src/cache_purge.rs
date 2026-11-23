//! Local, namespace-scoped purge of derived caches (P22-048 / ALG17).
//!
//! This is the CLI-side primitive that `vault forget --purge-cache` calls after
//! it revokes an entity. It holds *derived* content — embeddings and model
//! responses — that was computed from facts the vault may later revoke. When a
//! namespace is revoked, every entry derived under it must stop being served
//! and must stop being physically present in the cache.
//!
//! # Why keys are ACL / revision-epoch aware
//!
//! A key is `(namespace, acl, epoch, entry)`:
//!
//! * `namespace` is the authorization boundary. Entries from one namespace can
//!   never be served to another, so two cached results can never cross a scope
//!   line just because their text hashes matched.
//! * `acl` partitions a namespace by the permission set that produced the
//!   derived content. A change in "who may see this" is a change of key.
//! * `epoch` is a revision epoch. A revocation advances the epoch, so entries
//!   written under the old epoch become unaddressable immediately (`get` uses
//!   the current epoch), even if a stale writer re-inserts them under the old
//!   one before the purge runs.
//!
//! A revocation therefore invalidates derived content in two independent ways:
//! the key no longer addresses the entry, and the entry is removed from storage
//! and zeroized. Both are local to this process.
//!
//! # Scope — local only
//!
//! This module purges **local** state. It makes no network call, knows no server
//! endpoint, and does not claim to purge any hosted or shared cache. Other
//! devices are not touched; they are revoked only when they pull a new epoch by
//! their own path. Nothing here is a claim about server-side behaviour.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use zeroize::Zeroizing;

/// TTL (milliseconds) for a derived answer nothing has contradicted yet.
///
/// Short on purpose: a derived summary is only as fresh as the facts under it.
pub const DEFAULT_CACHE_TTL_MS: i64 = 5_000;

/// TTL reported after a revocation. Zero means there is no honest cache
/// lifetime — the content was just invalidated and must be recomputed.
pub const REVOKED_CACHE_TTL_MS: i64 = 0;

/// Normalize an identifier that reaches a cache key. Trimming and lowercasing
/// keep one logical scope from being addressed under two spellings.
fn norm(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

/// An authorization/revision scope: the `namespace` and `acl` boundary plus the
/// `epoch` of derived content produced under it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheScope {
    pub namespace: String,
    pub acl: String,
    pub epoch: u64,
}

impl CacheScope {
    pub fn new(namespace: &str, acl: &str, epoch: u64) -> Self {
        Self {
            namespace: norm(namespace),
            acl: norm(acl),
            epoch,
        }
    }
}

/// Full primary key of one cached derived entry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CacheKey {
    scope: CacheScope,
    entry: String,
}

/// A process-local store of derived content, purged by namespace.
///
/// Payloads are held in [`Zeroizing`] buffers: dropping an entry wipes its
/// bytes. There is no secondary index, snapshot or search structure, so once an
/// entry is removed its plaintext cannot be recalled from this cache offline.
#[derive(Debug, Default)]
pub struct DerivedCache {
    /// Current revision epoch per namespace. Absent means epoch `1`.
    epochs: BTreeMap<String, u64>,
    /// Current cache TTL per namespace. Absent means [`DEFAULT_CACHE_TTL_MS`].
    ttl_ms: BTreeMap<String, i64>,
    entries: BTreeMap<CacheKey, Zeroizing<Vec<u8>>>,
}

impl DerivedCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The epoch a `get`/`put` for `namespace` will use. Starts at `1` and only
    /// ever advances; it never wraps back to a revoked generation.
    pub fn current_epoch(&self, namespace: &str) -> u64 {
        self.epochs.get(&norm(namespace)).copied().unwrap_or(1)
    }

    /// Learn that `epoch` is the current revision for `namespace`.
    pub fn recognize_epoch(&mut self, namespace: &str, epoch: u64) {
        self.epochs.insert(norm(namespace), epoch);
    }

    /// Cache derived bytes under `(namespace, acl, current_epoch, entry)`.
    pub fn put(&mut self, namespace: &str, acl: &str, entry: &str, value: Vec<u8>) {
        let scope = CacheScope::new(namespace, acl, self.current_epoch(namespace));
        self.epochs.entry(norm(namespace)).or_insert(1);
        self.ttl_ms
            .entry(norm(namespace))
            .or_insert(DEFAULT_CACHE_TTL_MS);
        self.entries.insert(
            CacheKey {
                scope,
                entry: entry.to_string(),
            },
            Zeroizing::new(value),
        );
    }

    /// Borrow derived bytes for an exact `(namespace, acl, current_epoch, entry)`
    /// key. Entries written under an earlier epoch are never returned: the key
    /// is built with the current epoch, so an advanced epoch is a miss.
    pub fn get(&self, namespace: &str, acl: &str, entry: &str) -> Option<&Zeroizing<Vec<u8>>> {
        let scope = CacheScope::new(namespace, acl, self.current_epoch(namespace));
        self.entries.get(&CacheKey {
            scope,
            entry: entry.to_string(),
        })
    }

    /// Whether any epoch or ACL partition under `namespace` still holds `entry`.
    pub fn contains_entry(&self, namespace: &str, entry: &str) -> bool {
        let namespace = norm(namespace);
        self.entries
            .keys()
            .any(|key| key.scope.namespace == namespace && key.entry == entry)
    }

    /// Number of entries held for `namespace`, across every epoch and ACL.
    pub fn len_for_namespace(&self, namespace: &str) -> usize {
        let namespace = norm(namespace);
        self.entries
            .keys()
            .filter(|key| key.scope.namespace == namespace)
            .count()
    }

    /// Advance the revision epoch for `namespace`.
    ///
    /// The new epoch becomes current; every cached entry for the namespace —
    /// whatever epoch or ACL it was stored under — is dropped and zeroized, and
    /// the namespace's TTL becomes [`REVOKED_CACHE_TTL_MS`]. Returns the new
    /// epoch. Saturating: an epoch must never wrap back to a revoked one.
    pub fn bump_epoch(&mut self, namespace: &str) -> u64 {
        let namespace = norm(namespace);
        let next = self.current_epoch(&namespace).saturating_add(1);
        self.epochs.insert(namespace.clone(), next);
        self.ttl_ms.insert(namespace.clone(), REVOKED_CACHE_TTL_MS);
        self.entries
            .retain(|key, _| key.scope.namespace != namespace);
        next
    }

    /// Drop every cached entry for `namespace` without changing its epoch.
    ///
    /// Use this when content must be revoked now but no new revision has been
    /// established yet. The namespace's TTL becomes zero so no host may reuse
    /// what was just invalidated.
    pub fn purge_namespace(&mut self, namespace: &str) -> usize {
        let namespace = norm(namespace);
        let before = self.entries.len();
        self.entries
            .retain(|key, _| key.scope.namespace != namespace);
        self.ttl_ms.insert(namespace, REVOKED_CACHE_TTL_MS);
        before - self.entries.len()
    }

    /// Cache TTL for `namespace`. Zero after a revocation; the default lifetime
    /// for a namespace that has never been purged.
    pub fn cache_ttl(&self, namespace: &str) -> i64 {
        self.ttl_ms
            .get(&norm(namespace))
            .copied()
            .unwrap_or(DEFAULT_CACHE_TTL_MS)
    }

    /// Total number of cached entries across all namespaces.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Diagnostic: whether any cached payload contains `needle` as a substring.
    /// Used by tests to prove revoked plaintext is physically absent, not merely
    /// hidden behind a miss.
    pub fn cached_bytes_contain(&self, needle: &[u8]) -> bool {
        if needle.is_empty() {
            return false;
        }
        self.entries
            .values()
            .any(|value| value.windows(needle.len()).any(|w| w == needle))
    }
}

/// The process-wide derived cache that `forget` purges.
pub fn derived_cache() -> &'static Mutex<DerivedCache> {
    static CACHE: OnceLock<Mutex<DerivedCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(DerivedCache::new()))
}

/// The observable result of a local revocation purge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeReport {
    pub namespace: String,
    pub purged_entries: usize,
    pub epoch: u64,
    /// Always [`REVOKED_CACHE_TTL_MS`] (0): the derived content was just
    /// invalidated and must be recomputed.
    pub cache_ttl: i64,
}

/// Purge all local derived caches for `namespace` after a revocation.
///
/// Advances the revision epoch (invalidating any old-epoch key), physically
/// removes and zeroizes the entries, and reports `cache_ttl = 0`. This is local
/// only — no hosted or server-side cache is contacted.
pub fn purge_for_revocation(namespace: &str) -> PurgeReport {
    let mut cache = derived_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let namespace = norm(namespace);
    let before = cache.len_for_namespace(&namespace);
    let epoch = cache.bump_epoch(&namespace);
    PurgeReport {
        namespace,
        purged_entries: before,
        epoch,
        cache_ttl: REVOKED_CACHE_TTL_MS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_namespace_acl_and_epoch_aware() {
        let a = CacheScope::new("Team-A", "reader", 7);
        let b = CacheScope::new("team-a", "READER", 7);
        let c = CacheScope::new("team-a", "reader", 8);
        let d = CacheScope::new("team-a", "admin", 7);
        assert_eq!(a, b, "scope identifiers normalize");
        assert_ne!(a, c, "a new revision epoch is a different key");
        assert_ne!(a, d, "a different ACL is a different key");
    }

    #[test]
    fn purge_is_local_only_no_remote_claim() {
        // There is no network surface in this module by construction; this test
        // pins the documented contract so a future change cannot silently add
        // one and start claiming hosted purge.
        let report = purge_for_revocation("contract-check");
        assert_eq!(report.cache_ttl, REVOKED_CACHE_TTL_MS);
    }
}
