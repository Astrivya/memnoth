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
    render_callees, render_callers, render_callpath, render_cycles, render_dead_code, render_def,
    render_deps, render_diff_impact, render_explore, render_hotspots, render_impact, render_index,
    render_outline, render_owners, render_query, render_refs, render_review, render_search,
    render_status, render_tests_for, render_tree, render_unused_exports,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Serialize;

use crate::context::AppContext;
use extract::{ExtractedSymbol, KEYWORDS};
use memnoth_codegraph::impact::{RiskInputs, RiskScore};
use memnoth_codegraph::precision::{Precision, best_available};
use memnoth_search::workspace::{SearchScope, collect_file_entries, read_text_file};
use store::{FileRow, Graph, SymbolRow};

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
    /// `definition`, `call`, `reference` or `caller`.
    ///
    /// `call` and `reference` are the same relationship seen at different
    /// confidence: a mention whose source line reads as `name(…)` is what a
    /// person means by "this calls that", and everything else (a type position,
    /// a re-export, prose in a doc comment) is an incidental mention. The test
    /// is lexical — see [`is_call_shaped`] — so `call` is evidence of a call
    /// site, not proof of a resolved edge.
    pub kind: String,
    /// The source line, trimmed, or a description when it could not be read.
    pub context: String,
    /// Whether this row's source line is call-shaped.
    ///
    /// Kept beside `kind` because a caller wants to *filter* on it without
    /// string-matching a label: `kind` is presentation, this is the fact.
    pub call_shaped: bool,
}

/// What depends on a symbol.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactReport {
    /// The name that was asked for.
    pub symbol: String,
    /// Whether any declaration matched it.
    pub resolved: bool,
    /// How the edges behind this answer were produced.
    pub precision: Precision,
    /// Declarations for the resolved name(s).
    pub definitions: Vec<ImpactHit>,
    /// Mentions outside the declaration line.
    pub references: Vec<ImpactHit>,
    /// Of `references`, the ones that name this symbol directly. The rest are
    /// the optional second hop, which is a much weaker claim and is counted
    /// apart so a 2-reference symbol cannot read as 380.
    pub references_direct: usize,
    /// Of `references`, the ones reached through a symbol declared in a file
    /// that mentions this one (`0` at `--depth 1`).
    pub references_transitive: usize,
    /// Distinct files involved (definitions + references).
    pub files: usize,
    /// Referencing files that look like tests.
    pub tests: Vec<String>,
    /// Whether the reference list hit its cap.
    pub truncated: bool,
    /// `fan_in × churn(90d) × (1 − coverage)`, with every input beside it.
    ///
    /// `None` when git cannot count the churn (no repository, no git): a score
    /// with a guessed input would be a number nobody could check.
    pub risk: Option<RiskScore>,
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
    /// The symbol the path starts at.
    pub from: String,
    /// The symbol the path should reach.
    pub to: String,
    /// Whether a path was found. False when either name does not resolve.
    pub resolved: bool,
    /// The symbols along the path, in order.
    pub path: Vec<CallpathNode>,
    /// Edges between the first and last node.
    pub hops: usize,
}

/// One node in the call path.
#[derive(Debug, Clone, Serialize)]
pub struct CallpathNode {
    /// Symbol name at this hop.
    pub symbol: String,
    /// Declaration kind, or empty when the name was only mentioned.
    pub kind: String,
    /// File declaring it.
    pub file: String,
    /// Line of the declaration.
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
    /// The revision the review was taken against, when one was named.
    ///
    /// Carried because the numbers below it only mean something next to the
    /// question that produced them: "12 files" is a different claim under
    /// `--base main` than it is for the working tree. `null` when no revision
    /// was named, like every other optional field in this report.
    pub base: Option<String>,
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
    /// Mentions of this symbol outside its declaration line.
    pub references: usize,
}

/// Tests that cover a symbol.
#[derive(Debug, Clone, Serialize)]
pub struct TestsForReport {
    /// The name that was asked for.
    pub symbol: String,
    /// Whether any declaration matched it.
    pub resolved: bool,
    /// How the edges behind this answer were produced.
    pub precision: Precision,
    /// Tests that mention the symbol.
    pub tests: Vec<TestCoverage>,
    /// How many test files were found.
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

/// A file the walker kept, with its text already read.
struct SourceFile {
    relative: String,
    language: &'static str,
    bytes: i64,
    mtime_ms: i64,
    /// Where to read it from, when a decision says the text is needed.
    path: PathBuf,
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
    progress(&format!(
        "walked {} file(s) in {:.2}s",
        files.len(),
        started.elapsed().as_secs_f64()
    ));
    let mut phase = Instant::now();

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
    let mut texts: HashMap<String, String> = HashMap::new();
    for file in &changed {
        // A file that is not valid UTF-8 is skipped exactly as the old eager
        // read skipped it: it simply never becomes a row.
        let Some(text) = read_text_file(&file.path) else {
            continue;
        };
        extracted.insert(
            file.relative.clone(),
            extract::extract(file.language, &text),
        );
        texts.insert(file.relative.clone(), text);
    }

    progress(&format!(
        "parsed {} file(s) in {:.2}s",
        extracted.len(),
        phase.elapsed().as_secs_f64()
    ));
    phase = Instant::now();

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
    let added_names: Vec<String> = introduced.difference(&existing_names).cloned().collect();
    let scan_all = rebuild || !added_names.is_empty();

    if scan_all {
        for file in &files {
            if extracted.contains_key(&file.relative) {
                continue;
            }
            let Some(text) = read_text_file(&file.path) else {
                continue;
            };
            // An unchanged file can only carry a mention of a name this run
            // introduced if it already spelled that word. Skipping the rest is
            // what keeps a one-symbol edit from re-parsing a monorepo.
            if !added_names.iter().any(|name| text.contains(name.as_str())) {
                continue;
            }
            extracted.insert(
                file.relative.clone(),
                extract::extract(file.language, &text),
            );
            texts.insert(file.relative.clone(), text);
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
            // Mentions are evidence only from files whose declarations this build
            // can read. Markdown prose, CSS selectors, JSON schemas and lockfiles
            // were producing a third of the reference table on a normal frontend
            // repo, and every one of them surfaced as an `impact` row for a word
            // that merely appeared in a document.
            if !extract::has_scanner(file.language) || is_generated(&file.relative) {
                continue;
            }
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
            let Some(text) = texts.get(&file.relative) else {
                continue;
            };
            let refs = scan_refs(text, symbols, &names, budget);
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

    progress(&format!(
        "wrote the graph in {:.2}s",
        phase.elapsed().as_secs_f64()
    ));
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
            precision: Precision::Lexical,
            definitions: Vec::new(),
            references: Vec::new(),
            references_direct: 0,
            references_transitive: 0,
            files: 0,
            tests: Vec::new(),
            truncated: false,
            risk: None,
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
            // A declaration names the symbol; it never calls it.
            call_shaped: false,
        })
        .collect();

    let mut references: Vec<ImpactHit> = graph
        .refs_for(&names, MAX_IMPACT_REFS)?
        .into_iter()
        .map(|row| {
            let context =
                lines
                    .get(&row.path, row.line)
                    .unwrap_or_else(|| match row.container.as_deref() {
                        Some(container) => format!("{} mentioned in {container}", row.name),
                        None => format!("{} mentioned", row.name),
                    });
            let call = lines
                .raw(&row.path, row.line)
                .is_some_and(|source| is_call_shaped(&source, &row.name));
            ImpactHit {
                context,
                path: row.path,
                line: row.line,
                kind: if call { "call" } else { "reference" }.to_string(),
                call_shaped: call,
            }
        })
        .collect();
    let mut truncated = references.len() >= MAX_IMPACT_REFS;
    // Everything built so far names the symbol itself; the hop below does not.
    let direct = references.len();

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
            let call = lines
                .raw(&row.path, row.line)
                .is_some_and(|source| is_call_shaped(&source, &row.name));
            references.push(ImpactHit {
                context: lines
                    .get(&row.path, row.line)
                    .map(|source| format!("depth 2 · {source}"))
                    .unwrap_or_else(|| format!("depth 2 · {} mentioned", row.name)),
                path: row.path,
                line: row.line,
                // A second-hop row is a mention of some *other* declaration, so
                // it stays `reference`; `call_shaped` still describes the line,
                // and a caller reading it must keep both hops apart by index
                // (the first `references_direct` rows are this symbol's own).
                kind: "reference".to_string(),
                call_shaped: call,
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

    let definition_paths: Vec<String> = definitions.iter().map(|hit| hit.path.clone()).collect();
    let risk = risk_for(&ctx.root, &definition_paths, direct);

    Ok(ImpactReport {
        symbol: symbol.to_string(),
        resolved: true,
        precision: best_available(),
        definitions,
        references_direct: direct,
        references_transitive: references.len().saturating_sub(direct),
        references,
        files: files.len(),
        tests,
        truncated,
        risk,
    })
}

/// Commits touching `paths` in the last 90 days.
///
/// `None` when git cannot answer: a workspace outside a repository, or a
/// machine without git. An unmeasurable input is left out rather than counted
/// as zero, which would report a busy file as stable.
fn churn_90d(root: &Path, paths: &[String]) -> Option<u32> {
    if paths.is_empty() {
        return Some(0);
    }
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["log", "--since=90.days", "--format=%H", "--"]);
    for path in paths {
        command.arg(path);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.lines().filter(|line| !line.trim().is_empty()).count() as u32)
}

/// Coverage per file from `.memnoth/coverage.json`.
///
/// The document is `{"files": {"src/a.rs": 0.83}}`, the shape coverage tools
/// already emit after a path rewrite. A file missing from it counts as zero, so
/// the score errs toward caution rather than silence.
fn coverage_for(root: &Path, paths: &[String]) -> f64 {
    let path = root.join(".memnoth").join("coverage.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0.0;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 0.0;
    };
    let Some(files) = value.get("files").and_then(|files| files.as_object()) else {
        return 0.0;
    };
    let mut worst = 1.0_f64;
    for file in paths {
        let fraction = files
            .get(file)
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0);
        worst = worst.min(fraction.clamp(0.0, 1.0));
    }
    worst
}

fn risk_for(root: &Path, definitions: &[String], fan_in: usize) -> Option<RiskScore> {
    let churn_90d = churn_90d(root, definitions)?;
    Some(RiskScore::compute(RiskInputs {
        fan_in,
        churn_90d,
        test_coverage: coverage_for(root, definitions),
    }))
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
    let references: Vec<ImpactHit> = graph
        .refs_for(&names, MAX_IMPACT_REFS)?
        .into_iter()
        .map(|row| {
            let context =
                lines
                    .get(&row.path, row.line)
                    .unwrap_or_else(|| match row.container.as_deref() {
                        Some(container) => format!("{} mentioned in {container}", row.name),
                        None => format!("{} mentioned", row.name),
                    });
            let call = lines
                .raw(&row.path, row.line)
                .is_some_and(|source| is_call_shaped(&source, &row.name));
            ImpactHit {
                context,
                path: row.path,
                line: row.line,
                kind: if call { "call" } else { "reference" }.to_string(),
                call_shaped: call,
            }
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
        .map(|row| {
            let context =
                lines
                    .get(&row.path, row.line)
                    .unwrap_or_else(|| match row.container.as_deref() {
                        Some(container) => format!("{} mentioned in {container}", row.name),
                        None => format!("{} mentioned", row.name),
                    });
            let call = lines
                .raw(&row.path, row.line)
                .is_some_and(|source| is_call_shaped(&source, &row.name));
            ImpactHit {
                context,
                path: row.path,
                line: row.line,
                kind: if call { "call" } else { "reference" }.to_string(),
                call_shaped: call,
            }
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

/// The error a failed `git diff` becomes.
///
/// Outside a work tree git writes its entire usage text to stderr; pasting that
/// into the answer buries the one fact that matters.
fn git_diff_error(root: &Path, stderr: &[u8]) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(stderr);
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("git diff failed");
    if first.to_lowercase().contains("not a git repository") {
        anyhow::anyhow!(
            "not a git repository: {} — this verb reads `git diff`",
            root.display()
        )
    } else {
        anyhow::anyhow!("git diff failed: {first}")
    }
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
        return Err(git_diff_error(&ctx.root, &output.stderr));
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
                .map(|row| {
                    let context = lines.get(&row.path, row.line).unwrap_or_else(|| {
                        match row.container.as_deref() {
                            Some(container) => format!("{} mentioned in {container}", row.name),
                            None => format!("{} mentioned", row.name),
                        }
                    });
                    let call = lines
                        .raw(&row.path, row.line)
                        .is_some_and(|source| is_call_shaped(&source, &row.name));
                    ImpactHit {
                        context,
                        path: row.path,
                        line: row.line,
                        kind: if call { "call" } else { "reference" }.to_string(),
                        call_shaped: call,
                    }
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
        let context =
            lines
                .get(&row.path, row.line)
                .unwrap_or_else(|| match row.container.as_deref() {
                    Some(container) => format!("{} called in {container}", row.name),
                    None => format!("{} called", row.name),
                });
        let call = lines
            .raw(&row.path, row.line)
            .is_some_and(|source| is_call_shaped(&source, &row.name));
        out.push(ImpactHit {
            context,
            path: row.path,
            line: row.line,
            kind: "caller".to_string(),
            call_shaped: call,
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
        let context =
            lines
                .get(&row.path, row.line)
                .unwrap_or_else(|| match row.container.as_deref() {
                    Some(container) => format!("{} calls {container}", row.name),
                    None => format!("{} calls", row.name),
                });
        let call = lines
            .raw(&row.path, row.line)
            .is_some_and(|source| is_call_shaped(&source, &row.name));
        out.push(ImpactHit {
            context,
            path: row.path,
            line: row.line,
            kind: "callee".to_string(),
            call_shaped: call,
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
            hops: 0,
        });
    }
    if to_names.is_empty() {
        return Ok(CallpathReport {
            from: from.to_string(),
            to: to.to_string(),
            resolved: false,
            path: Vec::new(),
            hops: 0,
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

        // Callees are the declarations this symbol's own body mentions. Asking
        // for every symbol in the defining *file* — what this did — missed every
        // call that crossed a file and reported sibling declarations as calls.
        let callees = graph.callees_of(&current)?;

        for callee in callees {
            if visited.insert(callee.clone()) {
                let mut new_path = current_path.clone();
                new_path.push(callee.clone());
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
            let defs = graph
                .definitions_for(std::slice::from_ref(name))
                .unwrap_or_default();
            let (file, line) = defs
                .first()
                .map(|d| (d.path.clone(), d.line))
                .unwrap_or_default();
            CallpathNode {
                symbol: name.clone(),
                kind: defs.first().map(|d| d.kind.clone()).unwrap_or_default(),
                file,
                line,
            }
        })
        .collect();

    Ok(CallpathReport {
        from: from.to_string(),
        to: to.to_string(),
        resolved: !path.is_empty(),
        hops: path.len().saturating_sub(1),
        path: nodes,
    })
}

/// `memnoth kg outline` — structural outline of a file.
///
/// Symbols are looked up by *path*. They used to be looked up by name, which is
/// the wrong column: the argument is a file, so every call answered "no symbols"
/// for a file the graph had 30 rows for. A suffix (`landing/sections.tsx`) is
/// accepted when it matches at least one stored file, because that is how someone
/// refers to a file they are looking at rather than typing out.
pub fn outline(ctx: &AppContext, file: &str) -> Result<OutlineReport> {
    let graph = open_for_read(ctx)?;
    let wanted = workspace_relative(&ctx.root, file);
    let paths = match_paths(&graph, &wanted);

    let rows = graph.symbols_in_paths(&paths).unwrap_or_default();
    let mut taken = vec![false; rows.len()];
    let symbols = nest_outline(&rows, &mut taken, None, 0);

    Ok(OutlineReport {
        file: if paths.len() == 1 {
            paths[0].clone()
        } else {
            wanted
        },
        language: extract::language_for(&graph_path_label(&paths, file)).to_string(),
        symbols,
    })
}

/// The stored paths a user-supplied file argument means.
///
/// Exact match first; then suffix, which is how `landing/sections.tsx` finds
/// `src/components/memnoth/landing/sections.tsx`. An ambiguous suffix returns
/// every match rather than picking one arbitrarily.
fn match_paths(graph: &Graph, wanted: &str) -> Vec<String> {
    let exact = wanted
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string();
    let Ok(files) = graph.existing_files() else {
        return vec![exact];
    };
    if files.iter().any(|row| row.path == exact) {
        return vec![exact];
    }
    let suffix = format!("/{}", exact.trim_start_matches('/'));
    let matched: Vec<String> = files
        .into_iter()
        .map(|row| row.path)
        .filter(|path| path.ends_with(&suffix))
        .collect();
    if matched.is_empty() {
        vec![exact]
    } else {
        matched
    }
}

fn graph_path_label(paths: &[String], fallback: &str) -> String {
    paths
        .first()
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

/// Build the outline tree: a declaration becomes a child of whatever container
/// is open at its line. `depth` is bounded because a scanner that mis-pairs one
/// brace can produce a cycle (a symbol whose container is itself).
fn nest_outline(
    rows: &[SymbolRow],
    taken: &mut [bool],
    parent: Option<&str>,
    depth: usize,
) -> Vec<OutlineSymbol> {
    const MAX_DEPTH: usize = 8;
    if depth > MAX_DEPTH {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if taken[index] {
            continue;
        }
        let matches = match (parent, row.container.as_deref()) {
            (None, None) => true,
            (Some(wanted), Some(container)) => container == wanted,
            _ => false,
        };
        if !matches {
            continue;
        }
        taken[index] = true;
        out.push(OutlineSymbol {
            name: row.name.clone(),
            kind: row.kind.clone(),
            line: row.line,
            end_line: row.end_line,
            container: row.container.clone(),
            children: nest_outline(rows, taken, Some(&row.name), depth + 1),
        });
    }
    out
}

/// `memnoth kg tree` — symbol tree for a directory.
pub fn tree(ctx: &AppContext, dir: &str) -> Result<TreeReport> {
    let graph = open_for_read(ctx)?;
    let prefix = workspace_relative(&ctx.root, dir);

    // One query for the whole subtree, grouped by file. Asking per file with a
    // *name* lookup (what this did before) returned nothing for every file.
    let mut by_path: BTreeMap<String, Vec<TreeSymbol>> = BTreeMap::new();
    for row in graph.symbols_under(&prefix).unwrap_or_default() {
        by_path
            .entry(row.path.clone())
            .or_default()
            .push(TreeSymbol {
                name: row.name,
                kind: row.kind,
                line: row.line,
                container: row.container,
            });
    }
    // A file with no declarations is still part of the tree: it is how the
    // reader learns the graph scanned it and found nothing.
    for file in graph.existing_files().unwrap_or_default() {
        let under = prefix.is_empty()
            || file.path == prefix
            || file.path.starts_with(&format!("{prefix}/"));
        if under {
            by_path.entry(file.path).or_default();
        }
    }

    let entries = by_path
        .into_iter()
        .map(|(path, symbols)| TreeEntry {
            language: extract::language_for(&path).to_string(),
            path,
            symbols,
        })
        .collect();

    Ok(TreeReport {
        dir: dir.to_string(),
        entries,
    })
}

/// The `git diff` line a review runs, from the flags that ask for one.
///
/// `--staged` and `--base` are different questions — "what have I staged" versus
/// "what has this branch added since it diverged" — so asking both is refused
/// rather than silently answering whichever the code happens to check first.
///
/// The revision is three-dot (`rev...HEAD`): a review wants what this branch
/// added, not every change on the base branch since the fork point, which is
/// what a two-dot diff would mix in.
fn review_git_args(staged: bool, base: Option<&str>) -> Result<Vec<String>> {
    if staged && base.is_some() {
        bail!(
            "--staged and --base ask different questions: review what is staged, or what this \
             branch adds on top of a revision, not both"
        );
    }

    match base.map(str::trim) {
        Some("") => bail!("--base needs a revision, e.g. `memnoth kg review --base main`"),
        Some(rev) => Ok(vec![
            "diff".to_string(),
            "--name-status".to_string(),
            format!("{rev}...HEAD"),
        ]),
        None if staged => Ok(vec![
            "diff".to_string(),
            "--cached".to_string(),
            "--name-status".to_string(),
        ]),
        None => Ok(vec!["diff".to_string(), "--name-status".to_string()]),
    }
}

/// One `git diff --name-status` line as `(status letter, path)`.
///
/// A rename is two paths; the new one is the file a reviewer opens, so that is
/// the one returned.
fn parse_name_status(line: &str) -> Option<(String, String)> {
    let mut parts = line.split('\t');
    let status = parts.next()?.trim();
    let first = parts.next()?.trim();
    if status.is_empty() || first.is_empty() {
        return None;
    }
    let path = if status.starts_with('R') {
        parts.next().unwrap_or(first).trim()
    } else {
        first
    };
    (!path.is_empty()).then(|| (status.to_string(), path.to_string()))
}

/// The word a review report uses for a `--name-status` letter.
fn change_type(status: &str) -> &'static str {
    match status.chars().next() {
        Some('A') => "added",
        Some('D') => "deleted",
        Some('R') => "renamed",
        Some('C') => "copied",
        _ => "modified",
    }
}

/// `memnoth kg review` — graph-aware diff review.
pub fn review(ctx: &AppContext, staged: bool, base: Option<&str>) -> Result<ReviewReport> {
    use std::process::Command;

    let git_args = review_git_args(staged, base)?;
    let base = base
        .map(str::trim)
        .filter(|rev| !rev.is_empty())
        .map(str::to_string);

    let output = Command::new("git")
        .current_dir(&ctx.root)
        .args(&git_args)
        .output()
        .context("running git diff")?;

    if !output.status.success() {
        return Err(git_diff_error(&ctx.root, &output.stderr));
    }

    // `--name-status`, not `--name-only`: a deleted file is still a change a
    // reviewer must see, and the status is the only place it is recorded.
    let changed: Vec<(String, String)> = String::from_utf8(output.stdout)
        .unwrap_or_default()
        .lines()
        .filter_map(parse_name_status)
        .collect();

    if changed.is_empty() {
        return Ok(ReviewReport {
            staged,
            base,
            changes: Vec::new(),
            summary: ReviewSummary {
                files_changed: 0,
                symbols_changed: 0,
                references_affected: 0,
                test_coverage_delta: 0,
            },
        });
    }

    let graph = open_for_read(ctx)?;
    let mut changes = Vec::new();
    let mut total_symbols = 0;
    let mut total_refs = 0;

    for (status, file) in &changed {
        // By path: `file` is a path, and `definitions_for` looks names up, so
        // this answered "0 symbols" for every file that had any.
        let symbols = graph
            .symbols_in_paths(std::slice::from_ref(file))
            .unwrap_or_default();
        let mut symbol_changes = Vec::new();

        for symbol in &symbols {
            let refs = graph.refs_for(std::slice::from_ref(&symbol.name), MAX_IMPACT_REFS)?;
            total_refs += refs.len();

            symbol_changes.push(ReviewSymbol {
                name: symbol.name.clone(),
                kind: symbol.kind.clone(),
                line: symbol.line,
                references: refs.len(),
            });
        }

        total_symbols += symbol_changes.len();

        // Reuse the `tests-for` path: the graph already knows which test files
        // mention these names. An empty list used to be written unconditionally,
        // which reads as "no tests affected" rather than "not computed" — a
        // false negative in the reassuring direction.
        let names: Vec<String> = symbols.iter().map(|symbol| symbol.name.clone()).collect();
        let mut impacted_tests = Vec::new();
        if !names.is_empty() {
            let mut seen = HashSet::new();
            for path in graph
                .ref_paths_for(&names, MAX_TEST_REFS)
                .unwrap_or_default()
            {
                if is_test_path(&path) && seen.insert(path.clone()) {
                    impacted_tests.push(path);
                }
            }
        }

        changes.push(ReviewChange {
            file: file.clone(),
            change_type: change_type(status).to_string(),
            symbols: symbol_changes,
            impacted_tests,
        });
    }

    Ok(ReviewReport {
        staged,
        base,
        changes,
        summary: ReviewSummary {
            files_changed: changed.len(),
            symbols_changed: total_symbols,
            references_affected: total_refs,
            // The graph knows which tests mention a symbol, not whether they
            // still pass; coverage movement needs a test run.
            test_coverage_delta: 0,
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
            precision: Precision::Lexical,
            total_coverage: 0,
            tests: Vec::new(),
        });
    }

    let test_paths = graph.ref_paths_for(&names, MAX_TEST_REFS)?;
    let mut tests = Vec::new();
    let mut seen: HashSet<(String, usize)> = HashSet::new();

    for path in test_paths {
        if is_test_path(&path) {
            // By path, not by name: a path is never a symbol name.
            let symbols = graph
                .symbols_in_paths(std::slice::from_ref(&path))
                .unwrap_or_default();
            let name = symbols
                .first()
                .map(|s| s.name.clone())
                .unwrap_or(path.clone());
            let line = symbols.first().map(|s| s.line).unwrap_or(0);
            if seen.insert((path.clone(), line)) {
                tests.push(TestCoverage {
                    line,
                    path,
                    test_name: name,
                    coverage_type: "direct".to_string(),
                });
            }
        }
    }

    // Inline tests live in the file that declares the symbol, so no separate
    // test path mentions it: `#[cfg(test)] mod tests { … }` is the only place
    // they appear. The index records declarations, not cfg attributes, so the
    // declaring file is read and the block found textually. A hit is only
    // reported when a function inside that block actually names the symbol —
    // a `mod tests` that never mentions it is not coverage of it.
    let declaring: BTreeSet<String> = graph
        .definitions_for(&names)?
        .into_iter()
        .map(|row| row.path)
        .collect();
    for path in declaring {
        if is_test_path(&path) {
            continue;
        }
        for hit in inline_tests_in(&ctx.root, &path, &names) {
            if seen.insert((hit.path.clone(), hit.line)) {
                tests.push(hit);
            }
        }
    }

    Ok(TestsForReport {
        symbol: symbol.to_string(),
        resolved: true,
        precision: best_available(),
        total_coverage: tests.len(),
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
        // `git blame` porcelain-free output is
        // `<sha> (<author> <date> <line>) <content>`; the parenthesised group is
        // the only place the date and the source line number appear.
        for line in String::from_utf8(output.stdout).unwrap_or_default().lines() {
            let Some(open) = line.find('(') else { continue };
            let Some(close) = line.rfind(')') else {
                continue;
            };
            let mut fields = line[open + 1..close].split_whitespace();
            let author = fields.next().unwrap_or("").to_string();
            let rest: Vec<&str> = fields.collect();
            let number = rest
                .last()
                .and_then(|token| token.parse::<usize>().ok())
                .unwrap_or(0);
            let date = rest[..rest.len().saturating_sub(1)].join(" ");
            blame.push(BlameEntry {
                commit: line[..open]
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string(),
                author,
                date,
                line: number,
                content: line[close + 1..].trim().to_string(),
            });
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

    // Every declaration. `definitions_for(&[])` answers nothing by contract (an
    // empty name set is not a wildcard), so asking a whole-graph question that
    // way made this report say "no dead code" for every repository.
    let all_symbols = graph.all_symbols()?;
    let all_names: Vec<String> = all_symbols.iter().map(|s| s.name.clone()).collect();

    // A mention only counts as a use when it happens somewhere that does not
    // declare the name. An export referenced from its own file is still dead:
    // nothing outside can reach it.
    let declaring: HashMap<String, HashSet<String>> =
        all_symbols.iter().fold(HashMap::new(), |mut map, row| {
            map.entry(row.name.clone())
                .or_default()
                .insert(row.path.clone());
            map
        });
    let referenced: HashSet<String> = graph
        .refs_for(&all_names, MAX_REFS)?
        .into_iter()
        .filter(|row| {
            declaring
                .get(&row.name)
                .is_none_or(|files| !files.contains(&row.path))
        })
        .map(|row| row.name)
        .collect();

    // A module is the one kind the rule above gets wrong. `pub mod store;` is
    // reached through the crate's own module tree, and the only line that can
    // mention it by name is inside the file that declares it (`store::open` in
    // that same module). Discarding that mention made every self-contained
    // module read as "an export nothing outside reaches" — which is not even
    // the right question for a module, whose parent is the reachability edge.
    // The exception is scoped to `mod` so functions, types and constants keep
    // the stricter rule.
    let mut module_names: Vec<String> = all_symbols
        .iter()
        .filter(|row| row.kind == "mod")
        .map(|row| row.name.clone())
        .collect();
    module_names.sort();
    module_names.dedup();
    let module_used: HashSet<String> = if module_names.is_empty() {
        HashSet::new()
    } else {
        graph
            .refs_for(&module_names, MAX_REFS)?
            .into_iter()
            .filter(|row| {
                declaring
                    .get(&row.name)
                    .is_some_and(|files| files.contains(&row.path))
            })
            .map(|row| row.name)
            .collect()
    };

    // Find unreferenced public exports (functions, structs, enums, traits, etc.)
    let mut dead = Vec::new();
    for symbol in &all_symbols {
        // `exported` is what makes this a report instead of a list of every
        // local constant in the repository. Before it was stored, a frontend
        // repo produced 787 "unreferenced exports" that were function-locals.
        if symbol.exported
            // A method is reached through its type, not by its bare name: it is
            // not a module export, and counting `new` as dead in every crate
            // that defines one turned this report into noise (737 rows on this
            // repository).
            && symbol.container.is_none()
            && matches!(
                symbol.kind.as_str(),
                // The extractor stores Rust/C-family functions as `fn`; this
                // filter used to accept only `function`, so no Rust function
                // could ever be reported dead and the report always said
                // "no unreferenced exports found" for a Rust repository.
                "fn" | "function"
                    | "class"
                    | "interface"
                    | "struct"
                    | "enum"
                    | "trait"
                    | "type"
                    | "const"
                    | "static"
                    | "mod"
            )
            && !referenced.contains(&symbol.name)
            && !module_used.contains(&symbol.name)
            && symbol.name.len() >= MIN_REF_NAME_LEN
            && !symbol.name.starts_with('_')
        {
            dead.push(DeadExport {
                name: symbol.name.clone(),
                kind: symbol.kind.clone(),
                file: symbol.path.clone(),
                is_public: true,
                line: symbol.line,
            });
        }
    }

    dead.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    let total = dead.len();
    Ok(DeadCodeReport {
        exports: dead,
        total,
    })
}

/// `memnoth kg cycles` — import cycles.
pub fn cycles(ctx: &AppContext) -> Result<CyclesReport> {
    let graph = open_for_read(ctx)?;

    // A name declared in more than one file is not an edge. References are
    // textual, so `new` in one crate would otherwise "import" every other
    // crate's `new`, and the report opened with 2,156 one-symbol cycles.
    let all_symbols = graph.all_symbols()?;
    let mut declaring: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut by_path: HashMap<&str, Vec<&SymbolRow>> = HashMap::new();
    for symbol in &all_symbols {
        declaring
            .entry(symbol.name.as_str())
            .or_default()
            .insert(symbol.path.as_str());
        by_path
            .entry(symbol.path.as_str())
            .or_default()
            .push(symbol);
    }
    let unique = |name: &str| declaring.get(name).is_some_and(|files| files.len() == 1);

    // The edge is caller -> callee. Asking "who mentions this name" gives the
    // reverse direction, which turned every dependency into a backwards arrow.
    let container_of = |path: &str, line: usize| -> Option<&str> {
        by_path
            .get(path)?
            .iter()
            .filter(|symbol| symbol.line <= line && line <= symbol.end_line.max(symbol.line))
            .min_by_key(|symbol| symbol.end_line.saturating_sub(symbol.line))
            .map(|symbol| symbol.name.as_str())
    };

    let mut imports: HashMap<String, Vec<String>> = HashMap::new();
    for symbol in &all_symbols {
        if !unique(&symbol.name) {
            continue;
        }
        for row in graph.refs_for(std::slice::from_ref(&symbol.name), 1000)? {
            let Some(caller) = container_of(&row.path, row.line) else {
                continue;
            };
            if caller == row.name || !unique(caller) || !unique(&row.name) {
                continue;
            }
            imports
                .entry(caller.to_string())
                .or_default()
                .push(row.name);
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
                        // A one-symbol "cycle" is a name mentioning itself, not
                        // an import loop a person can break.
                        if cycle.len() < 2 {
                            continue;
                        }
                        cycles.push(ImportCycle {
                            length: cycle.len(),
                            files: cycle.clone(),
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
            dfs(
                node,
                &imports,
                &mut visited,
                &mut rec_stack,
                &mut path,
                &mut cycles,
            );
        }
    }

    // Deduplicate cycles
    cycles.sort_by_key(|a| a.length);
    cycles.dedup_by(|a, b| a.files == b.files);

    let total = cycles.len();
    Ok(CyclesReport { cycles, total })
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
        let symbols = graph
            .definitions_for(std::slice::from_ref(&file.path))
            .unwrap_or_default();
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
            symbols: symbols.iter().map(|s| s.name.clone()).collect(),
        });
    }

    // Sort by score descending
    hotspots.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total = hotspots.len();
    Ok(HotspotsReport { hotspots, total })
}

/// `memnoth kg unused-exports` — unused public exports (API hygiene).
pub fn unused_exports(ctx: &AppContext) -> Result<UnusedExportsReport> {
    let graph = open_for_read(ctx)?;

    // Same whole-graph read as `dead-code`: no empty name set, and a mention in
    // the declaring file is not a use by someone else.
    let all_symbols = graph.all_symbols()?;
    let all_names: Vec<String> = all_symbols.iter().map(|s| s.name.clone()).collect();
    let declaring: HashMap<String, HashSet<String>> =
        all_symbols.iter().fold(HashMap::new(), |mut map, row| {
            map.entry(row.name.clone())
                .or_default()
                .insert(row.path.clone());
            map
        });
    let referenced: HashSet<String> = graph
        .refs_for(&all_names, MAX_REFS)?
        .into_iter()
        .filter(|row| {
            declaring
                .get(&row.name)
                .is_none_or(|files| !files.contains(&row.path))
        })
        .map(|row| row.name)
        .collect();

    // Find unused public exports. Export status comes from the extractor, so a
    // TypeScript local is not mistaken for API surface.
    let mut unused = Vec::new();
    for symbol in &all_symbols {
        if symbol.exported
            && symbol.container.is_none()
            && !referenced.contains(&symbol.name)
            && symbol.name.len() >= MIN_REF_NAME_LEN
            && !symbol.name.starts_with('_')
        {
            unused.push(UnusedExport {
                name: symbol.name.clone(),
                kind: symbol.kind.clone(),
                file: symbol.path.clone(),
                line: symbol.line,
                visibility: "exported".to_string(),
            });
        }
    }

    unused.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    let total = unused.len();
    Ok(UnusedExportsReport {
        exports: unused,
        total,
    })
}

/// `memnoth kg deps` — import graph for a file.
pub fn deps(ctx: &AppContext, file: &str) -> Result<DepsReport> {
    let graph = open_for_read(ctx)?;

    let symbols = graph
        .definitions_for(&[file.to_string()])
        .unwrap_or_default();
    let mut edges = Vec::new();

    for symbol in &symbols {
        let refs = graph.refs_for(std::slice::from_ref(&symbol.name), MAX_IMPACT_REFS)?;
        for ref_row in refs {
            if ref_row.path != file {
                edges.push(ImportEdge {
                    from: file.to_string(),
                    to: ref_row.path,
                    kind: "reference".to_string(),
                    line: ref_row.line,
                });
            }
        }
    }

    edges.sort_by(|a, b| a.to.cmp(&b.to).then(a.line.cmp(&b.line)));

    // Every edge found here points *into* this file: another file mentions a
    // symbol this one declares. That is the reverse direction of `imports`.
    Ok(DepsReport {
        file: file.to_string(),
        imports: Vec::new(),
        imported_by: edges,
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
                    // `definitions_for(&[])` means "no names", not "all names":
                    // asking it for a kind filter returned an empty list for
                    // every kind the graph had. `all_symbols` is the wildcard.
                    let all_symbols = graph.all_symbols()?;
                    for symbol in all_symbols {
                        if symbol.kind == value {
                            results.push(QueryResult {
                                symbol: symbol.name,
                                kind: symbol.kind,
                                file: symbol.path,
                                line: symbol.line,
                                metadata: serde_json::json!({ "context": format!("kind:{}", value) }),
                            });
                        }
                    }
                }
                "file" => {
                    // A path is not a name. `match_paths` accepts an exact
                    // stored path or an unambiguous suffix of one.
                    let paths = match_paths(&graph, &workspace_relative(&ctx.root, value));
                    let symbols = graph.symbols_in_paths(&paths)?;
                    for symbol in symbols {
                        results.push(QueryResult {
                            symbol: symbol.name,
                            kind: symbol.kind,
                            file: symbol.path,
                            line: symbol.line,
                            metadata: serde_json::json!({ "context": format!("file:{}", value) }),
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
                            metadata: serde_json::json!({ "context": format!("name:{}", value) }),
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
                            metadata: serde_json::json!({ "context": format!("refs:{}", value) }),
                        });
                    }
                }
                "callers" => {
                    let mut line_cache = LineCache::new(&ctx.root);
                    let callers = find_callers(
                        &graph,
                        &[value.to_string()],
                        &mut line_cache,
                        MAX_IMPACT_REFS,
                    )?;
                    for hit in callers {
                        results.push(QueryResult {
                            symbol: hit.context.clone(),
                            kind: "caller".to_string(),
                            file: hit.path,
                            line: hit.line,
                            metadata: serde_json::json!({ "context": format!("callers:{}", value) }),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    // A bare word is the question people actually type (`kg query Globe`). Before
    // this, only `prefix:value` parts were read, so a plain name matched nothing
    // and looked like a broken index rather than unparsed input.
    let known = ["kind", "file", "name", "refs", "callers"];
    let interpreted = expr.split_whitespace().any(|part| {
        part.split_once(':')
            .is_some_and(|(prefix, _)| known.contains(&prefix))
    });
    if !interpreted && !expr.is_empty() {
        for symbol in graph.like_name(expr, MAX_SEARCH_SCAN)? {
            results.push(QueryResult {
                symbol: symbol.name,
                kind: symbol.kind,
                file: symbol.path,
                line: symbol.line,
                metadata: serde_json::json!({ "context": "name-search" }),
            });
        }
    }

    Ok(QueryReport {
        expr: expr.to_string(),
        count: results.len(),
        results,
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

/// Test functions inside `#[cfg(test)]` blocks of one file that name `names`.
///
/// A unit test lives beside the code it covers, so no separate test path mentions
/// the symbol and the by-path list cannot see it. The index stores declarations,
/// not cfg attributes, so the declaring file is read and the block is found by
/// braces; a function is only reported when its body names the symbol, because a
/// `mod tests` that never mentions it is not coverage of it. Reported as
/// `coverage_type: "inline"` so a caller can tell it from a file under `tests/`.
fn inline_tests_in(root: &Path, relative: &str, names: &[String]) -> Vec<TestCoverage> {
    let Ok(text) = std::fs::read_to_string(root.join(relative)) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();

    let mut index = 0usize;
    while index < lines.len() {
        if !lines[index].contains("#[cfg(test)]") {
            index += 1;
            continue;
        }
        // The attribute owns the `mod` under it; doc comments and other
        // attributes may sit between, so a few lines are allowed.
        let mut module_line = None;
        for offset in 0..4 {
            let Some(candidate) = lines.get(index + offset) else {
                break;
            };
            let trimmed = candidate.trim_start();
            let is_module = trimmed.starts_with("mod ")
                || trimmed.starts_with("pub mod ")
                || trimmed.starts_with("pub(crate) mod ")
                || trimmed.starts_with("pub(super) mod ");
            if is_module {
                module_line = Some(index + offset);
                break;
            }
        }
        let Some(module_line) = module_line else {
            index += 1;
            continue;
        };
        if !lines[module_line].contains('{') {
            // `#[cfg(test)] mod tests;` — the tests are in another file, which
            // the by-path list already covers.
            index = module_line + 1;
            continue;
        }

        // The module's body, by braces: from its `{` to the matching `}`.
        let mut depth = 0i32;
        let mut opened = false;
        let mut end = lines.len();
        for (cursor, line) in lines.iter().enumerate().skip(module_line) {
            for ch in line.chars() {
                match ch {
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if opened && depth <= 0 {
                end = cursor;
                break;
            }
        }

        let mut functions: Vec<(String, usize)> = Vec::new();
        for (cursor, line) in lines.iter().enumerate().take(end).skip(module_line + 1) {
            let trimmed = line.trim_start();
            let rest = trimmed
                .strip_prefix("pub(crate) ")
                .or_else(|| trimmed.strip_prefix("pub(super) "))
                .or_else(|| trimmed.strip_prefix("pub "))
                .or_else(|| trimmed.strip_prefix("async "))
                .unwrap_or(trimmed);
            let Some(rest) = rest.strip_prefix("fn ") else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                .collect();
            if !name.is_empty() {
                functions.push((name, cursor));
            }
        }

        for (position, (name, line_index)) in functions.iter().enumerate() {
            let body_end = functions
                .get(position + 1)
                .map(|(_, next)| *next)
                .unwrap_or(end);
            let mentions = lines[*line_index + 1..body_end]
                .iter()
                .any(|text| mentions_name(text, names));
            if mentions {
                out.push(TestCoverage {
                    path: relative.to_string(),
                    line: *line_index + 1,
                    test_name: name.clone(),
                    coverage_type: "inline".to_string(),
                });
            }
        }

        index = end.max(module_line + 1);
    }

    out
}

/// Whether one source line names any of `names` as a whole identifier.
fn mentions_name(text: &str, names: &[String]) -> bool {
    let boundary = |ch: Option<char>| ch.is_none_or(|c| !c.is_alphanumeric() && c != '_');
    names.iter().any(|name| {
        if name.is_empty() {
            return false;
        }
        let mut from = 0usize;
        while let Some(found) = text[from..].find(name.as_str()) {
            let start = from + found;
            let end = start + name.len();
            from = end;
            if boundary(text[..start].chars().last()) && boundary(text[end..].chars().next()) {
                return true;
            }
        }
        false
    })
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
        let line = code_part(line);
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

/// The code part of one line: a mention inside a comment is not a call.
///
/// Scanning whole lines made every doc comment a reference — `kg callers` listed
/// prose lines as callers and each blast radius counted them — so a comment-only
/// line is dropped and a trailing `//` comment is cut, unless the `//` sits
/// inside a double-quoted string (a URL, usually). A block comment that spans
/// lines is still scanned on the lines that do not start with `*`.
fn code_part(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with('*') || (trimmed.starts_with('#') && !trimmed.starts_with("#[")) {
        return "";
    }
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if byte == b'\\' {
                index += 1;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            return &line[..index];
        }
        index += 1;
    }
    line
}

/// The innermost declaration containing `line`, if any.
fn enclosing(ranges: &[(usize, usize, &str)], line: usize) -> Option<String> {
    ranges
        .iter()
        .filter(|(start, end, _)| *start <= line && line <= *end)
        .min_by_key(|(start, end, _)| end - start)
        .map(|(_, _, name)| (*name).to_string())
}

/// A file or directory argument as the graph stores it.
///
/// Rows are workspace-relative with forward slashes, but a shell argument is
/// whatever the user typed: `./src/a.ts`, `src\a.ts`, or an absolute path
/// inside the workspace. All three have to become one string before they can
/// join a stored row, and the absolute form also has to survive the `..` a
/// `--root` may carry.
fn workspace_relative(root: &Path, given: &str) -> String {
    let given = given.trim().replace('\\', "/");
    // One leading `./` only. `trim_start_matches("./")` also ate the start of a
    // dotfile — `.github/workflows` became `github/workflows` — and left `.`
    // alone, so `kg tree .` matched no stored path.
    let trimmed = given.strip_prefix("./").unwrap_or(&given);
    let trimmed = trimmed.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        return String::new();
    }
    let trimmed = trimmed.to_string();
    if !Path::new(&trimmed).is_absolute() {
        return trimmed;
    }
    let root_absolute = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let file_absolute = Path::new(&trimmed)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&trimmed));
    match file_absolute.strip_prefix(&root_absolute) {
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => trimmed,
    }
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
        files.push(SourceFile {
            language: extract::language_for(&entry.relative_path),
            relative: entry.relative_path,
            bytes: metadata.len() as i64,
            mtime_ms: mtime_ms(&metadata),
            path: entry.path,
        });
    }
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    files
}

/// Whether a path is machine output rather than something a person wrote.
///
/// These files still contribute declarations — `api.gen.ts` is real API surface
/// worth finding — but not mentions: a generated module repeats every name it
/// imports thousands of times, and that is the bulk of a misleading `impact`.
fn is_generated(relative_path: &str) -> bool {
    let lower = relative_path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    name.contains(".gen.")
        || name.contains(".generated.")
        || name.contains(".min.")
        || name.ends_with(".map")
        || matches!(
            name,
            "package-lock.json"
                | "pnpm-lock.yaml"
                | "yarn.lock"
                | "bun.lockb"
                | "cargo.lock"
                | "composer.lock"
                | "poetry.lock"
        )
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

/// Whether a source line mentions `name` in a call position.
///
/// The graph stores where a name appears, not what it binds to, so this is a
/// lexical reading of the one line: `name(` is a call site, `name<…>(` is a call
/// site with an explicit type argument, and `fn name(`, `name,` or `: name>` are
/// not. It deliberately does not try to resolve shadowing — the reports that use
/// it already say they are blast-radius evidence, and this narrows that evidence
/// without upgrading it.
fn is_call_shaped(source: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let bytes = source.as_bytes();
    let mut from = 0usize;
    while let Some(found) = source[from..].find(name) {
        let start = from + found;
        let end = start + name.len();
        from = end;

        // A whole word: `recallee(` must not count as a call to `callee`.
        let before = source[..start].chars().last();
        if before.is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
            continue;
        }
        // A declaration line names the symbol, it does not call it. `.` and `:`
        // are allowed on the left so `store::open(` and `self.open(` still read
        // as calls.
        let prefix = source[..start].trim_end();
        if prefix.ends_with("fn") || prefix.ends_with("def") || prefix.ends_with("function") {
            continue;
        }

        // Skip whitespace, then one `::<…>` turbofish or `<…>` type-argument
        // list: `store::open::<u32>(1)` calls `open` just as `store::open(1)`
        // does, and Rust's parser puts the colon before the angle bracket.
        let mut cursor = end;
        while bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b':')
        {
            cursor += 1;
        }
        if bytes.get(cursor) == Some(&b'<') {
            let mut depth = 0usize;
            while let Some(&byte) = bytes.get(cursor) {
                match byte {
                    b'<' => depth += 1,
                    b'>' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            cursor += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                cursor += 1;
            }
            while bytes
                .get(cursor)
                .is_some_and(|byte| byte.is_ascii_whitespace())
            {
                cursor += 1;
            }
        }
        if bytes.get(cursor) == Some(&b'(') {
            return true;
        }
    }
    false
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

    /// The line exactly as written (trimmed, not shortened).
    ///
    /// Classification needs the real text: the display copy is ellipsized, and a
    /// call whose `(` falls past the cut would read as a plain mention.
    fn raw(&mut self, relative: &str, line: usize) -> Option<String> {
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

    /// One question per review, and the revision lands in the diff range.
    ///
    /// This is the part of `review` that can be wrong without git: the flags are
    /// turned into a command line here, before anything runs.
    #[test]
    fn a_review_asks_one_question_at_a_time() {
        assert!(
            review_git_args(true, Some("main")).is_err(),
            "--staged plus --base must be refused, not silently resolved"
        );

        // Three dots: what this branch adds, not the base branch's own changes.
        assert_eq!(
            review_git_args(false, Some("main")).unwrap().join(" "),
            "diff --name-status main...HEAD"
        );
        assert!(
            review_git_args(false, Some("  ")).is_err(),
            "a blank revision is not a revision"
        );
        assert_eq!(
            review_git_args(true, None).unwrap().join(" "),
            "diff --cached --name-status"
        );
        assert_eq!(
            review_git_args(false, None).unwrap().join(" "),
            "diff --name-status"
        );
    }

    #[test]
    fn name_status_keeps_deletions_and_opens_renames_at_the_new_path() {
        assert_eq!(
            parse_name_status("M\tsrc/lib.rs"),
            Some(("M".into(), "src/lib.rs".into()))
        );
        assert_eq!(
            parse_name_status("D\tsrc/gone.rs"),
            Some(("D".into(), "src/gone.rs".into()))
        );
        // A rename is two paths; the reviewer opens the destination.
        assert_eq!(
            parse_name_status("R100\tsrc/old.rs\tsrc/new.rs"),
            Some(("R100".into(), "src/new.rs".into()))
        );
        assert_eq!(parse_name_status(""), None);
        assert_eq!(change_type("D"), "deleted");
        assert_eq!(change_type("A"), "added");
        assert_eq!(change_type("R100"), "renamed");
        assert_eq!(change_type("M"), "modified");
    }

    /// Run git, failing the test with the command on a non-zero status.
    fn git(root: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .expect("git is installed");
        assert!(status.success(), "git {args:?} failed");
    }

    /// The T2 gate: a deletion must not read as "nothing affected".
    ///
    /// The symbol rows live in the graph, which still has them after the file
    /// is gone — so the review can name the symbol and the test that mentions
    /// it. This is the case `--name-only` made invisible and an empty
    /// `impacted_tests` used to contradict.
    #[test]
    fn a_deleted_symbol_still_shows_the_tests_that_mention_it() {
        let (dir, ctx) = fixture();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["add", "--", "src", "tests"]);
        git(
            root,
            &[
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=T",
                "commit",
                "-qm",
                "base",
            ],
        );
        index(&ctx, false, true, &mut |_| {}).unwrap();

        std::fs::remove_file(root.join("src/lib.rs")).unwrap();
        git(root, &["add", "-A", "--", "src", "tests"]);
        git(
            root,
            &[
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=T",
                "commit",
                "-qm",
                "delete",
            ],
        );

        let report = review(&ctx, false, Some("HEAD~1")).expect("review runs");
        let change = report
            .changes
            .iter()
            .find(|change| change.file == "src/lib.rs")
            .expect("a deleted file is still a change");
        assert_eq!(change.change_type, "deleted");
        assert!(
            change.symbols.iter().any(|symbol| symbol.name == "callee"),
            "deleted symbols stay visible: {:?}",
            change.symbols
        );
        assert!(
            change.impacted_tests.contains(&"tests/it.rs".to_string()),
            "the test that mentions the symbol is named: {:?}",
            change.impacted_tests
        );
    }

    fn context(root: PathBuf) -> AppContext {
        AppContext {
            graph_path: root.join(".memnoth").join("graph.db"),
            home: root.join("home"),
            vault_path: root.join("home").join("vault.db"),
            namespace: "default".to_string(),
            sessions_dir: root.join("home").join("sessions"),
            config_path: root.join("home").join("config.toml"),
            exe_path: PathBuf::from("memnoth"),
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

    #[test]
    fn a_mention_inside_a_comment_is_not_a_reference() {
        let (dir, ctx) = fixture();
        std::fs::write(
            dir.path().join("src/documented.rs"),
            "/// Calls `callee` on purpose.\n// callee is named here too\npub fn documented() -> u32 {\n    crate::callee(2)\n}\n",
        )
        .unwrap();
        index(&ctx, true, true, &mut |_| {}).unwrap();
        let report = impact(&ctx, "callee", 1).unwrap();
        let mentions = report
            .references
            .iter()
            .filter(|hit| hit.path == "src/documented.rs")
            .count();
        assert_eq!(
            mentions, 1,
            "the call counts, the two comments do not: {:?}",
            report.references
        );
    }

    #[test]
    fn a_call_path_follows_calls_rather_than_the_defining_file() {
        let (_dir, ctx) = fixture();
        index(&ctx, false, true, &mut |_| {}).unwrap();

        let direct = callpath(&ctx, "caller", "callee").unwrap();
        assert!(direct.resolved);
        assert_eq!(direct.hops, 1);

        // `other` (src/other.rs) calls `caller`, which calls `callee`: a hop that
        // only a real edge crosses, and one the name-per-file walk could not see.
        let indirect = callpath(&ctx, "other", "callee").unwrap();
        assert!(indirect.resolved, "other -> caller -> callee exists");
        let names: Vec<&str> = indirect
            .path
            .iter()
            .map(|node| node.symbol.as_str())
            .collect();
        assert_eq!(names, ["other", "caller", "callee"]);

        // A symbol in the same file that nothing calls is not a callee.
        let invented = callpath(&ctx, "callee", "other").unwrap();
        assert!(!invented.resolved, "callee does not call other");
    }

    #[test]
    fn the_tree_of_the_workspace_root_lists_the_workspace() {
        let (_dir, ctx) = fixture();
        index(&ctx, false, true, &mut |_| {}).unwrap();
        let report = tree(&ctx, ".").unwrap();
        let paths: Vec<&str> = report
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert!(paths.contains(&"src/lib.rs"), "{paths:?}");
    }

    #[test]
    fn an_argument_becomes_a_workspace_relative_path() {
        let (_dir, ctx) = fixture();
        assert_eq!(workspace_relative(&ctx.root, "."), "");
        assert_eq!(workspace_relative(&ctx.root, "./src/"), "src");
        // A dotfile keeps its dot: only one leading `./` is stripped.
        assert_eq!(
            workspace_relative(&ctx.root, ".github/workflows"),
            ".github/workflows"
        );
    }

    #[test]
    fn tests_for_names_the_test_that_calls_the_symbol() {
        let (_dir, ctx) = fixture();
        index(&ctx, false, true, &mut |_| {}).unwrap();
        let report = tests_for(&ctx, "callee").unwrap();
        assert!(report.resolved);
        let hit = report.tests.first().expect("tests/it.rs calls callee");
        assert_eq!(hit.path, "tests/it.rs");
        assert_eq!(hit.test_name, "it_works", "the declaration, not the path");
        assert_eq!(hit.line, 1);
    }

    /// `pub mod store` is reached through the crate tree: the file that declares
    /// it is exactly where its name is used (`store::open_vault()` in the same
    /// module), so that mention must not leave it looking like dead API surface.
    #[test]
    fn an_import_cycle_is_reported_only_between_unambiguous_names() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/a.rs"),
            "pub fn alpha() -> u32 {
    crate::beta()
}
",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/b.rs"),
            "pub fn beta() -> u32 {
    crate::alpha()
}
",
        )
        .unwrap();
        index(&ctx, false, true, &mut |_| {}).unwrap();

        let report = cycles(&ctx).unwrap();
        assert!(
            report.cycles.iter().any(|cycle| cycle.length == 2),
            "alpha and beta call each other: {:?}",
            report.cycles
        );
        assert!(
            report.cycles.iter().all(|cycle| cycle.length >= 2),
            "a name mentioning itself is not a cycle: {:?}",
            report.cycles
        );
    }

    #[test]
    fn a_method_is_not_a_dead_module_export() {
        // `Cache::new` is reached through the type. Reporting it as an
        // unreferenced export made the list 737 rows long on this repository,
        // which is a report nobody reads.
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/cache.rs"),
            "pub struct Cache;\nimpl Cache {\n    pub fn new() -> Self {\n        Cache\n    }\n}\n",
        )
        .unwrap();
        index(&ctx, false, true, &mut |_| {}).unwrap();

        let report = dead_code(&ctx).unwrap();
        assert!(
            !report.exports.iter().any(|export| export.name == "new"),
            "a method is not a module export: {:?}",
            report.exports
        );
    }

    #[test]
    fn a_public_function_nothing_mentions_is_reported_as_dead() {
        // The kind filter accepted `function` while the extractor stores `fn`,
        // so this report was empty for every Rust repository.
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\npub fn never_called() -> u32 { 7 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/use.rs"),
            "pub fn caller() -> i32 { crate::add(1, 2) }\n",
        )
        .unwrap();
        index(&ctx, false, true, &mut |_| {}).unwrap();

        let report = dead_code(&ctx).unwrap();
        let names: Vec<&str> = report
            .exports
            .iter()
            .map(|export| export.name.as_str())
            .collect();
        assert!(names.contains(&"never_called"), "{names:?}");
        assert!(
            !names.contains(&"add"),
            "a referenced export is not dead: {names:?}"
        );
    }

    #[test]
    fn coverage_reads_the_worst_listed_file_and_counts_a_missing_one_as_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(coverage_for(dir.path(), &["src/a.rs".to_string()]), 0.0);
        std::fs::create_dir_all(dir.path().join(".memnoth")).unwrap();
        std::fs::write(
            dir.path().join(".memnoth/coverage.json"),
            r#"{"files":{"src/a.rs":0.5,"src/b.rs":0.9}}"#,
        )
        .unwrap();
        assert_eq!(
            coverage_for(
                dir.path(),
                &["src/a.rs".to_string(), "src/b.rs".to_string()]
            ),
            0.5
        );
        assert_eq!(
            coverage_for(dir.path(), &["src/missing.rs".to_string()]),
            0.0
        );
    }

    #[test]
    fn churn_is_unknown_outside_a_git_checkout_so_the_risk_score_is_not_faked() {
        let dir = tempfile::tempdir().unwrap();
        assert!(churn_90d(dir.path(), &["src/a.rs".to_string()]).is_none());
        assert!(
            risk_for(dir.path(), &["src/a.rs".to_string()], 9).is_none(),
            "a score with a guessed input is worse than no score"
        );
    }

    #[test]
    fn churn_counts_the_commits_touching_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap()
        };
        if !git(&["--version"]).status.success() {
            return;
        }
        assert!(git(&["init", "-q"]).status.success());
        std::fs::write(dir.path().join("a.rs"), "pub fn a() {}\n").unwrap();
        assert!(git(&["add", "a.rs"]).status.success());
        let commit = std::process::Command::new("git")
            .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
            .arg("-C")
            .arg(dir.path())
            .args(["commit", "-q", "-m", "add a"])
            .output()
            .unwrap();
        assert!(commit.status.success(), "{commit:?}");
        assert_eq!(churn_90d(dir.path(), &["a.rs".to_string()]), Some(1));
    }

    #[test]
    fn a_module_used_only_inside_its_own_file_is_not_dead_code() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/other.rs"),
            "pub mod store {\n    pub fn open_vault() -> u32 {\n        1\n    }\n}\n\npub fn uses_it() -> u32 {\n    store::open_vault()\n}\n",
        )
        .unwrap();
        index(&ctx, false, true, &mut |_| {}).unwrap();

        let report = dead_code(&ctx).unwrap();
        assert!(
            !report.exports.iter().any(|export| export.name == "store"),
            "a module is used by its own file: {:?}",
            report.exports
        );
    }

    /// A unit test lives in the file it covers, so the by-path list never sees
    /// it; it is coverage all the same, and is labelled as inline.
    #[test]
    fn tests_for_counts_inline_cfg_test_modules() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn covered(value: u32) -> u32 {\n    value + 1\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn it_covers() {\n        assert_eq!(covered(1), 2);\n    }\n}\n",
        )
        .unwrap();
        index(&ctx, false, true, &mut |_| {}).unwrap();

        let report = tests_for(&ctx, "covered").unwrap();
        let hit = report
            .tests
            .iter()
            .find(|test| test.coverage_type == "inline")
            .expect("the inline module names `covered`");
        assert_eq!(hit.path, "src/lib.rs");
        assert_eq!(hit.test_name, "it_covers");
        assert_eq!(hit.line, 10, "the test function's own line");
    }

    /// The split is lexical, and it has to be honest about both sides: a call
    /// site is a call, a bare reference is not, and a declaration is neither.
    #[test]
    fn call_shape_is_lexical_and_excludes_declarations() {
        assert!(is_call_shaped("    target(1)", "target"));
        assert!(is_call_shaped("let x = store::open::<u32> (1);", "open"));
        assert!(!is_call_shaped("fn target(value: u32) -> u32 {", "target"));
        assert!(!is_call_shaped("    target", "target"));
        assert!(!is_call_shaped("let x = recallee(1);", "callee"));
    }

    #[test]
    fn a_call_site_is_a_call_and_a_mention_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn target(value: u32) -> u32 {\n    value\n}\n\npub fn by_call() -> u32 {\n    target(1)\n}\n\npub fn by_mention() -> fn(u32) -> u32 {\n    target\n}\n",
        )
        .unwrap();
        index(&ctx, false, true, &mut |_| {}).unwrap();

        let report = refs(&ctx, "target", 50).unwrap();
        assert!(report.resolved);
        let call = report
            .references
            .iter()
            .find(|hit| hit.call_shaped)
            .unwrap_or_else(|| panic!("`target(1)` is a call site: {:?}", report.references));
        assert_eq!(call.kind, "call");
        assert_eq!(call.line, 6);

        let mention = report
            .references
            .iter()
            .find(|hit| hit.line == 10)
            .unwrap_or_else(|| panic!("`target` alone is a mention: {:?}", report.references));
        assert!(!mention.call_shaped);
        assert_eq!(mention.kind, "reference");
    }
}
