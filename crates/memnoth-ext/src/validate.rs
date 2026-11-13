//! Extension manifest validation.
//!
//! Rules enforced here are pure and filesystem-independent so they are
//! testable without touching disk:
//! - extension name must match its directory name (checked by callers)
//! - version must be parseable semver
//! - mcp tool/resource/prompt names must be non-empty
//! - no empty tags or depends_on entries

use super::Extension;
use anyhow::{Result, bail};

/// Report of a validation run.
#[derive(Debug, Clone)]
pub struct ValidationReport {
    /// Whether the manifest passed validation.
    pub valid: bool,

    /// Human-readable problems found (empty when valid).
    pub problems: Vec<String>,
}

/// Validate an extension manifest. Returns `Ok(())` when valid, or an error
/// describing the first problem found.
pub fn validate_extension(manifest: &Extension) -> Result<()> {
    let report = validate_extension_report(manifest);
    if report.valid {
        Ok(())
    } else {
        bail!(
            "extension validation failed: {}",
            report.problems.join("; ")
        )
    }
}

/// Validate an extension manifest and return a full report.
pub fn validate_extension_report(manifest: &Extension) -> ValidationReport {
    let mut problems: Vec<String> = Vec::new();

    let m = &manifest.manifest;
    if m.name.is_empty() {
        problems.push("extension name is required".to_string());
    }
    if !is_semver(&m.version) {
        problems.push(format!(
            "extension version {} is not valid semver",
            m.version
        ));
    }
    if m.version.is_empty() {
        problems.push("extension version is required".to_string());
    }

    if m.tags.iter().any(|t| t.is_empty()) {
        problems.push("extension tags must not be empty".to_string());
    }
    if m.depends_on.iter().any(|d| d.is_empty()) {
        problems.push("extension depends_on entries must not be empty".to_string());
    }

    if manifest.mcp.tools.iter().any(|t| t.is_empty()) {
        problems.push("mcp tools must not be empty".to_string());
    }
    if manifest.mcp.resources.iter().any(|r| r.is_empty()) {
        problems.push("mcp resources must not be empty".to_string());
    }
    if manifest.mcp.prompts.iter().any(|p| p.is_empty()) {
        problems.push("mcp prompts must not be empty".to_string());
    }

    if let Some(readme) = &manifest.readme
        && readme.file.is_empty()
    {
        problems.push("readme file must not be empty".to_string());
    }

    ValidationReport {
        valid: problems.is_empty(),
        problems,
    }
}

fn is_semver(version: &str) -> bool {
    let version = version.trim_start_matches('v');
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(name: &str, version: &str) -> Extension {
        Extension {
            manifest: crate::ExtensionManifest {
                name: name.to_string(),
                version: version.to_string(),
                description: String::new(),
                min_version: String::new(),
                tags: Vec::new(),
                depends_on: Vec::new(),
            },
            agents: Default::default(),
            skills: Default::default(),
            commands: Default::default(),
            hooks: Default::default(),
            mcp: Default::default(),
            themes: Default::default(),
            readme: None,
        }
    }

    #[test]
    fn valid_manifest_passes() {
        let ext = manifest("test-ext", "1.0.0");
        assert!(validate_extension(&ext).is_ok());
    }

    #[test]
    fn empty_name_fails() {
        let ext = manifest("", "1.0.0");
        assert!(validate_extension(&ext).is_err());
    }

    #[test]
    fn bad_version_fails() {
        let ext = manifest("test-ext", "not-a-version");
        assert!(validate_extension(&ext).is_err());
    }

    #[test]
    fn empty_mcp_tool_fails() {
        let mut ext = manifest("test-ext", "1.0.0");
        ext.mcp.tools = vec!["".to_string()];
        assert!(validate_extension(&ext).is_err());
    }

    #[test]
    fn report_captures_all_problems() {
        let mut ext = manifest("", "bad");
        ext.mcp.tools = vec!["".to_string()];
        ext.mcp.resources = vec!["".to_string()];
        ext.mcp.prompts = vec!["".to_string()];
        let report = validate_extension_report(&ext);
        assert!(!report.valid);
        assert!(!report.problems.is_empty());
    }
}
