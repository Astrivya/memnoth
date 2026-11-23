//! P22-048 / ALG17 — local cache purge on `vault forget`.
//!
//! A revocation must not leave derived content (embeddings, model responses)
//! addressable in a local cache. These tests pin the four required properties:
//! an entry that was cached is purged by forget, another namespace is spared,
//! `cache_ttl = 0` is reported, and the plaintext is physically absent from the
//! cache after revocation. The mechanism is local only — no hosted purge.

use std::path::PathBuf;

use memnoth::context::{AppContext, Config};
use memnoth::vault::cache_purge::{DEFAULT_CACHE_TTL_MS, REVOKED_CACHE_TTL_MS, derived_cache};
use memnoth::vault::{forget_with_cache_purge, init, remember};

fn context(dir: &std::path::Path, namespace: &str) -> AppContext {
    AppContext {
        root: dir.to_path_buf(),
        home: dir.join("home"),
        vault_path: dir.join("home").join(format!("vault-{namespace}.db")),
        namespace: namespace.to_string(),
        graph_path: dir.join("graph.db"),
        sessions_dir: dir.join("home").join("sessions"),
        config_path: dir.join("home").join("config.toml"),
        exe_path: PathBuf::from("memnoth"),
        config: Config::default(),
        json: false,
        quiet: true,
    }
}

fn temp(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("memnoth-cache-purge-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn entry_cached_then_forget_purges_it() {
    let dir = temp("e2e");
    let ctx = context(&dir, "purge-e2e");
    init(&ctx).unwrap();
    remember(
        &ctx,
        "revoked_fact",
        "fact",
        None,
        "concept",
        &["shadow secret".into()],
        None,
    )
    .unwrap();

    // An embedding/response derived under this namespace is in the local cache.
    {
        let mut cache = derived_cache().lock().unwrap();
        cache.put(
            "purge-e2e",
            "reader",
            "entry-1",
            b"shadow secret vector".to_vec(),
        );
        assert!(cache.get("purge-e2e", "reader", "entry-1").is_some());
    }

    let outcome =
        forget_with_cache_purge(&ctx, "revoked_fact", "no longer authorized", true, true).unwrap();

    assert_eq!(outcome.data["cache_ttl"], REVOKED_CACHE_TTL_MS);
    assert!(
        outcome.data["cache_purge"]["purged_entries"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert_eq!(outcome.data["cache_purge"]["scope"], "local");

    let cache = derived_cache().lock().unwrap();
    assert!(cache.get("purge-e2e", "reader", "entry-1").is_none());
    assert!(!cache.contains_entry("purge-e2e", "entry-1"));
}

#[test]
fn purging_one_namespace_spares_the_other() {
    let mut cache = memnoth::vault::cache_purge::DerivedCache::new();
    cache.put("ns-a", "reader", "a-1", b"alpha secret".to_vec());
    cache.put("ns-b", "reader", "b-1", b"beta secret".to_vec());

    let removed = cache.purge_namespace("ns-a");

    assert_eq!(removed, 1);
    assert!(cache.get("ns-a", "reader", "a-1").is_none());
    assert_eq!(
        cache.get("ns-b", "reader", "b-1").map(|v| v.as_slice()),
        Some(b"beta secret".as_slice())
    );
}

#[test]
fn cache_ttl_zero_is_reported_after_revocation() {
    let mut cache = memnoth::vault::cache_purge::DerivedCache::new();
    cache.put("ttl-ns", "reader", "e", b"payload".to_vec());
    assert_eq!(cache.cache_ttl("ttl-ns"), DEFAULT_CACHE_TTL_MS);

    let report = memnoth::vault::cache_purge::purge_for_revocation("ttl-ns");
    assert_eq!(report.cache_ttl, REVOKED_CACHE_TTL_MS);
    assert_eq!(report.namespace, "ttl-ns");

    let cache = derived_cache().lock().unwrap();
    assert_eq!(cache.cache_ttl("ttl-ns"), REVOKED_CACHE_TTL_MS);
}

#[test]
fn offline_plaintext_is_physically_unrecallable_from_the_cache() {
    let marker = b"CLASSIFIED-MARKER-7d31";
    let mut cache = memnoth::vault::cache_purge::DerivedCache::new();
    cache.put("secret-ns", "reader", "doc", marker.to_vec());
    assert!(cache.cached_bytes_contain(marker), "present before purge");

    cache.bump_epoch("secret-ns");

    assert!(!cache.cached_bytes_contain(marker), "wiped after purge");
    assert!(cache.get("secret-ns", "reader", "doc").is_none());
    assert!(!cache.contains_entry("secret-ns", "doc"));
    assert_eq!(cache.len_for_namespace("secret-ns"), 0);
    assert_eq!(cache.cache_ttl("secret-ns"), REVOKED_CACHE_TTL_MS);
}

#[test]
fn forget_without_the_flag_leaves_the_cache_and_reports_no_ttl() {
    let dir = temp("noflag");
    let ctx = context(&dir, "purge-noflag");
    init(&ctx).unwrap();
    remember(
        &ctx,
        "kept_fact",
        "fact",
        None,
        "concept",
        &["keep me".into()],
        None,
    )
    .unwrap();

    {
        let mut cache = derived_cache().lock().unwrap();
        cache.put(
            "purge-noflag",
            "reader",
            "keep-1",
            b"keep me vector".to_vec(),
        );
    }

    let outcome = forget_with_cache_purge(&ctx, "kept_fact", "stale", true, false).unwrap();
    assert_eq!(outcome.data["purge_cache"], false);
    assert!(outcome.data["cache_ttl"].is_null());

    let cache = derived_cache().lock().unwrap();
    assert!(cache.get("purge-noflag", "reader", "keep-1").is_some());
}
