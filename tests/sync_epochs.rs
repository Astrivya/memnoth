//! Epoch-keyed cache isolation, revocation and fail-closed behaviour.
//!
//! These tests exercise only the local cache primitive. They do not claim
//! hosted semantic search or remote revocation: a bump here invalidates this
//! process's plaintext, and another device is revoked only when it pulls the
//! new epoch. Evicted plaintext is zeroized and is not recoverable offline.

use memnoth::sync_crypto::{
    CacheError, CryptoError, OrgKey, OrgSalt, PlaintextCache, RevisionEpoch, ScopeId, encrypt,
};
use zeroize::Zeroizing;

fn plaintext(bytes: &[u8]) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(bytes.to_vec())
}

#[test]
fn two_epochs_isolate_entries() {
    let mut cache = PlaintextCache::new();
    let scope = ScopeId::new("project-a", "org-a");
    let epoch_one = RevisionEpoch::new(1);
    let epoch_two = RevisionEpoch::new(2);
    cache.recognize_epoch(&scope, epoch_one);
    cache.recognize_epoch(&scope, epoch_two);

    cache
        .put(&scope, epoch_one, "blob", plaintext(b"epoch one plaintext"))
        .expect("recognized epoch accepts a write");
    cache
        .put(&scope, epoch_two, "blob", plaintext(b"epoch two plaintext"))
        .expect("recognized epoch accepts a write");

    assert_eq!(
        cache
            .get(&scope, epoch_one, "blob")
            .expect("epoch one is known")
            .expect("hit")
            .as_slice(),
        b"epoch one plaintext"
    );
    assert_eq!(
        cache
            .get(&scope, epoch_two, "blob")
            .expect("epoch two is known")
            .expect("hit")
            .as_slice(),
        b"epoch two plaintext"
    );
    // The epoch is part of the key: a missing entry is a miss, not a neighbour
    // from another epoch or another scope.
    assert!(
        cache
            .get(&scope, epoch_one, "absent")
            .expect("known epoch")
            .is_none()
    );
    assert_eq!(cache.len(), 2);
}

#[test]
fn scope_ids_normalize_like_the_salts() {
    let first = ScopeId::new(" Project-A ", "Org:A");
    let second = ScopeId::new("project-a", "org:a");
    assert_eq!(first, second);

    let mut cache = PlaintextCache::new();
    cache.recognize_epoch(&first, RevisionEpoch::new(3));
    assert!(
        cache
            .get(&second, RevisionEpoch::new(3), "x")
            .expect("normalized scope is recognized")
            .is_none()
    );
    assert_ne!(first, ScopeId::new("project-b", "org:a"));
}

#[test]
fn bump_invalidates_all_cached_plaintext_for_the_scope() {
    let mut cache = PlaintextCache::new();
    let scope = ScopeId::new("project-a", "org-a");
    let other = ScopeId::new("project-b", "org-a");
    let epoch_one = RevisionEpoch::new(1);
    let epoch_two = RevisionEpoch::new(2);
    cache.recognize_epoch(&scope, epoch_one);
    cache.recognize_epoch(&scope, epoch_two);
    cache.recognize_epoch(&other, epoch_one);

    cache
        .put(&scope, epoch_one, "a", plaintext(b"old a"))
        .unwrap();
    cache
        .put(&scope, epoch_two, "b", plaintext(b"new b"))
        .unwrap();
    cache
        .put(&other, epoch_one, "c", plaintext(b"other c"))
        .unwrap();
    assert_eq!(cache.len(), 3);

    let bumped = cache.bump_epoch(&scope).expect("bump a known scope");
    assert_eq!(bumped, RevisionEpoch::new(3));
    assert_eq!(bumped.get(), 3);
    assert_eq!(cache.len(), 1, "only the other scope's plaintext survives");

    // Both old epochs are now uncertified: reads fail closed rather than
    // returning a miss or, worse, stale plaintext.
    assert!(matches!(
        cache.get(&scope, epoch_one, "a"),
        Err(CacheError::UnknownEpoch { .. })
    ));
    assert!(matches!(
        cache.get(&scope, epoch_two, "b"),
        Err(CacheError::UnknownEpoch { .. })
    ));
    assert!(
        cache
            .get(&scope, bumped, "a")
            .expect("the new epoch is recognized")
            .is_none(),
        "the bump dropped the entry as well as forgetting its epoch"
    );

    // The bumped scope's new epoch is writable again; the other scope is
    // untouched by the revocation.
    cache
        .put(&scope, bumped, "fresh", plaintext(b"after bump"))
        .unwrap();
    assert_eq!(
        cache
            .get(&scope, bumped, "fresh")
            .unwrap()
            .unwrap()
            .as_slice(),
        b"after bump"
    );
    assert_eq!(
        cache
            .get(&other, epoch_one, "c")
            .unwrap()
            .unwrap()
            .as_slice(),
        b"other c"
    );
}

#[test]
fn unknown_epoch_refuses_to_store_or_serve_plaintext() {
    let mut cache = PlaintextCache::new();
    let scope = ScopeId::new("project-a", "org-a");
    let known = RevisionEpoch::new(1);
    cache.recognize_epoch(&scope, known);

    let unknown = RevisionEpoch::new(99);
    assert!(matches!(
        cache.put(&scope, unknown, "a", plaintext(b"secret")),
        Err(CacheError::UnknownEpoch { epoch, .. }) if epoch == unknown
    ));
    assert!(matches!(
        cache.get(&scope, unknown, "a"),
        Err(CacheError::UnknownEpoch { epoch, .. }) if epoch == unknown
    ));
    assert!(matches!(
        cache.require_epoch(&scope, unknown),
        Err(CacheError::UnknownEpoch { .. })
    ));

    // A scope the process never heard of is uncertain too: it cannot be read,
    // written, or bumped, and guessing an epoch does not create one.
    let never = ScopeId::new("never", "seen");
    assert!(matches!(
        cache.get(&never, RevisionEpoch::new(1), "a"),
        Err(CacheError::UnknownScope)
    ));
    assert!(matches!(
        cache.put(&never, RevisionEpoch::new(1), "a", plaintext(b"secret")),
        Err(CacheError::UnknownScope)
    ));
    assert!(matches!(
        cache.bump_epoch(&never),
        Err(CacheError::UnknownScope)
    ));

    assert!(cache.is_empty(), "refused writes leave no plaintext behind");
}

#[test]
fn decrypt_cached_fails_closed_before_touching_plaintext() {
    let salt = OrgSalt::for_org("epoch-cache-test-org");
    let key = OrgKey::derive("correct horse battery staple", &salt).expect("key derivation");
    let envelope = encrypt(&key, b"decrypted local data").expect("encrypt");
    let mut cache = PlaintextCache::new();
    let scope = ScopeId::new("project", "org");
    let known = RevisionEpoch::new(4);
    cache.recognize_epoch(&scope, known);

    // An uncertain epoch never reaches the cipher or the cache.
    let unknown = RevisionEpoch::new(5);
    assert!(matches!(
        cache.decrypt_cached(&key, &scope, unknown, "blob", &envelope),
        Err(CacheError::UnknownEpoch { .. })
    ));
    assert!(cache.is_empty());

    let first = cache
        .decrypt_cached(&key, &scope, known, "blob", &envelope)
        .expect("decrypt under a known epoch");
    assert_eq!(first.as_slice(), b"decrypted local data");
    let second = cache
        .decrypt_cached(&key, &scope, known, "blob", &envelope)
        .expect("cached read");
    assert_eq!(second.as_slice(), first.as_slice());
    assert_eq!(cache.len(), 1, "the second read was served from the cache");

    // A wrong key still fails closed at the unchanged AEAD layer and is not
    // cached under any entry id.
    let wrong = OrgKey::derive("not the recovery phrase", &salt).unwrap();
    assert!(matches!(
        cache.decrypt_cached(&wrong, &scope, known, "blob-two", &envelope),
        Err(CacheError::Crypto(CryptoError::AuthenticationFailed))
    ));
    assert_eq!(cache.len(), 1);

    // Revoking the scope removes the cached plaintext even though the caller
    // presents no epoch to the revocation.
    let bumped = cache.bump_epoch(&scope).unwrap();
    assert!(cache.is_empty());
    assert!(matches!(
        cache.get(&scope, known, "blob"),
        Err(CacheError::UnknownEpoch { .. })
    ));
    assert_eq!(bumped, known.next());
}

#[test]
fn invalidate_scope_drops_plaintext_without_changing_the_epoch() {
    let mut cache = PlaintextCache::new();
    let scope = ScopeId::new("project", "org");
    let epoch = RevisionEpoch::new(7);
    cache.recognize_epoch(&scope, epoch);
    cache
        .put(&scope, epoch, "blob", plaintext(b"local plaintext"))
        .unwrap();

    cache.invalidate_scope(&scope);
    assert!(cache.is_empty());
    assert!(cache.is_epoch_known(&scope, epoch));
    assert!(
        cache
            .get(&scope, epoch, "blob")
            .expect("epoch is still recognized")
            .is_none()
    );
}
