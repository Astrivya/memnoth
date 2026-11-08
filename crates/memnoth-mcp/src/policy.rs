//! A blocking tool policy for the MCP server.
//!
//! The plugin hooks next door are **observability**: they run detached and a
//! failing hook cannot stop a call. That is the right shape for telemetry and
//! the wrong shape for governance, so this module is separate and synchronous,
//! and its refusal happens before any dispatcher, approval flow or plugin hook
//! sees the call.
//!
//! # Where the policy comes from
//!
//! `MEMNOTH_MCP_POLICY` names a file explicitly; otherwise
//! `.memnoth/policy.json` beside the vault is used when it exists, and no file
//! means no restriction. The document is deliberately small:
//!
//! ```json
//! { "deny": ["mem_ingest"], "deny_globs": ["coord_*"] }
//! ```
//!
//! # Fail-closed, but only where it can matter
//!
//! A policy file that exists and cannot be parsed refuses **mutating** tools
//! rather than being ignored: a typo must not silently disable the allow-list.
//! Read-only tools continue, because a read cannot perform the change the
//! policy exists to prevent, and bricking every query over a malformed file
//! would make the safe response the one nobody deploys.
//!
//! Every refusal names the rule that produced it — the file and the exact
//! entry — so the model can ask for the smallest widening instead of retrying
//! the same call.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// What the policy says about one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// No rule matches.
    Allow,
    /// A rule refuses the call. `rule` names the file and the entry.
    Deny {
        /// Human sentence naming the exact rule, for the agent and the log.
        rule: String,
    },
}

/// The policy, as loaded for one call.
#[derive(Debug, Clone)]
pub enum Policy {
    /// No file was named and none exists beside the vault.
    Absent,
    /// A parsed document.
    Valid {
        /// Where it came from, so a denial can name it.
        path: PathBuf,
        /// Exact tool names that are refused.
        deny: Vec<String>,
        /// Glob patterns whose matches are refused.
        deny_globs: Vec<String>,
    },
    /// A file that exists but cannot be used. Mutating calls fail closed.
    Invalid {
        /// The path and the parse error.
        reason: String,
    },
}

#[derive(Debug, Deserialize)]
struct Document {
    #[serde(default)]
    deny: Vec<String>,
    #[serde(default)]
    deny_globs: Vec<String>,
}

/// The policy path for this server: the explicit env var, else the file beside
/// the vault when one exists.
pub fn path_from_env_or_vault(vault: Option<&Path>) -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("MEMNOTH_MCP_POLICY") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Some(PathBuf::from(explicit));
        }
    }
    let beside = vault?.parent()?.join("policy.json");
    beside.is_file().then_some(beside)
}

/// Load the policy at `path`. A missing file is [`Policy::Absent`], not an
/// error: most installs have no policy and must keep working.
pub fn load(path: Option<&Path>) -> Policy {
    let Some(path) = path else {
        return Policy::Absent;
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Policy::Absent,
        Err(error) => {
            return Policy::Invalid {
                reason: format!("{} cannot be read: {error}", path.display()),
            };
        }
    };
    match serde_json::from_str::<Document>(&text) {
        Ok(document) => Policy::Valid {
            path: path.to_path_buf(),
            deny: document.deny,
            deny_globs: document.deny_globs,
        },
        Err(error) => Policy::Invalid {
            reason: format!("{} is not a valid policy: {error}", path.display()),
        },
    }
}

impl Policy {
    /// Decide one call. `mutating` comes from the tool's approval class; it is
    /// what makes a malformed policy fail closed for writes and stay out of the
    /// way of reads.
    pub fn decide(&self, tool: &str, mutating: bool) -> Decision {
        match self {
            Policy::Absent => Decision::Allow,
            Policy::Invalid { reason } if mutating => Decision::Deny {
                rule: format!(
                    "an unusable policy ({reason}); mutating tools fail closed until it is fixed"
                ),
            },
            Policy::Invalid { .. } => Decision::Allow,
            Policy::Valid {
                path,
                deny,
                deny_globs,
            } => {
                if deny.iter().any(|entry| entry == tool) {
                    return Decision::Deny {
                        rule: format!("the `deny` entry `{tool}` in {}", path.display()),
                    };
                }
                if let Some(entry) = deny_globs.iter().find(|entry| glob_match(entry, tool)) {
                    return Decision::Deny {
                        rule: format!("the `deny_globs` entry `{entry}` in {}", path.display()),
                    };
                }
                Decision::Allow
            }
        }
    }
}

/// `*` matches any run of characters, including none. Deliberately only `*`:
/// a policy language with more metacharacters is a policy language that needs
/// a parser and a specification, and this one needs neither.
fn glob_match(pattern: &str, name: &str) -> bool {
    let mut pattern_chars = pattern.chars().peekable();
    let mut name_chars = name.chars().peekable();
    while let Some(piece) = pattern_chars.next() {
        if piece == '*' {
            // A trailing star accepts the rest.
            if pattern_chars.peek().is_none() {
                return true;
            }
            // Try every split for the next literal piece.
            let rest: String = pattern_chars.collect();
            let remaining: Vec<char> = name_chars.collect();
            for start in 0..=remaining.len() {
                let candidate: String = remaining[start..].iter().collect();
                if glob_match(&rest, &candidate) {
                    return true;
                }
            }
            return false;
        }
        if name_chars.next() != Some(piece) {
            return false;
        }
    }
    name_chars.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memnoth-policy-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn no_policy_allows_everything() {
        let policy = load(None);
        assert_eq!(policy.decide("mem_ingest", true), Decision::Allow);
    }

    #[test]
    fn an_exact_denial_names_the_entry_and_the_file() {
        let dir = tmp("exact");
        let path = dir.join("policy.json");
        std::fs::write(&path, r#"{"deny":["mem_ingest"]}"#).unwrap();
        let policy = load(Some(&path));
        match policy.decide("mem_ingest", true) {
            Decision::Deny { rule } => {
                assert!(rule.contains("mem_ingest"), "{rule}");
                assert!(rule.contains("policy.json"), "{rule}");
            }
            other => panic!("expected a denial, got {other:?}"),
        }
        assert_eq!(policy.decide("mem_search", false), Decision::Allow);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_glob_denial_covers_the_family_but_not_the_neighbours() {
        let dir = tmp("glob");
        let path = dir.join("policy.json");
        std::fs::write(&path, r#"{"deny_globs":["coord_*"]}"#).unwrap();
        let policy = load(Some(&path));
        assert!(matches!(
            policy.decide("coord_claim", true),
            Decision::Deny { .. }
        ));
        assert!(matches!(
            policy.decide("coord_status", false),
            Decision::Deny { .. }
        ));
        assert_eq!(policy.decide("mem_search", false), Decision::Allow);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_malformed_policy_fails_closed_for_writes_only() {
        let dir = tmp("broken");
        let path = dir.join("policy.json");
        std::fs::write(&path, "{ not json").unwrap();
        let policy = load(Some(&path));
        assert!(matches!(
            policy.decide("mem_ingest", true),
            Decision::Deny { .. }
        ));
        assert_eq!(policy.decide("mem_search", false), Decision::Allow);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_star_is_the_only_metacharacter() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("mem_*", "mem_ingest"));
        assert!(!glob_match("mem_*", "kg_index"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b", "abx"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "exactly"));
    }
}
