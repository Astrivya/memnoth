//! Kernel-enforced deny paths for sandbox profiles.
//!
//! Port note: the deny model (exact paths vs glob patterns, fail-closed
//! validation, resolve/sort/dedup, symlink-target masking) follows
//! grok-build's `xai-grok-sandbox/src/deny/{mod,glob}.rs`
//! (see THIRD-PARTY-NOTICES.md), reimplemented on `walkdir` + `globset` so
//! the crate builds and enforces identically on every OS including Windows.
//! macOS Seatbelt-regex emission and Linux bwrap re-exec are phase 2; v1
//! expands globs to concrete paths at resolve time on all platforms.

mod glob;

pub(crate) use glob::find_bare_alternation;
pub use glob::{
    DENY_GLOB_CAPS, DenyGlobCaps, expand_deny_globs, split_glob_root, validate_deny_glob,
};

use std::path::{Path, PathBuf};

/// Whether a raw config entry is a glob pattern rather than an exact path.
/// True iff it contains a gitignore-style metacharacter (`*`, `?`, `[`).
/// The single classifier for both `deny` entries and allow paths.
pub fn is_glob(entry: &str) -> bool {
    entry.contains(['*', '?', '['])
}

/// Split a profile's raw deny entries into exact paths (literal / subpath
/// enforcement) and glob patterns. Non-glob entries pass through unchanged.
pub fn partition_deny_entries(deny: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut exact = Vec::new();
    let mut globs = Vec::new();
    for entry in deny {
        match entry.to_str() {
            Some(s) if is_glob(s) => globs.push(s.to_string()),
            _ => exact.push(entry.clone()),
        }
    }
    (exact, globs)
}

/// Resolve deny entries against the workspace: relative paths join with
/// `workspace`, absolute paths pass through as-is.
pub fn resolve_deny_paths(workspace: &Path, deny: &[PathBuf]) -> Vec<PathBuf> {
    deny.iter()
        .map(|p| {
            if p.is_absolute() {
                p.clone()
            } else {
                workspace.join(p)
            }
        })
        .collect()
}

/// Resolve, sort, and dedup a profile's EXACT (non-glob) deny entries into
/// the canonical set of paths to enforce.
pub fn effective_deny_paths(workspace: &Path, deny: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = resolve_deny_paths(workspace, deny);
    paths.sort();
    paths.dedup();
    paths
}

/// Whether a deny path should be treated as a directory (whole-subtree deny)
/// rather than a single file. True for existing directories, false otherwise.
/// If a path is later created as a directory its children are not covered —
/// prefer denying the parent directory for paths that do not exist yet.
pub fn deny_path_is_dir(path: &Path) -> bool {
    path.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_detection() {
        assert!(is_glob("**/.env"));
        assert!(is_glob("**/*.pem"));
        assert!(is_glob("secrets/**"));
        assert!(is_glob("a?b"));
        assert!(is_glob("[abc].txt"));
        assert!(!is_glob(".env"));
        assert!(!is_glob("src/server.pem"));
        assert!(!is_glob("/etc/shadow"));
    }

    #[test]
    fn partition_separates_globs_from_exact() {
        let deny = vec![
            PathBuf::from(".env"),
            PathBuf::from("**/*.pem"),
            PathBuf::from("/etc/shadow"),
            PathBuf::from("secrets/**"),
        ];
        let (exact, globs) = partition_deny_entries(&deny);
        assert_eq!(
            exact,
            vec![PathBuf::from(".env"), PathBuf::from("/etc/shadow")]
        );
        assert_eq!(
            globs,
            vec!["**/*.pem".to_string(), "secrets/**".to_string()]
        );
    }

    #[test]
    fn resolve_relative_against_workspace() {
        let ws = PathBuf::from("/tmp/project");
        let deny = vec![PathBuf::from(".env"), PathBuf::from("/etc/shadow")];
        let resolved = resolve_deny_paths(&ws, &deny);
        assert_eq!(resolved[0], PathBuf::from("/tmp/project/.env"));
        assert_eq!(resolved[1], PathBuf::from("/etc/shadow"));
    }

    #[test]
    fn effective_paths_sort_and_dedup() {
        let ws = PathBuf::from("/ws");
        let exact = vec![
            PathBuf::from("src/server.pem"),
            PathBuf::from(".env"),
            PathBuf::from("/etc/shadow"),
            PathBuf::from(".env"),
        ];
        let paths = effective_deny_paths(&ws, &exact);
        assert!(paths.iter().any(|p| p == &PathBuf::from("/ws/.env")));
        assert!(
            paths
                .iter()
                .any(|p| p == &PathBuf::from("/ws/src/server.pem"))
        );
        assert!(paths.iter().any(|p| p == &PathBuf::from("/etc/shadow")));
        let mut sorted = paths.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(paths, sorted);
    }
}
