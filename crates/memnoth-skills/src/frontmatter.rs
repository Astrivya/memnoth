//! SKILL.md frontmatter parsing.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Full frontmatter structure for a SKILL.md file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    /// Skill metadata (required fields)
    #[serde(flatten)]
    pub metadata: SkillMetadata,

    /// Arguments accepted by the skill (optional)
    #[serde(default)]
    pub arguments: Vec<SkillArgument>,

    /// Example invocations (optional)
    #[serde(default)]
    pub examples: Vec<SkillExample>,

    /// Raw content after frontmatter (the skill instructions)
    #[serde(skip)]
    pub content: String,
}

/// Core skill metadata.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillMetadata {
    /// Unique skill name (required)
    pub name: String,

    /// Human-readable description (required)
    pub description: String,

    /// Version string (optional)
    #[serde(default)]
    pub version: String,

    /// Author information (optional)
    #[serde(default)]
    pub author: String,

    /// Tags for categorization (optional)
    #[serde(default)]
    pub tags: Vec<String>,

    /// Whether the skill is published (default: false)
    #[serde(default)]
    pub publish: bool,

    /// Minimum memnoth version required (optional)
    #[serde(default)]
    pub min_version: String,

    /// Dependencies on other skills (optional)
    #[serde(default)]
    pub depends_on: Vec<String>,

    /// Required capabilities/tools (optional)
    #[serde(default)]
    pub requires: Vec<String>,

    /// Working directory for the skill (optional)
    #[serde(default)]
    pub working_dir: String,
}

/// Skill argument definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillArgument {
    /// Argument name (required)
    pub name: String,

    /// Human-readable description (required)
    pub description: String,

    /// Argument type (default: "string")
    #[serde(default = "default_arg_type", rename = "type", alias = "arg_type")]
    pub arg_type: String,

    /// Whether argument is required (default: true)
    #[serde(default = "default_true")]
    pub required: bool,

    /// Default value if not provided (optional)
    #[serde(default)]
    pub default: Option<serde_json::Value>,

    /// Enum values for validation (optional)
    #[serde(default)]
    pub enum_values: Vec<String>,

    /// Whether this argument accepts multiple values (default: false)
    #[serde(default)]
    pub multiple: bool,
}

fn default_arg_type() -> String {
    "string".to_string()
}

fn default_true() -> bool {
    true
}

/// Skill example invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillExample {
    /// Example description
    pub description: String,

    /// Example arguments
    pub args: HashMap<String, serde_json::Value>,
}

/// Parse a SKILL.md file and extract frontmatter and content.
pub fn parse_skill_file(path: &Path) -> Result<SkillFrontmatter> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read skill file: {}", path.display()))?;

    parse_skill_content(&content)
}

/// Parse SKILL.md content from a string.
pub fn parse_skill_content(content: &str) -> Result<SkillFrontmatter> {
    // Check for frontmatter delimiter
    let (frontmatter_str, body) = if let Some(opened) = content.strip_prefix("---\n") {
        let end = opened
            .find("\n---\n")
            .or_else(|| opened.find("\n---"))
            .ok_or_else(|| anyhow::anyhow!("Unclosed frontmatter delimiter"))?;
        let fm = &opened[..end];
        let body = &opened[end + 5..]; // Skip "\n---\n"
        (fm, body.trim_start())
    } else if let Some(opened) = content.strip_prefix("---\r\n") {
        let end = opened
            .find("\r\n---\r\n")
            .or_else(|| opened.find("\r\n---"))
            .ok_or_else(|| anyhow::anyhow!("Unclosed frontmatter delimiter"))?;
        let fm = &opened[..end];
        let body = &opened[end + 6..]; // Skip "\r\n---\r\n"
        (fm, body.trim_start())
    } else {
        // No frontmatter - treat entire content as body with minimal metadata
        return Ok(SkillFrontmatter {
            metadata: SkillMetadata {
                name: "unnamed".to_string(),
                description: "Skill without frontmatter".to_string(),
                ..Default::default()
            },
            arguments: Vec::new(),
            examples: Vec::new(),
            content: content.to_string(),
        });
    };

    let mut frontmatter: SkillFrontmatter =
        toml::from_str(frontmatter_str).with_context(|| "Failed to parse frontmatter as TOML")?;

    frontmatter.content = body.trim().to_string();

    // Validate required fields
    if frontmatter.metadata.name.trim().is_empty() {
        return Err(anyhow::anyhow!("Skill name is required in frontmatter"));
    }
    if frontmatter.metadata.description.trim().is_empty() {
        return Err(anyhow::anyhow!(
            "Skill description is required in frontmatter"
        ));
    }

    // Set default version if not provided
    if frontmatter.metadata.version.is_empty() {
        frontmatter.metadata.version = "0.1.0".to_string();
    }

    // Normalize arg_type to lowercase
    for arg in &mut frontmatter.arguments {
        arg.arg_type = arg.arg_type.to_lowercase();
    }

    Ok(frontmatter)
}

/// Validate a skill's frontmatter.
pub fn validate_skill(frontmatter: &SkillFrontmatter) -> Result<()> {
    // Check for duplicate argument names
    let mut seen = std::collections::HashSet::new();
    for arg in &frontmatter.arguments {
        if !seen.insert(&arg.name) {
            return Err(anyhow::anyhow!("Duplicate argument name: {}", arg.name));
        }
    }

    // Validate enum arguments have values
    for arg in &frontmatter.arguments {
        if arg.arg_type == "enum" && arg.enum_values.is_empty() {
            return Err(anyhow::anyhow!(
                "Enum argument '{}' must have enum_values",
                arg.name
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_skill_with_frontmatter() {
        let content = r#"---
name = "test-skill"
description = "A test skill"
version = "1.0.0"
author = "Test Author"
tags = ["test", "example"]
publish = false
requires = ["fs_read", "fs_write"]
---

# Test Skill

This is the skill content.
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let skill = parse_skill_file(file.path()).unwrap();

        assert_eq!(skill.metadata.name, "test-skill");
        assert_eq!(skill.metadata.description, "A test skill");
        assert_eq!(skill.metadata.version, "1.0.0");
        assert_eq!(skill.metadata.author, "Test Author");
        assert_eq!(skill.metadata.tags, vec!["test", "example"]);
        assert!(!skill.metadata.publish);
        assert_eq!(skill.metadata.requires, vec!["fs_read", "fs_write"]);
        assert_eq!(skill.content, "# Test Skill\n\nThis is the skill content.");
    }

    #[test]
    fn test_parse_skill_with_arguments() {
        let content = r#"---
name = "test-skill"
description = "A test skill with arguments"
arguments = [
  { name = "input", description = "Input file", type = "string", required = true },
  { name = "output", description = "Output file", type = "string", required = false, default = "out.txt" },
  { name = "mode", description = "Processing mode", type = "enum", enum_values = ["fast", "slow"], required = false }
]
---

Skill content here.
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let skill = parse_skill_file(file.path()).unwrap();

        assert_eq!(skill.arguments.len(), 3);
        assert_eq!(skill.arguments[0].name, "input");
        assert_eq!(skill.arguments[0].arg_type, "string");
        assert!(skill.arguments[0].required);
        assert_eq!(skill.arguments[1].name, "output");
        assert_eq!(
            skill.arguments[1].default,
            Some(serde_json::json!("out.txt"))
        );
        assert_eq!(skill.arguments[2].arg_type, "enum");
        assert_eq!(skill.arguments[2].enum_values, vec!["fast", "slow"]);
    }

    #[test]
    fn test_parse_skill_without_frontmatter() {
        let content = "# Just content\n\nNo frontmatter here.";

        let skill = parse_skill_content(content).unwrap();

        assert_eq!(skill.metadata.name, "unnamed");
        assert_eq!(skill.content, content);
    }

    #[test]
    fn test_validate_skill_duplicate_args() {
        let content = r#"---
name = "test"
description = "Test"
arguments = [
  { name = "foo", description = "First" },
  { name = "foo", description = "Second" }
]
---
"#;

        let skill = parse_skill_content(content).unwrap();
        let result = validate_skill(&skill);

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Duplicate argument")
        );
    }
}
