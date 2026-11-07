//! Reading and editing workspace files for the session's agent.
//!
//! A model that can change a file needs three things this module provides, in
//! this order:
//!
//! 1. **A path it cannot escape.** Every path is relative, is resolved under the
//!    workspace root, and is refused if any component is `..` or absolute. The
//!    parent is canonicalised before the check so a symlink cannot turn
//!    `src/link` into `/etc`.
//! 2. **A change it cannot misfire.** `edit_file` replaces one exact snippet
//!    that must occur exactly once. Zero matches means the model guessed the
//!    text and must read the file; two matches means the model has not said
//!    which one, and picking one is the change nobody reviewed.
//! 3. **A preview a person can read before it happens.** [`Preview`] carries both
//!    sides and a short diff; the TUI shows it and the write only happens after
//!    an explicit yes.
//!
//! The store also refuses to resize a file past [`MAX_FILE_BYTES`] and refuses
//! non-UTF-8, because a text edit it cannot show is not one a person can approve.
//! The state directories are off-limits: an agent editing `.memnoth/graph.db` or
//! `.git/` is not working on the code, it is damaging the tools that describe it.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Largest file the agent will read or write. The same bound the harness uses
/// for a file edit, stated once here.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;

/// Lines a `read_file` returns when no range is asked for.
pub const MAX_READ_LINES: usize = 2000;

/// Lines of diff shown in a confirmation, per side.
pub const PREVIEW_LINES: usize = 14;

/// A change that has been worked out but not written.
#[derive(Debug, Clone)]
pub struct Preview {
    /// Where the write will land, absolute.
    pub path: PathBuf,
    /// The canonical directory `path` was resolved in, re-checked at write time
    /// so a parent swapped for a symlink after the preview cannot redirect it.
    pub parent: PathBuf,
    /// The same path relative to the workspace, for the screen.
    pub display: String,
    /// The file before the change (empty when it is being created).
    pub before: String,
    /// The file after it.
    pub after: String,
    /// Whether the file does not exist yet.
    pub created: bool,
    /// Lines the replaced snippet had.
    pub removed: usize,
    /// Lines it now has.
    pub added: usize,
}

impl Preview {
    /// The text a confirmation box shows: what, where, and the change.
    ///
    /// Both sides, not a line count. "1 line(s) → 1" is a change nobody can
    /// judge, and a person who cannot judge a prompt learns to press `y`.
    pub fn confirmation(&self) -> String {
        let verb = if self.created { "create" } else { "edit" };
        let mut out = format!(
            "{verb} {} ({} line(s) → {})\n",
            self.display, self.removed, self.added
        );
        if self.created {
            for line in clip_lines(&self.after, PREVIEW_LINES) {
                out.push_str(&format!("+ {line}\n"));
            }
            return out;
        }
        out.push_str(&hunk(&self.before, &self.after, PREVIEW_LINES).text());
        out
    }
}

/// The part of a change a person needs to see.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hunk {
    context_before: Vec<String>,
    removed: Vec<String>,
    added: Vec<String>,
    context_after: Vec<String>,
    removed_elided: usize,
    added_elided: usize,
}

impl Hunk {
    /// Render as a diff: `  ` for context, `-` and `+` for the change.
    fn text(&self) -> String {
        let mut out = String::new();
        for line in &self.context_before {
            out.push_str(&format!("  {line}\n"));
        }
        for line in &self.removed {
            out.push_str(&format!("- {line}\n"));
        }
        if self.removed_elided > 0 {
            out.push_str(&format!(
                "… ({} more line(s) removed)\n",
                self.removed_elided
            ));
        }
        for line in &self.added {
            out.push_str(&format!("+ {line}\n"));
        }
        if self.added_elided > 0 {
            out.push_str(&format!("… ({} more line(s) added)\n", self.added_elided));
        }
        for line in &self.context_after {
            out.push_str(&format!("  {line}\n"));
        }
        out
    }
}

/// The differing block of two texts, with a little context on each side.
///
/// A common prefix and suffix are trimmed first, so a one-line change in a
/// thousand-line file does not turn the confirmation into the file. Each side is
/// clipped to `limit` lines, and the remainder is counted rather than hidden.
fn hunk(before: &str, after: &str, limit: usize) -> Hunk {
    const CONTEXT: usize = 2;
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();

    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let start = prefix.saturating_sub(CONTEXT);
    let context_before = old[start..prefix]
        .iter()
        .take(CONTEXT)
        .map(|line| line.to_string())
        .collect();

    let old_change: Vec<String> = old[prefix..old.len() - suffix]
        .iter()
        .map(|line| line.to_string())
        .collect();
    let new_change: Vec<String> = new[prefix..new.len() - suffix]
        .iter()
        .map(|line| line.to_string())
        .collect();

    let context_after = old[old.len() - suffix..(old.len() - suffix + CONTEXT).min(old.len())]
        .iter()
        .map(|line| line.to_string())
        .collect();

    let (removed, removed_elided) = clip(old_change, limit);
    let (added, added_elided) = clip(new_change, limit);

    Hunk {
        context_before,
        removed,
        added,
        context_after,
        removed_elided,
        added_elided,
    }
}

fn clip(lines: Vec<String>, limit: usize) -> (Vec<String>, usize) {
    if lines.len() <= limit {
        return (lines, 0);
    }
    let elided = lines.len() - limit;
    (lines.into_iter().take(limit).collect(), elided)
}

fn clip_lines(text: &str, limit: usize) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().take(limit).map(str::to_string).collect();
    if text.lines().count() > limit {
        lines.push("…".to_string());
    }
    lines
}

/// Resolve a model-supplied path under `root`, or refuse it by name.
pub fn resolve(root: &Path, relative: &str) -> Result<PathBuf> {
    let trimmed = relative.trim();
    if trimmed.is_empty() {
        bail!("the path is empty");
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        bail!("{trimmed} is absolute; give a path relative to the workspace");
    }
    for component in candidate.components() {
        match component {
            Component::ParentDir => {
                bail!("{trimmed} contains `..`; the agent stays inside the workspace")
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("{trimmed} is not a relative path")
            }
            _ => {}
        }
    }
    if let Some(first) = candidate.components().next()
        && let Component::Normal(name) = first
        && (name == ".git" || name == ".memnoth")
    {
        bail!("{trimmed} is inside tool state; the agent does not edit it");
    }

    let path = root.join(candidate);
    // Canonicalise the parent, not the file: the file may be about to be
    // created. If the parent is a symlink out of the workspace, this is where
    // that is caught rather than after the write.
    let parent = path.parent().unwrap_or(root);
    let parent = parent
        .canonicalize()
        .with_context(|| format!("{} does not exist", parent.display()))?;
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("workspace {} is unreadable", root.display()))?;
    if !parent.starts_with(&canonical_root) {
        bail!(
            "{trimmed} resolves outside the workspace ({}), so it is refused",
            parent.display()
        );
    }
    let name = path
        .file_name()
        .with_context(|| format!("{trimmed} does not name a file"))?;
    let resolved = parent.join(name);

    // Canonicalising the parent is not enough on its own: `src/link` may be a
    // symlink *file* that points at `/etc/passwd`, and a read would follow it.
    // The target is checked too, so reading and writing agree that the path
    // lands inside the workspace.
    if resolved.exists() {
        let target = resolved
            .canonicalize()
            .with_context(|| format!("{trimmed} cannot be resolved"))?;
        if !target.starts_with(&canonical_root) {
            bail!(
                "{trimmed} resolves outside the workspace ({}), so it is refused",
                target.display()
            );
        }
    }
    Ok(resolved)
}

/// Read a file for the model, optionally a line range.
///
/// Raw text, not numbered lines: an edit is a byte-exact replacement, and line
/// numbers in the output would be copied into `old` and never match.
pub fn read(
    root: &Path,
    relative: &str,
    start: Option<usize>,
    end: Option<usize>,
) -> Result<String> {
    let path = resolve(root, relative)?;
    let text = read_utf8(&path)?;
    let total = text.lines().count();

    if start.is_none() && end.is_none() {
        return Ok(text);
    }

    let first = start.unwrap_or(1).max(1);
    let last = end.unwrap_or(total).max(first).min(total);
    if first > total {
        bail!("{relative} has {total} line(s); line {first} does not exist");
    }
    let body: Vec<&str> = text
        .lines()
        .skip(first - 1)
        .take(last - first + 1)
        .collect();
    let mut out = body.join("\n");
    if last < total {
        out.push_str(&format!("\n… (lines {first}–{last} of {total})"));
    }
    Ok(out)
}

/// Work out an edit without writing it.
///
/// `old` must occur exactly once. An empty `old` creates a new file and is
/// refused when the file already exists: the difference between "create" and
/// "replace this file entirely" is too large to infer from an empty string.
pub fn preview_edit(root: &Path, relative: &str, old: &str, new: &str) -> Result<Preview> {
    let path = resolve(root, relative)?;
    let parent = path
        .parent()
        .map(Path::to_path_buf)
        .with_context(|| format!("{relative} does not name a file in a directory"))?;

    if new.len() as u64 > MAX_FILE_BYTES {
        bail!("the new content is larger than {MAX_FILE_BYTES} bytes");
    }

    let exists = path.exists();
    if old.is_empty() {
        if exists {
            bail!("{relative} already exists; give the text to replace as `old`");
        }
        if new.is_empty() {
            bail!("refusing to create an empty file; give content in `new`");
        }
        return Ok(Preview {
            path,
            parent,
            display: relative.trim().to_string(),
            before: String::new(),
            after: new.to_string(),
            created: true,
            removed: 0,
            added: new.lines().count(),
        });
    }

    if !exists {
        bail!("{relative} does not exist; read the file or create it with an empty `old`");
    }
    let before = read_utf8(&path)?;

    let matches = before.matches(old).count();
    match matches {
        0 => bail!(
            "the text in `old` does not appear in {relative}; read the file and copy the exact snippet"
        ),
        1 => {}
        many => bail!(
            "the text in `old` appears {many} times in {relative}; include enough surrounding text to make it unique"
        ),
    }

    let after = before.replacen(old, new, 1);
    if after == before {
        bail!("`new` is identical to `old`, so there is nothing to change");
    }
    if after.len() as u64 > MAX_FILE_BYTES {
        bail!("the result would be larger than {MAX_FILE_BYTES} bytes");
    }

    Ok(Preview {
        path,
        parent,
        display: relative.trim().to_string(),
        before,
        after,
        created: false,
        removed: old.lines().count().max(1),
        added: new.lines().count(),
    })
}

/// Write a previewed change. The caller must have confirmed it first.
///
/// The confirmation is the last word, so what is written has to still be what
/// was confirmed. Three things are re-checked here rather than trusted from the
/// preview, because a prompt gives another process time to move things:
///
/// 1. the directory still resolves inside the workspace — a parent swapped for a
///    symlink after the preview would otherwise redirect the write;
/// 2. the target is not a symlink — writing through one leaves the workspace;
/// 3. the file still holds the text the preview was computed from — a second
///    writer would otherwise be silently overwritten by a change that was
///    approved against the old file.
pub fn apply(preview: &Preview) -> Result<String> {
    let parent = preview
        .path
        .parent()
        .with_context(|| format!("{} has no directory", preview.path.display()))?
        .canonicalize()
        .with_context(|| format!("{} does not exist", preview.path.display()))?;
    if parent != preview.parent {
        bail!(
            "{} moved while the change was being confirmed; nothing was written",
            preview.display
        );
    }
    if preview.path.exists()
        && std::fs::symlink_metadata(&preview.path)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false)
    {
        bail!(
            "{} is a symlink, and a write through it would land outside the workspace; \
             nothing was written",
            preview.display
        );
    }
    if !preview.created {
        let current = read_utf8(&preview.path)?;
        if current != preview.before {
            bail!(
                "{} changed while the change was being confirmed; nothing was written",
                preview.display
            );
        }
    }

    std::fs::write(&preview.path, &preview.after)
        .with_context(|| format!("cannot write {}", preview.path.display()))?;
    if preview.created {
        Ok(format!(
            "created {} ({} lines). The code graph is stale for this path until `memnoth kg index`.",
            preview.display,
            preview.after.lines().count()
        ))
    } else {
        Ok(format!(
            "edited {} ({} lines → {}). The code graph is stale for this path until `memnoth kg index`.",
            preview.display, preview.removed, preview.added
        ))
    }
}

fn read_utf8(path: &Path) -> Result<String> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a file", path.display());
    }
    if meta.len() > MAX_FILE_BYTES {
        bail!(
            "{} is {} bytes; the agent edits files up to {MAX_FILE_BYTES}",
            path.display(),
            meta.len()
        );
    }
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    String::from_utf8(bytes).with_context(|| {
        format!(
            "{} is not UTF-8 text, so it cannot be edited here",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        (root, dir)
    }

    #[test]
    fn an_edit_replaces_one_unique_snippet_and_keeps_the_rest_verbatim() {
        let (root, _guard) = workspace();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn a() {}\r\nfn b() {}\r\n").unwrap();

        let preview = preview_edit(&root, "src/a.rs", "fn b() {}", "fn b() { todo!() }").unwrap();
        assert!(!preview.created);
        assert_eq!(preview.removed, 1);
        assert_eq!(preview.added, 1);
        assert!(preview.confirmation().contains("edit src/a.rs"));

        let message = apply(&preview).unwrap();
        assert!(message.contains("kg index"), "a stale graph must be named");
        let written = std::fs::read_to_string(root.join("src/a.rs")).unwrap();
        assert_eq!(written, "fn a() {}\r\nfn b() { todo!() }\r\n");
    }

    #[test]
    fn the_confirmation_shows_both_sides_of_the_change() {
        let (root, _guard) = workspace();
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();

        let preview = preview_edit(&root, "a.txt", "two", "TWO").unwrap();
        let box_text = preview.confirmation();
        assert!(box_text.contains("- two"), "{box_text}");
        assert!(box_text.contains("+ TWO"), "{box_text}");
        // Context, so a person can see where the change lands.
        assert!(box_text.contains("  one"), "{box_text}");
        assert!(box_text.contains("  three"), "{box_text}");
    }

    #[test]
    fn a_long_change_is_clipped_and_says_how_much_it_left_out() {
        let before = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let after = (0..40)
            .map(|i| format!("changed {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let hunk = hunk(&before, &after, PREVIEW_LINES);
        assert_eq!(hunk.removed.len(), PREVIEW_LINES);
        assert_eq!(hunk.removed_elided, 40 - PREVIEW_LINES);

        let text = hunk.text();
        assert!(text.contains("more line(s) removed"), "{text}");
        assert!(text.contains("more line(s) added"), "{text}");
    }

    #[test]
    fn a_file_that_changed_after_the_preview_is_not_overwritten() {
        let (root, _guard) = workspace();
        let path = root.join("a.txt");
        std::fs::write(&path, "before\n").unwrap();

        let preview = preview_edit(&root, "a.txt", "before", "after").unwrap();
        // Something else writes between the confirmation and the write.
        std::fs::write(&path, "someone else\n").unwrap();

        let error = apply(&preview).unwrap_err().to_string();
        assert!(
            error.contains("changed while the change was being confirmed"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "someone else\n");
    }

    #[test]
    fn an_ambiguous_snippet_is_refused_rather_than_guessed() {
        let (root, _guard) = workspace();
        std::fs::write(root.join("a.txt"), "same\nsame\n").unwrap();
        let error = preview_edit(&root, "a.txt", "same", "other")
            .unwrap_err()
            .to_string();
        assert!(error.contains("2 times"), "{error}");
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "same\nsame\n",
            "a refused preview must not touch the file"
        );
    }

    #[test]
    fn a_snippet_that_is_not_there_says_to_read_the_file_first() {
        let (root, _guard) = workspace();
        std::fs::write(root.join("a.txt"), "real text\n").unwrap();
        let error = preview_edit(&root, "a.txt", "imagined text", "x")
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not appear"), "{error}");
        assert!(error.contains("read the file"), "{error}");
    }

    #[test]
    fn an_empty_old_creates_a_file_but_never_overwrites_one() {
        let (root, _guard) = workspace();
        let preview = preview_edit(&root, "new.txt", "", "hello\n").unwrap();
        assert!(preview.created);
        assert!(preview.confirmation().starts_with("create new.txt"));
        apply(&preview).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("new.txt")).unwrap(),
            "hello\n"
        );

        let error = preview_edit(&root, "new.txt", "", "again")
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists"), "{error}");
    }

    #[test]
    fn a_path_that_leaves_the_workspace_is_refused() {
        let (root, _guard) = workspace();
        for bad in ["../outside.txt", "a/../../outside.txt", "/etc/passwd"] {
            let error = resolve(&root, bad).unwrap_err().to_string();
            assert!(
                error.contains("..") || error.contains("absolute") || error.contains("relative"),
                "{bad}: {error}"
            );
        }
        let error = resolve(&root, ".git/config").unwrap_err().to_string();
        assert!(error.contains("tool state"), "{error}");
        let error = resolve(&root, ".memnoth/graph.db").unwrap_err().to_string();
        assert!(error.contains("tool state"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_cannot_be_read_out_of_the_workspace() {
        // The parent canonicalises to the workspace, so only the target check
        // stands between the agent and a file that is not in the tree.
        let (root, _guard) = workspace();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "not yours\n").unwrap();
        std::os::unix::fs::symlink(&secret, root.join("link.txt")).unwrap();

        let error = read(&root, "link.txt", None, None).unwrap_err().to_string();
        assert!(error.contains("outside the workspace"), "{error}");
        assert!(resolve(&root, "link.txt").is_err());
    }

    #[test]
    fn reading_a_range_counts_lines_from_one_and_names_what_it_left_out() {
        let (root, _guard) = workspace();
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        assert_eq!(
            read(&root, "a.txt", None, None).unwrap(),
            "one\ntwo\nthree\nfour\n"
        );
        let ranged = read(&root, "a.txt", Some(2), Some(3)).unwrap();
        assert!(ranged.starts_with("two\nthree"));
        assert!(ranged.contains("lines 2–3 of 4"), "{ranged}");
        assert!(read(&root, "a.txt", Some(9), None).is_err());
    }

    #[test]
    fn a_binary_or_oversized_file_is_refused_instead_of_mangled() {
        let (root, _guard) = workspace();
        std::fs::write(root.join("blob.bin"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
        let error = read(&root, "blob.bin", None, None).unwrap_err().to_string();
        assert!(error.contains("UTF-8"), "{error}");

        std::fs::write(
            root.join("big.txt"),
            "x".repeat(MAX_FILE_BYTES as usize + 1),
        )
        .unwrap();
        let error = read(&root, "big.txt", None, None).unwrap_err().to_string();
        assert!(error.contains("bytes"), "{error}");
    }
}
