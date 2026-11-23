#![allow(dead_code)] // Included by #[path] here and (once wired) by the library.

//! ALG07 — incremental index reconciliation, exercised without network or a model.
//!
//! The module is included by path so this suite stays runnable before the
//! `pub mod incremental_index;` line lands in `src/lib.rs`. Wiring does not
//! change the behaviour under test.

#[path = "../src/incremental_index.rs"]
mod incremental_index;

use std::collections::BTreeMap;

use incremental_index::{
    BranchOverlay, IncrementalIndex, RelPath, WatchEvent, snapshot, source_with_deps,
};
use serde_json::{Value, json};

#[test]
fn unchanged_file_is_not_reparsed() {
    let mut index = IncrementalIndex::new();
    let files = snapshot(&[("src/a.rs", "fn alpha() {}")]);

    let first = index.reconcile_scan(&files);
    assert_eq!(first.parsed, vec!["src/a.rs"]);
    let id = index.record("src/a.rs").unwrap().id;
    assert_eq!(index.record("src/a.rs").unwrap().parse_count, 1);

    let second = index.reconcile_scan(&files);
    assert!(
        second.parsed.is_empty(),
        "an unchanged file must be skipped"
    );
    assert_eq!(second.unchanged, 1);
    assert_eq!(index.record("src/a.rs").unwrap().parse_count, 1);
    assert_eq!(index.record("src/a.rs").unwrap().id, id);
    assert_eq!(second.root_hash, first.root_hash);
}

#[test]
fn changed_file_invalidates_only_its_dependents() {
    let mut index = IncrementalIndex::new();
    let files = snapshot(&[
        ("a.rs", "fn a() {}"),
        ("b.rs", &source_with_deps("fn b() {}", &["a.rs"])),
        ("d.rs", &source_with_deps("fn d() {}", &["b.rs"])),
        ("c.rs", "fn c() {}"),
    ]);
    index.reconcile_scan(&files);

    let mut edited = files.clone();
    edited.insert(
        "a.rs".to_string(),
        b"fn a() { let changed = true; }".to_vec(),
    );
    let report = index.reconcile_scan(&edited);

    assert_eq!(report.changed, vec!["a.rs"]);
    // b depends on a directly, d depends on b transitively. c is independent.
    assert_eq!(report.invalidated, vec!["b.rs", "d.rs"]);
    assert_eq!(report.parsed, vec!["a.rs"], "dependents are not re-parsed");
    assert_eq!(index.record("b.rs").unwrap().analysis_version, 1);
    assert_eq!(index.record("d.rs").unwrap().analysis_version, 1);
    assert_eq!(index.record("c.rs").unwrap().analysis_version, 0);
    assert_eq!(index.record("c.rs").unwrap().parse_count, 1);
    assert_eq!(index.record("a.rs").unwrap().parse_count, 2);

    // A second scan with the same bytes is entirely quiet.
    let quiet = index.reconcile_scan(&edited);
    assert!(quiet.parsed.is_empty());
    assert!(quiet.invalidated.is_empty());
}

#[test]
fn rename_preserves_identity_and_does_not_reparse() {
    let mut index = IncrementalIndex::new();
    index.reconcile_scan(&snapshot(&[("src/old.rs", "fn alpha() {}")]));
    let id = index.record("src/old.rs").unwrap().id;
    let parses = index.record("src/old.rs").unwrap().parse_count;

    let moved = snapshot(&[("src/new.rs", "fn alpha() {}")]);
    let report = index.reconcile_scan(&moved);

    assert_eq!(
        report.renamed,
        vec![("src/old.rs".to_string(), "src/new.rs".to_string())]
    );
    assert!(report.parsed.is_empty(), "a pure move needs no parse");
    assert!(index.record("src/old.rs").is_none());
    let record = index.record("src/new.rs").unwrap();
    assert_eq!(record.id, id);
    assert_eq!(record.parse_count, parses);
    assert_eq!(
        record.fingerprint,
        IncrementalIndex::fingerprint_of(b"fn alpha() {}")
    );

    // The watcher-event path preserves the same identity.
    let mut via_event = IncrementalIndex::new();
    via_event.reconcile_scan(&snapshot(&[("x.rs", "fn alpha() {}")]));
    let event_id = via_event.record("x.rs").unwrap().id;
    let event_report = via_event.apply_events(&[WatchEvent::Renamed {
        from: "x.rs".to_string(),
        to: "y.rs".to_string(),
        content: b"fn alpha() {}".to_vec(),
    }]);
    assert_eq!(event_report.renamed.len(), 1);
    assert_eq!(via_event.record("y.rs").unwrap().id, event_id);
}

#[test]
fn rename_rewrites_dependent_edges() {
    let mut index = IncrementalIndex::new();
    index.reconcile_scan(&snapshot(&[
        ("a.rs", "fn a() {}"),
        ("b.rs", &source_with_deps("fn b() {}", &["a.rs"])),
    ]));

    let moved = snapshot(&[
        ("renamed/a.rs", "fn a() {}"),
        ("b.rs", &source_with_deps("fn b() {}", &["a.rs"])),
    ]);
    let report = index.reconcile_scan(&moved);

    assert_eq!(report.renamed.len(), 1);
    assert_eq!(
        index.record("b.rs").unwrap().dependencies,
        vec!["renamed/a.rs"]
    );
    // The dependent was invalidated via the new path, but never re-parsed.
    assert_eq!(report.invalidated, vec!["b.rs"]);
    assert_eq!(index.record("b.rs").unwrap().parse_count, 1);
}

#[test]
fn branch_overlay_does_not_mutate_base() {
    let base_files = snapshot(&[("a.rs", "fn a() {}"), ("b.rs", "fn b() {}")]);
    let mut base = IncrementalIndex::new();
    base.reconcile_scan(&base_files);
    let base_root = base.root_hash();
    let base_a = base.record("a.rs").unwrap().clone();

    let mut changed = base_files.clone();
    changed.insert("a.rs".to_string(), b"fn a() { let x = 1; }".to_vec());

    let branch_root;
    {
        let mut branch = BranchOverlay::new(&base);
        let report = branch.reconcile_scan(&changed);
        assert_eq!(report.changed, vec!["a.rs"]);
        branch_root = branch.working().root_hash();
        assert_ne!(branch_root, base_root);

        // The base is byte-for-byte the same struct it was before the branch.
        assert_eq!(branch.base().root_hash(), base_root);
        assert_eq!(base.root_hash(), base_root);
        assert_eq!(base.record("a.rs"), Some(&base_a));
    }

    let committed = BranchOverlay::new(&base).commit();
    assert_eq!(
        committed.root_hash(),
        base_root,
        "an untouched branch equals base"
    );

    let mut branch = BranchOverlay::new(&base);
    branch.reconcile_scan(&changed);
    assert_eq!(branch.commit().root_hash(), branch_root);
    assert_eq!(base.root_hash(), base_root);
}

#[test]
fn root_hash_changes_only_when_content_changes() {
    let files = snapshot(&[("a.rs", "fn a() {}"), ("b.rs", "fn b() {}")]);
    let mut a = IncrementalIndex::new();
    a.reconcile_scan(&files);
    let root = a.root_hash();

    // A no-op scan keeps the root.
    let quiet = a.reconcile_scan(&files);
    assert_eq!(quiet.root_hash, root);
    assert_eq!(quiet.previous_root_hash, root);
    assert_eq!(a.root_hash(), root);

    // Metadata-only mutation keeps the root.
    a.record_mut("a.rs").unwrap().parse_count += 99;
    a.record_mut("a.rs").unwrap().analysis_version += 7;
    a.record_mut("a.rs").unwrap().id.0 += 500;
    assert_eq!(a.root_hash(), root);

    // The same content with different identities and insertion order agrees.
    let mut b = IncrementalIndex::new();
    b.reconcile_scan(&snapshot(&[("b.rs", "fn b() {}"), ("a.rs", "fn a() {}")]));
    assert_eq!(b.root_hash(), root);

    // Changing bytes moves the root.
    let mut edited = files.clone();
    edited.insert("a.rs".to_string(), b"fn a() { changed }".to_vec());
    let report = a.reconcile_scan(&edited);
    assert_ne!(report.root_hash, root);
    assert_eq!(a.root_hash(), report.root_hash);
}

#[test]
fn corrupted_cache_fails_closed() {
    let mut index = IncrementalIndex::new();
    index.reconcile_scan(&snapshot(&[("a.rs", "fn a() {}")]));

    let json = index.to_cache_json().unwrap();
    let loaded = IncrementalIndex::from_cache_json(&json).unwrap();
    assert_eq!(loaded.root_hash(), index.root_hash());
    assert_eq!(
        loaded.record("a.rs").unwrap().id,
        index.record("a.rs").unwrap().id
    );

    // Malformed JSON is refused rather than treated as an empty index.
    assert!(IncrementalIndex::from_cache_json("{not json").is_err());

    // A bad checksum (single flipped digit in the digest) is refused.
    let mut tampered: Value = serde_json::from_str(&json).unwrap();
    tampered["checksum"] = json!("0".repeat(64));
    assert!(IncrementalIndex::from_cache_json(&tampered.to_string()).is_err());

    // Content tampering inside the indexed document is caught by the checksum.
    let envelope: Value = serde_json::from_str(&json).unwrap();
    let inner = envelope["index_json"].as_str().unwrap().to_string();
    let mut changed_inner: Value = serde_json::from_str(&inner).unwrap();
    changed_inner["records"][0]["fingerprint"] =
        json!("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
    let mut changed_envelope = envelope.clone();
    changed_envelope["index_json"] = json!(changed_inner.to_string());
    assert!(IncrementalIndex::from_cache_json(&changed_envelope.to_string()).is_err());

    // An unsupported format version is refused.
    let mut future: Value = serde_json::from_str(&json).unwrap();
    future["format"] = json!(999);
    assert!(IncrementalIndex::from_cache_json(&future.to_string()).is_err());

    // `load` propagates the failure from a file on disk.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.json");
    std::fs::write(&path, tampered.to_string()).unwrap();
    assert!(IncrementalIndex::load(&path).is_err());

    // A valid file round-trips through save/load.
    index.save(&path).unwrap();
    assert_eq!(
        IncrementalIndex::load(&path).unwrap().root_hash(),
        index.root_hash()
    );
}

#[test]
fn watch_events_verify_bytes_before_trusting_the_notification() {
    let mut index = IncrementalIndex::new();
    index.reconcile_scan(&snapshot(&[("a.rs", "fn a() {}")]));

    let spurious = index.apply_events(&[WatchEvent::Modified {
        path: "a.rs".to_string(),
        content: b"fn a() {}".to_vec(),
    }]);
    assert!(
        spurious.parsed.is_empty(),
        "identical bytes are not a change"
    );
    assert_eq!(index.record("a.rs").unwrap().parse_count, 1);

    let real = index.apply_events(&[WatchEvent::Modified {
        path: "a.rs".to_string(),
        content: b"fn a() { changed }".to_vec(),
    }]);
    assert_eq!(real.changed, vec!["a.rs"]);
    assert_eq!(real.parsed, vec!["a.rs"]);
    assert_eq!(index.record("a.rs").unwrap().parse_count, 2);
}

#[test]
fn metadata_helpers_keep_paths_normalized() {
    let source = source_with_deps("fn x() {}", &["dir\\a.rs", "b.rs"]);
    let mut index = IncrementalIndex::new();
    let mut files = BTreeMap::new();
    files.insert("x.rs".to_string(), source.into_bytes());
    index.reconcile_scan(&files);
    assert_eq!(
        index.record("x.rs").unwrap().dependencies,
        vec!["b.rs".to_string(), "dir/a.rs".to_string()]
    );

    let snap: BTreeMap<RelPath, Vec<u8>> = snapshot(&[("one.rs", "fn one() {}")]);
    assert_eq!(snap.len(), 1);
}
