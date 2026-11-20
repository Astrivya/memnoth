//! Pure staged multi-file diff model for pre-write review.
//!
//! This module performs no I/O. It models *what a person has decided* about a
//! proposed patch and never mutates the workspace. [`StagedDiff::to_edits`]
//! returns only the hunks the user accepted; rejected and pending hunks never
//! become edit requests, so rejecting leaves workspace bytes untouched by
//! construction. A real apply path — one that revalidates the captured base
//! revision immediately before writing — is deliberately out of scope here.
//!
//! This file is declared as a child of `overlays::diff` via a `#[path]`
//! attribute in `diff.rs`. It is intentionally not added to `overlays/mod.rs`.

/// A 1-based line span, as written on the left or right side of a `@@` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// First line of the span (1-based). `0` is treated as the first line.
    pub start: usize,
    /// Number of lines in the span. Zero is an insertion point.
    pub count: usize,
}

impl Span {
    pub fn new(start: usize, count: usize) -> Self {
        Self { start, count }
    }
}

/// The decision a person made for one hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HunkDecision {
    /// No decision yet. The default for every newly staged hunk.
    #[default]
    Pending,
    /// Include this hunk in the emitted edit requests.
    Accept,
    /// Exclude this hunk; its bytes stay as they are in the base revision.
    Reject,
}

/// One line of a staged hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedLine {
    Context(String),
    Removed(String),
    Added(String),
}

impl StagedLine {
    /// The line text without its diff prefix.
    pub fn text(&self) -> &str {
        match self {
            StagedLine::Context(s) | StagedLine::Removed(s) | StagedLine::Added(s) => s,
        }
    }
}

/// One hunk staged for review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedHunk {
    /// The verbatim `@@ ... @@` header, for display.
    pub header: String,
    /// Exact changed lines in the captured base that this hunk replaces.
    /// Context lines are not part of the span.
    pub before: Span,
    /// Exact changed lines in the proposed revision that replace `before`.
    pub after: Span,
    /// The hunk's lines in unified-diff order, for display and for deriving
    /// the replacement text.
    pub lines: Vec<StagedLine>,
    /// The person's decision. Always [`HunkDecision::Pending`] on staging.
    pub decision: HunkDecision,
}

impl StagedHunk {
    pub fn new(header: String, before: Span, after: Span, lines: Vec<StagedLine>) -> Self {
        Self {
            header,
            before,
            after,
            lines,
            decision: HunkDecision::Pending,
        }
    }

    /// The '+' lines of this hunk: the exact bytes that would replace `before`.
    pub fn added_lines(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter_map(|line| match line {
                StagedLine::Added(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn is_pending(&self) -> bool {
        self.decision == HunkDecision::Pending
    }
}

/// A file with its staged hunks and the base revision they were computed against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    /// Workspace-relative path as shown to the person.
    pub path: String,
    /// Captured base revision. A future apply path must compare this to the
    /// on-disk bytes immediately before writing; the model never writes.
    pub base: String,
    /// Hunks in encounter order. All start pending.
    pub hunks: Vec<StagedHunk>,
}

/// An in-memory collection of proposed file changes awaiting review.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StagedDiff {
    pub files: Vec<StagedFile>,
}

/// A concrete, single-file edit request derived from *accepted* hunks only.
///
/// This is data for a future apply path, not an executed change. `base_revision`
/// is what an apply path must revalidate before it is allowed to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRequest {
    pub path: String,
    pub base_revision: String,
    /// Accepted hunks in file order. Rejected and pending hunks are absent.
    pub hunks: Vec<AcceptedEdit>,
}

/// One accepted hunk, reduced to what a writer would need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedEdit {
    pub before: Span,
    pub after: Span,
    /// The added lines of the hunk, in order.
    pub replacement: Vec<String>,
}

/// Why an in-memory projection could not be computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionError {
    /// A span fell outside the base revision.
    OutOfBounds,
    /// Two accepted spans overlap, so their order is ambiguous.
    Overlap,
}

impl StagedDiff {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.files.iter().all(|file| file.hunks.is_empty())
    }

    pub fn hunk_count(&self) -> usize {
        self.files.iter().map(|file| file.hunks.len()).sum()
    }

    /// Stage a file with an optionally captured base revision. Returns its index.
    pub fn stage_file(&mut self, path: impl Into<String>, base: impl Into<String>) -> usize {
        self.files.push(StagedFile {
            path: path.into(),
            base: base.into(),
            hunks: Vec::new(),
        });
        self.files.len() - 1
    }

    /// Finish staging a file by adding one pending hunk to it.
    pub fn add_hunk(
        &mut self,
        file_index: usize,
        header: String,
        before: Span,
        after: Span,
        lines: Vec<StagedLine>,
    ) -> bool {
        match self.files.get_mut(file_index) {
            Some(file) => {
                file.hunks
                    .push(StagedHunk::new(header, before, after, lines));
                true
            }
            None => false,
        }
    }

    /// Stable `(file index, hunk index)` pairs, in encounter order.
    pub fn hunk_refs(&self) -> Vec<(usize, usize)> {
        let mut refs = Vec::new();
        for (fi, file) in self.files.iter().enumerate() {
            for hi in 0..file.hunks.len() {
                refs.push((fi, hi));
            }
        }
        refs
    }

    /// Look up a hunk by its position in [`StagedDiff::hunk_refs`].
    pub fn flat_hunk(&self, flat: usize) -> Option<(&StagedFile, &StagedHunk)> {
        self.hunk_refs().get(flat).and_then(|&(fi, hi)| {
            self.files
                .get(fi)
                .and_then(|f| f.hunks.get(hi).map(|h| (f, h)))
        })
    }

    /// Set the decision of a hunk by its flat position. Returns false if absent.
    pub fn decide_flat(&mut self, flat: usize, decision: HunkDecision) -> bool {
        let mut index = 0;
        for file in &mut self.files {
            for hunk in &mut file.hunks {
                if index == flat {
                    hunk.decision = decision;
                    return true;
                }
                index += 1;
            }
        }
        false
    }

    /// Set every pending hunk to `decision`. Returns how many changed.
    pub fn decide_all_pending(&mut self, decision: HunkDecision) -> usize {
        let mut changed = 0;
        for file in &mut self.files {
            for hunk in &mut file.hunks {
                if hunk.decision == HunkDecision::Pending {
                    hunk.decision = decision;
                    changed += 1;
                }
            }
        }
        changed
    }

    pub fn count_decided(&self, decision: HunkDecision) -> usize {
        self.files
            .iter()
            .flat_map(|file| file.hunks.iter())
            .filter(|hunk| hunk.decision == decision)
            .count()
    }

    /// Build edit requests from accepted hunks only.
    ///
    /// This never writes. A file with no accepted hunks is omitted, so rejecting
    /// every hunk yields an empty list and therefore no edits at all.
    pub fn to_edits(&self) -> Vec<EditRequest> {
        let mut requests = Vec::new();
        for file in &self.files {
            let hunks: Vec<AcceptedEdit> = file
                .hunks
                .iter()
                .filter(|hunk| hunk.decision == HunkDecision::Accept)
                .map(|hunk| AcceptedEdit {
                    before: hunk.before,
                    after: hunk.after,
                    replacement: hunk.added_lines(),
                })
                .collect();
            if !hunks.is_empty() {
                requests.push(EditRequest {
                    path: file.path.clone(),
                    base_revision: file.base.clone(),
                    hunks,
                });
            }
        }
        requests
    }

    /// Best-effort parse of a unified diff (one or more files) into pending
    /// hunks. Malformed or empty input yields a smaller or empty model; it never
    /// panics. No base bytes are available from a diff alone, so callers that
    /// need revalidation must attach them with [`StagedDiff::stage_file`] or by
    /// setting `StagedFile::base` directly.
    pub fn from_unified_diff(diff: &str) -> Self {
        let lines: Vec<&str> = diff.lines().collect();
        let mut files: Vec<StagedFile> = Vec::new();
        let mut i = 0usize;

        while i < lines.len() {
            let line = lines[i];

            if let Some(old_path) = old_file_path(line) {
                let mut path = old_path;
                if i + 1 < lines.len()
                    && let Some(new_path) = new_file_path(lines[i + 1])
                {
                    path = new_path;
                    i += 1;
                }
                files.push(StagedFile {
                    path,
                    base: String::new(),
                    hunks: Vec::new(),
                });
                i += 1;
                continue;
            }

            if line.starts_with("@@") {
                let header = line.to_string();
                let (old, new) = parse_ranges(line);
                let mut hunk_lines = Vec::new();
                i += 1;
                while i < lines.len()
                    && !lines[i].starts_with("@@")
                    && old_file_path(lines[i]).is_none()
                    && !lines[i].starts_with("diff --git")
                {
                    let body = lines[i];
                    if let Some(rest) = body.strip_prefix('+') {
                        hunk_lines.push(StagedLine::Added(rest.to_string()));
                    } else if let Some(rest) = body.strip_prefix('-') {
                        hunk_lines.push(StagedLine::Removed(rest.to_string()));
                    } else if let Some(rest) = body.strip_prefix(' ') {
                        hunk_lines.push(StagedLine::Context(rest.to_string()));
                    } else if body.starts_with('\\') {
                        // "\ No newline at end of file" carries no diff line.
                    } else if !body.is_empty() {
                        // A malformed line is ignored, never fatal.
                    }
                    i += 1;
                }

                let (before, after) = precise_spans(old, new, &hunk_lines);
                if files.is_empty() {
                    files.push(StagedFile {
                        path: "unknown".to_string(),
                        base: String::new(),
                        hunks: Vec::new(),
                    });
                }
                if let Some(file) = files.last_mut() {
                    file.hunks
                        .push(StagedHunk::new(header, before, after, hunk_lines));
                }
                continue;
            }

            i += 1;
        }

        Self { files }
    }
}

/// Pure, in-memory projection of accepted edits onto a captured base.
///
/// This touches no filesystem; it is a preview/verification helper only. A real
/// apply path must still revalidate the base immediately before any write.
pub fn project_accepted(base: &str, hunks: &[AcceptedEdit]) -> Result<String, ProjectionError> {
    if hunks.is_empty() {
        return Ok(base.to_string());
    }

    let lines: Vec<&str> = base.split('\n').collect();
    let mut edits: Vec<(usize, usize, &Vec<String>)> = hunks
        .iter()
        .map(|hunk| {
            (
                hunk.before.start.saturating_sub(1),
                hunk.before.count,
                &hunk.replacement,
            )
        })
        .collect();
    edits.sort_by_key(|edit| edit.0);

    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    for (first, count, replacement) in edits {
        let end = first
            .checked_add(count)
            .ok_or(ProjectionError::OutOfBounds)?;
        if first < cursor {
            return Err(ProjectionError::Overlap);
        }
        if first > lines.len() || end > lines.len() {
            return Err(ProjectionError::OutOfBounds);
        }
        for line in &lines[cursor..first] {
            out.push((*line).to_string());
        }
        for line in replacement {
            out.push(line.clone());
        }
        cursor = end;
    }
    for line in &lines[cursor..] {
        out.push((*line).to_string());
    }
    Ok(out.join("\n"))
}

/// Strip an `a/` prefix from a `--- a/path` header. A bare `---` separator is
/// not a file header.
fn old_file_path(line: &str) -> Option<String> {
    if line == "---" || !line.starts_with("--- ") {
        return None;
    }
    let raw = line[4..].trim();
    if raw.is_empty() {
        return None;
    }
    Some(normalize_header_path(raw))
}

fn new_file_path(line: &str) -> Option<String> {
    if line == "+++" || !line.starts_with("+++ ") {
        return None;
    }
    let raw = line[4..].trim();
    if raw.is_empty() || raw == "/dev/null" {
        return None;
    }
    Some(normalize_header_path(raw))
}

fn normalize_header_path(raw: &str) -> String {
    let raw = raw.split('\t').next().unwrap_or(raw).trim();
    raw.strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw)
        .to_string()
}

/// Parse the two ranges of a `@@ -a,b +c,d @@` header, with forgiving defaults.
fn parse_ranges(header: &str) -> ((usize, usize), (usize, usize)) {
    let body = header
        .strip_prefix("@@")
        .and_then(|rest| rest.strip_suffix("@@"))
        .unwrap_or(header);

    let mut old = (0usize, 0usize);
    let mut new = (0usize, 0usize);
    for token in body.split_whitespace() {
        if let Some(rest) = token.strip_prefix('-') {
            old = parse_range(rest);
        } else if let Some(rest) = token.strip_prefix('+') {
            new = parse_range(rest);
        }
    }
    (old, new)
}

/// Parse `start,count` or just `start` (count defaults to 1).
fn parse_range(raw: &str) -> (usize, usize) {
    let clean = raw
        .split(|c: char| !c.is_ascii_digit() && c != ',')
        .next()
        .unwrap_or("");
    match clean.split_once(',') {
        Some((start, count)) => (start.parse().unwrap_or(0), count.parse().unwrap_or(1)),
        None => (clean.parse().unwrap_or(0), 1),
    }
}

/// Compute the precise changed region from a hunk's lines. Leading context
/// lines shift both sides; the spans themselves cover only the changed lines.
fn precise_spans(old: (usize, usize), new: (usize, usize), lines: &[StagedLine]) -> (Span, Span) {
    let leading_context = lines
        .iter()
        .take_while(|line| matches!(line, StagedLine::Context(_)))
        .count();
    let removed = lines
        .iter()
        .filter(|line| matches!(line, StagedLine::Removed(_)))
        .count();
    let added = lines
        .iter()
        .filter(|line| matches!(line, StagedLine::Added(_)))
        .count();

    let before_start = if old.1 == 0 {
        old.0.saturating_add(1)
    } else {
        old.0.saturating_add(leading_context).max(1)
    };
    let after_start = if new.1 == 0 {
        new.0.saturating_add(1)
    } else {
        new.0.saturating_add(leading_context).max(1)
    };

    (
        Span::new(before_start, removed),
        Span::new(after_start, added),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(
        header: &str,
        before: (usize, usize),
        after: (usize, usize),
        lines: Vec<StagedLine>,
    ) -> StagedHunk {
        StagedHunk::new(
            header.to_string(),
            Span::new(before.0, before.1),
            Span::new(after.0, after.1),
            lines,
        )
    }

    fn line(s: &str) -> StagedLine {
        StagedLine::Added(s.to_string())
    }

    #[test]
    fn construction_stages_pending_and_emits_no_edits() {
        // No auto-apply: a freshly staged diff has only pending hunks and
        // produces no edit requests until someone accepts something.
        let mut staged = StagedDiff::new();
        let fi = staged.stage_file("src/a.rs", "a\nb\n");
        assert!(staged.add_hunk(
            fi,
            "@@ -1,1 +1,1 @@".to_string(),
            Span::new(1, 1),
            Span::new(1, 1),
            vec![StagedLine::Removed("a".into()), line("A")],
        ));

        assert_eq!(staged.hunk_count(), 1);
        assert_eq!(staged.count_decided(HunkDecision::Pending), 1);
        assert_eq!(staged.count_decided(HunkDecision::Accept), 0);
        assert!(staged.to_edits().is_empty());
    }

    #[test]
    fn multi_file_diff_stages_every_file_and_hunk() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,3 @@
 context
-old
+new
 tail
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -10,2 +10,3 @@
 foo
+bar
 baz
";
        let staged = StagedDiff::from_unified_diff(diff);
        assert_eq!(staged.files.len(), 2);
        assert_eq!(staged.files[0].path, "src/a.rs");
        assert_eq!(staged.files[1].path, "src/b.rs");
        assert_eq!(staged.hunk_count(), 2);
        assert_eq!(staged.count_decided(HunkDecision::Pending), 2);
        // Context lines are excluded from the precise change span.
        assert_eq!(staged.files[0].hunks[0].before, Span::new(2, 1));
        assert_eq!(staged.files[0].hunks[0].after, Span::new(2, 1));
        // Staging alone never produces edits.
        assert!(staged.to_edits().is_empty());
    }

    #[test]
    fn per_hunk_accept_and_reject() {
        let mut staged = StagedDiff::new();
        let fi = staged.stage_file("f", "x\ny\n");
        staged.files[fi].hunks = vec![
            hunk(
                "@@ -1,1 +1,1 @@",
                (1, 1),
                (1, 1),
                vec![StagedLine::Removed("x".into()), line("X")],
            ),
            hunk(
                "@@ -2,1 +2,1 @@",
                (2, 1),
                (2, 1),
                vec![StagedLine::Removed("y".into()), line("Y")],
            ),
        ];

        assert!(staged.decide_flat(0, HunkDecision::Accept));
        assert!(staged.decide_flat(1, HunkDecision::Reject));
        assert_eq!(staged.files[fi].hunks[0].decision, HunkDecision::Accept);
        assert_eq!(staged.files[fi].hunks[1].decision, HunkDecision::Reject);
        assert_eq!(staged.count_decided(HunkDecision::Accept), 1);
        assert_eq!(staged.count_decided(HunkDecision::Reject), 1);

        let edits = staged.to_edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].hunks.len(), 1);
        assert_eq!(edits[0].hunks[0].replacement, vec!["X"]);
    }

    #[test]
    fn rejected_replacement_bytes_are_absent_from_edits() {
        let mut staged = StagedDiff::new();
        let fi = staged.stage_file("f", "a\nb\n");
        staged.files[fi].hunks = vec![
            hunk(
                "h1",
                (1, 1),
                (1, 1),
                vec![StagedLine::Removed("a".into()), line("ACCEPTED_ONE")],
            ),
            hunk(
                "h2",
                (2, 1),
                (2, 1),
                vec![StagedLine::Removed("b".into()), line("REJECTED_MARKER")],
            ),
        ];
        staged.decide_flat(0, HunkDecision::Accept);
        staged.decide_flat(1, HunkDecision::Reject);

        let edits = staged.to_edits();
        let emitted: Vec<String> = edits
            .iter()
            .flat_map(|request| request.hunks.iter())
            .flat_map(|h| h.replacement.iter().cloned())
            .collect();
        assert!(emitted.iter().any(|s| s == "ACCEPTED_ONE"));
        assert!(!emitted.iter().any(|s| s == "REJECTED_MARKER"));
        // The rejected hunk's original bytes are still present in the captured
        // base that an apply path would revalidate against.
        assert!(edits[0].base_revision.contains('b'));
    }

    #[test]
    fn partial_accept_preserves_only_the_accepted_hunk() {
        let mut staged = StagedDiff::new();
        let fi = staged.stage_file("f", "one\ntwo\nthree\n");
        staged.files[fi].hunks = vec![
            hunk(
                "h1",
                (1, 1),
                (1, 1),
                vec![StagedLine::Removed("one".into()), line("ONE")],
            ),
            hunk(
                "h2",
                (2, 1),
                (2, 1),
                vec![StagedLine::Removed("two".into()), line("TWO")],
            ),
            hunk(
                "h3",
                (3, 1),
                (3, 1),
                vec![StagedLine::Removed("three".into()), line("THREE")],
            ),
        ];
        staged.decide_flat(1, HunkDecision::Accept);

        let edits = staged.to_edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].hunks.len(), 1);
        assert_eq!(edits[0].hunks[0].before, Span::new(2, 1));
        assert_eq!(edits[0].hunks[0].replacement, vec!["TWO"]);

        let after = project_accepted(&edits[0].base_revision, &edits[0].hunks).unwrap();
        assert_eq!(after, "one\nTWO\nthree\n");
    }

    #[test]
    fn projection_keeps_rejected_regions_byte_identical() {
        let mut staged = StagedDiff::new();
        let fi = staged.stage_file("f", "a\nb\nc\nd");
        staged.files[fi].hunks = vec![
            hunk(
                "change b",
                (2, 1),
                (2, 1),
                vec![StagedLine::Removed("b".into()), line("B")],
            ),
            hunk(
                "change d",
                (4, 1),
                (4, 1),
                vec![StagedLine::Removed("d".into()), line("D")],
            ),
        ];
        staged.decide_flat(0, HunkDecision::Accept);
        staged.decide_flat(1, HunkDecision::Reject);

        let edits = staged.to_edits();
        let after = project_accepted(&edits[0].base_revision, &edits[0].hunks).unwrap();
        // Accepted line changed; rejected line and the missing trailing newline
        // are preserved exactly.
        assert_eq!(after, "a\nB\nc\nd");
    }

    #[test]
    fn accepting_or_rejecting_everything_is_explicit() {
        let mut staged = StagedDiff::new();
        let fi = staged.stage_file("f", "x\n");
        staged.files[fi].hunks = vec![hunk(
            "h1",
            (1, 1),
            (1, 1),
            vec![StagedLine::Removed("x".into()), line("X")],
        )];
        assert_eq!(staged.decide_all_pending(HunkDecision::Accept), 1);
        assert_eq!(staged.to_edits().len(), 1);
        // A second pass only touches still-pending hunks.
        assert_eq!(staged.decide_all_pending(HunkDecision::Reject), 0);
        staged.decide_flat(0, HunkDecision::Pending);
        assert_eq!(staged.decide_all_pending(HunkDecision::Reject), 1);
        assert!(staged.to_edits().is_empty());
    }

    #[test]
    fn empty_and_malformed_input_never_panics() {
        assert!(StagedDiff::from_unified_diff("").is_empty());
        assert!(StagedDiff::from_unified_diff("not a diff\n---\n+++ ???\n").is_empty());

        let staged = StagedDiff::from_unified_diff(
            "--- a/x\n+++ b/x\n@@ nonsense @@\n? weird\n\\ marker\n@@ -1 +1 @@\n-old\n+new\n",
        );
        // The malformed header produces forgiving defaults; the parse still
        // yields a model that can be rendered without panicking.
        assert!(staged.hunk_count() >= 1);
        assert!(staged.to_edits().is_empty());
    }

    #[test]
    fn projection_rejects_overlaps_and_out_of_bounds() {
        let base = "a\nb\n";
        let overlapping = vec![
            AcceptedEdit {
                before: Span::new(1, 2),
                after: Span::new(1, 2),
                replacement: vec!["X".into()],
            },
            AcceptedEdit {
                before: Span::new(2, 1),
                after: Span::new(2, 1),
                replacement: vec!["Y".into()],
            },
        ];
        assert_eq!(
            project_accepted(base, &overlapping),
            Err(ProjectionError::Overlap)
        );

        let out = vec![AcceptedEdit {
            before: Span::new(9, 1),
            after: Span::new(9, 1),
            replacement: vec!["Z".into()],
        }];
        assert_eq!(
            project_accepted(base, &out),
            Err(ProjectionError::OutOfBounds)
        );
    }

    #[test]
    fn insertion_hunk_keeps_surrounding_bytes() {
        let mut staged = StagedDiff::new();
        let fi = staged.stage_file("f", "a\nb\n");
        staged.files[fi].hunks = vec![hunk(
            "@@ -1,0 +2,1 @@",
            (2, 0),
            (2, 1),
            vec![line("inserted")],
        )];
        staged.decide_flat(0, HunkDecision::Accept);
        let edits = staged.to_edits();
        let after = project_accepted(&edits[0].base_revision, &edits[0].hunks).unwrap();
        assert_eq!(after, "a\ninserted\nb\n");
    }
}
