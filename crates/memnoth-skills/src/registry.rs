//! Skill registry for managing installed skills.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::{default_skills_dir, frontmatter::{parse_skill_file, SkillFrontmatter, validate_skill}, template::{RenderContext, TemplateEngine}};

/// A skill entry in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillEntry {
    /// Skill frontmatter
    pub frontmatter: SkillFrontmatter,
    
    /// Path to the skill directory
    pub path: PathBuf,
    
    /// Whether this skill is from the local user directory (vs built-in)
    pub is_local: bool,
}

/// Skill registry managing all available skills.
#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: HashMap<String, SkillEntry>,
    skills_dir: PathBuf,
}

impl SkillRegistry {
    /// Create a new skill registry with the default skills directory.
    pub fn new() -> Result<Self> {
        let skills_dir = default_skills_dir()?;
        Self::with_dir(skills_dir)
    }
    
    /// Create a new skill registry with a custom skills directory.
    pub fn with_dir<P: AsRef<Path>>(skills_dir: P) -> Result<Self> {
        let skills_dir = skills_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&skills_dir)
            .with_context(|| format!("Failed to create skills directory: {}", skills_dir.display()))?;
        
        let mut registry = Self {
            skills: HashMap::new(),
            skills_dir,
        };
        
        registry.refresh()?;
        Ok(registry)
    }
    
    /// Get the skills directory path.
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }
    
    /// Refresh the registry by scanning the skills directory.
    pub fn refresh(&mut self) -> Result<()> {
        self.skills.clear();
        
        // Scan local skills directory
        self.scan_directory(&self.skills_dir.clone(), true)?;
        
        Ok(())
    }
    
    /// Scan a directory for skills.
    fn scan_directory(&mut self, dir: &Path, is_local: bool) -> Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        
        for entry in WalkDir::new(dir).min_depth(1).max_depth(1).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_dir() {
                continue;
            }
            
            let skill_dir = entry.path();
            let skill_file = skill_dir.join("SKILL.md");
            
            if !skill_file.exists() {
                continue;
            }
            
            match parse_skill_file(&skill_file) {
                Ok(frontmatter) => {
                    if let Err(e) = validate_skill(&frontmatter) {
                        eprintln!("Warning: Skill validation failed for {}: {}", skill_dir.display(), e);
                        continue;
                    }
                    
                    let name = frontmatter.metadata.name.clone();
                    let entry = SkillEntry {
                        frontmatter,
                        path: skill_dir.to_path_buf(),
                        is_local,
                    };
                    
                    // Check for duplicate names
                    if self.skills.contains_key(&name) {
                        eprintln!("Warning: Duplicate skill name '{}', skipping {}", name, skill_dir.display());
                        continue;
                    }
                    
                    self.skills.insert(name, entry);
                }
                Err(e) => {
                    eprintln!("Warning: Failed to parse skill at {}: {}", skill_dir.display(), e);
                }
            }
        }
        
        Ok(())
    }
    
    /// Get a skill by name.
    pub fn get(&self, name: &str) -> Option<&SkillEntry> {
        self.skills.get(name)
    }
    
    /// Get a mutable reference to a skill by name.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut SkillEntry> {
        self.skills.get_mut(name)
    }
    
    /// List all skill names.
    pub fn list_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.skills.keys().cloned().collect();
        names.sort();
        names
    }
    
    /// List all skills with basic info.
    pub fn list(&self) -> Vec<&SkillEntry> {
        let mut skills: Vec<_> = self.skills.values().collect();
        skills.sort_by(|a, b| a.frontmatter.metadata.name.cmp(&b.frontmatter.metadata.name));
        skills
    }
    
    /// Add a new skill from a SKILL.md file.
    pub fn add_skill<P: AsRef<Path>>(&mut self, skill_file: P) -> Result<String> {
        let skill_file = skill_file.as_ref();
        let frontmatter = parse_skill_file(skill_file)?;
        validate_skill(&frontmatter)?;
        
        let name = frontmatter.metadata.name.clone();
        
        // Create skill directory
        let skill_dir = self.skills_dir.join(&name);
        std::fs::create_dir_all(&skill_dir)
            .with_context(|| format!("Failed to create skill directory: {}", skill_dir.display()))?;
        
        // Copy SKILL.md to the skill directory
        let dest_file = skill_dir.join("SKILL.md");
        std::fs::copy(skill_file, &dest_file)
            .with_context(|| format!("Failed to copy skill file to: {}", dest_file.display()))?;
        
        // Copy any additional files in the same directory as the source
        if let Some(parent) = skill_file.parent() {
            for entry in WalkDir::new(parent).min_depth(1).into_iter().filter_map(|e| e.ok()) {
                let src = entry.path();
                if src == skill_file {
                    continue;
                }
                let rel = src.strip_prefix(parent).unwrap();
                let dest = skill_dir.join(rel);
                if src.is_dir() {
                    std::fs::create_dir_all(&dest)
                        .with_context(|| format!("Failed to create directory: {}", dest.display()))?;
                } else {
                    if let Some(p) = dest.parent() {
                        std::fs::create_dir_all(p)
                            .with_context(|| format!("Failed to create parent directory: {}", p.display()))?;
                    }
                    std::fs::copy(src, &dest)
                        .with_context(|| format!("Failed to copy file: {} -> {}", src.display(), dest.display()))?;
                }
            }
        }
        
        let entry = SkillEntry {
            frontmatter,
            path: skill_dir,
            is_local: true,
        };
        
        self.skills.insert(name.clone(), entry);
        
        Ok(name)
    }
    
    /// Remove a skill by name.
    pub fn remove_skill(&mut self, name: &str) -> Result<()> {
        let entry = self.skills.remove(name)
            .ok_or_else(|| anyhow::anyhow!("Skill '{}' not found", name))?;
        
        // Remove the skill directory
        if entry.path.exists() {
            std::fs::remove_dir_all(&entry.path)
                .with_context(|| format!("Failed to remove skill directory: {}", entry.path.display()))?;
        }
        
        Ok(())
    }
    
    /// Get the skill content rendered with arguments.
    pub fn render_skill(&self, name: &str, args: HashMap<String, serde_json::Value>) -> Result<String> {
        let entry = self.get(name)
            .ok_or_else(|| anyhow::anyhow!("Skill '{}' not found", name))?;
        
        let mut context = RenderContext::new()
            .with_args(args)
            .with_metadata("name", serde_json::json!(entry.frontmatter.metadata.name))
            .with_metadata("version", serde_json::json!(entry.frontmatter.metadata.version))
            .with_metadata("description", serde_json::json!(entry.frontmatter.metadata.description));
        
        // Add working directory if specified
        if !entry.frontmatter.metadata.working_dir.is_empty() {
            context = context.with_cwd(&entry.frontmatter.metadata.working_dir);
        }
        
        // Add current working directory as fallback
        if let Ok(cwd) = std::env::current_dir() {
            context = context.with_cwd(cwd.to_string_lossy().to_string());
        }
        
        let engine = TemplateEngine::new();
        engine.render(&entry.frontmatter.content, &context)
    }
    
    /// Get skill arguments for validation/completion.
    pub fn get_skill_arguments(&self, name: &str) -> Option<Vec<crate::frontmatter::SkillArgument>> {
        self.get(name).map(|e| e.frontmatter.arguments.clone())
    }
    
    /// Check if a skill exists.
    pub fn has_skill(&self, name: &str) -> bool {
        self.skills.contains_key(name)
    }
    
    /// Get the number of skills.
    pub fn len(&self) -> usize {
        self.skills.len()
    }
    
    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::io::Write;
    
    #[test]
    fn test_registry_creation() {
        let temp_dir = TempDir::new().unwrap();
        let registry = SkillRegistry::with_dir(temp_dir.path()).unwrap();
        assert_eq!(registry.len(), 0);
    }
    
    #[test]
    fn test_add_and_get_skill() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = SkillRegistry::with_dir(temp_dir.path()).unwrap();
        
        // Create a skill file
        let skill_content = r#"---
name = "test-skill"
description = "A test skill"
arguments = [
  { name = "input", description = "Input value", type = "string", required = true }
]
---

Hello {{input}}!
"#;
        
        let skill_file = temp_dir.path().join("test-skill.md");
        let mut file = std::fs::File::create(&skill_file).unwrap();
        file.write_all(skill_content.as_bytes()).unwrap();
        
        let name = registry.add_skill(&skill_file).unwrap();
        assert_eq!(name, "test-skill");
        assert_eq!(registry.len(), 1);
        
        let entry = registry.get("test-skill").unwrap();
        assert_eq!(entry.frontmatter.metadata.name, "test-skill");
        assert_eq!(entry.frontmatter.metadata.description, "A test skill");
        assert_eq!(entry.frontmatter.arguments.len(), 1);
    }
    
    #[test]
    fn test_remove_skill() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = SkillRegistry::with_dir(temp_dir.path()).unwrap();
        
        let skill_content = r#"---
name = "test-skill"
description = "A test skill"
---

Content
"#;
        
        let skill_file = temp_dir.path().join("test-skill.md");
        std::fs::write(&skill_file, skill_content).unwrap();
        
        registry.add_skill(&skill_file).unwrap();
        assert_eq!(registry.len(), 1);
        
        registry.remove_skill("test-skill").unwrap();
        assert_eq!(registry.len(), 0);
        assert!(!registry.has_skill("test-skill"));
    }
    
    #[test]
    fn test_render_skill() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = SkillRegistry::with_dir(temp_dir.path()).unwrap();
        
        let skill_content = r#"---
name = "greet"
description = "Greet someone"
arguments = [
  { name = "name", description = "Name to greet", type = "string", required = true }
]
---

Hello {{name}}!
"#;
        
        let skill_file = temp_dir.path().join("greet.md");
        std::fs::write(&skill_file, skill_content).unwrap();
        
        registry.add_skill(&skill_file).unwrap();
        
        let mut args = HashMap::new();
        args.insert("name".to_string(), serde_json::json!("World"));
        
        let rendered = registry.render_skill("greet", args).unwrap();
        assert_eq!(rendered, "Hello World!");
    }
    
    #[test]
    fn test_list_skills() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = SkillRegistry::with_dir(temp_dir.path()).unwrap();
        
        for name in ["alpha", "beta", "gamma"] {
            let content = format!(r#"---
name = "{}"
description = "Skill {}
---
"#, name, name);
            let skill_file = temp_dir.path().join(format!("{}.md", name));
            std::fs::write(&skill_file, content).unwrap();
            registry.add_skill(&skill_file).unwrap();
        }
        
        let names = registry.list_names();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    }
}