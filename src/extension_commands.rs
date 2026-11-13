//! Extension commands: `/ext:<name>` and `mn extension run <ext>:<name>`.
//!
//! An extension ships commands as Markdown files under `commands/`. Resolving
//! one means: find the installed extension by name, read
//! `commands/<name>.md`, and hand the body back. Nothing is executed here —
//! the body is a prompt, and the session is what runs a prompt.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use memnoth_skills::extension::{ExtensionStore, InstalledExtension};

/// One resolved extension command.
#[derive(Debug, Clone)]
pub struct CommandPrompt {
    /// The extension that owns it.
    pub extension: String,
    /// The command name, without the extension prefix.
    pub name: String,
    /// The Markdown body the command ships.
    pub body: String,
    /// The file the body came from.
    pub path: PathBuf,
}

/// Split `<extension>:<command>`, accepting a leading `/` or `ext:`.
///
/// Both spellings reach here: `/ext:reviewer:review` from a session and
/// `reviewer:review` from `mn extension run`.
pub fn parse_qualified(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim();
    let raw = raw.strip_prefix('/').unwrap_or(raw);
    let raw = raw.strip_prefix("ext:").unwrap_or(raw);
    let (extension, name) = raw.split_once(':')?;
    let (extension, name) = (extension.trim(), name.trim());
    if extension.is_empty() || name.is_empty() {
        return None;
    }
    Some((extension.to_string(), name.to_string()))
}

/// Read one installed extension command's body.
pub fn resolve(home: &Path, qualified: &str) -> Result<CommandPrompt> {
    let (extension, name) = parse_qualified(qualified).ok_or_else(|| {
        anyhow::anyhow!(
            "{qualified:?} is not <extension>:<command> — try ext:<extension>:<command>"
        )
    })?;

    let store = ExtensionStore::new(home.join("extensions"));
    let installed = store.list()?;
    let found = installed
        .iter()
        .find(|candidate| candidate.name == extension)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no extension named {extension} — installed: {}",
                installed_names(&installed)
            )
        })?;

    let path = found.path.join("commands").join(format!("{name}.md"));
    if !path.is_file() {
        bail!(
            "extension {extension} has no command {name} — available: {}",
            available_commands(found)
        );
    }
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    Ok(CommandPrompt {
        extension,
        name,
        body,
        path,
    })
}

/// Expand a session line that begins with `/ext:`.
///
/// `Ok(None)` means the line is not an extension command at all, so the caller
/// keeps parsing it the usual way.
pub fn expand(home: &Path, line: &str) -> Result<Option<String>> {
    if !line.trim().starts_with("/ext:") {
        return Ok(None);
    }
    Ok(Some(resolve(home, line)?.body))
}

fn installed_names(installed: &[InstalledExtension]) -> String {
    if installed.is_empty() {
        return "none (mn extension new <path> installs one)".to_string();
    }
    installed
        .iter()
        .map(|extension| extension.name.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

fn available_commands(extension: &InstalledExtension) -> String {
    let mut names: Vec<String> = extension.manifest.commands.clone();
    if let Ok(entries) = std::fs::read_dir(extension.path.join("commands")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    if names.is_empty() {
        "none".to_string()
    } else {
        names
            .into_iter()
            .map(|name| format!("{}:{name}", extension.name))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn extension_source(root: &Path, name: &str, command: &str, body: &str) -> PathBuf {
        let dir = root.join(format!("{name}-src"));
        fs::create_dir_all(dir.join("commands")).unwrap();
        fs::write(
            dir.join("manifest.json"),
            serde_json::json!({
                "name": name,
                "version": "0.1.0",
                "description": "test extension",
                "agents": [], "skills": [],
                "commands": [command],
                "hooks": [], "mcp": [], "themes": [],
                "readme": null,
            })
            .to_string(),
        )
        .unwrap();
        fs::write(dir.join("commands").join(format!("{command}.md")), body).unwrap();
        dir
    }

    #[test]
    fn both_spellings_parse_to_the_same_pair() {
        assert_eq!(
            parse_qualified("/ext:reviewer:review"),
            Some(("reviewer".into(), "review".into()))
        );
        assert_eq!(
            parse_qualified("reviewer:review"),
            Some(("reviewer".into(), "review".into()))
        );
        assert_eq!(parse_qualified("/ext:"), None);
        assert_eq!(parse_qualified("noseparator"), None);
    }

    #[test]
    fn an_installed_command_resolves_to_its_markdown_body() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let source = extension_source(home, "reviewer", "review", "Review the diff.");
        ExtensionStore::new(home.join("extensions"))
            .install(&source)
            .unwrap();

        let prompt = resolve(home, "/ext:reviewer:review").unwrap();
        assert_eq!(prompt.extension, "reviewer");
        assert_eq!(prompt.name, "review");
        assert_eq!(prompt.body, "Review the diff.");
        assert!(prompt.path.ends_with("commands/review.md"));
    }

    #[test]
    fn a_missing_command_names_what_the_extension_has() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let source = extension_source(home, "reviewer", "review", "Review the diff.");
        ExtensionStore::new(home.join("extensions"))
            .install(&source)
            .unwrap();

        let error = resolve(home, "/ext:reviewer:ship").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("no command ship"), "{text}");
        assert!(text.contains("reviewer:review"), "{text}");
    }

    #[test]
    fn a_missing_extension_names_what_is_installed() {
        let root = tempfile::tempdir().unwrap();
        let error = resolve(root.path(), "/ext:ghost:review").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("no extension named ghost"), "{text}");
        assert!(text.contains("mn extension new"), "{text}");
    }

    #[test]
    fn expand_ignores_lines_that_are_not_extension_commands() {
        let root = tempfile::tempdir().unwrap();
        assert!(expand(root.path(), "/help").unwrap().is_none());
        assert!(expand(root.path(), "just text").unwrap().is_none());
    }

    #[test]
    fn install_resolve_uninstall_is_one_round_trip() {
        // W11-06: one format, installed with `new` and removed with
        // `uninstall`, with the command resolvable exactly while installed.
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let store = ExtensionStore::new(home.join("extensions"));
        let source = extension_source(home, "reviewer", "review", "Review the diff.");

        let installed = store.install(&source).unwrap();
        assert_eq!(installed.name, "reviewer");
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(resolve(home, "reviewer:review").is_ok());

        store.uninstall("reviewer").unwrap();
        assert!(store.list().unwrap().is_empty());
        assert!(resolve(home, "reviewer:review").is_err());
    }
}
