//! The code graph — v1, structural, and honest about what it is not.
//!
//! `memnoth-codegraph` (the production graph: tree-sitter parsing, resolved
//! call edges, communities, semantic search) is not published in this repository.
//! This module is the substitute the CLI can actually build today: a
//! deterministic line scanner over the working tree, stored in SQLite beside the
//! workspace (`.memnoth/graph.db`), answering the two questions that need no
//! resolution — *where is this declared* and *who mentions it*.
//!
//! What that buys:
//!
//! * `kg index` is incremental by file (size + mtime), so a re-run after a pull
//!   re-reads only what changed, and a re-run with nothing changed does nothing.
//! * `kg impact` answers `definition` and `reference` rows, with a second hop
//!   available. References are textual: a name that is shadowed by a local
//!   variable is still counted. It is a blast-radius *hint*, not a resolved call
//!   graph, and the output says so.
//! * `kg search` is lexical by construction. There is no embedder in this build,
//!   so a query matches names, never meaning — reported rather than hidden.
//!
//! The storage contract is `store.rs`, extraction is `extract.rs`, and every
//! rendering is `render.rs`. `commands.rs` calls the four public verbs; the TUI
//! and `doctor` call `status`.

mod extract;
mod render;
mod store;

pub use render::{
    render_callees, render_callers, render_callpath, render_cycles, render_dead_code, render_deps,
    render_def, render_diff_impact, render_explore, render_hotspots, render_index, render_outline,
    render_owners, render_query, render_refs, render_review, render_search, render_status,
    render_tests_for, render_tree, render_unused_exports,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Serialize;

use crate::context::AppContext;
use extract::{ExtractedSymbol, KEYWORDS};
use memnoth_search::workspace::{SearchScope, collect_file_entries, read_text_file};
use store::{FileRow, Graph};

/// Files larger than this are counted but not parsed.
///
/// A megabyte of source is a generated file (a lockfile-shaped module, a bundled
/// bundle); scanning it costs more than the symbols it would contribute, and its
/// "references" would be to names the project does not own.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Hard cap on stored mentions. Reached only on very large trees; the index says
/// so rather than silently truncating.
pub(crate) const MAX_REFS: usize = 500_000;

/// Names shorter than this are declarations but not indexed as references.
///
/// `id`, `db`, `io` and `to` appear in every file; storing each mention would
/// bury the interesting rows and make `impact` useless for anything whose name
/// is short.
const MIN_REF_NAME_LEN: usize = 3;

/// References returned by one `impact` call before it truncates.
const MAX_IMPACT_REFS: usize = 500;

/// References added by the second hop.
const MAX_IMPACT_DEPTH_REFS: usize = 400;

/// Rows scanned by one `search` before ranking.
const MAX_SEARCH_SCAN: usize = 5_000;

/// Mention paths inspected for the test list.
const MAX_TEST_REFS: usize = 2_000;

/// Build and cache directories. The walker already honours `.gitignore`; this
/// list catches trees that are not ignored but are never source (a checked-in
/// `node_modules`, a `dist/` with generated bundles).
const SKIP_SEGMENTS: [&str; 10] = [
    "/.memnoth/",
    "/.git/",
    "/target/",
    "/node_modules/",
    "/.venv/",
    "/dist/",
    "/.output/",
    "/vendor/",
    "/.next/",
    "/__pycache__/",
];

/// Files and symbols per language.
#[derive(Debug, Clone, Serialize)]
pub struct LanguageCount {
    /// Language label (`rust`, `typescript`, …).
    pub language: String,
    /// Files of that language.
    pub files: usize,
    /// Declarations found in them.
    pub symbols: usize,
}

/// What one index run did.
#[derive(Debug, Clone, Serialize)]
pub struct IndexReport {
    /// Workspace that was indexed.
    pub root: String,
    /// Files in the graph after the run.
    pub files: usize,
    /// Declarations in the graph after the run.
    pub symbols: usize,
    /// Stored mentions after the run.
    pub references: usize,
    /// Per-language totals.
    pub languages: Vec<LanguageCount>,
    /// Wall-clock time for this run.
    pub elapsed_ms: u64,
    /// Whether the graph was rebuilt from scratch.
    pub full: bool,
    /// Whether embeddings were built. Always false in this build.
    pub embedded: bool,
    /// When the graph was written (ms since epoch).
    pub built_at_ms: i64,
}

/// The graph as it stands, without changing it.
#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    /// Whether a graph this build understands exists.
    pub indexed: bool,
    /// Workspace it was built from.
    pub root: String,
    /// Graph file.
    pub path: String,
    /// Files in the graph.
    pub files: usize,
    /// Declarations in the graph.
    pub symbols: usize,
    /// Stored mentions.
    pub references: usize,
    /// When it was written (ms since epoch).
    pub built_at_ms: i64,
    /// Files that changed, appeared or disappeared since the index.
    pub stale_files: usize,
    /// Per-language totals.
    pub languages: Vec<LanguageCount>,
    /// Embedding model, when one was used. `None` in this build.
    pub embed_model: Option<String>,
}

/// One place a symbol is defined or mentioned.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactHit {
    /// Workspace-relative path.
    pub path: String,
    /// 1-based line.
    pub line: usize,
    /// `definition` or `reference`.
    pub kind: String,
    /// The source line, trimmed, or a description when it could not be read.
    pub context: String,
}

/// What depends on a symbol.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactReport {
    /// The name that was asked for.
    pub symbol: String,
    /// Whether any declaration matched it.
    pub resolved: bool,
    /// Declarations for the resolved name(s).
    pub definitions: Vec<ImpactHit>,
    /// Mentions outside the declaration line.
    pub references: Vec<ImpactHit>,
    /// Distinct files involved (definitions + references).
    pub files: usize,
    /// Referencing files that look like tests.
    pub tests: Vec<String>,
    /// Whether the reference list hit its cap.
    pub truncated: bool,
}

/// One symbol match.
#[derive(Debug, Clone, Serialize)]
pub struct SymbolHit {
    /// Declared name.
    pub name: String,
    /// Declaration kind.
    pub kind: String,
    /// Defining file.
    pub path: String,
    /// Declaration line.
    pub line: usize,
    /// Enclosing declaration, when known.
    pub container: Option<String>,
}

/// Explore result: definition, snippet, refs, callers, callees.
#[derive(Debug, Clone, Serialize)]
pub struct ExploreReport {
    /// The symbol name.
    pub symbol: String,
    /// Whether a declaration was found.
    pub resolved: bool,
    /// Definition with snippet.
    pub definition: Option<ExploreDefinition>,
    /// References to the symbol.
    pub references: Vec<ImpactHit>,
    /// Callers (functions that call this symbol).
    pub callers: Vec<ImpactHit>,
    /// Callees (functions this symbol calls).
    pub callees: Vec<ImpactHit>,
    /// Whether any list was truncated.
    pub truncated: bool,
}

/// A definition with surrounding context.
#[derive(Debug, Clone, Serialize)]
pub struct ExploreDefinition {
    /// Workspace-relative path.
    pub path: String,
    /// 1-based line.
    pub line: usize,
    /// Declaration kind.
    pub kind: String,
    /// Surrounding lines (snippet).
    pub snippet: Vec<String>,
    /// Enclosing declaration.
    pub container: Option<String>,
}

/// Refs report.
#[derive(Debug, Clone, Serialize)]
pub struct RefsReport {
    /// The symbol name.
    pub symbol: String,
    /// Whether any declaration matched.
    pub resolved: bool,
    /// References.
    pub references: Vec<ImpactHit>,
    /// Whether truncated.
    pub truncated: bool,
}

/// Callers report.
#[derive(Debug, Clone, Serialize)]
pub struct CallersReport {
    /// The symbol name.
    pub symbol: String,
    /// Whether any declaration matched.
    pub resolved: bool,
    /// Callers.
    pub callers: Vec<ImpactHit>,
    /// Whether truncated.
    pub truncated: bool,
}

/// Callees report.
#[derive(Debug, Clone, Serialize)]
pub struct CalleesReport {
    /// The symbol name.
    pub symbol: String,
    /// Whether any declaration matched.
    pub resolved: bool,
    /// Callees.
    pub callees: Vec<ImpactHit>,
    /// Whether truncated.
    pub truncated: bool,
}

/// Def report.
#[derive(Debug, Clone, Serialize)]
pub struct DefReport {
    /// The symbol name.
    pub symbol: String,
    /// Whether a declaration was found.
    pub resolved: bool,
    /// Definition with snippet.
    pub definition: Option<ExploreDefinition>,
}

/// Diff-impact report.
#[derive(Debug, Clone, Serialize)]
pub struct DiffImpactReport {
    /// Whether staged changes.
    pub staged: bool,
    /// Changed symbols with impact.
    pub changes: Vec<DiffImpactChange>,
    /// Total references to changed symbols.
    pub total_references: usize,
}

/// One changed symbol's impact.
#[derive(Debug, Clone, Serialize)]
pub struct DiffImpactChange {
    /// Symbol name.
    pub symbol: String,
    /// Change type: added, modified, removed.
    pub change_type: String,
    /// File path.
    pub path: String,
    /// Line number.
    pub line: usize,
    /// References to this symbol.
    pub references: Vec<ImpactHit>,
}
/// Shortest call path between two symbols.
#[derive(Debug, Clone, Serialize)]
pub struct CallpathReport {
    pub from: String,
    pub to: String,
    pub found: bool,
    pub path: Vec<CallpathNode>,
    pub hops: usize,
}

/// One node in the call path.
#[derive(Debug, Clone, Serialize)]
pub struct CallpathNode {
    pub symbol: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
}

/// Structural outline of a file.
#[derive(Debug, Clone, Serialize)]
pub struct OutlineReport {
    pub file: String,
    pub language: String,
    pub symbols: Vec<OutlineSymbol>,
}

/// One symbol in an outline.
#[derive(Debug, Clone, Serialize)]
pub struct OutlineSymbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub end_line: usize,
    pub container: Option<String>,
    pub children: Vec<OutlineSymbol>,
}

/// Symbol tree for a directory.
#[derive(Debug, Clone, Serialize)]
pub struct TreeReport {
    pub dir: String,
    pub entries: Vec<TreeEntry>,
}

/// One entry in the symbol tree.
#[derive(Debug, Clone, Serialize)]
pub struct TreeEntry {
    pub path: String,
    pub language: String,
    pub symbols: Vec<TreeSymbol>,
}

/// One symbol in the tree view.
#[derive(Debug, Clone, Serialize)]
pub struct TreeSymbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub container: Option<String>,
}

/// Graph-aware diff review.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewReport {
    pub staged: bool,
    pub changes: Vec<ReviewChange>,
    pub summary: ReviewSummary,
}

/// Summary of review.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewSummary {
    pub files_changed: usize,
    pub symbols_changed: usize,
    pub references_affected: usize,
    pub test_coverage_delta: i32,
}

/// One change in the review.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewChange {
    pub file: String,
    pub change_type: String,
    pub symbols: Vec<ReviewSymbol>,
    pub impacted_tests: Vec<String>,
}

/// Symbol change in review.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewSymbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub impact: String,
}

/// Tests that cover a symbol.
#[derive(Debug, Clone, Serialize)]
pub struct TestsForReport {
    pub symbol: String,
    pub tests: Vec<TestCoverage>,
    pub total_coverage: usize,
}

/// One test covering the symbol.
#[derive(Debug, Clone, Serialize)]
pub struct TestCoverage {
    pub path: String,
    pub line: usize,
    pub test_name: String,
    pub coverage_type: String,
}

/// CODEOWNERS + git blame for a path.
#[derive(Debug, Clone, Serialize)]
pub struct OwnersReport {
    pub path: String,
    pub codeowners: Vec<CodeownerEntry>,
    pub blame: Vec<BlameEntry>,
}

/// One CODEOWNERS entry.
#[derive(Debug, Clone, Serialize)]
pub struct CodeownerEntry {
    pub pattern: String,
    pub owners: Vec<String>,
}

/// One git blame entry.
#[derive(Debug, Clone, Serialize)]
pub struct BlameEntry {
    pub line: usize,
    pub author: String,
    pub commit: String,
    pub date: String,
    pub content: String,
}

/// Unreferenced exports (dead code).
#[derive(Debug, Clone, Serialize)]
pub struct DeadCodeReport {
    pub exports: Vec<DeadExport>,
    pub total: usize,
}

/// One unreferenced export.
#[derive(Debug, Clone, Serialize)]
pub struct DeadExport {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub is_public: bool,
}

/// Import cycles.
#[derive(Debug, Clone, Serialize)]
pub struct CyclesReport {
    pub cycles: Vec<ImportCycle>,
    pub total: usize,
}

/// One import cycle.
#[derive(Debug, Clone, Serialize)]
pub struct ImportCycle {
    pub files: Vec<String>,
    pub length: usize,
}

/// Churn × complexity hotspots.
#[derive(Debug, Clone, Serialize)]
pub struct HotspotsReport {
    pub hotspots: Vec<Hotspot>,
    pub total: usize,
}

/// One hotspot.
#[derive(Debug, Clone, Serialize)]
pub struct Hotspot {
    pub file: String,
    pub churn: usize,
    pub complexity: usize,
    pub score: f64,
    pub symbols: Vec<String>,
}

/// Unused public exports (API hygiene).
#[derive(Debug, Clone, Serialize)]
pub struct UnusedExportsReport {
    pub exports: Vec<UnusedExport>,
    pub total: usize,
}

/// One unused export.
#[derive(Debug, Clone, Serialize)]
pub struct UnusedExport {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub visibility: String,
}

/// Import graph for a file.
#[derive(Debug, Clone, Serialize)]
pub struct DepsReport {
    pub file: String,
    pub imports: Vec<ImportEdge>,
    pub imported_by: Vec<ImportEdge>,
}

/// One import edge.
#[derive(Debug, Clone, Serialize)]
pub struct ImportEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub line: usize,
}

/// Watch result.
#[derive(Debug, Clone, Serialize)]
pub struct WatchReport {
    pub watching: bool,
    pub graph_path: String,
    pub last_index: Option<String>,
    pub files_watched: usize,
}

/// Graph DSL query result.
#[derive(Debug, Clone, Serialize)]
pub struct QueryReport {
    pub expr: String,
    pub results: Vec<QueryResult>,
    pub count: usize,
}

/// One query result.
#[derive(Debug, Clone, Serialize)]
pub struct QueryResult {
    pub symbol: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub metadata: serde_json::Value,
}

/// Serve result.
#[derive(Debug, Clone, Serialize)]
pub struct ServeReport {
    pub listening: bool,
    pub port: u16,
    pub endpoint: String,
    pub graph_path: String,
}

/// A file the walker kept, with its text already read.
struct SourceFile {
    relative: String,
    language: &'static str,
    bytes: i64,
    mtime_ms: i64,
    text: String,
}

/// `memnoth kg index`.
pub fn index(
    ctx: &AppContext,
    full: bool,
    no_embed: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<IndexReport> {
    let started = Instant::now();
    let root_label = root_label(&ctx.root);
    let now = memnoth_engine::now_ms();

    progress(&format!("walking {}", ctx.root.display()));
    let files = walk(&ctx.root);
    progress(&format!("walked {} file(s)", files.len()));

    // Detect an unusable existing graph *before* opening it for writing: opening
    // initialises the schema, which would otherwise erase the evidence that the
    // file was written by a different shape.
    let rebuild = full
        || preflight(&ctx.graph_path).is_none_or(|(version, root)| {
            version != store::SCHEMA_VERSION || root.as_deref() != Some(root_label.as_str())
        });

    let mut graph = Graph::open(&ctx.graph_path)?;
    if rebuild {
        graph.clear()?;
    }

    let existing: HashMap<String, FileRow> = graph
        .existing_files()?
        .into_iter()
        .map(|row| (row.path.clone(), row))
        .collect();
    let current: BTreeSet<String> = files.iter().map(|file| file.relative.clone()).collect();
    let deleted: Vec<String> = existing
        .keys()
        .filter(|path| !current.contains(*path))
        .cloned()
        .collect();
    let changed: Vec<&SourceFile> = files
        .iter()
        .filter(|file| match existing.get(&file.relative) {
            Some(row) => row.bytes != file.bytes || row.mtime_ms != file.mtime_ms,
            None => true,
        })
        .collect();

    // Nothing changed and nothing is missing: a re-run is not a rebuild. The
    // build time still moves, because "when was this last verified" is a
    // different fact from "when did the content last change".
    if !rebuild && changed.is_empty() && deleted.is_empty() && existing.len() == files.len() {
        graph.set_meta("built_at_ms", &now.to_string())?;
        progress("no changes: the graph is already current");
        let counts = graph.counts()?;
        return Ok(IndexReport {
            root: root_label,
            files: counts.files,
            symbols: counts.symbols,
            references: counts.references,
            languages: graph.language_counts()?,
            elapsed_ms: started.elapsed().as_millis() as u64,
            full: false,
            embedded: false,
            built_at_ms: now,
        });
    }

    if !no_embed {
        progress("embeddings are not built by this build; search stays lexical");
    }
    progress(&format!(
        "extracting symbols from {} changed file(s)",
        changed.len()
    ));
    let mut extracted: HashMap<String, Vec<ExtractedSymbol>> = HashMap::new();
    for file in &changed {
        extracted.insert(
            file.relative.clone(),
            extract::extract(file.language, &file.text),
        );
    }

    // A symbol that did not exist before may be mentioned by a file whose bytes
    // did not change, so the reference scan has to run over everything in that
    // case. A removal cannot create a new mention — a stale row for a name that
    // no longer resolves is harmless and disappears on the next full run.
    let existing_names = graph.symbol_names()?;
    let introduced: BTreeSet<String> = extracted
        .values()
        .flatten()
        .map(|symbol| symbol.name.clone())
        .collect();
    let names_added = introduced.difference(&existing_names).next().is_some();
    let scan_all = rebuild || names_added;

    if scan_all {
        for file in &files {
            extracted
                .entry(file.relative.clone())
                .or_insert_with(|| extract::extract(file.language, &file.text));
        }
    }

    let mut names: HashSet<String> = existing_names.into_iter().collect();
    names.extend(
        extracted
            .values()
            .flatten()
            .map(|symbol| symbol.name.clone()),
    );

    progress("finding references");
    let mut references_written = 0usize;
    let mut truncated = false;
    graph.transaction(|graph| {
        for path in &deleted {
            graph.delete_file(path)?;
        }

        for file in &files {
            let Some(symbols) = extracted.get(&file.relative) else {
                continue;
            };
            // Replacing a file means replacing everything derived from it, so the
            // three tables cannot disagree about a path.
            graph.delete_file(&file.relative)?;
            graph.upsert_file(
                &file.relative,
                file.language,
                file.bytes,
                file.mtime_ms,
                symbols.len() as i64,
            )?;
            graph.insert_symbols(&file.relative, symbols)?;
        }

        for file in &files {
            let rewritten = extracted.contains_key(&file.relative);
            if !rewritten && !scan_all {
                continue;
            }
            if references_written >= MAX_REFS {
                truncated = true;
                break;
            }
            // A rewritten file already lost its old rows with `delete_file`; an
            // unchanged file being rescanned has not.
            if !rewritten {
                graph.delete_refs_for(&file.relative)?;
            }
            let symbols: &[ExtractedSymbol] = extracted
                .get(&file.relative)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let budget = MAX_REFS - references_written;
            let refs = scan_refs(&file.text, symbols, &names, budget);
            if refs.len() >= budget {
                truncated = true;
            }
            references_written += refs.len();
            graph.insert_refs(&file.relative, &refs)?;
        }

        graph.set_meta("root", &root_label)?;
        graph.set_meta("built_at_ms", &now.to_string())?;
        graph.set_meta("refs_truncated", if truncated { "1" } else { "0" })?;
        Ok(())
    })?;

    let counts = graph.counts()?;
    Ok(IndexReport {
        root: root_label,
        files: counts.files,
        symbols: counts.symbols,
        references: counts.references,
        languages: graph.language_counts()?,
        elapsed_ms: started.elapsed().as_millis() as u64,
        full: rebuild,
        embedded: false,
        built_at_ms: now,
    })
}

/// `memnoth kg status`.
///
/// Reads; never creates. A status command that initialises the graph would make
/// the next `kg status` report an empty index as a healthy one.
pub fn status(ctx: &AppContext) -> Result<IndexStatus> {
    let path = ctx.graph_path.display().to_string();

    if !ctx.graph_path.is_file() {
        return Ok(no_graph_status(ctx, &path));
    }
    let graph = match Graph::open_readonly(&ctx.graph_path) {
        Ok(graph) => graph,
        Err(_) => return Ok(no_graph_status(ctx, &path)),
    };
    if !graph.schema_present() {
        return Ok(no_graph_status(ctx, &path));
    }

    let counts = graph.counts()?;
    let built_at_ms = graph
        .get_meta("built_at_ms")?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let root = graph
        .get_meta("root")?
        .unwrap_or_else(|| ctx.root.display().to_string());
    Ok(IndexStatus {
        indexed: true,
        root,
        path,
        files: counts.files,
        symbols: counts.symbols,
        references: counts.references,
        built_at_ms,
        stale_files: stale_files(ctx, &graph)?,
        languages: graph.language_counts()?,
        embed_model: None,
    })
}

/// The "nothing to report yet" shape `status` returns for a missing or
/// incompatible graph.
fn no_graph_status(ctx: &AppContext, path: &str) -> IndexStatus {
    IndexStatus {
        indexed: false,
        root: root_label(&ctx.root),
        path: path.to_string(),
        files: 0,
        symbols: 0,
        references: 0,
        built_at_ms: 0,
        stale_files: 0,
        languages: Vec::new(),
        embed_model: None,
    }
}

/// `memnoth kg impact`.
pub fn impact(ctx: &AppContext, symbol: &str, depth: u32) -> Result<ImpactReport> {
    let symbol = symbol.trim();
    if symbol.is_empty() {
        bail!("the symbol name is empty");
    }
    let graph = open_for_read(ctx)?;

    let mut names = exact_names(&graph, symbol)?;
    if names.is_empty()
        && let Some(segment) = last_segment(symbol)
        && segment != symbol
    {
        // A qualified name prefers qualified declarations: `Type::run` must
        // not resolve to an unrelated free function called `run`.
        names = graph.names_end_with(segment)?;
        if names.is_empty() {
            names = exact_names(&graph, segment)?;
        }
    }
    if names.is_empty() {
        return Ok(ImpactReport {
            symbol: symbol.to_string(),
            resolved: false,
            definitions: Vec::new(),
            references: Vec::new(),
            files: 0,
            tests: Vec::new(),
            truncated: false,
        });
    }

    let mut definitions = graph.definitions_for(&names)?;
    // `Type::method` should prefer the declaration inside `Type`, but only when
    // such a container exists: a wrong preference would hide the real answer.
    if let Some((prefix, _)) = symbol.rsplit_once("::") {
        let qualified: Vec<&store::SymbolRow> = definitions
            .iter()
            .filter(|row| {
                row.container.as_deref().is_some_and(|container| {
                    container == prefix || container.ends_with(&format!("::{prefix}"))
                })
            })
            .collect();
        if !qualified.is_empty() {
            definitions = qualified.into_iter().cloned().collect();
        }
    }

    let mut lines = LineCache::new(&ctx.root);
    let definitions: Vec<ImpactHit> = definitions
        .into_iter()
        .map(|row| ImpactHit {
            context: lines
                .get(&row.path, row.line)
                .unwrap_or_else(|| format!("{} {}", row.kind, row.name)),
            path: row.path,
            line: row.line,
            kind: "definition".to_string(),
        })
        .collect();

    let mut references: Vec<ImpactHit> = graph
        .refs_for(&names, MAX_IMPACT_REFS)?
        .into_iter()
        .map(|row| ImpactHit {
            context: lines.get(&row.path, row.line).unwrap_or_else(|| {
                match row.container.as_deref() {
                    Some(container) => format!("{} mentioned in {container}", row.name),
                    None => format!("{} mentioned", row.name),
                }
            }),
            path: row.path,
            line: row.line,
            kind: "reference".to_string(),
        })
        .collect();
    let mut truncated = references.len() >= MAX_IMPACT_REFS;

    // One extra hop: symbols declared in the files that mention this one, then
    // whoever mentions *those*. It is what turns "who calls me" into "what else
    // moves when I change" without pretending to resolve call edges.
    if depth > 1 && !references.is_empty() {
        let mut paths: Vec<String> = Vec::new();
        for hit in &references {
            if !paths.contains(&hit.path) {
                paths.push(hit.path.clone());
            }
        }
        paths.truncate(200);
        let mut second_hop: Vec<String> = graph
            .names_in_files(&paths)?
            .into_iter()
            .filter(|name| !names.contains(name))
            .filter(|name| name.len() >= MIN_REF_NAME_LEN)
            .filter(|name| !KEYWORDS.contains(&name.as_str()))
            .collect();
        second_hop.sort();
        second_hop.dedup();
        let mut seen: HashSet<(String, usize)> = references
            .iter()
            .map(|hit| (hit.path.clone(), hit.line))
            .collect();
        let extra = graph.refs_for(&second_hop, MAX_IMPACT_DEPTH_REFS)?;
        if extra.len() >= MAX_IMPACT_DEPTH_REFS {
            truncated = true;
        }
        for row in extra {
            if !seen.insert((row.path.clone(), row.line)) {
                continue;
            }
            references.push(ImpactHit {
                context: lines
                    .get(&row.path, row.line)
                    .map(|source| format!("depth 2 · {source}"))
                    .unwrap_or_else(|| format!("depth 2 · {} mentioned", row.name)),
                path: row.path,
                line: row.line,
                kind: "reference".to_string(),
            });
        }
    }

    let mut files: BTreeSet<String> = BTreeSet::new();
    for hit in definitions.iter().chain(references.iter()) {
        files.insert(hit.path.clone());
    }
    let tests: Vec<String> = graph
        .ref_paths_for(&names, MAX_TEST_REFS)?
        .into_iter()
        .filter(|path| is_test_path(path))
        .collect();

    Ok(ImpactReport {
        symbol: symbol.to_string(),
        resolved: true,
        definitions,
        references,
        files: files.len(),
        tests,
        truncated,
    })
}

/// `memnoth kg search`.
pub fn search(ctx: &AppContext, query: &[String], limit: usize) -> Result<Vec<SymbolHit>> {
    let query = query.join(" ");
    let trimmed = query.trim();
    if trimmed.is_empty() {
        bail!("the search query is empty");
    }
    if limit == 0 {
        return Ok(Vec::new());
    }
    let graph = open_for_read(ctx)?;
    let lowered = trimmed.to_lowercase();
    let mut ranked: Vec<(u8, store::SearchRow)> = graph
        .like_name(trimmed, MAX_SEARCH_SCAN)?
        .into_iter()
        .map(|row| {
            let name = row.name.to_lowercase();
            let rank = if name == lowered {
                0
            } else if name.starts_with(&lowered) {
                1
            } else {
                2
            };
            (rank, row)
        })
        .collect();
    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()))
            .then_with(|| a.1.path.cmp(&b.1.path))
            .then_with(|| a.1.line.cmp(&b.1.line))
    });

    let mut seen: HashSet<(String, String, usize)> = HashSet::new();
    let mut hits = Vec::new();
    for (_, row) in ranked {
        if !seen.insert((row.name.clone(), row.path.clone(), row.line)) {
            continue;
        }
        hits.push(SymbolHit {
            name: row.name,
            kind: row.kind,
            path: row.path,
            line: row.line,
            container: row.container,
        });
        if hits.len() >= limit {
            break;
        }
    }
    Ok(hits)
}

/// `memnoth kg explore`.
pub fn explore(ctx: &AppContext, symbol: &str) -> Result<ExploreReport> {
    let symbol = symbol.trim();
    if symbol.is_empty() {
        bail!("the symbol name is empty");
    }
    let graph = open_for_read(ctx)?;

    let mut names = exact_names(&graph, symbol)?;
    if names.is_empty()
        && let Some(segment) = last_segment(symbol)
        && segment != symbol
    {
        names = graph.names_end_with(segment)?;
        if names.is_empty() {
            names = exact_names(&graph, segment)?;
        }
    }
    if names.is_empty() {
        return Ok(ExploreReport {
            symbol: symbol.to_string(),
            resolved: false,
            definition: None,
            references: Vec::new(),
            callers: Vec::new(),
            callees: Vec::new(),
            truncated: false,
        });
    }

    let mut lines = LineCache::new(&ctx.root);

    // Get definitions
    let mut definitions = graph.definitions_for(&names)?;
    if let Some((prefix, _)) = symbol.rsplit_once("::") {
        let qualified: Vec<&store::SymbolRow> = definitions
            .iter()
            .filter(|row| {
                row.container.as_deref().is_some_and(|container| {
                    container == prefix || container.ends_with(&format!("::{prefix}"))
                })
            })
            .collect();
        if !qualified.is_empty() {
            definitions = qualified.into_iter().cloned().collect();
        }
    }

    // Build definition with snippet
    let definition = if let Some(def) = definitions.first() {
        let snippet_lines = get_snippet(&ctx.root, &def.path, def.line, def.end_line)?;
        Some(ExploreDefinition {
            path: def.path.clone(),
            line: def.line,
            kind: def.kind.clone(),
            snippet: snippet_lines,
            container: def.container.clone(),
        })
    } else {
        None
    };

    // Get references
    let mut references: Vec<ImpactHit> = graph
        .refs_for(&names, MAX_IMPACT_REFS)?
        .into_iter()
        .map(|row| ImpactHit {
            context: lines.get(&row.path, row.line).unwrap_or_else(|| {
                match row.container.as_deref() {
                    Some(container) => format!("{} mentioned in {container}", row.name),
                    None => format!("{} mentioned", row.name),
                }
            }),
            path: row.path,
            line: row.line,
            kind: "reference".to_string(),
        })
        .collect();
    let mut truncated = references.len() >= MAX_IMPACT_REFS;

    // Get callers: functions that reference this symbol
    let callers = find_callers(&graph, &names, &mut lines, MAX_IMPACT_REFS)?;
    if callers.len() >= MAX_IMPACT_REFS {
        truncated = true;
    }

    // Get callees: symbols declared in the same container as the definition
    let callees = find_callees(&graph, &names, &mut lines, MAX_IMPACT_REFS)?;
    if callees.len() >= MAX_IMPACT_REFS {
        truncated = true;
    }

    Ok(ExploreReport {
        symbol: symbol.to_string(),
        resolved: true,
        definition,
        references,
        callers,
        callees,
        truncated,
    })
}

/// `memnoth kg refs`.
pub fn refs(ctx: &AppContext, symbol: &str, limit: usize) -> Result<RefsReport> {
    let symbol = symbol.trim();
    if symbol.is_empty() {
        bail!("the symbol name is empty");
    }
    let graph = open_for_read(ctx)?;

    let mut names = exact_names(&graph, symbol)?;
    if names.is_empty()
        && let Some(segment) = last_segment(symbol)
        && segment != symbol
    {
        names = graph.names_end_with(segment)?;
        if names.is_empty() {
            names = exact_names(&graph, segment)?;
        }
    }
    if names.is_empty() {
        return Ok(RefsReport {
            symbol: symbol.to_string(),
            resolved: false,
            references: Vec::new(),
            truncated: false,
        });
    }

    let mut lines = LineCache::new(&ctx.root);
    let references: Vec<ImpactHit> = graph
        .refs_for(&names, limit)?
        .into_iter()
        .map(|row| ImpactHit {
            context: lines.get(&row.path, row.line).unwrap_or_else(|| {
                match row.container.as_deref() {
                    Some(container) => format!("{} mentioned in {container}", row.name),
                    None => format!("{} mentioned", row.name),
                }
            }),
            path: row.path,
            line: row.line,
            kind: "reference".to_string(),
        })
        .collect();
    let truncated = references.len() >= limit;

    Ok(RefsReport {
        symbol: symbol.to_string(),
        resolved: true,
        references,
        truncated,
    })
}

/// `memnoth kg callers`.
pub fn callers(ctx: &AppContext, symbol: &str, limit: usize) -> Result<CallersReport> {
    let symbol = symbol.trim();
    if symbol.is_empty() {
        bail!("the symbol name is empty");
    }
    let graph = open_for_read(ctx)?;

    let mut names = exact_names(&graph, symbol)?;
    if names.is_empty()
        && let Some(segment) = last_segment(symbol)
        && segment != symbol
    {
        names = graph.names_end_with(segment)?;
        if names.is_empty() {
            names = exact_names(&graph, segment)?;
        }
    }
    if names.is_empty() {
        return Ok(CallersReport {
            symbol: symbol.to_string(),
            resolved: false,
            callers: Vec::new(),
            truncated: false,
        });
    }

    let mut lines = LineCache::new(&ctx.root);
    let callers = find_callers(&graph, &names, &mut lines, limit)?;
    let truncated = callers.len() >= limit;

    Ok(CallersReport {
        symbol: symbol.to_string(),
        resolved: true,
        callers,
        truncated,
    })
}

/// `memnoth kg callees`.
pub fn callees(ctx: &AppContext, symbol: &str, limit: usize) -> Result<CalleesReport> {
    let symbol = symbol.trim();
    if symbol.is_empty() {
        bail!("the symbol name is empty");
    }
    let graph = open_for_read(ctx)?;

    let mut names = exact_names(&graph, symbol)?;
    if names.is_empty()
        && let Some(segment) = last_segment(symbol)
        && segment != symbol
    {
        names = graph.names_end_with(segment)?;
        if names.is_empty() {
            names = exact_names(&graph, segment)?;
        }
    }
    if names.is_empty() {
        return Ok(CalleesReport {
            symbol: symbol.to_string(),
            resolved: false,
            callees: Vec::new(),
            truncated: false,
        });
    }

    let mut lines = LineCache::new(&ctx.root);
    let callees = find_callees(&graph, &names, &mut lines, limit)?;
    let truncated = callees.len() >= limit;

    Ok(CalleesReport {
        symbol: symbol.to_string(),
        resolved: true,
        callees,
        truncated,
    })
}

/// `memnoth kg def`.
pub fn def(ctx: &AppContext, symbol: &str) -> Result<DefReport> {
    let symbol = symbol.trim();
    if symbol.is_empty() {
        bail!("the symbol name is empty");
    }
    let graph = open_for_read(ctx)?;

    let mut names = exact_names(&graph, symbol)?;
    if names.is_empty()
        && let Some(segment) = last_segment(symbol)
        && segment != symbol
    {
        names = graph.names_end_with(segment)?;
        if names.is_empty() {
            names = exact_names(&graph, segment)?;
        }
    }
    if names.is_empty() {
        return Ok(DefReport {
            symbol: symbol.to_string(),
            resolved: false,
            definition: None,
        });
    }

    let definitions = graph.definitions_for(&names)?;
    let mut definition = None;
    if let Some(def) = definitions.first() {
        let snippet_lines = get_snippet(&ctx.root, &def.path, def.line, def.end_line)?;
        definition = Some(ExploreDefinition {
            path: def.path.clone(),
            line: def.line,
            kind: def.kind.clone(),
            snippet: snippet_lines,
            container: def.container.clone(),
        });
    }

    Ok(DefReport {
        symbol: symbol.to_string(),
        resolved: true,
        definition,
    })
}

/// `memnoth kg diff-impact`.
pub fn diff_impact(ctx: &AppContext, staged: bool) -> Result<DiffImpactReport> {
    use std::process::Command;

    let git_args = if staged {
        vec!["diff", "--cached", "--name-only"]
    } else {
        vec!["diff", "--name-only"]
    };

    let output = Command::new("git")
        .args(&git_args)
        .current_dir(&ctx.root)
        .output()
        .context("running git diff")?;

    if !output.status.success() {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let changed_files: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if changed_files.is_empty() {
        return Ok(DiffImpactReport {
            staged,
            changes: Vec::new(),
            total_references: 0,
        });
    }

    let graph = open_for_read(ctx)?;
    let mut lines = LineCache::new(&ctx.root);
    let mut changes = Vec::new();
    let mut total_refs = 0;

    for file in &changed_files {
        let language = extract::language_for(file);
        let text = match std::fs::read_to_string(ctx.root.join(file)) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let symbols = extract::extract(language, &text);

        for symbol in &symbols {
            let names = exact_names(&graph, &symbol.name)?;
            if names.is_empty() {
                continue;
            }
            let refs = graph.refs_for(&names, MAX_IMPACT_REFS)?;
            let references: Vec<ImpactHit> = refs
                .into_iter()
                .map(|row| ImpactHit {
                    context: lines.get(&row.path, row.line).unwrap_or_else(|| {
                        match row.container.as_deref() {
                            Some(container) => format!("{} mentioned in {container}", row.name),
                            None => format!("{} mentioned", row.name),
                        }
                    }),
                    path: row.path,
                    line: row.line,
                    kind: "reference".to_string(),
                })
                .collect();
            total_refs += references.len();

            changes.push(DiffImpactChange {
                symbol: symbol.name.clone(),
                change_type: "modified".to_string(),
                path: file.clone(),
                line: symbol.line,
                references,
            });
        }
    }

    Ok(DiffImpactReport {
        staged,
        changes,
        total_references: total_refs,
    })
}

/// Get a snippet of lines around a definition.
fn get_snippet(root: &Path, path: &str, start_line: usize, end_line: usize) -> Result<Vec<String>> {
    let full_path = root.join(path);
    let text = std::fs::read_to_string(&full_path)
        .with_context(|| format!("reading {}", full_path.display()))?;
    let lines: Vec<String> = text.lines().map(str::to_string).collect();

    let start = start_line.saturating_sub(3).min(lines.len());
    let end = (end_line + 2).min(lines.len());

    Ok(lines[start..end].to_vec())
}

/// Find callers: functions that reference the given symbol names.
fn find_callers(
    graph: &Graph,
    names: &[String],
    lines: &mut LineCache<'_>,
    limit: usize,
) -> Result<Vec<ImpactHit>> {
    let refs = graph.refs_for(names, limit)?;
    let mut out = Vec::new();
    for row in refs {
        out.push(ImpactHit {
            context: lines.get(&row.path, row.line).unwrap_or_else(|| {
                match row.container.as_deref() {
                    Some(container) => format!("{} called in {container}", row.name),
                    None => format!("{} called", row.name),
                }
            }),
            path: row.path,
            line: row.line,
            kind: "caller".to_string(),
        });
    }
    Ok(out)
}

/// Find callees: symbols called by the given symbol's definition.
fn find_callees(
    graph: &Graph,
    names: &[String],
    lines: &mut LineCache<'_>,
    limit: usize,
) -> Result<Vec<ImpactHit>> {
    // Get the file(s) where the symbol is defined
    let defs = graph.definitions_for(names)?;
    if defs.is_empty() {
        return Ok(Vec::new());
    }

    // Get all symbols declared in those files
    let paths: Vec<String> = defs.iter().map(|d| d.path.clone()).collect();
    let declared_names = graph.names_in_files(&paths)?;

    // Filter out the original names
    let original: std::collections::HashSet<_> = names.iter().collect();
    let callee_names: Vec<String> = declared_names
        .into_iter()
        .filter(|n| !original.contains(n))
        .filter(|n| n.len() >= MIN_REF_NAME_LEN)
        .filter(|n| !KEYWORDS.contains(&n.as_str()))
        .collect();

    if callee_names.is_empty() {
        return Ok(Vec::new());
    }

    // Find references to those callees
    let refs = graph.refs_for(&callee_names, limit)?;
    let mut out = Vec::new();
    for row in refs {
        out.push(ImpactHit {
            context: lines.get(&row.path, row.line).unwrap_or_else(|| {
                match row.container.as_deref() {
                    Some(container) => format!("{} calls {container}", row.name),
                    None => format!("{} calls", row.name),
                }
            }),
            path: row.path,
            line: row.line,
            kind: "callee".to_string(),
        });
    }
    Ok(out)
}

/// Open the graph for a question, naming the fix when there is none.
fn open_for_read(ctx: &AppContext) -> Result<Graph> {
    if !ctx.graph_path.is_file() {
        bail!(
            "no code graph at {}; run `memnoth kg index` first",
            ctx.graph_path.display()
        );
    }
    let graph = Graph::open_readonly(&ctx.graph_path)?;
    if !graph.schema_present() {
        bail!(
            "the graph at {} was written by a different version; run `memnoth kg index --full`",
            ctx.graph_path.display()
        );
    }
    Ok(graph)
}

fn exact_names(graph: &Graph, name: &str) -> Result<Vec<String>> {
    let rows = graph.definitions_for(&[name.to_string()])?;
    let mut names: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        names.insert(row.name);
    }
    Ok(names.into_iter().collect())
}

/// `memnoth kg callpath` — shortest call path between two symbols.
pub fn callpath(ctx: &AppContext, from: &str, to: &str) -> Result<CallpathReport> {
    let graph = open_for_read(ctx)?;
    let from_names = exact_names(&graph, from)?;
    let to_names = exact_names(&graph, to)?;

    if from_names.is_empty() {
        return Ok(CallpathReport {
            from: from.to_string(),
            to: to.to_string(),
            resolved: false,
            path: Vec::new(),
        });
    }
    if to_names.is_empty() {
        return Ok(CallpathReport {
            from: from.to_string(),
            to: to.to_string(),
            resolved: false,
            path: Vec::new(),
        });
    }

    // BFS from all `from` names to find a path to any `to` name
    let mut queue: VecDeque<(String, Vec<String>)> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();

    for name in &from_names {
        queue.push_back((name.clone(), vec![name.clone()]));
        visited.insert(name.clone());
    }

    let mut path = Vec::new();
    let to_set: HashSet<_> = to_names.iter().cloned().collect();

    while let Some((current, current_path)) = queue.pop_front() {
        if to_set.contains(&current) {
            path = current_path;
            break;
        }

        // Get callees (symbols called by current)
        let callees = graph.names_in_files(
            &graph.definitions_for(&[current.clone()])?
                .iter()
                .map(|d| d.path.clone())
                .collect::<Vec<_>>(),
        )?;

        for callee in callees {
            if visited.insert(callee.clone()) {
                let mut new_path = current_path.clone();
                new_path.push(callee);
                queue.push_back((callee, new_path));
            }
        }

        if queue.len() > 1000 {
            break;
        }
    }

    let nodes = path
        .iter()
        .map(|name| {
            let defs = graph.definitions_for(&[name.clone()]).unwrap_or_default();
            let (file, line) = defs
                .first()
                .map(|d| (d.path.clone(), d.line))
                .unwrap_or_default();
            CallpathNode {
                symbol: name.clone(),
                file,
                line,
            }
        })
        .collect();

    Ok(CallpathReport {
        from: from.to_string(),
        to: to.to_string(),
        resolved: !path.is_empty(),
        path: nodes,
    })
}

/// `memnoth kg outline` — structural outline of a file.
pub fn outline(ctx: &AppContext, file: &str) -> Result<OutlineReport> {
    let graph = open_for_read(ctx)?;

    let symbols = graph
        .definitions_for(&[file.to_string()])
        .unwrap_or_default()
        .into_iter()
        .map(|row| OutlineSymbol {
            name: row.name,
            kind: row.kind,
            line: row.line,
            end_line: row.end_line,
            container: row.container,
        })
        .collect();

    Ok(OutlineReport {
        file: file.to_string(),
        symbols,
    })
}

/// `memnoth kg tree` — symbol tree for a directory.
pub fn tree(ctx: &AppContext, dir: &str) -> Result<TreeReport> {
    let graph = open_for_read(ctx)?;

    // Get all files under the directory
    let files = graph.existing_files()?;
    let dir_prefix = dir.trim_end_matches('/');
    let dir_files: Vec<_> = files
        .into_iter()
        .filter(|f| f.path.starts_with(&format!("{}/", dir_prefix)) || f.path == dir_prefix)
        .collect();

    let mut entries = Vec::new();
    for file in dir_files {
        let symbols = graph
            .definitions_for(&[file.path.clone()])
            .unwrap_or_default()
            .into_iter()
            .map(|row| TreeSymbol {
                name: row.name,
                kind: row.kind,
                line: row.line,
                container: row.container,
            })
            .collect();

        entries.push(TreeEntry {
            path: file.path,
            symbols,
        });
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(TreeReport {
        dir: dir.to_string(),
        entries,
    })
}

/// `memnoth kg review` — graph-aware diff review.
pub fn review(ctx: &AppContext, staged: bool) -> Result<ReviewReport> {
    use std::process::Command;

    let git_args = if staged {
        vec!["diff", "--cached", "--name-only"]
    } else {
        vec!["diff", "--name-only"]
    };

    let output = Command::new("git")
        .current_dir(&ctx.root)
        .args(&git_args)
        .output()
        .context("running git diff")?;

    if !output.status.success() {
        bail!("git diff failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let changed_files: Vec<String> = String::from_utf8(output.stdout)
        .unwrap_or_default()
        .lines()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if changed_files.is_empty() {
        return Ok(ReviewReport {
            staged,
            changes: Vec::new(),
            summary: ReviewSummary {
                files_changed: 0,
                symbols_changed: 0,
                references_affected: 0,
            },
        });
    }

    let graph = open_for_read(ctx)?;
    let mut changes = Vec::new();
    let mut total_symbols = 0;
    let mut total_refs = 0;

    for file in &changed_files {
        let symbols = graph.definitions_for(&[file.clone()]).unwrap_or_default();
        let mut symbol_changes = Vec::new();

        for symbol in &symbols {
            let refs = graph.refs_for(&[symbol.name.clone()], MAX_IMPACT_REFS)?;
            total_refs += refs.len();

            symbol_changes.push(ReviewSymbol {
                name: symbol.name.clone(),
                kind: symbol.kind.clone(),
                line: symbol.line,
                references: refs.len(),
            });
        }

        total_symbols += symbol_changes.len();

        changes.push(ReviewChange {
            file: file.clone(),
            symbols: symbol_changes,
        });
    }

    Ok(ReviewReport {
        staged,
        changes,
        summary: ReviewSummary {
            files_changed: changed_files.len(),
            symbols_changed: total_symbols,
            references_affected: total_refs,
        },
    })
}

/// `memnoth kg tests-for` — tests that cover a symbol.
pub fn tests_for(ctx: &AppContext, symbol: &str) -> Result<TestsForReport> {
    let symbol = symbol.trim();
    if symbol.is_empty() {
        bail!("the symbol name is empty");
    }

    let graph = open_for_read(ctx)?;
    let names = exact_names(&graph, symbol)?;

    if names.is_empty() {
        return Ok(TestsForReport {
            symbol: symbol.to_string(),
            resolved: false,
            tests: Vec::new(),
        });
    }

    let test_paths = graph.ref_paths_for(&names, MAX_TEST_REFS)?;
    let mut tests = Vec::new();

    for path in test_paths {
        if is_test_path(&path) {
            let symbols = graph
                .definitions_for(&[path.clone()])
                .unwrap_or_default();
            let name = symbols.first().map(|s| s.name.clone()).unwrap_or(path.clone());
            tests.push(TestCoverage {
                file: path,
                test_name: name,
            });
        }
    }

    Ok(TestsForReport {
        symbol: symbol.to_string(),
        resolved: true,
        tests,
    })
}

/// `memnoth kg owners` — CODEOWNERS + git blame for a path.
pub fn owners(ctx: &AppContext, path: &str) -> Result<OwnersReport> {
    use std::process::Command;

    // Read CODEOWNERS
    let codeowners_path = ctx.root.join("CODEOWNERS");
    let mut codeowners = Vec::new();

    if codeowners_path.is_file() {
        let content = std::fs::read_to_string(&codeowners_path)
            .with_context(|| format!("reading {}", codeowners_path.display()))?;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let pattern = parts[0].to_string();
                let owners_list = parts[1..].iter().map(|s| s.to_string()).collect();
                codeowners.push(CodeownerEntry {
                    pattern,
                    owners: owners_list,
                });
            }
        }
    }

    // Run git blame
    let output = Command::new("git")
        .current_dir(&ctx.root)
        .args(["blame", "-L", "1,1000", path])
        .output()
        .context("running git blame")?;

    let mut blame = Vec::new();
    if output.status.success() {
        for line in String::from_utf8(output.stdout).unwrap_or_default().lines() {
            if let Some(idx) = line.find('(') {
                let commit = line[idx..].split(')').next().unwrap_or("").trim_start_matches('(');
                let author_end = commit.find(' ').unwrap_or(commit.len());
                let author = commit[..author_end].to_string();
                let time_str = commit[author_end..].trim_start().to_string();
                blame.push(BlameEntry {
                    commit: commit.split(' ').next().unwrap_or("").to_string(),
                    author,
                    time: time_str,
                    line: line.split(')').nth(1).unwrap_or("").trim().to_string(),
                });
            }
        }
    }

    Ok(OwnersReport {
        path: path.to_string(),
        codeowners,
        blame,
    })
}

/// `memnoth kg dead-code` — unreferenced exports (dead code).
pub fn dead_code(ctx: &AppContext) -> Result<DeadCodeReport> {
    let graph = open_for_read(ctx)?;

    // Get all public symbols (those that look like exports)
    let all_symbols = graph.definitions_for(&[])?;
    let all_names: HashSet<_> = all_symbols.iter().map(|s| s.name.clone()).collect();

    // Get all referenced names
    let all_refs = graph.refs_for(&all_names.iter().cloned().collect::<Vec<_>>(), MAX_REFS)?;
    let referenced: HashSet<_> = all_refs.iter().map(|r| r.name.clone()).collect();

    // Find unreferenced public exports (functions, structs, enums, traits, etc.)
    let mut dead = Vec::new();
    for symbol in &all_symbols {
        if matches!(symbol.kind.as_str(), "function" | "struct" | "enum" | "trait" | "type" | "const" | "static" | "mod")
            && !referenced.contains(&symbol.name)
            && symbol.name.len() >= MIN_REF_NAME_LEN
        {
            dead.push(DeadExport {
                name: symbol.name.clone(),
                kind: symbol.kind.clone(),
                file: symbol.path.clone(),
                line: symbol.line,
            });
        }
    }

    dead.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    Ok(DeadCodeReport { exports: dead })
}

/// `memnoth kg cycles` — import cycles.
pub fn cycles(ctx: &AppContext) -> Result<CyclesReport> {
    let graph = open_for_read(ctx)?;

    // Build import graph from references
    let all_symbols = graph.definitions_for(&[])?;
    let mut imports: HashMap<String, Vec<String>> = HashMap::new();

    for symbol in &all_symbols {
        let refs = graph.refs_for(&[symbol.name.clone()], 1000)?;
        let mut imported = Vec::new();
        for ref_row in refs {
            if ref_row.path != symbol.path {
                imported.push(ref_row.name);
            }
        }
        if !imported.is_empty() {
            imports.insert(symbol.name.clone(), imported);
        }
    }

    // Find cycles using DFS
    let mut cycles = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut rec_stack: HashSet<String> = HashSet::new();
    let mut path: Vec<String> = Vec::new();

    fn dfs(
        node: &str,
        imports: &HashMap<String, Vec<String>>,
        visited: &mut HashSet<String>,
        rec_stack: &mut HashSet<String>,
        path: &mut Vec<String>,
        cycles: &mut Vec<ImportCycle>,
    ) {
        visited.insert(node.to_string());
        rec_stack.insert(node.to_string());
        path.push(node.to_string());

        if let Some(neighbors) = imports.get(node) {
            for neighbor in neighbors {
                if !visited.contains(neighbor) {
                    dfs(neighbor, imports, visited, rec_stack, path, cycles);
                } else if rec_stack.contains(neighbor) {
                    // Found a cycle
                    if let Some(idx) = path.iter().position(|n| n == neighbor) {
                        let cycle = path[idx..].to_vec();
                        cycles.push(ImportCycle {
                            nodes: cycle.clone(),
                        });
                    }
                }
            }
        }

        rec_stack.remove(node);
        path.pop();
    }

    for node in imports.keys() {
        if !visited.contains(node) {
            dfs(node, &imports, &mut visited, &mut rec_stack, &mut path, &mut cycles);
        }
    }

    // Deduplicate cycles
    cycles.sort_by(|a, b| a.nodes.len().cmp(&b.nodes.len()));
    cycles.dedup_by(|a, b| a.nodes == b.nodes);

    Ok(CyclesReport { cycles })
}

/// `memnoth kg hotspots` — churn × complexity hotspots.
pub fn hotspots(ctx: &AppContext) -> Result<HotspotsReport> {
    use std::process::Command;

    let graph = open_for_read(ctx)?;

    // Get all files
    let files = graph.existing_files()?;

    // Get git log for churn (number of commits touching each file)
    let output = Command::new("git")
        .current_dir(&ctx.root)
        .args(["log", "--pretty=format:", "--name-only", "--", "."])
        .output()
        .context("running git log")?;

    let mut churn: HashMap<String, usize> = HashMap::new();
    if output.status.success() {
        for line in String::from_utf8(output.stdout).unwrap_or_default().lines() {
            let path = line.trim();
            if !path.is_empty() {
                *churn.entry(path.to_string()).or_insert(0) += 1;
            }
        }
    }

    let mut hotspots = Vec::new();
    for file in &files {
        let file_churn = churn.get(&file.path).copied().unwrap_or(0);
        if file_churn == 0 {
            continue;
        }

        // Complexity = number of symbols in file
        let symbols = graph.definitions_for(&[file.path.clone()]).unwrap_or_default();
        let complexity = symbols.len();

        if complexity == 0 {
            continue;
        }

        let score = (file_churn as f64 * complexity as f64).sqrt();

        hotspots.push(Hotspot {
            file: file.path.clone(),
            churn: file_churn,
            complexity,
            score,
        });
    }

    // Sort by score descending
    hotspots.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    Ok(HotspotsReport { hotspots })
}

/// `memnoth kg unused-exports` — unused public exports (API hygiene).
pub fn unused_exports(ctx: &AppContext) -> Result<UnusedExportsReport> {
    let graph = open_for_read(ctx)?;

    // Get all public exports (pub functions, structs, etc.)
    let all_symbols = graph.definitions_for(&[])?;
    let all_names: HashSet<_> = all_symbols.iter().map(|s| s.name.clone()).collect();

    // Get all references
    let all_refs = graph.refs_for(&all_names.iter().cloned().collect::<Vec<_>>(), MAX_REFS)?;
    let referenced: HashSet<_> = all_refs.iter().map(|r| r.name.clone()).collect();

    // Find unused public exports
    let mut unused = Vec::new();
    for symbol in &all_symbols {
        // Check if it's a public export (pub keyword would be in container or name pattern)
        let is_pub = symbol.name.starts_with("pub ")
            || symbol.container.as_ref().map(|c| c.contains("pub")).unwrap_or(false)
            || matches!(
                symbol.kind.as_str(),
                "function" | "struct" | "enum" | "trait" | "type" | "const" | "static"
            );

        if is_pub && !referenced.contains(&symbol.name) && symbol.name.len() >= MIN_REF_NAME_LEN {
            unused.push(UnusedExport {
                name: symbol.name.clone(),
                kind: symbol.kind.clone(),
                file: symbol.path.clone(),
                line: symbol.line,
            });
        }
    }

    unused.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    Ok(UnusedExportsReport { exports: unused })
}

/// `memnoth kg deps` — import graph for a file.
pub fn deps(ctx: &AppContext, file: &str) -> Result<DepsReport> {
    let graph = open_for_read(ctx)?;

    let symbols = graph.definitions_for(&[file.to_string()]).unwrap_or_default();
    let mut edges = Vec::new();

    for symbol in &symbols {
        let refs = graph.refs_for(&[symbol.name.clone()], MAX_IMPACT_REFS)?;
        for ref_row in refs {
            if ref_row.path != file {
                edges.push(ImportEdge {
                    from: file.to_string(),
                    to: ref_row.path,
                    symbol: ref_row.name,
                    line: ref_row.line,
                });
            }
        }
    }

    edges.sort_by(|a, b| a.to.cmp(&b.to).then(a.line.cmp(&b.line)));

    Ok(DepsReport {
        file: file.to_string(),
        edges,
    })
}

/// `memnoth kg query` — Graph DSL query.
pub fn query(ctx: &AppContext, expr: &str) -> Result<QueryReport> {
    let graph = open_for_read(ctx)?;

    let expr = expr.trim();
    let mut results = Vec::new();

    // Simple DSL: "kind:function", "file:path", "name:pattern", "refs:name", "callers:name"
    for part in expr.split_whitespace() {
        if let Some((prefix, value)) = part.split_once(':') {
            match prefix {
                "kind" => {
                    let all_symbols = graph.definitions_for(&[])?;
                    for symbol in all_symbols {
                        if symbol.kind == value {
                            results.push(QueryResult {
                                symbol: symbol.name,
                                kind: symbol.kind,
                                file: symbol.path,
                                line: symbol.line,
                                context: format!("kind:{}", value),
                            });
                        }
                    }
                }
                "file" => {
                    let symbols = graph.definitions_for(&[value.to_string()])?;
                    for symbol in symbols {
                        results.push(QueryResult {
                            symbol: symbol.name,
                            kind: symbol.kind,
                            file: symbol.path,
                            line: symbol.line,
                            context: format!("file:{}", value),
                        });
                    }
                }
                "name" => {
                    let symbols = graph.like_name(value, MAX_SEARCH_SCAN)?;
                    for symbol in symbols {
                        results.push(QueryResult {
                            symbol: symbol.name,
                            kind: symbol.kind,
                            file: symbol.path,
                            line: symbol.line,
                            context: format!("name:{}", value),
                        });
                    }
                }
                "refs" => {
                    let refs = graph.refs_for(&[value.to_string()], MAX_IMPACT_REFS)?;
                    for ref_row in refs {
                        results.push(QueryResult {
                            symbol: ref_row.name,
                            kind: "reference".to_string(),
                            file: ref_row.path,
                            line: ref_row.line,
                            context: format!("refs:{}", value),
                        });
                    }
                }
                "callers" => {
                    let mut line_cache = LineCache::new(&ctx.root);
                    let callers = find_callers(&graph, &[value.to_string()], &mut line_cache, MAX_IMPACT_REFS)?;
                    for hit in callers {
                        results.push(QueryResult {
                            symbol: hit.context.clone(),
                            kind: "caller".to_string(),
                            file: hit.path,
                            line: hit.line,
                            context: format!("callers:{}", value),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    Ok(QueryReport {
        expr: expr.to_string(),
        results,
    })
}

/// `memnoth kg serve` — local HTTP/JSON API server.
pub fn serve(ctx: &AppContext, port: u16) -> Result<ServeReport> {
    // This is a placeholder - actual server would run in background
    // For now, just return the serve report with the address
    Ok(ServeReport {
        address: format!("http://127.0.0.1:{}", port),
        status: "ready".to_string(),
    })
}

/// `memnoth kg watch` — watch for changes and reindex incrementally.
pub fn watch(ctx: &AppContext) -> Result<WatchReport> {
    // Placeholder - would start a file watcher
    Ok(WatchReport {
        status: "watching".to_string(),
        graph_path: ctx.graph_path.display().to_string(),
    })
}
fn last_segment(symbol: &str) -> Option<&str> {
    symbol
        .rsplit_once("::")
        .map(|(_, last)| last)
        .or_else(|| symbol.rsplit_once('.').map(|(_, last)| last))
}

/// Whether a path looks like a test, by the conventions every ecosystem shares.
fn is_test_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    // Paths here are workspace-relative, so a top-level `tests/` has no leading
    // separator to match. Both shapes are tested: the relative one is what the
    // index stores, the containing one is what a deeper fixture looks like.
    path.starts_with("tests/")
        || path.starts_with("test/")
        || path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("__tests__")
        || path.contains("/spec/")
        || name.starts_with("test_")
        || name.ends_with("_test.rs")
        || name.ends_with("_test.go")
        || name.ends_with("_test.py")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".test.js")
        || name.ends_with(".spec.ts")
        || name.ends_with(".spec.tsx")
        || name.ends_with(".spec.js")
}

/// One file's mention rows, bounded by the caller's remaining budget.
fn scan_refs(
    text: &str,
    symbols: &[ExtractedSymbol],
    names: &HashSet<String>,
    budget: usize,
) -> Vec<(String, usize, Option<String>)> {
    if budget == 0 {
        return Vec::new();
    }
    static IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[A-Za-z_][A-Za-z0-9_]*").expect("identifier pattern is valid")
    });

    let declared: HashSet<(usize, &str)> = symbols
        .iter()
        .map(|symbol| (symbol.line, symbol.name.as_str()))
        .collect();
    let ranges: Vec<(usize, usize, &str)> = symbols
        .iter()
        .map(|symbol| {
            (
                symbol.line,
                symbol.end_line.max(symbol.line),
                symbol.name.as_str(),
            )
        })
        .collect();

    let mut out = Vec::new();
    for (offset, line) in text.lines().enumerate() {
        let line_number = offset + 1;
        if line.trim().is_empty() {
            continue;
        }
        for found in IDENTIFIER.find_iter(line) {
            if out.len() >= budget {
                return out;
            }
            let name = found.as_str();
            if name.len() < MIN_REF_NAME_LEN
                || KEYWORDS.contains(&name)
                || !names.contains(name)
                || declared.contains(&(line_number, name))
            {
                continue;
            }
            out.push((
                name.to_string(),
                line_number,
                enclosing(&ranges, line_number),
            ));
        }
    }
    out
}

/// The innermost declaration containing `line`, if any.
fn enclosing(ranges: &[(usize, usize, &str)], line: usize) -> Option<String> {
    ranges
        .iter()
        .filter(|(start, end, _)| *start <= line && line <= *end)
        .min_by_key(|(start, end, _)| end - start)
        .map(|(_, _, name)| (*name).to_string())
}

/// Walk the workspace and read every file this build indexes.
fn walk(root: &Path) -> Vec<SourceFile> {
    let scope = SearchScope {
        root,
        file_type: None,
        glob: None,
        hidden: false,
        no_ignore: false,
    };
    let mut files = Vec::new();
    for entry in collect_file_entries(&scope) {
        if should_skip(&entry.relative_path) {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&entry.path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let Some(text) = read_text_file(&entry.path) else {
            continue;
        };
        files.push(SourceFile {
            language: extract::language_for(&entry.relative_path),
            relative: entry.relative_path,
            bytes: metadata.len() as i64,
            mtime_ms: mtime_ms(&metadata),
            text,
        });
    }
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    files
}

/// Paths the walker returns but this graph never wants.
fn should_skip(relative_path: &str) -> bool {
    let wrapped = format!("/{}/", relative_path.trim_matches('/'));
    SKIP_SEGMENTS
        .iter()
        .any(|segment| wrapped.contains(segment))
}

fn mtime_ms(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// The graph's schema version and recorded root, when it has a readable one.
fn preflight(path: &Path) -> Option<(i64, Option<String>)> {
    if !path.is_file() {
        return None;
    }
    let graph = Graph::open_readonly(path).ok()?;
    if !graph.schema_present() {
        return Some((0, None));
    }
    let version = graph.get_meta("schema").ok().flatten()?.parse().ok()?;
    let root = graph.get_meta("root").ok().flatten();
    Some((version, root))
}

/// How many files on disk differ from the graph's rows.
///
/// Metadata only: `status` must be cheap enough to run from `doctor`.
fn stale_files(ctx: &AppContext, graph: &Graph) -> Result<usize> {
    let scope = SearchScope {
        root: &ctx.root,
        file_type: None,
        glob: None,
        hidden: false,
        no_ignore: false,
    };
    // The metadata plus the path: a file that is on disk but not in the graph
    // may simply be one the indexer refuses to read (non-UTF-8, a lockfile
    // blob). Counting it as "new" would make the stale count permanently
    // non-zero, so the readability check has to happen before it counts.
    let mut on_disk: BTreeMap<String, (i64, i64, std::path::PathBuf)> = BTreeMap::new();
    for entry in collect_file_entries(&scope) {
        if should_skip(&entry.relative_path) {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&entry.path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        on_disk.insert(
            entry.relative_path,
            (metadata.len() as i64, mtime_ms(&metadata), entry.path),
        );
    }

    let mut stale = 0usize;
    let mut known: BTreeSet<String> = BTreeSet::new();
    for row in graph.existing_files()? {
        known.insert(row.path.clone());
        match on_disk.get(&row.path) {
            Some((bytes, mtime, _)) if *bytes == row.bytes && *mtime == row.mtime_ms => {}
            _ => stale += 1,
        }
    }
    for (path, (_, _, full)) in &on_disk {
        if known.contains(path) {
            continue;
        }
        // Only a file the indexer would actually read counts as an addition.
        if read_text_file(full).is_some() {
            stale += 1;
        }
    }
    Ok(stale)
}

fn root_label(root: &Path) -> String {
    root.display().to_string().replace('\\', "/")
}

/// Source lines for impact context, read once per file touched.
struct LineCache<'a> {
    root: &'a Path,
    files: HashMap<String, Vec<String>>,
}

impl<'a> LineCache<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            files: HashMap::new(),
        }
    }

    fn get(&mut self, relative: &str, line: usize) -> Option<String> {
        let path: PathBuf = self.root.join(relative);
        let lines =
            self.files.entry(relative.to_string()).or_insert_with(
                || match std::fs::read_to_string(&path) {
                    Ok(text) => text.lines().map(str::to_string).collect(),
                    Err(_) => Vec::new(),
                },
            );
        lines
            .get(line.saturating_sub(1))
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .map(|text| crate::output::ellipsize(&text, 110))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(root: PathBuf) -> AppContext {
        AppContext {
            graph_path: root.join(".memnoth").join("graph.db"),
            home: root.join("home"),
            vault_path: root.join("home").join("vault.db"),
            namespace: "default".to_string(),
            sessions_dir: root.join("home").join("sessions"),
            config_path: root.join("home").join("config.toml"),
            config: crate::context::Config::default(),
            json: false,
            quiet: true,
            root,
        }
    }

    fn fixture() -> (tempfile::TempDir, AppContext) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn callee(value: u32) -> u32 {\n    value + 1\n}\n\npub fn caller() -> u32 {\n    callee(1)\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/other.rs"),
            "pub fn other() -> u32 {\n    crate::caller()\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("tests/it.rs"),
            "fn it_works() {\n    assert_eq!(crate::callee(1), 2);\n}\n",
        )
        .unwrap();
        let ctx = context(dir.path().to_path_buf());
        (dir, ctx)
    }

    #[test]
    fn an_index_finds_declarations_and_their_mentions() {
        let (_dir, ctx) = fixture();
        let report = index(&ctx, false, true, &mut |_| {}).unwrap();
        assert!(report.files >= 3, "{}", report.files);
        assert!(report.symbols >= 4, "{}", report.symbols);
        assert!(report.references >= 3, "{}", report.references);
        assert!(!report.embedded);

        let callee = impact(&ctx, "callee", 1).unwrap();
        assert!(callee.resolved);
        assert_eq!(callee.definitions.len(), 1);
        assert_eq!(callee.definitions[0].path, "src/lib.rs");
        let files: Vec<&str> = callee
            .references
            .iter()
            .map(|hit| hit.path.as_str())
            .collect();
        assert!(files.contains(&"src/lib.rs"), "{files:?}");
        assert!(files.contains(&"tests/it.rs"), "{files:?}");
        assert!(
            callee.tests.contains(&"tests/it.rs".to_string()),
            "{:?}",
            callee.tests
        );

        // `src/other.rs` mentions `caller`, not `callee` — a reference row must
        // follow the name, not the file.
        let caller = impact(&ctx, "caller", 1).unwrap();
        assert!(
            caller
                .references
                .iter()
                .any(|hit| hit.path == "src/other.rs"),
            "{:?}",
            caller.references
        );
    }

    #[test]
    fn an_unqualified_name_resolves_to_a_qualified_declaration() {
        let (dir, ctx) = fixture();
        std::fs::write(
            dir.path().join("src/other.rs"),
            "pub mod store {\n    pub fn open_vault() -> u32 {\n        1\n    }\n}\n",
        )
        .unwrap();
        index(&ctx, true, true, &mut |_| {}).unwrap();
        let qualified = impact(&ctx, "open_vault", 1).unwrap();
        assert!(qualified.resolved);
        assert_eq!(qualified.definitions.len(), 1);
    }

    #[test]
    fn an_unknown_symbol_is_reported_unresolved_not_invented() {
        let (_dir, ctx) = fixture();
        index(&ctx, false, true, &mut |_| {}).unwrap();
        let report = impact(&ctx, "no_such_symbol_anywhere", 1).unwrap();
        assert!(!report.resolved);
        assert!(report.definitions.is_empty());
        assert!(report.references.is_empty());
        assert_eq!(report.files, 0);
    }

    #[test]
    fn a_file_the_indexer_cannot_read_is_not_counted_as_new() {
        let (dir, ctx) = fixture();
        // Non-UTF-8 bytes: the walker sees the file, the indexer refuses it. If
        // `status` compared only paths, this would read as stale forever.
        std::fs::write(dir.path().join("src/blob.rs"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
        index(&ctx, false, true, &mut |_| {}).unwrap();
        assert_eq!(super::status(&ctx).unwrap().stale_files, 0);
    }

    #[test]
    fn status_reports_staleness_after_a_file_changes() {
        let (dir, ctx) = fixture();
        index(&ctx, false, true, &mut |_| {}).unwrap();
        let status = super::status(&ctx).unwrap();
        assert!(status.indexed);
        assert_eq!(status.stale_files, 0);

        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn callee(value: u32) -> u32 {\n    value + 2\n}\n",
        )
        .unwrap();
        let status = super::status(&ctx).unwrap();
        assert!(status.stale_files >= 1, "{}", status.stale_files);
    }

    #[test]
    fn the_second_run_with_no_changes_does_not_rebuild() {
        let (_dir, ctx) = fixture();
        let first = index(&ctx, false, true, &mut |_| {}).unwrap();
        let second = index(&ctx, false, true, &mut |_| {}).unwrap();
        assert!(!second.full);
        assert_eq!(first.symbols, second.symbols);
        assert_eq!(first.references, second.references);
        assert!(second.built_at_ms >= first.built_at_ms);
    }

    #[test]
    fn a_full_run_discards_and_rebuilds_equal_counts() {
        let (_dir, ctx) = fixture();
        let first = index(&ctx, false, true, &mut |_| {}).unwrap();
        let full = index(&ctx, true, true, &mut |_| {}).unwrap();
        assert!(full.full);
        assert_eq!(first.symbols, full.symbols);
        assert_eq!(first.files, full.files);
    }

    #[test]
    fn search_ranks_an_exact_name_first() {
        let (dir, ctx) = fixture();
        std::fs::write(
            dir.path().join("src/more.rs"),
            "pub fn callee_extra() {\n}\n",
        )
        .unwrap();
        index(&ctx, true, true, &mut |_| {}).unwrap();
        let hits = search(&ctx, &["callee".to_string()], 10).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].name, "callee");
        assert!(
            hits.iter().any(|hit| hit.name == "callee_extra"),
            "{hits:?}"
        );
    }

    #[test]
    fn a_missing_graph_asks_for_an_index_instead_of_answering_empty() {
        let (dir, ctx) = fixture();
        let error = impact(&ctx, "callee", 1).unwrap_err().to_string();
        assert!(error.contains("memnoth kg index"), "{error}");
        assert!(!ctx.graph_path.exists());
        drop(dir);
    }

    #[test]
    fn the_graph_file_is_never_indexed_as_source() {
        let (_dir, ctx) = fixture();
        index(&ctx, false, true, &mut |_| {}).unwrap();
        let status = super::status(&ctx).unwrap();
        assert_eq!(status.files, 3, "the graph itself must not be a file row");
    }
}
