//! A small `.env` loader for local development.
//!
//! ## Why this exists
//!
//! The CLI reads a dozen optional environment variables (`MEMNOTH_CLOUD_URL`,
//! `MEMNOTH_MCP_PROFILE`, the log filter). A developer running the cloud locally
//! otherwise has to export them in every new shell. `.env` is the usual answer;
//! a dependency is not needed for it.
//!
//! ## The rules, and why each one is a rule
//!
//! - **The real environment always wins.** A variable already exported in the
//!   shell is never overwritten, so a one-off `MEMNOTH_CLOUD_URL=… mn …` still
//!   does what it says.
//! - **Nothing is printed.** Not a value, not a key, not a count. A loader that
//!   logs its input is a loader that puts a token in a terminal scrollback and a
//!   support bundle.
//! - **A malformed line is skipped, not guessed at.** A missing `=`, a key with a
//!   space, or a value carrying a NUL is ignored rather than repaired: the file is
//!   a convenience, and guessing at a broken secret is worse than not loading it.
//! - **The file is bounded.** 64 KiB is far more than this file will ever be, and
//!   a bound means a mistyped path cannot turn startup into a large read.
//! - **Loaded once, at the very start.** Before `AppContext` reads a setting, so
//!   what the loader set is what the rest of the process sees.
//!
//! `.env` is ignored by git (`.gitignore` holds `.env` and `.env.*`); only
//! `.env.example` is tracked. The file this reads is local by construction.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// The largest file this will read. A convenience file that is larger than this
/// is not one a person wrote by hand.
const MAX_BYTES: u64 = 64 * 1024;

/// Load the nearest `.env`, if there is one.
///
/// Silent by design: a missing file is the normal case for an installed binary,
/// and a broken file must not stop a command that does not need it.
pub fn load() {
    for path in candidates() {
        let Some(values) = read_file(&path) else {
            continue;
        };
        for (key, value) in values {
            if std::env::var_os(&key).is_none() {
                // SAFETY: called once, before any product thread starts reading
                // the environment. `run()` is the only caller and it is the first
                // statement there, ahead of `Cli::parse()` and `AppContext::new`.
                unsafe { std::env::set_var(&key, &value) };
            }
        }
    }
}

/// Where to look, in order: the working directory first, then the directory the
/// binary lives in. The working directory wins because a repository-local `.env`
/// is the more specific intent when both exist.
fn candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        paths.push(cwd.join(".env"));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let beside = dir.join(".env");
        if !paths.contains(&beside) {
            paths.push(beside);
        }
    }
    paths
}

fn read_file(path: &std::path::Path) -> Option<BTreeMap<String, String>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    Some(parse(&text))
}

/// Parse `.env` text into pairs. Later duplicates win, matching the shell's own
/// last-assignment behaviour.
///
/// Public to the crate so the rules above can be tested without touching the
/// process environment. It deliberately returns a map and not an iterator of
/// results: a skipped line is a decision, not a failure to report.
pub(crate) fn parse(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !valid_key(key) {
            continue;
        }
        let value = unquote(value.trim());
        if value.contains('\0') {
            continue;
        }
        values.insert(key.to_string(), value);
    }
    values
}

/// A key is what a shell would accept: a letter or underscore, then letters,
/// digits or underscores. This is the set every environment variable in the
/// product uses, and refusing anything else keeps a typo from silently setting a
/// variable nothing reads.
fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Strip one matching pair of single or double quotes, the two forms a `.env`
/// file conventionally allows. Unbalanced quotes are left as written rather than
/// truncated.
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_pair_is_read() {
        let values = parse("MEMNOTH_CLOUD_URL=http://127.0.0.1:8787\n");
        assert_eq!(
            values.get("MEMNOTH_CLOUD_URL").map(String::as_str),
            Some("http://127.0.0.1:8787")
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let values = parse("# a comment\n\n   \nA=1\n");
        assert_eq!(values.len(), 1);
        assert_eq!(values.get("A").map(String::as_str), Some("1"));
    }

    #[test]
    fn export_and_quotes_are_accepted() {
        let values = parse("export A=\"one two\"\nB='three'\n");
        assert_eq!(values.get("A").map(String::as_str), Some("one two"));
        assert_eq!(values.get("B").map(String::as_str), Some("three"));
    }

    #[test]
    fn a_later_duplicate_wins() {
        let values = parse("A=1\nA=2\n");
        assert_eq!(values.get("A").map(String::as_str), Some("2"));
    }

    #[test]
    fn an_invalid_key_is_skipped_rather_than_repaired() {
        let values = parse("A B=1\n1A=2\n=3\nGOOD=4\n");
        assert_eq!(values.len(), 1);
        assert!(values.contains_key("GOOD"));
    }

    #[test]
    fn a_value_with_a_nul_is_skipped() {
        let values = parse("A=one\0two\nB=fine\n");
        assert!(!values.contains_key("A"));
        assert!(values.contains_key("B"));
    }

    #[test]
    fn an_unbalanced_quote_is_left_as_written() {
        let values = parse("A=\"unclosed\n");
        assert_eq!(values.get("A").map(String::as_str), Some("\"unclosed"));
    }

    #[test]
    fn an_empty_value_is_allowed() {
        let values = parse("A=\n");
        assert_eq!(values.get("A").map(String::as_str), Some(""));
    }
}
