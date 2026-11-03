//! Normalization of `read_only` / `read_write` config entries.
//!
//! Port note: semantics follow grok-build's
//! `xai-grok-sandbox/src/allow_path.rs` (see THIRD-PARTY-NOTICES.md),
//! reimplemented. Allow paths are literal directory grants. The only rewrite
//! ever performed is stripping one trailing recursive glob down to the
//! directory the user plainly meant. Everything else passes through
//! byte-for-byte or is rejected — never widened.

use std::path::PathBuf;

use crate::deny::is_glob;

/// Normalize one allow-path entry.
///
/// - One trailing `/**/*`, `/**/`, `/**`, or `/*` strips to the parent
///   (root forms grant `/`).
/// - Surrounding whitespace is rejected (whitespace is significant in
///   literal paths; trimming would widen or relocate the grant).
/// - Anything still glob-shaped after the strip is rejected, never widened.
/// - `deny` globs are unaffected (they live in [`crate::deny`]).
pub fn normalize_allow_path(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    if raw.trim() != raw {
        return None;
    }

    let mut s = raw;
    if let Some(parent) = s
        .strip_suffix("/**/*")
        .or_else(|| s.strip_suffix("/**/"))
        .or_else(|| s.strip_suffix("/**"))
        .or_else(|| s.strip_suffix("/*"))
    {
        s = parent.trim_end_matches('/');
        if s.is_empty() {
            // The whole path was a root glob (`/**`, `/**/`, `/**/*`, `/*`).
            s = "/";
        }
    }

    if is_glob(s) {
        return None;
    }

    Some(PathBuf::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_allow_path_cases() {
        for (raw, expected) in [
            ("/home/u/.cargo/cache/**", Some("/home/u/.cargo/cache")),
            ("/home/u/.cargo/cache/**/", Some("/home/u/.cargo/cache")),
            ("/home/u/.cargo/cache/**/*", Some("/home/u/.cargo/cache")),
            ("/tmp/scratch/*", Some("/tmp/scratch")),
            ("/**", Some("/")),
            ("/**/", Some("/")),
            ("/**/*", Some("/")),
            ("/*", Some("/")),
            ("/tmp/scratch", Some("/tmp/scratch")),
            ("/tmp/scratch/", Some("/tmp/scratch")),
            ("/", Some("/")),
            // Still glob-shaped after one strip: skip, never widen further.
            ("/a/**/**", None),
            ("/a/*/*", None),
            ("/home/**/cache", None),
            ("/tmp/foo*", None),
            // Surrounding whitespace is rejected, never trimmed.
            ("/tmp/* ", None),
            ("/srv/cache ", None),
            (" /srv/cache", None),
            ("   ", None),
            ("", None),
        ] {
            assert_eq!(
                normalize_allow_path(raw),
                expected.map(PathBuf::from),
                "normalize_allow_path({raw:?})"
            );
        }
    }
}
