//! Exact lexical search over the working tree.
//!
//! Always in-process (never shells out): `regex` matches line by line,
//! `ignore` walks the tree, results sort by path for determinism. One design
//! rule above all: **exact matches are exact** — the same tree and query
//! produce the same matches in the same order, every time.

use crate::cli::GrepArgs;
use crate::structure::{StructureItem, extract_file_structure};
use crate::workspace::{SearchScope, collect_file_entries, read_text_file};
use regex::Regex;
use std::path::Path;

const OTHER_SYMBOLS_LIMIT: usize = 4;
const DENSE_MATCH_LIMITED_GROUPING_THRESHOLD: usize = 12;
const DENSE_MATCH_SKIP_STRUCTURE_THRESHOLD: usize = 24;
const DENSE_GROUPS_LIMIT: usize = 8;
const DENSE_OTHER_SYMBOLS_LIMIT: usize = 2;
const MAX_MATCH_LINE_CHARS: usize = 240;
const MATCH_LINE_PREFIX_CONTEXT_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepResult {
    pub query: String,
    pub regex: bool,
    pub root: String,
    pub files: Vec<FileMatches>,
    pub total_files: usize,
    pub total_matches: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatches {
    pub path: String,
    pub language: String,
    pub role: String,
    pub matches: Vec<LineMatch>,
    pub groups: Vec<MatchGroup>,
    pub total_symbols: usize,
    pub matched_symbol_count: usize,
    pub other_symbols: Vec<StructureItem>,
    pub other_symbols_omitted_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMatch {
    pub line_number: usize,
    pub line_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchGroup {
    pub kind: String,
    pub label: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub match_indices: Vec<usize>,
}

impl MatchGroup {
    pub fn resolved_matches<'a>(
        &'a self,
        matches: &'a [LineMatch],
    ) -> impl Iterator<Item = &'a LineMatch> + 'a {
        self.match_indices.iter().map(move |&idx| &matches[idx])
    }
}

pub fn run_grep(root: &Path, args: &GrepArgs) -> Result<GrepResult, String> {
    let search_root = match &args.path {
        Some(path) => root.join(path),
        None => root.to_path_buf(),
    };
    let scope = SearchScope {
        root: &search_root,
        file_type: args.file_type.as_deref(),
        glob: args.glob.as_deref(),
        hidden: args.hidden,
        no_ignore: args.no_ignore,
    };
    let matcher = Matcher::new(&args.query, args.regex)?;

    if args.paths_only {
        let mut paths: Vec<String> = collect_file_entries(&scope)
            .into_iter()
            .filter(|entry| file_has_match(entry, &matcher))
            .map(|entry| entry.relative_path)
            .collect();
        paths.sort();
        // Report paths relative to the caller's root, not the scoped subtree.
        let files = paths
            .into_iter()
            .map(|path| FileMatches {
                path,
                language: String::new(),
                role: String::new(),
                matches: Vec::new(),
                groups: Vec::new(),
                total_symbols: 0,
                matched_symbol_count: 0,
                other_symbols: Vec::new(),
                other_symbols_omitted_count: 0,
            })
            .collect::<Vec<_>>();
        return Ok(GrepResult {
            query: args.query.clone(),
            regex: args.regex,
            root: root.display().to_string(),
            total_files: files.len(),
            total_matches: 0,
            files,
        });
    }

    let entries = collect_file_entries(&scope);
    let worker_count = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(1)
        .min(entries.len().max(1));

    let mut results = Vec::new();
    let mut total_matches = 0;

    if worker_count <= 1 || entries.len() <= 8 {
        for entry in entries {
            if let Some(file_matches) = process_file(entry, &matcher) {
                total_matches += file_matches.matches.len();
                results.push(file_matches);
            }
        }
    } else {
        let chunk_size = entries.len().div_ceil(worker_count);
        let partials = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for chunk in entries.chunks(chunk_size) {
                let matcher = matcher.clone();
                handles.push(scope.spawn(move || {
                    let mut partial = Vec::new();
                    let mut partial_total = 0;
                    for entry in chunk.iter().cloned() {
                        if let Some(file_matches) = process_file(entry, &matcher) {
                            partial_total += file_matches.matches.len();
                            partial.push(file_matches);
                        }
                    }
                    (partial, partial_total)
                }));
            }

            // A worker that panics takes its own answers with it, and they are
            // files this search has already read. Resuming without them would
            // break the one contract this module is built on — *exact matches are
            // exact* — by returning fewer matches than the tree contains, which
            // no caller can tell from a tree that has fewer. So the search says
            // it failed instead; `String` is this function's error type.
            let mut partials = Vec::with_capacity(handles.len());
            for handle in handles {
                match handle.join() {
                    Ok(partial) => partials.push(partial),
                    Err(_) => {
                        return Err(
                            "a grep worker panicked; the matches it had found are lost, so the \
                             result would be incomplete"
                                .to_string(),
                        );
                    }
                }
            }
            Ok(partials)
        })?;

        for (mut partial, partial_total) in partials {
            total_matches += partial_total;
            results.append(&mut partial);
        }
    }

    results.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(GrepResult {
        query: args.query.clone(),
        regex: args.regex,
        root: root.display().to_string(),
        total_files: results.len(),
        total_matches,
        files: results,
    })
}

fn file_has_match(entry: &crate::workspace::FileEntry, matcher: &Matcher) -> bool {
    let Some(text) = read_text_file(&entry.path) else {
        return false;
    };
    text.lines().any(|line| matcher.is_match(line))
}

fn process_file(entry: crate::workspace::FileEntry, matcher: &Matcher) -> Option<FileMatches> {
    let text = read_text_file(&entry.path)?;

    let mut matches = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if matcher.is_match(line) {
            matches.push(LineMatch {
                line_number: idx + 1,
                line_text: matcher.display_line_text(line),
            });
        }
    }

    if matches.is_empty() {
        return None;
    }

    if matches.len() >= DENSE_MATCH_SKIP_STRUCTURE_THRESHOLD {
        return Some(build_dense_file_matches(
            entry.relative_path,
            &entry.path,
            matches,
        ));
    }

    let structure = extract_file_structure(&entry.path, &entry.relative_path, &text);
    let grouping = group_matches(&structure.items, &matches);

    Some(FileMatches {
        path: entry.relative_path,
        language: structure.language,
        role: structure.role,
        matches,
        groups: grouping.groups,
        total_symbols: structure.items.len(),
        matched_symbol_count: grouping.matched_symbol_count,
        other_symbols: grouping.other_symbols,
        other_symbols_omitted_count: grouping.other_symbols_omitted_count,
    })
}

fn build_dense_file_matches(
    path: String,
    absolute_path: &Path,
    matches: Vec<LineMatch>,
) -> FileMatches {
    FileMatches {
        language: infer_language(absolute_path),
        role: crate::structure::infer_role(&path),
        path,
        groups: vec![file_scope_group((0..matches.len()).collect())],
        matches,
        total_symbols: 0,
        matched_symbol_count: 0,
        other_symbols: Vec::new(),
        other_symbols_omitted_count: 0,
    }
}

fn infer_language(path: &Path) -> String {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
    {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "md" => "markdown",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        _ => "text",
    }
    .to_string()
}

#[derive(Clone)]
struct Matcher {
    kind: MatcherKind,
}

impl Matcher {
    fn new(query: &str, regex: bool) -> Result<Self, String> {
        let kind = if regex {
            MatcherKind::Regex(Regex::new(query).map_err(|err| {
                format!(
                    "invalid regex: {err}\n\nhint: omit regex=true (or escape special characters) to search for this text literally"
                )
            })?)
        } else {
            MatcherKind::Literal(query.to_string())
        };
        Ok(Self { kind })
    }

    fn is_match(&self, line: &str) -> bool {
        match &self.kind {
            MatcherKind::Literal(literal) => line.contains(literal),
            MatcherKind::Regex(regex) => regex.is_match(line),
        }
    }

    fn display_line_text(&self, line: &str) -> String {
        compact_match_line(line, self.first_match_span(line))
    }

    fn first_match_span(&self, line: &str) -> Option<(usize, usize)> {
        match &self.kind {
            MatcherKind::Literal(literal) => line
                .find(literal)
                .map(|start| (start, start.saturating_add(literal.len()))),
            MatcherKind::Regex(regex) => regex.find(line).map(|found| (found.start(), found.end())),
        }
    }
}

fn compact_match_line(line: &str, match_span: Option<(usize, usize)>) -> String {
    let char_count = line.chars().count();
    if char_count <= MAX_MATCH_LINE_CHARS {
        return line.to_string();
    }

    let (match_start, match_end) = match_span.unwrap_or((0, 0));
    let match_start_char = line[..match_start.min(line.len())].chars().count();
    let match_end_char = line[..match_end.min(line.len())].chars().count();
    let match_len_chars = match_end_char.saturating_sub(match_start_char).max(1);

    let start_char = match_start_char.saturating_sub(MATCH_LINE_PREFIX_CONTEXT_CHARS);
    let mut end_char = start_char
        .saturating_add(MAX_MATCH_LINE_CHARS)
        .max(match_start_char.saturating_add(match_len_chars));
    if end_char > char_count {
        end_char = char_count;
    }
    let start_char = end_char
        .saturating_sub(MAX_MATCH_LINE_CHARS)
        .min(start_char);

    let omitted_prefix = start_char;
    let omitted_suffix = char_count.saturating_sub(end_char);
    let snippet: String = line
        .chars()
        .skip(start_char)
        .take(end_char.saturating_sub(start_char))
        .collect();

    match (omitted_prefix > 0, omitted_suffix > 0) {
        (true, true) => format!(
            "…{} … [truncated: {} chars before, {} chars after]",
            snippet, omitted_prefix, omitted_suffix
        ),
        (true, false) => format!("…{} [truncated: {} chars before]", snippet, omitted_prefix),
        (false, true) => format!("{} … [truncated: {} chars after]", snippet, omitted_suffix),
        (false, false) => snippet,
    }
}

#[derive(Clone)]
enum MatcherKind {
    Literal(String),
    Regex(Regex),
}

struct GroupingResult {
    groups: Vec<MatchGroup>,
    matched_symbol_count: usize,
    other_symbols: Vec<StructureItem>,
    other_symbols_omitted_count: usize,
}

fn group_matches(items: &[StructureItem], matches: &[LineMatch]) -> GroupingResult {
    let options = if matches.len() >= DENSE_MATCH_LIMITED_GROUPING_THRESHOLD {
        GroupingOptions {
            max_groups: DENSE_GROUPS_LIMIT,
            other_symbols_limit: DENSE_OTHER_SYMBOLS_LIMIT,
        }
    } else {
        GroupingOptions {
            max_groups: usize::MAX,
            other_symbols_limit: OTHER_SYMBOLS_LIMIT,
        }
    };

    group_matches_with_options(items, matches, options)
}

struct GroupingOptions {
    max_groups: usize,
    other_symbols_limit: usize,
}

fn group_matches_with_options(
    items: &[StructureItem],
    matches: &[LineMatch],
    options: GroupingOptions,
) -> GroupingResult {
    let mut symbol_groups = Vec::new();
    let mut matched_indices = Vec::new();
    let mut file_scope_matches = Vec::new();
    let mut item_idx = 0usize;
    let mut last_grouped_item_idx = None;

    for (match_idx, line_match) in matches.iter().enumerate() {
        while item_idx < items.len() && items[item_idx].end_line < line_match.line_number {
            item_idx += 1;
        }

        if let Some(item) = items.get(item_idx)
            && item.start_line <= line_match.line_number
            && line_match.line_number <= item.end_line
        {
            if matched_indices.last().copied() != Some(item_idx) {
                matched_indices.push(item_idx);
                if symbol_groups.len() < options.max_groups {
                    symbol_groups.push(MatchGroup {
                        kind: item.kind.clone(),
                        label: item.label.clone(),
                        start_line: Some(item.start_line),
                        end_line: Some(item.end_line),
                        match_indices: vec![match_idx],
                    });
                    last_grouped_item_idx = Some(item_idx);
                } else {
                    file_scope_matches.push(match_idx);
                    last_grouped_item_idx = None;
                }
            } else if last_grouped_item_idx == Some(item_idx) {
                // `last_grouped_item_idx` says a group was opened for this item,
                // and if that ever stopped being true the match is still a match:
                // it belongs to the file scope rather than nowhere, because
                // dropping it to preserve a group shape would be `grep` losing a
                // line it found.
                match symbol_groups.last_mut() {
                    Some(group) => group.match_indices.push(match_idx),
                    None => file_scope_matches.push(match_idx),
                }
            } else {
                file_scope_matches.push(match_idx);
            }
        } else {
            file_scope_matches.push(match_idx);
        }
    }

    let mut groups =
        Vec::with_capacity(symbol_groups.len() + usize::from(!file_scope_matches.is_empty()));
    if !file_scope_matches.is_empty() {
        groups.push(MatchGroup {
            kind: "file-scope".to_string(),
            label: "<file scope>".to_string(),
            start_line: None,
            end_line: None,
            match_indices: file_scope_matches,
        });
    }
    groups.extend(symbol_groups);

    let matched_symbol_count = matched_indices.len();
    let mut other_symbols = Vec::new();
    let mut other_symbols_omitted_count = 0;
    let mut matched_iter = matched_indices.into_iter().peekable();
    for (idx, item) in items.iter().enumerate() {
        if matched_iter.peek().copied() == Some(idx) {
            matched_iter.next();
            continue;
        }
        if other_symbols.len() < options.other_symbols_limit {
            other_symbols.push(item.clone());
        } else {
            other_symbols_omitted_count += 1;
        }
    }

    GroupingResult {
        matched_symbol_count,
        groups,
        other_symbols,
        other_symbols_omitted_count,
    }
}

fn file_scope_group(match_indices: Vec<usize>) -> MatchGroup {
    MatchGroup {
        kind: "file-scope".to_string(),
        label: "<file scope>".to_string(),
        start_line: None,
        end_line: None,
        match_indices,
    }
}
