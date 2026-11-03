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

/// Default skills directory: ~/.memnoth/skills/
pub fn default_skills_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    Ok(home.join(".memnoth").join("skills"))
}

pub use frontmatter::{SkillFrontmatter, SkillMetadata, SkillArgument};
pub use registry::{SkillRegistry, SkillEntry};
pub use template::{TemplateEngine, RenderContext};
pub use commands::{SkillCommand, build_skill_commands};