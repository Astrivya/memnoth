//! Extension loading: filesystem discovery and manifest parsing.
//!
//! See [`crate::load_extension`] and [`crate::load_extorations`].

use super::{Extension, LoadedExtension};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Error type for extension loading.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionLoadError {
    /// The extension directory does not exist.
    #[error("extension directory not found: {0}")]
    NotFound(String),
    /// `extension.toml` is not valid for this format.
    #[error("failed to parse extension.toml: {0}")]
    Parse(#[from] toml::de::Error),
    /// `extension.toml` could not be read.
    #[error("failed to read extension.toml: {0}")]
    Io(#[from] std::io::Error),
    /// The manifest parsed but failed validation.
    #[error("extension validation failed: {0}")]
    Validation(String),
}

/// Default extensions directory: `$MEMNOTH_EXTENSIONS_DIR`, else
/// `~/.memnoth/extensions/`.
pub fn default_extensions_dir() -> Result<PathBuf, ExtensionLoadError> {
    if let Some(dir) = std::env::var_os("MEMNOTH_EXTENSIONS_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = dirs::home_dir().ok_or_else(|| {
        ExtensionLoadError::Validation("could not find the home directory".to_string())
    })?;
    Ok(home.join(".memnoth").join("extensions"))
}

/// Resolve the directory for a single extension by name.
pub fn extension_dir(name: &str) -> Result<PathBuf, ExtensionLoadError> {
    Ok(default_extensions_dir()?.join(name))
}

/// Load a single extension by name from the default extensions directory.
pub fn load_extension(name: &str) -> Result<LoadedExtension, ExtensionLoadError> {
    let root = extension_dir(name)?;
    let toml_path = root.join("extension.toml");
    if !toml_path.is_file() {
        return Err(ExtensionLoadError::NotFound(name.to_string()));
    }
    let content = std::fs::read_to_string(&toml_path).map_err(ExtensionLoadError::Io)?;
    let manifest: Extension = toml::from_str(&content)?;

    crate::validate::validate_extension(&manifest)
        .map_err(|error| ExtensionLoadError::Validation(format!("{error:#}")))?;

    let agents_dir = if manifest.agents.enabled {
        let p = root.join(&manifest.agents.dir);
        if p.is_dir() { Some(p) } else { None }
    } else {
        None
    };
    let skills_dir = if manifest.skills.enabled {
        let p = root.join(&manifest.skills.dir);
        if p.is_dir() { Some(p) } else { None }
    } else {
        None
    };
    let commands_dir = if manifest.commands.enabled {
        let p = root.join(&manifest.commands.dir);
        if p.is_dir() { Some(p) } else { None }
    } else {
        None
    };
    let hooks_dir = if manifest.hooks.enabled {
        let p = root.join(&manifest.hooks.dir);
        if p.is_dir() { Some(p) } else { None }
    } else {
        None
    };
    let themes_dir = if manifest.themes.enabled {
        let p = root.join(&manifest.themes.dir);
        if p.is_dir() { Some(p) } else { None }
    } else {
        None
    };
    let readme_path = manifest.readme.as_ref().and_then(|r| {
        let p = root.join(&r.file);
        if p.is_file() { Some(p) } else { None }
    });

    Ok(LoadedExtension {
        manifest,
        root,
        agents_dir,
        skills_dir,
        commands_dir,
        hooks_dir,
        themes_dir,
        readme_path,
    })
}

/// Load all extensions from the default extensions directory.
pub fn load_extorations() -> Result<BTreeMap<String, LoadedExtension>, ExtensionLoadError> {
    let dir = default_extensions_dir()?;
    let mut out = BTreeMap::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in walkdir::WalkDir::new(&dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(ext) = load_extension(&name) {
            out.insert(name, ext);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_extension_missing_dir_returns_not_found() {
        let err = load_extension("does-not-exist").unwrap_err();
        assert!(matches!(err, ExtensionLoadError::NotFound(_)));
    }
}
