//! Skill system for memnoth CLI.
//!
//! This crate provides:
//! - SKILL.md frontmatter parsing
//! - Argument templating
//! - Skill registry and management
//! - Slash command exposure in TUI

pub mod commands;
pub mod extension;
pub mod frontmatter;
pub mod registry;
pub mod template;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Default skills directory: `$MEMNOTH_SKILLS_DIR`, else `~/.memnoth/skills/`.
///
/// The override is not a convenience: the platform home directory is read from
/// `USERPROFILE` on Windows and `HOME` elsewhere, so a test that exports `HOME`
/// still resolves the real home on Windows and would create and delete skills
/// in a developer's actual directory. Tests and CI pin this variable instead.
pub fn default_skills_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("MEMNOTH_SKILLS_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    Ok(home.join(".memnoth").join("skills"))
}
/// Render installed skills as the instruction block a model reads from.
///
/// Names each skill and its purpose without inlining its instructions, so the
/// whole prefix budget is not spent on skills the turn will not use. Returns
/// `None` when there are no skills to list, so callers can skip an empty slot.
pub fn skills_block_from(dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        return None;
    }
    let registry = registry::SkillRegistry::with_dir(dir).ok()?;
    let skills = registry.list();
    if skills.is_empty() {
        return None;
    }
    let mut text = String::from(
        "Installed skills, available through `mn skill run <name>`. Run one only when it fits \
         the request, and say why:",
    );
    for entry in skills {
        text.push_str(&format!(
            "\n- {}: {}",
            entry.frontmatter.metadata.name, entry.frontmatter.metadata.description
        ));
    }
    Some(text)
}

/// Render installed skills from the default skills directory.
pub fn skills_block() -> Option<String> {
    let dir = default_skills_dir().ok()?;
    skills_block_from(&dir)
}

pub use commands::{SkillCommand, build_skill_commands, execute_skill_command};
pub use frontmatter::{SkillArgument, SkillFrontmatter, SkillMetadata};
pub use registry::{SkillEntry, SkillRegistry};
pub use template::{RenderContext, TemplateEngine};
