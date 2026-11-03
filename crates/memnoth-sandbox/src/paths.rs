//! Filesystem path tables for sandbox profiles.
//!
//! Port note: the table *shape* (state dir, device lists, temp dirs,
//! essential writable sets per profile) follows grok-build's
//! `xai-grok-sandbox/src/paths.rs` (see THIRD-PARTY-NOTICES.md).
//! All names and memnoth-specific choices are original.

use std::path::{Path, PathBuf};

/// memnoth state directory (`$MEMNOTH_HOME` or `~/.memnoth`).
pub fn memnoth_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("MEMNOTH_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs_home_fallback().join(".memnoth")
}

fn dirs_home_fallback() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/root"))
}

/// On-disk JSONL audit log for sandbox events (under the sessions dir, so the
/// strict profile's sessions-dir write grant covers it without granting the
/// state-dir parent).
pub fn sandbox_events_log_path() -> PathBuf {
    memnoth_home().join("sessions").join("sandbox-events.jsonl")
}

/// On-disk JSONL log of policy *denials* (`DenialRecord` lines), the input to
/// the "suggested rules" loop: shapes denied repeatedly become one-click rule
/// candidates in the `/permissions` sheet (§9).
///
/// A sibling of [`sandbox_events_log_path`] and under `sessions/` for the same
/// reason — the strict profile grants that directory, not the state-dir parent,
/// so an enforcing profile can still record what it refused. Without that, the
/// profiles that deny most are exactly the ones that would produce no
/// suggestions.
pub fn denials_log_path() -> PathBuf {
    memnoth_home()
        .join("sessions")
        .join("sandbox-denials.jsonl")
}

/// Sandbox config filename, looked up as `$MEMNOTH_HOME/sandbox.toml`
/// (global) and `<workspace>/.memnoth/sandbox.toml` (project, additive only).
pub const SANDBOX_CONFIG_FILENAME: &str = "sandbox.toml";

/// Project config directory name.
pub const PROJECT_CONFIG_DIR: &str = ".memnoth";

/// Device files that need read/write for normal tool operation on unix.
/// (Individual files; directory nodes belong in [`DEVICE_DIRS`].)
#[cfg(unix)]
pub const DEVICE_FILES: &[&str] = &[
    "/dev/null",    // output sink — used by virtually every CLI tool
    "/dev/zero",    // zero source — used by memory allocators
    "/dev/random",  // entropy — used by crypto/TLS
    "/dev/urandom", // entropy — used by crypto/TLS
];

/// Device directories that need access (granted as paths, not files).
#[cfg(unix)]
pub const DEVICE_DIRS: &[&str] = &[
    "/dev/pts", // PTY slaves (Linux)
    "/dev/fd",  // fd table (symlink to /proc/self/fd on Linux; a directory)
];

/// Temporary directories that need write access.
pub fn temp_writable_paths() -> Vec<PathBuf> {
    let mut paths = vec![std::env::temp_dir()];
    for extra in ["/tmp", "/var/tmp"] {
        let pb = PathBuf::from(extra);
        if pb.exists() && pb.is_dir() && !paths.contains(&pb) {
            paths.push(pb);
        }
    }
    // macOS: the real TMPDIR lives under /private/var/folders.
    if cfg!(target_os = "macos") {
        for p in ["/private/tmp", "/private/var/tmp", "/private/var/folders"] {
            let pb = PathBuf::from(p);
            if pb.exists() && pb.is_dir() && !paths.contains(&pb) {
                paths.push(pb);
            }
        }
    }
    // Respect $TMPDIR if it points somewhere else.
    if let Ok(tmpdir) = std::env::var("TMPDIR") {
        let pb = PathBuf::from(&tmpdir);
        if !tmpdir.is_empty() && pb.exists() && pb.is_dir() && !paths.contains(&pb) {
            paths.push(pb);
        }
    }
    paths
}

/// Writable paths for the workspace profile (workspace + full state dir + temp).
pub fn essential_writable_paths(workspace: &Path) -> Vec<PathBuf> {
    let mut paths = vec![workspace.to_path_buf(), memnoth_home()];
    paths.extend(temp_writable_paths());
    paths
}

/// Writable paths for the strict profile (workspace + sessions only + temp).
/// Strict may read the state dir but must not write the parent; the events
/// log lives under `sessions/`.
pub fn essential_writable_paths_strict(workspace: &Path) -> Vec<PathBuf> {
    let mut paths = vec![workspace.to_path_buf(), memnoth_home().join("sessions")];
    paths.extend(temp_writable_paths());
    paths
}

/// Writable paths for the read-only profile (minimal: state dir + temp).
pub fn essential_writable_paths_minimal() -> Vec<PathBuf> {
    let mut paths = vec![memnoth_home()];
    paths.extend(temp_writable_paths());
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_writes_sessions_not_home() {
        let ws = PathBuf::from("/tmp/ws-test-memnoth-sandbox");
        let strict = essential_writable_paths_strict(&ws);
        assert!(strict.contains(&memnoth_home().join("sessions")));
        assert!(!strict.contains(&memnoth_home()));
        assert!(strict.contains(&ws));
    }

    #[test]
    fn workspace_writes_home() {
        let ws = PathBuf::from("/tmp/ws-test-memnoth-sandbox");
        assert!(essential_writable_paths(&ws).contains(&memnoth_home()));
    }

    #[test]
    fn events_log_lives_under_sessions() {
        assert!(sandbox_events_log_path().starts_with(memnoth_home().join("sessions")));
    }

    #[test]
    fn denials_log_lives_under_sessions() {
        // Same grant as the events log: the strict profile can read policy but
        // may only write `sessions/`, so a denial log outside it would be
        // silently unrecordable in exactly the profile that denies the most.
        assert!(denials_log_path().starts_with(memnoth_home().join("sessions")));
    }
}
