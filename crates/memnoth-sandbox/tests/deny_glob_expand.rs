//! Fail-closed deny-glob expansion tests (lane F gate, part 2).
//!
//! Validation is unit-tested in `deny::glob`; here the launch-time walk is
//! pinned: matches are found (including hidden files), non-matches are
//! excluded, caps refuse startup instead of silently covering nothing, and
//! invalid globs fail closed.

use memnoth_sandbox::deny::{DENY_GLOB_CAPS, DenyGlobCaps, expand_deny_globs};

fn caps(depth: usize, matches: usize, entries: usize) -> DenyGlobCaps {
    DenyGlobCaps {
        depth,
        matches,
        entries,
    }
}

#[test]
fn matches_nested_pem_and_dotenv_excludes_control() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path();
    std::fs::create_dir_all(ws.join("sub/dir")).unwrap();
    std::fs::write(ws.join("sub/dir/key.pem"), "x").unwrap();
    std::fs::write(ws.join(".env"), "x").unwrap(); // hidden
    std::fs::write(ws.join("readable.txt"), "x").unwrap();
    let globs = vec!["**/*.pem".to_string(), "**/.env".to_string()];
    let expanded = expand_deny_globs(ws, &globs, DENY_GLOB_CAPS).expect("should expand");
    assert!(
        expanded.iter().any(|p| p.ends_with("key.pem")),
        "{expanded:?}"
    );
    assert!(expanded.iter().any(|p| p.ends_with(".env")), "{expanded:?}");
    assert!(
        !expanded.iter().any(|p| p.ends_with("readable.txt")),
        "{expanded:?}"
    );
}

#[test]
fn empty_when_nothing_matches() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let expanded =
        expand_deny_globs(dir.path(), &["**/*.pem".to_string()], DENY_GLOB_CAPS).unwrap();
    assert!(expanded.is_empty(), "{expanded:?}");
}

#[test]
fn fails_closed_on_match_cap() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..5 {
        std::fs::write(dir.path().join(format!("k{i}.pem")), "x").unwrap();
    }
    let err =
        expand_deny_globs(dir.path(), &["**/*.pem".to_string()], caps(64, 2, 200_000)).unwrap_err();
    assert!(err.contains("matched over 2 files"), "{err}");
    assert!(expand_deny_globs(dir.path(), &["**/*.pem".to_string()], caps(64, 5, 200_000)).is_ok());
}

#[test]
fn fails_closed_on_depth_cap() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
    std::fs::write(dir.path().join("a/b/c/k.pem"), "x").unwrap();
    let err = expand_deny_globs(
        dir.path(),
        &["**/*.pem".to_string()],
        caps(1, 4096, 200_000),
    )
    .unwrap_err();
    assert!(err.contains("scan limit"), "{err}");
}

#[test]
fn fails_closed_on_entries_cap_and_names_the_stop_point() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..10 {
        std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
    }
    let err =
        expand_deny_globs(dir.path(), &["**/*.pem".to_string()], caps(64, 4096, 3)).unwrap_err();
    assert!(err.contains("visited over 3 entries"), "{err}");
}

#[test]
fn rejects_unsupported_glob() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        expand_deny_globs(dir.path(), &["**/*.{pem,key}".to_string()], DENY_GLOB_CAPS).is_err()
    );
}

#[test]
fn literal_prefix_narrows_the_walk_root() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("big")).unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    for i in 0..20 {
        std::fs::write(dir.path().join(format!("big/f{i}.txt")), "x").unwrap();
    }
    std::fs::write(dir.path().join("sub/key.pem"), "x").unwrap();
    // `sub/**` walks only `<workspace>/sub`; the budget is smaller than `big/`.
    let expanded = expand_deny_globs(dir.path(), &["sub/**".to_string()], caps(64, 4096, 5))
        .expect("narrowed walk stays under the budget");
    assert!(
        expanded.iter().any(|p| p.ends_with("key.pem")),
        "{expanded:?}"
    );
}

#[test]
#[cfg(unix)]
fn does_not_descend_symlinked_dir() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.pem"), "x").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
    let expanded =
        expand_deny_globs(dir.path(), &["**/*.pem".to_string()], DENY_GLOB_CAPS).unwrap();
    assert!(
        !expanded.iter().any(|p| p.contains("secret.pem")),
        "symlinked dir must not be descended: {expanded:?}"
    );
}

#[test]
#[cfg(unix)]
fn symlinked_match_masks_canonical_target() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("real.pem"), "x").unwrap();
    std::os::unix::fs::symlink(outside.path().join("real.pem"), dir.path().join("link.pem"))
        .unwrap();
    let expanded =
        expand_deny_globs(dir.path(), &["**/*.pem".to_string()], DENY_GLOB_CAPS).unwrap();
    assert!(
        expanded.iter().any(|p| p.ends_with("link.pem")),
        "{expanded:?}"
    );
    let canonical = std::fs::canonicalize(outside.path().join("real.pem")).unwrap();
    assert!(
        expanded
            .iter()
            .any(|p| std::path::Path::new(p) == canonical),
        "canonical target of a matched symlink must be masked too: {expanded:?}"
    );
}
