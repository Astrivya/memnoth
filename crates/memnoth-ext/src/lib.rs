//! memnoth-ext — the memnoth extension format.
//!
//! An extension lives at `.memnoth/extensions/<name>/` and is described by
//! `extension.toml`. It bundles:
//! - **agents** — agent definitions the extension registers
//! - **skills** — skill bundles the extension ships
//! - **commands** — CLI/TUI command specs the extension contributes
//! - **hooks** — lifecycle hook declarations
//! - **MCP** — MCP server capability declarations (tools, resources, prompts)
//! - **themes** — TUI/theme overrides
//! - **README** — human-readable extension documentation
//!
//! # Format
//!
//! ```toml
//! [extension]
//! name = "my-extension"
//! version = "1.0.0"
//! description = "...""
//! min_version = "0.1.0-beta.2"
//!
//! [agents]
//! enabled = true
//! dir = "agents"
//!
//! [skills]
//! enabled = true
//! dir = "skills"
//!
//! [commands]
//! enabled = true
//! dir = "commands"
//!
//! [hooks]
//! enabled = true
//! dir = "hooks"
//!
//! [mcp]
//! enabled = true
//! tools = ["my_tool"]
//! resources = ["my://resource"]
//! prompts = ["my_prompt"]
//!
//! [themes]
//! enabled = true
//! dir = "themes"
//!
//! [readme]
//! file = "README.md"
//! ```

#![warn(missing_docs)]
// TODO: Change to #![deny(missing_docs)] after all public items are documented

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod loader;
pub mod validate;

pub use loader::{
    ExtensionLoadError, default_extensions_dir, extension_dir, load_extension, load_extorations,
};
pub use validate::{ValidationReport, validate_extension};

/// Top-level extension manifest parsed from `extension.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extension {
    /// Extension identity and versioning, under `[extension]`.
    #[serde(rename = "extension")]
    pub manifest: ExtensionManifest,

    /// Agent bundle declarations.
    #[serde(default)]
    pub agents: AgentSection,

    /// Skill bundle declarations.
    #[serde(default)]
    pub skills: SkillSection,

    /// Command contribution declarations.
    #[serde(default)]
    pub commands: CommandSection,

    /// Lifecycle hook declarations.
    #[serde(default)]
    pub hooks: HookSection,

    /// MCP capability declarations.
    #[serde(default)]
    pub mcp: McpSection,

    /// Theme override declarations.
    #[serde(default)]
    pub themes: ThemeSection,

    /// README pointer.
    #[serde(default)]
    pub readme: Option<ReadmeSection>,
}

/// Identity and versioning for an extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    /// Unique extension name (matches the directory name).
    pub name: String,

    /// Semantic version of the extension.
    pub version: String,

    /// Human-readable description.
    #[serde(default)]
    pub description: String,

    /// Minimum memnoth CLI version required.
    #[serde(default)]
    pub min_version: String,

    /// Tags for categorization.
    #[serde(default)]
    pub tags: Vec<String>,

    /// Dependencies on other extensions.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// Agents section of an extension manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSection {
    /// Whether agents are enabled for this extension.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Directory within the extension containing agent definitions.
    #[serde(default = "default_agents_dir")]
    pub dir: String,
}

/// Skills section of an extension manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSection {
    /// Whether skills are enabled for this extension.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Directory within the extension containing skill bundles.
    #[serde(default = "default_skills_dir")]
    pub dir: String,
}

/// Commands section of an extension manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSection {
    /// Whether commands are enabled for this extension.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Directory within the extension containing command specs.
    #[serde(default = "default_commands_dir")]
    pub dir: String,
}

/// Hooks section of an extension manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSection {
    /// Whether hooks are enabled for this extension.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Directory within the extension containing hook declarations.
    #[serde(default = "default_hooks_dir")]
    pub dir: String,
}

/// MCP section of an extension manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSection {
    /// Whether MCP capabilities are enabled for this extension.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Tool names the extension contributes.
    #[serde(default)]
    pub tools: Vec<String>,

    /// Resource URI templates the extension contributes.
    #[serde(default)]
    pub resources: Vec<String>,

    /// Prompt names the extension contributes.
    #[serde(default)]
    pub prompts: Vec<String>,
}

/// Themes section of an extension manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeSection {
    /// Whether themes are enabled for this extension.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Directory within the extension containing theme files.
    #[serde(default = "default_themes_dir")]
    pub dir: String,
}

/// README pointer for an extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadmeSection {
    /// README file path within the extension directory.
    pub file: String,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: default_agents_dir(),
        }
    }
}

impl Default for SkillSection {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: default_skills_dir(),
        }
    }
}

impl Default for CommandSection {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: default_commands_dir(),
        }
    }
}

impl Default for HookSection {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: default_hooks_dir(),
        }
    }
}

impl Default for McpSection {
    fn default() -> Self {
        Self {
            enabled: true,
            tools: Vec::new(),
            resources: Vec::new(),
            prompts: Vec::new(),
        }
    }
}

impl Default for ThemeSection {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: default_themes_dir(),
        }
    }
}

/// A loaded extension: the parsed manifest plus resolved filesystem paths.
#[derive(Debug, Clone)]
pub struct LoadedExtension {
    /// The parsed manifest.
    pub manifest: Extension,

    /// Resolved absolute path to the extension directory.
    pub root: PathBuf,

    /// Resolved absolute path to the agents directory (if present).
    pub agents_dir: Option<PathBuf>,

    /// Resolved absolute path to the skills directory (if present).
    pub skills_dir: Option<PathBuf>,

    /// Resolved absolute path to the commands directory (if present).
    pub commands_dir: Option<PathBuf>,

    /// Resolved absolute path to the hooks directory (if present).
    pub hooks_dir: Option<PathBuf>,

    /// Resolved absolute path to the themes directory (if present).
    pub themes_dir: Option<PathBuf>,

    /// Resolved absolute path to the README file (if present).
    pub readme_path: Option<PathBuf>,
}

fn default_true() -> bool {
    true
}

fn default_agents_dir() -> String {
    "agents".to_string()
}

fn default_skills_dir() -> String {
    "skills".to_string()
}

fn default_commands_dir() -> String {
    "commands".to_string()
}

fn default_hooks_dir() -> String {
    "hooks".to_string()
}

fn default_themes_dir() -> String {
    "themes".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_extensions_dir_uses_env_override() {
        unsafe { std::env::set_var("MEMNOTH_EXTENSIONS_DIR", "/tmp/ext-test") };
        let dir = default_extensions_dir().unwrap();
        assert_eq!(dir, PathBuf::from("/tmp/ext-test"));
        unsafe { std::env::remove_var("MEMNOTH_EXTENSIONS_DIR") };
    }

    #[test]
    fn parse_minimal_extension_toml() {
        let toml = r#"
[extension]
name = "test-ext"
version = "1.0.0"
"#;
        let ext: Extension = toml::from_str(toml).unwrap();
        assert_eq!(ext.manifest.name, "test-ext");
        assert_eq!(ext.manifest.version, "1.0.0");
        assert!(ext.agents.enabled);
        assert!(ext.skills.enabled);
        assert_eq!(ext.agents.dir, "agents");
    }

    #[test]
    fn parse_full_extension_toml() {
        let toml = r#"
[extension]
name = "my-ext"
version = "2.0.0"
description = "A test extension"
min_version = "0.1.0-beta.2"
tags = ["mcp", "skills"]
depends_on = ["base"]

[agents]
enabled = true
dir = "agents"

[skills]
enabled = true
dir = "skills"

[commands]
enabled = true
dir = "commands"

[hooks]
enabled = true
dir = "hooks"

[mcp]
enabled = true
tools = ["my_tool"]
resources = ["my://resource"]
prompts = ["my_prompt"]

[themes]
enabled = true
dir = "themes"

[readme]
file = "README.md"
"#;
        let ext: Extension = toml::from_str(toml).unwrap();
        assert_eq!(ext.manifest.name, "my-ext");
        assert_eq!(ext.manifest.version, "2.0.0");
        assert_eq!(ext.manifest.description, "A test extension");
        assert_eq!(ext.mcp.tools, vec!["my_tool"]);
        assert_eq!(ext.mcp.resources, vec!["my://resource"]);
        assert_eq!(ext.mcp.prompts, vec!["my_prompt"]);
        assert_eq!(ext.readme.as_ref().unwrap().file, "README.md");
    }

    #[test]
    fn section_defaults_are_enabled() {
        let toml = r#"
[extension]
name = "x"
version = "1.0.0"
"#;
        let ext: Extension = toml::from_str(toml).unwrap();
        assert!(ext.agents.enabled);
        assert!(ext.skills.enabled);
        assert!(ext.commands.enabled);
        assert!(ext.hooks.enabled);
        assert!(ext.mcp.enabled);
        assert!(ext.themes.enabled);
        assert_eq!(ext.agents.dir, "agents");
        assert_eq!(ext.skills.dir, "skills");
        assert_eq!(ext.commands.dir, "commands");
        assert_eq!(ext.hooks.dir, "hooks");
        assert_eq!(ext.themes.dir, "themes");
    }
}
