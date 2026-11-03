//! Namespaces: several isolated vaults inside one workspace or profile.
//!
//! # Why file-per-namespace rather than a column
//!
//! The obvious multi-tenancy design is a `namespace` column and a `WHERE`
//! clause on every query. That design has one failure mode that matters: a
//! single query missing its filter leaks another tenant's memory, silently, and
//! the leak is invisible until someone reads the wrong thing.
//!
//! Here a namespace **is a file**. The default vault stays `vault.db`; every
//! other namespace is `vault-<name>.db` beside it. Isolation is enforced by the
//! filesystem, not by reviewer discipline, and a missing filter cannot leak what
//! is not in the connection. It also means dropping a namespace is a file
//! delete rather than a mass delete of rows inside a shared file — nothing else
//! can be caught in the blast radius.
//!
//! # Names must be validated, not trusted
//!
//! The name reaches a filename, so [`validate_namespace`] rejects path
//! traversal, separators, control characters, Windows reserved device names and
//! trailing dots/spaces (which Windows silently strips), and caps the length at
//! 48 characters. A caller never builds a path from an unvalidated name —
//! [`open_namespace`] validates first, and so does every other entry point in
//! this module.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::store::{MemoryEngine, VaultOptions};

/// The namespace a plain `vault.db` belongs to.
pub const DEFAULT_NAMESPACE: &str = "default";

/// Longest accepted namespace name. Leaves room for the `vault-` prefix and the
/// `.db` suffix inside filesystem limit.
pub const MAX_NAMESPACE_LEN: usize = 48;

/// Windows reserved device names — a file called `nul.db` cannot be created on
/// this platform, so the name is refused up front rather than failing at write.
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Validate a namespace name. Pure; no filesystem access.
///
/// Accepted: `[A-Za-z0-9]` first, then `[A-Za-z0-9._-]`, up to
/// [`MAX_NAMESPACE_LEN`] characters. Everything else — `.`, `..`, separators,
/// control characters, reserved device names, trailing dot or space — is
/// refused with a message naming the rule that failed.
pub fn validate_namespace(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("namespace name is empty");
    }
    if name.len() > MAX_NAMESPACE_LEN {
        bail!(
            "namespace name is {} chars; limit is {}",
            name.len(),
            MAX_NAMESPACE_LEN
        );
    }
    if name.trim() != name {
        bail!("namespace name has leading or trailing whitespace");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        bail!("namespace name ends with a dot or space (stripped on some filesystems)");
    }
    if name == "." || name == ".." {
        bail!("namespace name must not be a path segment");
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-'))
    {
        bail!("namespace name contains a disallowed character {c:?}");
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty checked above");
    if !first.is_ascii_alphanumeric() {
        bail!("namespace name must start with a letter or digit");
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        bail!("namespace name {stem:?} is a reserved device name on Windows");
    }
    Ok(())
}

/// The vault file for `name` inside `root`.
///
/// `default` (and the empty string) map to `root/vault.db`, matching the
/// convention every existing surface already uses; anything else maps to
/// `root/vault-<name>.db`.
pub fn vault_path(root: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name == DEFAULT_NAMESPACE {
        return Ok(root.join("vault.db"));
    }
    validate_namespace(name)?;
    Ok(root.join(format!("vault-{name}.db")))
}

/// Open (creating if needed) the vault for a namespace.
pub fn open_namespace(root: &Path, name: &str, options: VaultOptions) -> Result<MemoryEngine> {
    let path = vault_path(root, name)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create vault dir {}", parent.display()))?;
    }
    MemoryEngine::open_with_options(&path, options)
}

/// Namespace names that currently have a vault file in `root`, sorted.
///
/// A missing `root` reads as "no namespaces" rather than an error: the first
/// call on a fresh machine is not a failure.
pub fn list_namespaces(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return Ok(out),
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if name == "vault.db" {
            out.push(DEFAULT_NAMESPACE.to_string());
        } else if let Some(rest) = name
            .strip_prefix("vault-")
            .and_then(|r| r.strip_suffix(".db"))
            && validate_namespace(rest).is_ok()
        {
            out.push(rest.to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// What one namespace holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespaceInfo {
    /// Namespace name (`default` for the un-suffixed vault).
    pub name: String,
    /// Absolute-or-as-given vault path.
    pub path: String,
    /// Current (non-superseded) entities, or `None` when the file could not be
    /// read — reported rather than hidden, so a corrupt tenant is visible.
    pub entities_current: Option<i64>,
    /// Bytes on disk for the main database file.
    pub bytes: u64,
}

/// Inspect every namespace in `root` without modifying any of them.
///
/// Each vault is opened **read-only**, so an inventory pass cannot create tables
/// in a tenant that has not been initialized yet. A file that fails to open is
/// still listed, with `entities_current: None`.
pub fn inspect(root: &Path) -> Result<Vec<NamespaceInfo>> {
    let mut out = Vec::new();
    for name in list_namespaces(root)? {
        let path = vault_path(root, &name)?;
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let count = read_only_count(&path).ok();
        out.push(NamespaceInfo {
            name,
            path: path.display().to_string(),
            entities_current: count,
            bytes,
        });
    }
    Ok(out)
}

fn read_only_count(path: &Path) -> Result<i64> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open {} read-only", path.display()))?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM entities WHERE valid_to IS NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(n)
}

/// Delete a namespace's vault file, with its `-wal` and `-shm` siblings.
///
/// Refuses the default namespace, and refuses any namespace that still holds
/// current entities unless `force` is set — deleting live memory should be a
/// deliberate act, not a typo in a maintenance script.
pub fn drop_namespace(root: &Path, name: &str, force: bool) -> Result<()> {
    if name.is_empty() || name == DEFAULT_NAMESPACE {
        bail!("refusing to drop the default namespace");
    }
    let path = vault_path(root, name)?;
    if !path.exists() {
        bail!("namespace {name:?} has no vault at {}", path.display());
    }
    if !force {
        let live = read_only_count(&path).unwrap_or(0);
        if live > 0 {
            bail!("namespace {name:?} still holds {live} current entities; pass force to drop");
        }
    }
    std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", path.display()));
        if side.exists() {
            let _ = std::fs::remove_file(&side);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EntityKind;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("memnoth-ns-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn names_that_reach_a_filename_are_validated() {
        for good in ["team-1", "acme.prod", "a", "x_9"] {
            assert!(validate_namespace(good).is_ok(), "{good} should be valid");
        }
        for bad in [
            "",
            " ",
            "..",
            ".",
            "a/b",
            "a\\b",
            "a b",
            "a\nb",
            "con",
            "NUL",
            "com1",
            "team.",
            "-lead",
            "Team Lead",
        ] {
            assert!(
                validate_namespace(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
        assert!(validate_namespace(&"x".repeat(MAX_NAMESPACE_LEN + 1)).is_err());
        assert!(validate_namespace(&"x".repeat(MAX_NAMESPACE_LEN)).is_ok());
    }

    #[test]
    fn path_resolution_keeps_the_default_convention() {
        let root = Path::new("C:/ws/.memnoth");
        assert!(vault_path(root, "default").unwrap().ends_with("vault.db"));
        assert!(vault_path(root, "").unwrap().ends_with("vault.db"));
        assert!(
            vault_path(root, "team-1")
                .unwrap()
                .ends_with("vault-team-1.db")
        );
        // Traversal cannot escape the root because it cannot be a valid name.
        assert!(vault_path(root, "../secrets").is_err());
        assert!(vault_path(root, "..\\secrets").is_err());
    }

    #[test]
    fn two_namespaces_never_see_each_others_memory() {
        let root = tmp("isolation");
        let a = open_namespace(&root, "alpha", VaultOptions::default()).expect("alpha");
        let b = open_namespace(&root, "beta", VaultOptions::default()).expect("beta");
        let now = 1_000_000_000_000;
        a.add_entity(EntityKind::Concept, "alpha-only", 0.9, now)
            .expect("write");
        b.add_entity(EntityKind::Concept, "beta-only", 0.9, now)
            .expect("write");

        assert_eq!(a.stats().unwrap().entities_current, 1);
        assert_eq!(b.stats().unwrap().entities_current, 1);
        assert!(
            a.find_entities_by_name("beta-only", now)
                .unwrap()
                .is_empty()
        );
        assert!(
            b.find_entities_by_name("alpha-only", now)
                .unwrap()
                .is_empty()
        );

        let names = list_namespaces(&root).unwrap();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_default_namespace_shares_the_vault_file_every_surface_uses() {
        let root = tmp("default");
        let e = open_namespace(&root, "default", VaultOptions::default()).expect("default");
        e.add_entity(EntityKind::Concept, "shared", 0.9, 1)
            .expect("write");
        assert!(root.join("vault.db").exists());
        assert_eq!(list_namespaces(&root).unwrap(), vec!["default".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inspect_reports_counts_and_sizes_without_writing() {
        let root = tmp("inspect");
        let e = open_namespace(&root, "gamma", VaultOptions::default()).expect("gamma");
        e.add_entity(EntityKind::Concept, "one", 0.9, 1)
            .expect("write");
        let report = inspect(&root).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].name, "gamma");
        assert_eq!(report[0].entities_current, Some(1));
        assert!(report[0].bytes > 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dropping_refuses_the_default_and_live_data_but_honours_force() {
        let root = tmp("drop");
        assert!(drop_namespace(&root, "default", true).is_err());

        let e = open_namespace(&root, "old", VaultOptions::default()).expect("old");
        e.add_entity(EntityKind::Concept, "live", 0.9, 1)
            .expect("write");
        assert!(drop_namespace(&root, "old", false).is_err());
        assert!(root.join("vault-old.db").exists());
        // Release the vault before dropping it: on Windows an open SQLite handle
        // keeps the file undeletable (`os error 32`), so this test could never
        // have passed there while `e` was still alive. Dropping the vault is a
        // precondition of the force drop, not an incidental detail.
        drop(e);
        drop_namespace(&root, "old", true).expect("force drop");
        assert!(!root.join("vault-old.db").exists());
        assert!(list_namespaces(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_busy_timeout_is_actually_applied_to_the_connection() {
        let root = tmp("busy");
        let e = open_namespace(
            &root,
            "tuned",
            VaultOptions {
                busy_timeout_ms: Some(1234),
            },
        )
        .expect("open");
        assert_eq!(e.busy_timeout_ms().unwrap(), 1234);
        let default = open_namespace(&root, "plain", VaultOptions::default()).expect("open");
        assert_eq!(
            default.busy_timeout_ms().unwrap(),
            crate::store::DEFAULT_BUSY_TIMEOUT_MS as i64
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn durability_survives_a_reopen_and_the_integrity_verdict_is_honest() {
        let root = tmp("durable");
        let now = 1_000_000_000_000;
        {
            let e = open_namespace(&root, "durable", VaultOptions::default()).expect("open");
            let id = e
                .add_entity(EntityKind::Concept, "persistent", 0.9, now)
                .expect("write");
            e.add_observation(id, "survives a reopen", "fact", 0.9, now)
                .expect("obs");
            e.checkpoint().expect("checkpoint");
        }
        let again = open_namespace(&root, "durable", VaultOptions::default()).expect("reopen");
        assert_eq!(again.stats().unwrap().entities_current, 1);
        assert_eq!(again.integrity_check().unwrap(), "ok");
        assert_eq!(again.foreign_key_violations().unwrap(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }
}
