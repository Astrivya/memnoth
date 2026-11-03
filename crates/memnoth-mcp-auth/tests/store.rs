//! Integration tests for the user-token store.

use memnoth_mcp_auth::store::{UserTokenMeta, UserTokenStore, default_exp_from_now};

fn meta(jti: &str, exp: i64) -> UserTokenMeta {
    UserTokenMeta {
        jti: jti.into(),
        exp,
        user_id: format!("user_for_{jti}"),
    }
}

#[test]
fn store_round_trip() {
    let store = UserTokenStore::new();
    let exp = default_exp_from_now(1_000_000);
    store.insert("token-aaa", meta("jti-aaa", exp));
    store.insert("token-bbb", meta("jti-bbb", exp));
    let a = store.lookup("token-aaa", 1_000_001).expect("must exist");
    let b = store.lookup("token-bbb", 1_000_001).expect("must exist");
    assert_eq!(a.jti, "jti-aaa");
    assert_eq!(b.jti, "jti-bbb");
    assert_eq!(store.len(), 2);
}

#[test]
fn revoke_by_jti_is_idempotent() {
    let store = UserTokenStore::new();
    let exp = default_exp_from_now(1_000_000);
    store.insert("token-aaa", meta("jti-aaa", exp));
    assert!(store.revoke("jti-aaa"));
    assert!(!store.revoke("jti-aaa"), "second revoke must return false");
    assert!(store.lookup("token-aaa", 1_000_001).is_none());
}

#[test]
fn many_revocations_under_concurrent_load() {
    use std::sync::Arc;
    use std::thread;
    let store = Arc::new(UserTokenStore::new());
    let exp = i64::MAX;
    // Insert 1000 tokens.
    for i in 0..1000 {
        store.insert(&format!("token-{i}"), meta(&format!("jti-{i}"), exp));
    }
    assert_eq!(store.len(), 1000);
    // Spawn 8 threads, each revoking a different disjoint range of jtis.
    let mut handles = Vec::new();
    for t in 0..8u32 {
        let store = store.clone();
        handles.push(thread::spawn(move || {
            for i in (t as usize * 125)..((t as usize + 1) * 125) {
                let _ = store.revoke(&format!("jti-{i}"));
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    // All entries should be gone.
    assert_eq!(store.len(), 0);
    for i in 0..1000 {
        assert!(store.lookup(&format!("token-{i}"), 0).is_none());
    }
}
