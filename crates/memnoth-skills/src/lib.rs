//! Skill system for memnoth CLI.
//!
//! This crate provides:
//! - SKILL.md frontmatter parsing
//! - Argument templating
//! - Skill registry and management
//! - Slash command exposure in TUI

pub mod frontmatter;
pub mod registry;
pub mod template;
pub mod commands;

use anyhow::Result;
use std::path::PathBuf;

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

pub use frontmatter::{SkillFrontmatter, SkillMetadata, SkillArgument};
pub use registry::{SkillRegistry, SkillEntry};
pub use template::{TemplateEngine, RenderContext};
pub use commands::{SkillCommand, build_skill_commands, execute_skill_command};