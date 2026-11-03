use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use memnoth_command::{
    spec::{AuthLevel, CommandSpec, DangerLevel, OverlayType, ParamSpec, ParamType, SlashSpec},
    RegistryBuilder,
};
use serde_json;

use crate::{default_skills_dir, registry::SkillRegistry};

/// Skill management commands.
#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// Add a new skill from a SKILL.md file.
    Add(AddArgs),
    
    /// List all installed skills.
    List(ListArgs),
    
    /// Remove a skill by name.
    Remove(RemoveArgs),
    
    /// Run a skill with arguments.
    Run(RunArgs),
    
    /// Edit a skill's SKILL.md file.
    Edit(EditArgs),
}

/// Arguments for `mn skill add`.
#[derive(Debug, Args)]
pub struct AddArgs {
    /// Path to the SKILL.md file to add.
    #[arg(value_name = "PATH")]
    pub path: String,
    
    /// Override the skill name (uses frontmatter name by default).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
}

/// Arguments for `mn skill list`.
#[derive(Debug, Args)]
pub struct ListArgs {
    /// Show detailed information for each skill.
    #[arg(long, short)]
    pub detail: bool,
    
    /// Filter by tag.
    #[arg(long, value_name = "TAG")]
    pub tag: Option<String>,
    
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `mn skill remove`.
#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Name of the skill to remove.
    #[arg(value_name = "NAME")]
    pub name: String,
    
    /// Force removal without confirmation.
    #[arg(long, short)]
    pub force: bool,
}

/// Arguments for `mn skill run`.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Name of the skill to run.
    #[arg(value_name = "NAME")]
    pub name: String,
    
    /// Arguments to pass to the skill (KEY=VALUE format).
    #[arg(value_name = "KEY=VALUE", trailing_var_arg = true)]
    pub args: Vec<String>,
    
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `mn skill edit`.
#[derive(Debug, Args)]
pub struct EditArgs {
    /// Name of the skill to edit.
    #[arg(value_name = "NAME")]
    pub name: String,
    
    /// Editor to use (defaults to $EDITOR or vi).
    #[arg(long, short, value_name = "EDITOR")]
    pub editor: Option<String>,
}

/// Build the skill command specs for the registry.
pub fn build_skill_commands(mut builder: RegistryBuilder) -> RegistryBuilder {
    // skill add
    builder = builder
        .command(CommandSpec {
            id: "skill.add".into(),
            cli_path: vec!["skill".into(), "add".into()],
            slash: SlashSpec {
                name: "skill add".into(),
                description: "Add a new skill from a SKILL.md file".into(),
                long_description: None,
                params: vec![
                    ParamSpec::required("path", "Path to SKILL.md file", ParamType::String),
                    ParamSpec {
                        name: "name".into(),
                        description: "Override skill name".into(),
                        param_type: ParamType::String,
                        required: false,
                        default: None,
                        enum_values: Vec::new(),
                        multiple: false,
                    },
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Session,
                profile: None,
                categories: vec!["skill".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec![
                    "skill add ./my-skill.md".into(),
                    "skill add ./my-skill.md --name custom-name".into(),
                ],
            },
            handler: "skill_add".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["skill".into()],
            related: vec!["skill.list".into(), "skill.edit".into()],
        })
        .unwrap();

    // skill list
    builder = builder
        .command(CommandSpec {
            id: "skill.list".into(),
            cli_path: vec!["skill".into(), "list".into()],
            slash: SlashSpec {
                name: "skill list".into(),
                description: "List all installed skills".into(),
                long_description: None,
                params: vec![
                    ParamSpec {
                        name: "detail".into(),
                        description: "Show detailed information".into(),
                        param_type: ParamType::Bool,
                        required: false,
                        default: Some(serde_json::json!(false)),
                        enum_values: Vec::new(),
                        multiple: false,
                    },
                    ParamSpec {
                        name: "tag".into(),
                        description: "Filter by tag".into(),
                        param_type: ParamType::String,
                        required: false,
                        default: None,
                        enum_values: Vec::new(),
                        multiple: false,
                    },
                    ParamSpec {
                        name: "json".into(),
                        description: "Output as JSON".into(),
                        param_type: ParamType::Bool,
                        required: false,
                        default: Some(serde_json::json!(false)),
                        enum_values: Vec::new(),
                        multiple: false,
                    },
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Session,
                profile: None,
                categories: vec!["skill".into()],
                aliases: vec!["skills".into()],
                hidden: false,
                deprecated: None,
                examples: vec![
                    "skill list".into(),
                    "skill list --detail".into(),
                    "skill list --tag testing".into(),
                ],
            },
            handler: "skill_list".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["skill".into()],
            related: vec!["skill.add".into(), "skill.run".into()],
        })
        .unwrap();

    // skill remove
    builder = builder
        .command(CommandSpec {
            id: "skill.remove".into(),
            cli_path: vec!["skill".into(), "remove".into()],
            slash: SlashSpec {
                name: "skill remove".into(),
                description: "Remove a skill by name".into(),
                long_description: None,
                params: vec![
                    ParamSpec::required("name", "Skill name to remove", ParamType::String),
                    ParamSpec {
                        name: "force".into(),
                        description: "Force removal without confirmation".into(),
                        param_type: ParamType::Bool,
                        required: false,
                        default: Some(serde_json::json!(false)),
                        enum_values: Vec::new(),
                        multiple: false,
                    },
                ],
                overlay: OverlayType::Confirm,
                danger: DangerLevel::Dangerous,
                auth: AuthLevel::Session,
                profile: None,
                categories: vec!["skill".into()],
                aliases: vec!["skill rm".into()],
                hidden: false,
                deprecated: None,
                examples: vec![
                    "skill remove old-skill".into(),
                    "skill remove old-skill --force".into(),
                ],
            },
            handler: "skill_remove".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["skill".into()],
            related: vec!["skill.list".into()],
        })
        .unwrap();

    // skill run
    builder = builder
        .command(CommandSpec {
            id: "skill.run".into(),
            cli_path: vec!["skill".into(), "run".into()],
            slash: SlashSpec {
                name: "skill run".into(),
                description: "Run a skill with arguments".into(),
                long_description: None,
                params: vec![
                    ParamSpec::required("name", "Skill name to run", ParamType::String),
                    ParamSpec {
                        name: "args".into(),
                        description: "Skill arguments as KEY=VALUE pairs".into(),
                        param_type: ParamType::Json,
                        required: false,
                        default: None,
                        enum_values: Vec::new(),
                        multiple: true,
                    },
                    ParamSpec {
                        name: "json".into(),
                        description: "Output as JSON".into(),
                        param_type: ParamType::Bool,
                        required: false,
                        default: Some(serde_json::json!(false)),
                        enum_values: Vec::new(),
                        multiple: false,
                    },
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Session,
                profile: None,
                categories: vec!["skill".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec![
                    "skill run greet name=World".into(),
                    "skill run deploy env=prod region=us-east-1".into(),
                ],
            },
            handler: "skill_run".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["skill".into()],
            related: vec!["skill.list".into(), "skill.edit".into()],
        })
        .unwrap();

    // skill edit
    builder = builder
        .command(CommandSpec {
            id: "skill.edit".into(),
            cli_path: vec!["skill".into(), "edit".into()],
            slash: SlashSpec {
                name: "skill edit".into(),
                description: "Edit a skill's SKILL.md file".into(),
                long_description: None,
                params: vec![
                    ParamSpec::required("name", "Skill name to edit", ParamType::String),
                    ParamSpec {
                        name: "editor".into(),
                        description: "Editor to use (defaults to $EDITOR or vi)".into(),
                        param_type: ParamType::String,
                        required: false,
                        default: None,
                        enum_values: Vec::new(),
                        multiple: false,
                    },
                ],
                overlay: OverlayType::Fullscreen,
                danger: DangerLevel::Caution,
                auth: AuthLevel::Session,
                profile: None,
                categories: vec!["skill".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec![
                    "skill edit greet".into(),
                    "skill edit greet --editor code".into(),
                ],
            },
            handler: "skill_edit".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["skill".into()],
            related: vec!["skill.add".into(), "skill.list".into()],
        })
        .unwrap();
    builder
}

/// Execute a skill command.
pub fn execute_skill_command(command: SkillCommand) -> Result<String> {
    match command {
        SkillCommand::Add(args) => execute_add(args),
        SkillCommand::List(args) => execute_list(args),
        SkillCommand::Remove(args) => execute_remove(args),
        SkillCommand::Run(args) => execute_run(args),
        SkillCommand::Edit(args) => execute_edit(args),
    }
}

fn execute_add(args: AddArgs) -> Result<String> {
    let mut registry = SkillRegistry::new()?;
    let name = registry.add_skill(&args.path)?;
    Ok(format!("Added skill: {}", name))
}

fn execute_list(args: ListArgs) -> Result<String> {
    let registry = SkillRegistry::new()?;
    let skills = registry.list();
    
    if args.json {
        let output: Vec<serde_json::Value> = skills.iter().map(|s| {
            let mut obj = serde_json::json!({
                "name": s.frontmatter.metadata.name,
                "description": s.frontmatter.metadata.description,
                "version": s.frontmatter.metadata.version,
                "tags": s.frontmatter.metadata.tags,
                "publish": s.frontmatter.metadata.publish,
                "arguments": s.frontmatter.arguments.len(),
            });
            if args.detail {
                obj["arguments"] = serde_json::to_value(&s.frontmatter.arguments).unwrap();
                obj["examples"] = serde_json::to_value(&s.frontmatter.examples).unwrap();
                obj["requires"] = serde_json::to_value(&s.frontmatter.metadata.requires).unwrap();
                obj["path"] = serde_json::Value::String(s.path.to_string_lossy().to_string());
            }
            obj
        }).collect();
        
        return Ok(serde_json::to_string_pretty(&output)?);
    }
    
    if skills.is_empty() {
        return Ok("No skills installed. Use `mn skill add <path>` to add a skill.".to_string());
    }
    
    let mut lines = Vec::new();
    lines.push(format!("{} skill(s) installed in {}", skills.len(), registry.skills_dir().display()));
    lines.push("".to_string());
    
    for skill in skills {
        if let Some(tag_filter) = &args.tag {
            if !skill.frontmatter.metadata.tags.contains(tag_filter) {
                continue;
            }
        }
        
        lines.push(format!("  {} v{}", skill.frontmatter.metadata.name, skill.frontmatter.metadata.version));
        lines.push(format!("    {}", skill.frontmatter.metadata.description));
        
        if !skill.frontmatter.metadata.tags.is_empty() {
            lines.push(format!("    tags: {}", skill.frontmatter.metadata.tags.join(", ")));
        }
        
        if args.detail {
            if !skill.frontmatter.arguments.is_empty() {
                lines.push("    arguments:".to_string());
                for arg in &skill.frontmatter.arguments {
                    let req = if arg.required { " (required)" } else { "" };
                    let default = arg.default.as_ref().map(|d| format!(" = {}", d)).unwrap_or_default();
                    lines.push(format!("      {}: {}{}{}", arg.name, arg.description, req, default));
                }
            }
            if !skill.frontmatter.metadata.requires.is_empty() {
                lines.push(format!("    requires: {}", skill.frontmatter.metadata.requires.join(", ")));
            }
            lines.push(format!("    path: {}", skill.path.display()));
        }
        
        lines.push("".to_string());
    }
    
    Ok(lines.join("\n"))
}

fn execute_remove(args: RemoveArgs) -> Result<String> {
    let mut registry = SkillRegistry::new()?;
    
    if !registry.has_skill(&args.name) {
        return Err(anyhow::anyhow!("Skill '{}' not found", args.name));
    }
    
    if !args.force {
        // In a real CLI, we'd prompt for confirmation
        // For now, we'll just require --force
        return Err(anyhow::anyhow!("Use --force to confirm removal"));
    }
    
    registry.remove_skill(&args.name)?;
    Ok(format!("Removed skill: {}", args.name))
}

fn execute_run(args: RunArgs) -> Result<String> {
    let registry = SkillRegistry::new()?;
    
    if !registry.has_skill(&args.name) {
        return Err(anyhow::anyhow!("Skill '{}' not found. Run `mn skill list` to see available skills.", args.name));
    }
    
    // Parse KEY=VALUE arguments
    let mut skill_args = std::collections::HashMap::new();
    for arg in &args.args {
        if let Some((key, value)) = arg.split_once('=') {
            // Try to parse as JSON, fall back to string
            let parsed = serde_json::from_str(value).unwrap_or_else(|_| serde_json::json!(value));
            skill_args.insert(key.to_string(), parsed);
        } else {
            return Err(anyhow::anyhow!("Invalid argument format: '{}'. Use KEY=VALUE", arg));
        }
    }
    
    // Validate required arguments
    if let Some(skill_args_spec) = registry.get_skill_arguments(&args.name) {
        for spec in &skill_args_spec {
            if spec.required && !skill_args.contains_key(&spec.name) {
                if let Some(default) = &spec.default {
                    skill_args.insert(spec.name.clone(), default.clone());
                } else {
                    return Err(anyhow::anyhow!("Missing required argument: {}", spec.name));
                }
            }
        }
    }
    
    let rendered = registry.render_skill(&args.name, skill_args)?;
    
    if args.json {
        let output = serde_json::json!({
            "skill": args.name,
            "rendered": rendered,
        });
        return Ok(serde_json::to_string_pretty(&output)?);
    }
    
    Ok(rendered)
}

fn execute_edit(args: EditArgs) -> Result<String> {
    let mut registry = SkillRegistry::new()?;
    
    let entry = registry.get_mut(&args.name)
        .ok_or_else(|| anyhow::anyhow!("Skill '{}' not found", args.name))?;
    
    let skill_file = entry.path.join("SKILL.md");
    
    // Determine editor
    let editor = args.editor
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| {
            if cfg!(windows) { "notepad".to_string() } else { "vi".to_string() }
        });
    
    // Open editor
    let status = std::process::Command::new(&editor)
        .arg(&skill_file)
        .status()
        .with_context(|| format!("Failed to launch editor: {}", editor))?;
    
    if !status.success() {
        return Err(anyhow::anyhow!("Editor exited with error"));
    }
    
    // Reload the skill to verify it's valid
    let frontmatter = crate::frontmatter::parse_skill_file(&skill_file)?;
    crate::frontmatter::validate_skill(&frontmatter)?;
    
    // Update the registry entry
    entry.frontmatter = frontmatter;
    
    Ok(format!("Edited skill: {}", args.name))
}

/// Get skill names for shell completion.
pub fn skill_names() -> Vec<String> {
    SkillRegistry::new()
        .map(|r| r.list_names())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::io::Write;
    
    #[test]
    fn test_skill_commands_integration() {
        let temp_dir = TempDir::new().unwrap();
        std::env::set_var("HOME", temp_dir.path());
        
        let skill_content = r#"---
name = "test-skill"
description = "A test skill"
arguments = [
  { name = "name", description = "Name to greet", type = "string", required = true }
]
---

Hello {{name}}!
"#;
        
        let skill_file = temp_dir.path().join("test-skill.md");
        std::fs::write(&skill_file, skill_content).unwrap();
        
        // Test add
        let result = execute_add(AddArgs { path: skill_file.to_string_lossy().to_string(), name: None }).unwrap();
        assert!(result.contains("test-skill"));
        
        // Test list
        let result = execute_list(ListArgs { detail: false, tag: None, json: false }).unwrap();
        assert!(result.contains("test-skill"));
        
        // Test run
        let result = execute_run(RunArgs { name: "test-skill".to_string(), args: vec!["name=World".to_string()], json: false }).unwrap();
        assert_eq!(result.trim(), "Hello World!");
        
        // Test remove
        let result = execute_remove(RemoveArgs { name: "test-skill".to_string(), force: true }).unwrap();
        assert!(result.contains("Removed"));
    }
}