//! Extension format: one manifest per extension directory.
//!
//! An extension is a directory containing `manifest.json` plus subdirectories
//! for agents, skills, commands, hooks, MCP, themes, and a README. One format,
//! installable with `mn extension new <path>` and removable with
//! `mn extension uninstall <name>`.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One extension directory: `manifest.json` + `agents/`, `skills/`,
/// `commands/`, `hooks/`, `mcp/`, `themes/`, `README.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub agents: Vec<String>,
    pub skills: Vec<String>,
    pub commands: Vec<String>,
    pub hooks: Vec<String>,
    pub mcp: Vec<String>,
    pub themes: Vec<String>,
    pub readme: Option<String>,
}

impl Default for ExtensionManifest {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: "0.1.0".into(),
            description: String::new(),
            agents: Vec::new(),
            skills: Vec::new(),
            commands: Vec::new(),
            hooks: Vec::new(),
            mcp: Vec::new(),
            themes: Vec::new(),
            readme: None,
        }
    }
}

/// One installed extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledExtension {
    pub name: String,
    pub version: String,
    pub path: PathBuf,
    pub manifest: ExtensionManifest,
}

/// The extensions directory: `$MEMNOTH_HOME/extensions/<name>/`.
#[derive(Debug, Clone)]
pub struct ExtensionStore {
    dir: PathBuf,
}

impl ExtensionStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// List installed extensions.
    pub fn list(&self) -> Result<Vec<InstalledExtension>> {
        let mut out = Vec::new();
        if !self.dir.is_dir() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }
            let manifest: ExtensionManifest =
                serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)
                    .with_context(|| format!("bad manifest in {}", path.display()))?;
            out.push(InstalledExtension {
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                path,
                manifest,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Install one extension from a source directory.
    pub fn install(&self, source: &Path) -> Result<InstalledExtension> {
        let manifest_path = source.join("manifest.json");
        if !manifest_path.is_file() {
            bail!(
                "extension directory has no manifest.json: {}",
                source.display()
            );
        }
        let manifest: ExtensionManifest =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)
                .with_context(|| format!("bad manifest in {}", source.display()))?;

        if manifest.name.is_empty() {
            bail!("extension manifest has no name");
        }

        let target = self.dir.join(&manifest.name);
        if target.exists() {
            bail!("extension {} is already installed", manifest.name);
        }

        std::fs::create_dir_all(&self.dir)?;
        copy_dir(source, &target)?;

        Ok(InstalledExtension {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            path: target,
            manifest,
        })
    }

    /// Uninstall one extension by name.
    pub fn uninstall(&self, name: &str) -> Result<()> {
        let target = self.dir.join(name);
        if !target.exists() {
            bail!("no extension named {name} installed");
        }
        std::fs::remove_dir_all(&target)
            .with_context(|| format!("cannot remove {}", target.display()))?;
        Ok(())
    }

    /// Run `extension doctor`: verify each installed extension's manifest.
    pub fn doctor(&self) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        for ext in self.list()? {
            if !ext.path.join("manifest.json").is_file() {
                problems.push(format!("{}: missing manifest.json", ext.name));
                continue;
            }
            if ext.manifest.name != ext.name {
                problems.push(format!(
                    "{}: manifest name {} does not match directory {}",
                    ext.name, ext.manifest.name, ext.name
                ));
            }
            let readme = ext.manifest.readme.as_deref().unwrap_or("README.md");
            if !ext.path.join(readme).is_file() {
                problems.push(format!("{}: missing {}", ext.name, readme));
            }
        }
        Ok(problems)
    }
}

/// Recursively copy `source` into `target`.
fn copy_dir(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir(&source_path, &target_path)?;
        } else {
            std::fs::copy(&source_path, &target_path).with_context(|| {
                format!(
                    "cannot copy {} to {}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn extension_manifest_round_trip() {
        let manifest = ExtensionManifest {
            name: "example".into(),
            version: "1.0.0".into(),
            description: "an example".into(),
            agents: vec!["scout".into()],
            skills: vec!["summarize".into()],
            commands: vec!["/example".into()],
            hooks: vec!["PreToolUse".into()],
            mcp: vec!["mem_search".into()],
            themes: vec!["dark".into()],
            readme: Some("README.md".into()),
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let back: ExtensionManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest.name, back.name);
        assert_eq!(manifest.agents, back.agents);
        let _ = json;
    }

    #[test]
    fn store_lists_empty_when_no_dir() {
        let store = ExtensionStore::new(PathBuf::from("/nonexistent-extensions"));
        assert!(store.list().unwrap().is_empty());
    }
}
