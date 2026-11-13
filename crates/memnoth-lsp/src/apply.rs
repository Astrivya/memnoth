//! Applying a `WorkspaceEdit` to files on disk.
//!
//! A language server answers a rename with ranges, not with new file contents.
//! This module turns those ranges into one write per file, refusing the two
//! cases that would corrupt a file: a URI outside the workspace, and two edits
//! whose ranges overlap (the order would decide the result).
//!
//! Edits are applied from the end of the file backwards, so an earlier range
//! stays valid while a later one is replaced.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::protocol::{TextEdit, WorkspaceEdit};
use crate::server::{offset_at, path_from_uri};

/// Apply every edit in `edit` under `root`, returning the files written.
pub fn apply_workspace_edit(root: &Path, edit: &WorkspaceEdit) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for (uri, edits) in edit.changes.iter().flatten() {
        let path = inside(root, &path_from_uri(uri))?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {} for a workspace edit", path.display()))?;
        let updated = apply_to_text(&text, edits)?;
        std::fs::write(&path, updated)
            .with_context(|| format!("writing {} for a workspace edit", path.display()))?;
        written.push(path);
    }
    written.sort();
    written.dedup();
    Ok(written)
}

/// Apply sorted, non-overlapping edits to one document.
pub fn apply_to_text(text: &str, edits: &[TextEdit]) -> Result<String> {
    let mut spans = Vec::with_capacity(edits.len());
    for edit in edits {
        let start = offset_at(text, &edit.range.start).with_context(|| {
            format!(
                "edit starts past the end of the file at {:?}",
                edit.range.start
            )
        })?;
        let end = offset_at(text, &edit.range.end).with_context(|| {
            format!("edit ends past the end of the file at {:?}", edit.range.end)
        })?;
        if end < start {
            bail!("an edit ends before it starts: {:?}", edit.range);
        }
        spans.push((start, end, edit.new_text.as_str()));
    }
    spans.sort_by_key(|(start, end, _)| (*start, *end));
    for pair in spans.windows(2) {
        if pair[0].1 > pair[1].0 {
            bail!(
                "two edits overlap ({}..{} and {}..{}); refusing to pick an order",
                pair[0].0,
                pair[0].1,
                pair[1].0,
                pair[1].1
            );
        }
    }
    let mut out = text.to_string();
    for (start, end, replacement) in spans.iter().rev() {
        out.replace_range(*start..*end, replacement);
    }
    Ok(out)
}

/// `path` resolved under `root`, refusing anything that escapes it.
fn inside(root: &Path, path: &Path) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("resolving workspace root {}", root.display()))?;
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    let parent = parent
        .canonicalize()
        .with_context(|| format!("resolving {} for a workspace edit", parent.display()))?;
    let name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?;
    let resolved = parent.join(name);
    if !resolved.starts_with(&root) {
        bail!("{} is outside the workspace", path.display());
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Position, Range};

    fn range(line: u32, start: u32, end: u32) -> Range {
        Range {
            start: Position {
                line,
                character: start,
            },
            end: Position {
                line,
                character: end,
            },
        }
    }

    fn workspace() -> (PathBuf, std::path::PathBuf) {
        // One directory per call, not per process: parallel tests share a
        // pid, and two of them that started in the same clock tick used to
        // build a workspace each in the same directory and stomp one
        // another's files mid-edit. The counter, not the clock, is what
        // makes the name unique (the clock's tick is coarser than a test).
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "memnoth-lsp-apply-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(base.join("src")).unwrap();
        let root = base.canonicalize().unwrap();
        (root, base)
    }

    #[test]
    fn several_edits_in_one_document_apply_backwards() {
        let text = "let a = 1;\nlet b = 2;\n";
        let edits = vec![
            TextEdit {
                range: range(0, 8, 9),
                new_text: "one".into(),
            },
            TextEdit {
                range: range(1, 8, 9),
                new_text: "two".into(),
            },
        ];
        assert_eq!(
            apply_to_text(text, &edits).unwrap(),
            "let a = one;\nlet b = two;\n"
        );
    }

    #[test]
    fn overlapping_edits_are_refused_rather_than_ordered() {
        let text = "alpha beta\n";
        let edits = vec![
            TextEdit {
                range: range(0, 0, 5),
                new_text: "x".into(),
            },
            TextEdit {
                range: range(0, 3, 9),
                new_text: "y".into(),
            },
        ];
        let err = apply_to_text(text, &edits).unwrap_err().to_string();
        assert!(err.contains("overlap"), "{err}");
    }

    #[test]
    fn a_workspace_edit_writes_each_named_file_once() {
        let (root, base) = workspace();
        std::fs::write(root.join("src/a.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "alpha();\n").unwrap();
        let edit = WorkspaceEdit {
            changes: Some(vec![
                (
                    format!(
                        "file://{}",
                        root.join("src/a.rs")
                            .display()
                            .to_string()
                            .replace('\\', "/")
                    ),
                    vec![TextEdit {
                        range: range(0, 3, 8),
                        new_text: "beta".into(),
                    }],
                ),
                (
                    format!(
                        "file://{}",
                        root.join("src/b.rs")
                            .display()
                            .to_string()
                            .replace('\\', "/")
                    ),
                    vec![TextEdit {
                        range: range(0, 0, 5),
                        new_text: "beta".into(),
                    }],
                ),
            ]),
        };
        let written = apply_workspace_edit(&root, &edit).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(
            std::fs::read_to_string(root.join("src/a.rs")).unwrap(),
            "fn beta() {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("src/b.rs")).unwrap(),
            "beta();\n"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn an_edit_outside_the_workspace_is_refused() {
        let (root, base) = workspace();
        let outside = std::env::temp_dir().join(format!("memnoth-outside-{}", std::process::id()));
        std::fs::write(&outside, "x\n").unwrap();
        let edit = WorkspaceEdit {
            changes: Some(vec![(
                format!(
                    "file://{}",
                    outside.display().to_string().replace('\\', "/")
                ),
                vec![TextEdit {
                    range: range(0, 0, 1),
                    new_text: "y".into(),
                }],
            )]),
        };
        let err = apply_workspace_edit(&root, &edit).unwrap_err().to_string();
        assert!(err.contains("outside the workspace"), "{err}");
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(base);
    }
}
