//! Glob deny entries: detection, fail-closed validation, and launch-time
//! expansion to concrete paths.
//!
//! Port note: the validation rules and caps follow grok-build's
//! `xai-grok-sandbox/src/deny/glob.rs` (see THIRD-PARTY-NOTICES.md),
//! reimplemented. A deny entry is a GLOB iff it contains a glob
//! metacharacter. Invalid globs, exceeded caps, unreadable walks, and
//! non-UTF8 paths all fail CLOSED (refuse startup) — never silently cover
//! nothing while the sandbox reports active.

use super::is_glob;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Launch-time expansion caps: the walk enumerates matches once at resolve
/// time, so exceeding a cap fails closed.
#[derive(Clone, Copy, Debug)]
pub struct DenyGlobCaps {
    /// Directory depth; a directory at the cap could hide deeper matches.
    pub depth: usize,
    /// Each match becomes an enforced deny entry.
    pub matches: usize,
    /// Visited-entry budget bounding resolve latency. The walk includes
    /// hidden files, so ordinary repos reach hundreds of thousands.
    pub entries: usize,
}

pub const DENY_GLOB_CAPS: DenyGlobCaps = DenyGlobCaps {
    depth: 64,
    matches: 4096,
    entries: 2_000_000,
};

/// Find a brace-alternation or backslash-escape metacharacter OUTSIDE
/// `[...]` classes (`[{]`/`[}]` match literal braces — no drift between
/// matchers). `Some(c)` names the offending character.
pub(crate) fn find_bare_alternation(glob: &str) -> Option<char> {
    let mut in_class = false;
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '{' | '}' | '\\' if !in_class => return Some(c),
            _ => {}
        }
        let _ = &chars;
    }
    None
}
/// Split a glob into its literal root and the tail from the first glob
/// component. E.g. `secrets/**` splits to `<workspace>/secrets` plus `**`,
/// and `/home/**/.ssh` to `/home` plus `**/.ssh`.
pub fn split_glob_root(workspace: &Path, glob: &str) -> (PathBuf, String) {
    let (mut root, rest) = match glob.strip_prefix('/') {
        Some(absolute) => (PathBuf::from("/"), absolute),
        None => (workspace.to_path_buf(), glob),
    };
    let segments: Vec<&str> = rest.split('/').collect();
    let Some(first_glob_index) = segments.iter().position(|s| is_glob(s)) else {
        return (root, rest.to_string());
    };
    for segment in &segments[..first_glob_index] {
        if !segment.is_empty() {
            root.push(segment);
        }
    }
    (root, segments[first_glob_index..].join("/"))
}

/// Validate a deny glob, failing closed on anything the enforcement backends
/// cannot interpret identically.
///
/// Rejected: `{`/`}`/`\` (brace alternation and backslash-escapes differ
/// between matchers — use separate entries); `**` inside a larger segment;
/// empty segments (doubled `//`, trailing `/`); `.`/`..` segments (they would
/// let a relative glob scan outside the workspace); drifting char-class
/// forms (leading-literal `]`, nested `[` / POSIX classes); anything
/// `globset` itself rejects.
pub fn validate_deny_glob(glob: &str) -> anyhow::Result<()> {
    if let Some(c) = find_bare_alternation(glob) {
        anyhow::bail!(
            "deny glob {glob:?} uses unsupported metacharacter '{c}' \
             (brace alternation and backslash-escapes are not supported outside \
             `[...]` classes; use separate deny entries)"
        );
    }
    for (index, segment) in glob.split('/').enumerate() {
        if segment.is_empty() && !(index == 0 && glob.starts_with('/')) {
            anyhow::bail!(
                "deny glob {glob:?}: empty path segment (a doubled '//' or \
                 trailing '/'); remove the extra slash in sandbox.toml"
            );
        }
        if segment == "." || segment == ".." {
            anyhow::bail!(
                "deny glob {glob:?}: `.` and `..` segments are not supported; \
                 write the path without them (use an absolute path to deny \
                 files outside the workspace)"
            );
        }
        if segment.contains("**") && segment != "**" {
            anyhow::bail!(
                "deny glob {glob:?}: `**` must be its own path segment (got {segment:?}); \
                 write it as `**/` or `/**`, e.g. `a/**/b`"
            );
        }
    }
    // Char classes: only the subset that every matcher interprets identically.
    let cc: Vec<char> = glob.chars().collect();
    let mut i = 0;
    while i < cc.len() {
        if cc[i] != '[' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        if matches!(cc.get(j), Some('!') | Some('^')) {
            j += 1;
        }
        if cc.get(j) == Some(&']') {
            anyhow::bail!("deny glob {glob:?}: a literal ']' as first class member is unsupported");
        }
        while j < cc.len() && cc[j] != ']' {
            if cc[j] == '[' {
                anyhow::bail!(
                    "deny glob {glob:?}: nested '[' / POSIX '[[:…:]]' classes are unsupported"
                );
            }
            j += 1;
        }
        // Unterminated class: let the globset build below report it uniformly.
        i = if j < cc.len() { j + 1 } else { cc.len() };
    }
    globset::GlobBuilder::new(glob)
        .literal_separator(true)
        .build()
        .map_err(|e| anyhow::anyhow!("invalid deny glob {glob:?}: {e}"))?;
    Ok(())
}

/// Expand deny globs into the concrete existing paths to enforce.
/// Fails closed on an invalid glob, an exceeded cap, a non-permission walk
/// error, or a non-UTF8 path. Only files that exist at resolve time are
/// covered — prefer exact parent-directory denies for paths created later.
pub fn expand_deny_globs(
    workspace: &Path,
    globs: &[String],
    caps: DenyGlobCaps,
) -> Result<Vec<String>, String> {
    use globset::{GlobBuilder, GlobSetBuilder};

    let Some(workspace_str) = workspace.to_str() else {
        return Err(format!("workspace path is not UTF-8: {workspace:?}"));
    };
    let mut builder = GlobSetBuilder::new();
    let mut roots: BTreeSet<PathBuf> = BTreeSet::new();
    for glob in globs {
        validate_deny_glob(glob).map_err(|e| e.to_string())?;
        // Relative globs get the escaped workspace prefix; `literal_separator`
        // keeps `*`/`?` from crossing `/` on every backend identically.
        let pattern = if glob.starts_with('/') {
            glob.clone()
        } else {
            format!("{}/{}", globset::escape(workspace_str), glob)
        };
        let compiled = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .build()
            .map_err(|_| format!("could not compile glob {glob:?}"))?;
        builder.add(compiled);
        roots.insert(split_glob_root(workspace, glob).0);
    }
    let glob_set = builder
        .build()
        .map_err(|_| "could not build glob set".to_string())?;

    let mut matched: BTreeSet<String> = BTreeSet::new();
    let mut visited: usize = 0;

    // Overlapping globs share one walk per root. A nested root already
    // covered by a walked ancestor (same canonical path below the ancestor,
    // no symlinks in between) is skipped.
    let mut walked: Vec<PathBuf> = Vec::new();
    for root in &roots {
        if !root.exists() {
            continue;
        }
        let covered = walked.iter().any(|w| {
            let Ok(relative) = root.strip_prefix(w) else {
                return false;
            };
            match (dunce::canonicalize(w), dunce::canonicalize(root)) {
                (Ok(parent), Ok(child)) => child == parent.join(relative),
                _ => false,
            }
        });
        if covered {
            continue;
        }
        walked.push(root.clone());

        // Include hidden files (denied secrets usually are); never descend
        // symlinked dirs, though a named root resolves through one.
        let walker = walkdir::WalkDir::new(root)
            .max_depth(caps.depth)
            .follow_links(false)
            .into_iter();
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    // Permission errors mean the agent is equally denied by
                    // the OS — skippable. Any other walk error could hide a
                    // readable match, so it fails closed.
                    if err
                        .io_error()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
                    {
                        continue;
                    }
                    return Err(format!("walk error while expanding deny globs: {err}"));
                }
            };
            visited += 1;
            if visited > caps.entries {
                return Err(format!(
                    "expanding the deny globs {globs:?} visited over {} entries \
                     across their roots (stopped in {} at {}; hidden files are \
                     included in the scan). Use narrower globs with a literal \
                     prefix, or deny exact paths",
                    caps.entries,
                    root.display(),
                    entry.path().display(),
                ));
            }
            // A directory at the depth cap may hide deeper matches.
            if entry.depth() >= caps.depth && entry.file_type().is_dir() {
                return Err(format!(
                    "{} is deeper than the {}-level scan limit for the deny \
                     globs {globs:?}, so deeper matches could be hidden; deny \
                     a parent directory as an exact path instead",
                    entry.path().display(),
                    caps.depth,
                ));
            }
            if glob_set.is_match(entry.path()) {
                insert_match(&mut matched, entry.path(), globs, caps.matches)?;
            }
        }
    }

    Ok(matched.into_iter().collect())
}

/// Insert a match plus its canonical target when the path involves a symlink.
/// Masking only the logical path leaves the content readable at its real
/// location. A non-UTF8 path fails closed.
fn insert_match(
    matched: &mut BTreeSet<String>,
    path: &Path,
    globs: &[String],
    max_matches: usize,
) -> Result<(), String> {
    let mut candidates = vec![path.to_path_buf()];
    candidates.extend(
        dunce::canonicalize(path)
            .ok()
            .filter(|canonical| canonical.as_path() != path),
    );
    for candidate in candidates {
        let Some(text) = candidate.to_str() else {
            return Err(format!(
                "deny-glob match has a non-UTF8 path: {candidate:?}"
            ));
        };
        matched.insert(text.to_owned());
        if matched.len() > max_matches {
            return Err(format!(
                "the deny globs {globs:?} matched over {max_matches} files; \
                 use narrower globs, or deny a parent directory as an exact \
                 path instead"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deny::{is_glob, partition_deny_entries};

    #[test]
    fn detects_metacharacters() {
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
    fn partition_separates_globs_from_exact_paths() {
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
    fn split_root_relative_vs_absolute() {
        let workspace = Path::new("/ws");
        assert_eq!(
            split_glob_root(workspace, "**/.env"),
            (PathBuf::from("/ws"), "**/.env".to_string())
        );
        assert_eq!(
            split_glob_root(workspace, "/home/**/.ssh"),
            (PathBuf::from("/home"), "**/.ssh".to_string())
        );
        assert_eq!(
            split_glob_root(workspace, "secrets/**"),
            (PathBuf::from("/ws/secrets"), "**".to_string())
        );
        assert_eq!(
            split_glob_root(workspace, "a/b/*.pem"),
            (PathBuf::from("/ws/a/b"), "*.pem".to_string())
        );
    }

    #[test]
    fn validate_accepts_subset_rejects_rest() {
        for glob in [
            "**/*.pem",
            "**/.env",
            "secrets/**",
            "[abc].txt",
            "[a-z].rs",
            "[!a]b",
            "[^a]b",
            "a?b",
            "/home/**/.ssh",
        ] {
            assert!(
                validate_deny_glob(glob).is_ok(),
                "{glob} should be supported"
            );
        }
        for glob in ["**/*.{pem,key}", "a\\*b", "{a,b}"] {
            assert!(
                validate_deny_glob(glob).is_err(),
                "{glob} should be rejected"
            );
        }
        for glob in ["[]a]", "[[:alpha:]]", "[a[b]"] {
            assert!(
                validate_deny_glob(glob).is_err(),
                "{glob} unsupported char class should be rejected"
            );
        }
        for glob in ["a**b", "**a", "[abc"] {
            assert!(
                validate_deny_glob(glob).is_err(),
                "{glob} should be rejected as malformed"
            );
        }
        for glob in ["a//b", "secrets//**", "a/b/"] {
            assert!(
                validate_deny_glob(glob).is_err(),
                "{glob} with an empty segment should be rejected"
            );
        }
        for glob in ["../up/**", "./x/*", "a/../b/*"] {
            assert!(
                validate_deny_glob(glob).is_err(),
                "{glob} with a dot segment should be rejected"
            );
        }
    }

    // Pinning the production caps IS the test (a cap change must be a
    // deliberate, reviewed edit), so the constant assertion is intentional.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn production_entries_cap_covers_large_workspaces() {
        assert!(DENY_GLOB_CAPS.entries >= 2_000_000);
        assert_eq!(DENY_GLOB_CAPS.depth, 64);
        assert_eq!(DENY_GLOB_CAPS.matches, 4096);
    }
}
