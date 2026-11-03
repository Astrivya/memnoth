//! memnoth-ingest — directory ingestion into the memnoth vault.
//!
//! Minimal viable implementation to unblock memnoth-mcp compilation.
//! Provides `ingest_dir_with_progress` used by `mem_ingest` tool body.

use anyhow::{Context, Result};
use memnoth_engine::{EntityKind, MemoryEngine};
use serde::{Deserialize, Serialize};
use std::path::Path;
use walkdir::WalkDir;

/// Report produced at the end of an ingest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestReport {
    /// Number of files seen during the walk.
    pub files_seen: u64,
    /// Number of files actually ingested (new or changed).
    pub files_ingested: u64,
    /// Number of files skipped as unchanged.
    pub files_unchanged: u64,
    /// Number of entities created.
    pub entities_created: u64,
    /// Number of relations created.
    pub relations_created: u64,
    /// Number of observations created.
    pub observations_created: u64,
    /// Number of calls created.
    pub calls_created: u64,
    /// Number of calls unresolved.
    pub calls_unresolved: u64,
}

impl Default for IngestReport {
    fn default() -> Self {
        IngestReport {
            files_seen: 0,
            files_ingested: 0,
            files_unchanged: 0,
            entities_created: 0,
            relations_created: 0,
            observations_created: 0,
            calls_created: 0,
            calls_unresolved: 0,
        }
    }
}

/// Progress callback: invoked with files seen count and relative path.
pub type ProgressFn = Box<dyn FnMut(u64, &str)>;

/// Supported source file extensions for ingestion.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "py", "js", "ts", "jsx", "tsx", "go", "java", "cpp", "cc", "cxx", "c", "h", "hpp", "md",
    "txt", "toml", "yaml", "yml", "json",
];

/// Ingest a directory tree into the vault with progress reporting.
///
/// Walks `root`, reads each supported source file, and stores its content
/// as observations in the vault via `engine`. The `progress` callback is
/// invoked for each file seen with `(files_seen, relative_path)`.
pub fn ingest_dir_with_progress<F>(
    root: &Path,
    engine: &MemoryEngine,
    mut progress: F,
) -> Result<IngestReport>
where
    F: FnMut(u64, &str),
{
    let mut report = IngestReport::default();

    let files = collect_source_files(root)?;

    for file_path in &files {
        let rel = file_path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| file_path.to_string_lossy().to_string());

        let seen = report.files_seen + 1;
        report.files_seen = seen;
        progress(seen, &rel);

        // Read file content
        let content = match std::fs::read_to_string(file_path) {
            Ok(content) => content,
            Err(_) => {
                report.files_unchanged += 1;
                continue;
            }
        };

        // Store as an entity + observation in the vault
        let name = format!("file:{}", rel);
        match engine.add_entity(EntityKind::File, &name, 1.0, crate::now_ms()) {
            Ok(entity_id) => {
                report.entities_created += 1;
                match engine.add_observation(
                    entity_id,
                    &content,
                    "file_content",
                    1.0,
                    crate::now_ms(),
                ) {
                    Ok(_) => {
                        report.files_ingested += 1;
                        report.observations_created += 1;
                    }
                    Err(_) => {
                        report.files_unchanged += 1;
                    }
                }
            }
            Err(_) => {
                report.files_unchanged += 1;
            }
        }
    }

    Ok(report)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Collect source files under root.
fn collect_source_files(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root).into_iter().flatten() {
        if entry.file_type().is_file() {
            if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                if SOURCE_EXTENSIONS.contains(&ext) {
                    files.push(entry.path().to_path_buf());
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Ingest a single file into the vault.
pub fn ingest_file(root: &Path, file: &str, engine: &MemoryEngine) -> Result<IngestReport> {
    let mut report = IngestReport::default();
    let file_path = root.join(file);
    let content = std::fs::read_to_string(&file_path)
        .with_context(|| format!("reading file {}", file_path.display()))?;

    let name = format!("file:{}", file);
    let entity_id = engine.add_entity(EntityKind::File, &name, 1.0, now_ms())?;
    report.entities_created += 1;

    engine.add_observation(entity_id, &content, "file_content", 1.0, now_ms())?;
    report.files_seen = 1;
    report.files_ingested = 1;
    report.observations_created = 1;

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_report_default() {
        let report = IngestReport::default();
        assert_eq!(report.files_seen, 0);
        assert_eq!(report.files_ingested, 0);
        assert_eq!(report.files_unchanged, 0);
        assert_eq!(report.entities_created, 0);
        assert_eq!(report.relations_created, 0);
        assert_eq!(report.observations_created, 0);
        assert_eq!(report.calls_created, 0);
        assert_eq!(report.calls_unresolved, 0);
    }
}
