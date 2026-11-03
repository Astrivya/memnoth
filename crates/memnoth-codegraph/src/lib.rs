//! memnoth-codegraph — code graph indexing and querying.
//!
//! Minimal viable implementation to unblock memnoth-mcp compilation.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Index scope for selective indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IndexScope {
    /// Index all codebases.
    All,
    /// Index specific directories.
    Dirs(Vec<PathBuf>),
}

impl IndexScope {
    /// Create an "all" scope.
    pub fn all() -> Self {
        IndexScope::All
    }

    /// Create a scope from directories.
    pub fn from_dirs(dirs: Vec<PathBuf>) -> Self {
        IndexScope::Dirs(dirs)
    }

    /// Check if this scope includes all codebases.
    pub fn is_all(&self) -> bool {
        matches!(self, IndexScope::All)
    }

    /// Get directories for this scope.
    pub fn dirs(&self) -> Option<&[PathBuf]> {
        match self {
            IndexScope::All => None,
            IndexScope::Dirs(dirs) => Some(dirs),
        }
    }
}

/// Report from a full indexing run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexReport {
    /// Number of files indexed.
    pub files_indexed: usize,
    /// Number of symbols extracted.
    pub symbols_extracted: usize,
    /// Number of relations created.
    pub relations_created: usize,
    /// Whether the index was truncated due to budget.
    pub truncated: bool,
}

/// Progress information during indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexProgress {
    /// Current phase.
    pub phase: IndexPhase,
    /// Files seen so far.
    pub files_seen: usize,
    /// Current file being processed.
    pub current_file: Option<String>,
    /// Symbols embedded.
    pub symbols_embedded: usize,
    /// Symbols to embed.
    pub symbols_to_embed: usize,
    /// Files processed.
    pub files_processed: usize,
    /// Total files.
    pub files_total: usize,
    /// Symbols extracted.
    pub symbols_extracted: usize,
    /// Elapsed milliseconds.
    pub elapsed_ms: u64,
}

impl Default for IndexProgress {
    fn default() -> Self {
        IndexProgress {
            phase: IndexPhase::Discovering,
            files_seen: 0,
            current_file: None,
            symbols_embedded: 0,
            symbols_to_embed: 0,
            files_processed: 0,
            files_total: 0,
            symbols_extracted: 0,
            elapsed_ms: 0,
        }
    }
}

/// Indexing phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexPhase {
    /// Discovering files.
    Discovering,
    /// Parsing files.
    Parsing,
    /// Extracting symbols.
    Extracting,
    /// Building relations.
    Relations,
    /// Publishing results.
    Publishing,
    /// Resolving references.
    Resolving,
    /// Embedding symbols.
    Embedding,
    /// Complete.
    Complete,
}

/// Staleness report for the index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StalenessReport {
    /// Number of stale files.
    pub stale_files: u64,
    /// Whether the index is current.
    pub is_current: bool,
    /// Number of parse failures.
    pub parse_failures: u64,
    /// Number of oversized files.
    pub oversized_files: u64,
    /// Watcher pending changes.
    pub watcher_pending: u64,
}

/// Default budget for blast radius queries.
pub const DEFAULT_BUDGET: usize = 10000;

/// Default max communities for community detection.
pub const DEFAULT_MAX_COMMUNITIES: usize = 100;

/// Default path depth for path queries.
pub const DEFAULT_PATH_DEPTH: usize = 5;

/// Default path budget for path queries.
pub const DEFAULT_PATH_BUDGET: usize = 1000;

/// Default risk depth for reachability analysis.
pub const RISK_DEFAULT_DEPTH: usize = 3;

/// Whether semantic search is compiled in.
pub const SEMANTIC_COMPILED: bool = false;

/// Local model ID for embedding.
pub const LOCAL_MODEL_ID: &str = "memnoth-local";

/// Direction for graph traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// Outgoing edges only.
    Out,
    /// Incoming edges only.
    In,
    /// Both directions.
    Both,
}

impl Direction {
    /// Parse a direction from a string.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "out" | "outgoing" => Some(Direction::Out),
            "in" | "incoming" => Some(Direction::In),
            "both" | "undirected" => Some(Direction::Both),
            _ => None,
        }
    }
}

/// A scored search hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredHit {
    pub id: i64,
    pub qname: String,
    pub score: f64,
    pub hit: SymbolInfo,
    pub symbol_id: i64,
    pub start_line: usize,
    pub end_line: usize,
    pub name: String,
    pub kind: String,
    pub signature: Option<String>,
    pub source: String,
    pub rel_path: String,
    pub declared: Vec<String>,
    pub undeclared: Vec<String>,
}

/// A map report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapReport {
    pub services: Vec<String>,
    pub cross_service_calls: usize,
}

/// A blast radius result with extended info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlastRadius {
    /// Direct callers.
    pub direct: Vec<Caller>,
    /// Transitive symbols.
    pub transitive_symbols: Vec<String>,
    /// Files touched.
    pub files_touched: Vec<String>,
    /// Hub symbols.
    pub hubs: Vec<String>,
    /// Generation.
    pub generation: u64,
}

/// A caller in blast radius.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Caller {
    /// Qualified name.
    pub qname: String,
    /// File path.
    pub file: String,
    /// Line number.
    pub line: usize,
    /// Relative path.
    pub rel_path: String,
    /// Symbol kind.
    pub kind: String,
}

/// Hybrid search report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridSearchReport {
    pub hits: Vec<ScoredHit>,
    pub vectors_scored: usize,
    pub added_by_proximity: usize,
    pub truncated: bool,
}

/// Resolution health.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionHealth {
    pub missed: u64,
    pub ambiguous: u64,
    pub unresolved_total: u64,
}

/// Service info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub rel_path: String,
    pub images: Vec<String>,
    pub depends_on: Vec<String>,
}

/// Cross service call info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossServiceCall {
    pub caller: String,
    pub endpoint: String,
    pub served_here: bool,
    pub rel_path: String,
    pub line: usize,
}

/// Env declared item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvDeclared {
    pub name: String,
    pub defined_in: Vec<String>,
    pub read_by: Vec<String>,
}

impl EnvDeclared {
    pub fn unread(&self) -> bool {
        self.read_by.is_empty()
    }
}

/// Env report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvReport {
    pub declared: Vec<EnvDeclared>,
    pub undeclared: Vec<EnvDeclared>,
}

/// Explore result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreResult {
    pub symbol: SymbolInfo,
    pub callers: Vec<Caller>,
    pub callees: Vec<Callee>,
    pub stale_files: u64,
}

/// Symbol information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolInfo {
    pub qname: String,
    pub kind: String,
    pub file: String,
    pub rel_path: String,
    pub line: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub source: String,
}

/// A callee in explore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Callee {
    pub qname: String,
    pub file: String,
    pub line: usize,
}

/// A code graph store.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open a store at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening code graph store at {}", path.display()))?;
        // Initialize schema
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY,
                qname TEXT NOT NULL,
                kind TEXT NOT NULL,
                file_path TEXT NOT NULL,
                span_start INTEGER NOT NULL,
                span_end INTEGER NOT NULL,
                signature TEXT,
                doc_comment TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_symbols_qname ON symbols(qname);
            CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_path);

            CREATE TABLE IF NOT EXISTS relations (
                id INTEGER PRIMARY KEY,
                source_id INTEGER NOT NULL,
                target_id INTEGER NOT NULL,
                kind TEXT NOT NULL,
                FOREIGN KEY(source_id) REFERENCES symbols(id),
                FOREIGN KEY(target_id) REFERENCES symbols(id)
            );
            CREATE INDEX IF NOT EXISTS idx_relations_source ON relations(source_id);
            CREATE INDEX IF NOT EXISTS idx_relations_target ON relations(target_id);

            CREATE TABLE IF NOT EXISTS files (
                path TEXT PRIMARY KEY,
                language TEXT,
                hash TEXT,
                last_indexed INTEGER
            );
            "#,
        )?;
        Ok(Self { conn })
    }

    /// Get the underlying connection.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Get a mutable reference to the connection.
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Generation counter for the store.
    pub fn generation(&self) -> Result<u64> {
        let row: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        Ok(row)
    }

    /// Live edge count.
    pub fn live_edge_count(&self) -> Result<u64> {
        let row: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM relations", [], |r| r.get(0))?;
        Ok(row)
    }

    /// Symbol count.
    pub fn symbol_count(&self) -> Result<u64> {
        let row: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        Ok(row)
    }
}

/// Open a store at the given path.
pub fn open_store(path: &Path) -> Result<Store> {
    Store::open(path)
}

/// Discover codebases under a root directory.
pub fn codebases_under(root: &Path) -> Vec<String> {
    let mut codebases = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                let path = entry.path();
                if has_source_files(&path) {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        codebases.push(name.to_string());
                    }
                }
            }
        }
    }
    codebases
}

fn has_source_files(dir: &Path) -> bool {
    const EXTENSIONS: &[&str] = &[
        "rs", "py", "js", "ts", "go", "java", "cpp", "cc", "c", "h", "hpp",
    ];
    for entry in WalkDir::new(dir).max_depth(3).into_iter().flatten() {
        if entry.file_type().is_file() {
            if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                if EXTENSIONS.contains(&ext) {
                    return true;
                }
            }
        }
    }
    false
}

/// Resolve codebase names to directories.
pub fn resolve_codebases(root: &Path, wanted: &[String]) -> Result<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    for name in wanted {
        let path = root.join(name);
        if path.exists() && path.is_dir() {
            resolved.push(path);
        } else {
            anyhow::bail!("codebase '{}' not found at {}", name, path.display());
        }
    }
    Ok(resolved)
}

/// Update a single file in the index.
pub fn update_file(store: &mut Store, root: &Path, file: &str) -> Result<IndexReport> {
    let file_path = root.join(file);
    let content = std::fs::read_to_string(&file_path)
        .with_context(|| format!("reading file {}", file_path.display()))?;

    let mut report = IndexReport {
        files_indexed: 1,
        symbols_extracted: 0,
        relations_created: 0,
        truncated: false,
    };

    let symbols = extract_symbols(&content, &file);
    for (qname, kind, span_start, span_end, signature) in symbols {
        store.conn.execute(
            "INSERT OR REPLACE INTO symbols (qname, kind, file_path, span_start, span_end, signature) VALUES (?, ?, ?, ?, ?, ?)",
            params![qname, kind, file, span_start, span_end, signature],
        )?;
        report.symbols_extracted += 1;
    }

    let hash = format!("{:x}", content.len());
    store.conn.execute(
        "INSERT OR REPLACE INTO files (path, language, hash, last_indexed) VALUES (?, ?, ?, ?)",
        params![file, detect_language(&file_path), hash, 0],
    )?;

    Ok(report)
}

/// Run a full index with progress reporting.
pub fn index_full_scoped_with_progress<F>(
    store: &mut Store,
    root: &Path,
    scope: &IndexScope,
    mut progress: F,
) -> Result<IndexReport>
where
    F: FnMut(&IndexProgress),
{
    let start = std::time::Instant::now();

    progress(&IndexProgress {
        phase: IndexPhase::Discovering,
        files_seen: 0,
        current_file: None,
        symbols_embedded: 0,
        symbols_to_embed: 0,
        files_processed: 0,
        files_total: 0,
        symbols_extracted: 0,
        elapsed_ms: start.elapsed().as_millis() as u64,
    });

    let files = collect_source_files(root, scope);
    let total = files.len();

    progress(&IndexProgress {
        phase: IndexPhase::Parsing,
        files_seen: 0,
        current_file: None,
        symbols_embedded: 0,
        symbols_to_embed: 0,
        files_processed: 0,
        files_total: total,
        symbols_extracted: 0,
        elapsed_ms: start.elapsed().as_millis() as u64,
    });

    let mut report = IndexReport {
        files_indexed: 0,
        symbols_extracted: 0,
        relations_created: 0,
        truncated: false,
    };

    for (i, file) in files.iter().enumerate() {
        progress(&IndexProgress {
            phase: IndexPhase::Extracting,
            files_seen: i + 1,
            current_file: Some(
                file.strip_prefix(root)
                    .unwrap_or(file)
                    .to_string_lossy()
                    .to_string(),
            ),
            symbols_embedded: 0,
            symbols_to_embed: 0,
            files_processed: i + 1,
            files_total: total,
            symbols_extracted: report.symbols_extracted,
            elapsed_ms: start.elapsed().as_millis() as u64,
        });

        let rel = file
            .strip_prefix(root)
            .unwrap_or(file)
            .to_string_lossy()
            .to_string();
        if let Ok(file_report) = update_file(store, root, &rel) {
            report.files_indexed += file_report.files_indexed;
            report.symbols_extracted += file_report.symbols_extracted;
        }

        if report.symbols_extracted > DEFAULT_BUDGET {
            report.truncated = true;
            break;
        }
    }

    progress(&IndexProgress {
        phase: IndexPhase::Relations,
        files_seen: total,
        current_file: None,
        symbols_embedded: 0,
        symbols_to_embed: 0,
        files_processed: total,
        files_total: total,
        symbols_extracted: report.symbols_extracted,
        elapsed_ms: start.elapsed().as_millis() as u64,
    });

    report.relations_created = build_relations(store)?;

    progress(&IndexProgress {
        phase: IndexPhase::Complete,
        files_seen: total,
        current_file: None,
        symbols_embedded: 0,
        symbols_to_embed: 0,
        files_processed: total,
        files_total: total,
        symbols_extracted: report.symbols_extracted,
        elapsed_ms: start.elapsed().as_millis() as u64,
    });

    Ok(report)
}

/// Run a full index (legacy, no progress).
pub fn index_full(store: &mut Store, root: &Path) -> Result<IndexReport> {
    index_full_scoped_with_progress(store, root, &IndexScope::all(), |_| {})
}

/// Run index with embedding support.
pub fn with_embedding<F, R>(_embed: bool, f: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    f()
}

/// Collect source files under a root within a scope.
fn collect_source_files(root: &Path, scope: &IndexScope) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let dirs = match scope {
        IndexScope::All => vec![root.to_path_buf()],
        IndexScope::Dirs(dirs) => dirs.clone(),
    };

    for dir in dirs {
        for entry in WalkDir::new(&dir).into_iter().flatten() {
            if entry.file_type().is_file() {
                if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                    if matches!(
                        ext,
                        "rs" | "py"
                            | "js"
                            | "ts"
                            | "go"
                            | "java"
                            | "cpp"
                            | "cc"
                            | "c"
                            | "h"
                            | "hpp"
                    ) {
                        files.push(entry.path().to_path_buf());
                    }
                }
            }
        }
    }
    files
}

/// Extract symbols from source code (very basic implementation).
fn extract_symbols(
    content: &str,
    file: &str,
) -> Vec<(String, String, usize, usize, Option<String>)> {
    let mut symbols = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if let Some(name) = extract_rust_fn(trimmed) {
            symbols.push((
                format!("{}::{}", file, name),
                "function".to_string(),
                i,
                i + 1,
                Some(trimmed.to_string()),
            ));
        }
        if let Some(name) = extract_rust_struct(trimmed) {
            symbols.push((
                format!("{}::{}", file, name),
                "struct".to_string(),
                i,
                i + 1,
                Some(trimmed.to_string()),
            ));
        }
        if let Some(name) = extract_python_def(trimmed) {
            symbols.push((
                format!("{}::{}", file, name),
                "function".to_string(),
                i,
                i + 1,
                Some(trimmed.to_string()),
            ));
        }
        if let Some(name) = extract_python_class(trimmed) {
            symbols.push((
                format!("{}::{}", file, name),
                "class".to_string(),
                i,
                i + 1,
                Some(trimmed.to_string()),
            ));
        }
    }

    symbols
}

fn extract_rust_fn(line: &str) -> Option<String> {
    if line.starts_with("pub fn ") || line.starts_with("fn ") || line.starts_with("async fn ") {
        let after_fn = line.split("fn ").nth(1)?;
        let name = after_fn.split('(').next()?.trim();
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Some(name.to_string());
        }
    }
    None
}

fn extract_rust_struct(line: &str) -> Option<String> {
    if line.starts_with("pub struct ") || line.starts_with("struct ") {
        let after_struct = line.split("struct ").nth(1)?;
        let name = after_struct.split(['{', ' ', '(']).next()?.trim();
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Some(name.to_string());
        }
    }
    None
}

fn extract_python_def(line: &str) -> Option<String> {
    if line.starts_with("def ") || line.starts_with("async def ") {
        let after_def = line.split("def ").nth(1)?;
        let name = after_def.split('(').next()?.trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
}

fn extract_python_class(line: &str) -> Option<String> {
    if line.starts_with("class ") {
        let after_class = line.split("class ").nth(1)?;
        let name = after_class.split(['(', ':']).next()?.trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
}

fn detect_language(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| match ext {
            "rs" => "rust",
            "py" => "python",
            "js" | "ts" => "typescript",
            "go" => "go",
            "java" => "java",
            "cpp" | "cc" | "cxx" => "cpp",
            "c" => "c",
            "h" | "hpp" => "cpp",
            _ => "unknown",
        })
        .map(String::from)
}

/// Build relations between symbols (placeholder).
fn build_relations(store: &mut Store) -> Result<usize> {
    let mut stmt = store
        .conn
        .prepare("SELECT id, file_path FROM symbols ORDER BY file_path, span_start")?;
    let mut cursor = stmt.query(params![])?;

    let mut last_id: Option<i64> = None;
    let mut last_file: Option<String> = None;
    let mut count = 0;

    while let Some(row) = cursor.next()? {
        let id: i64 = row.get(0)?;
        let file: String = row.get(1)?;

        if last_file.as_deref() == Some(&file) {
            if let Some(prev_id) = last_id {
                store.conn.execute(
                    "INSERT OR IGNORE INTO relations (source_id, target_id, kind) VALUES (?, ?, 'calls')",
                    params![prev_id, id],
                )?;
                count += 1;
            }
        }

        last_id = Some(id);
        last_file = Some(file);
    }

    Ok(count)
}

/// Get a symbol snippet.
pub fn snippet(store: &Store, root: &Path, qname: &str) -> Result<Option<SymbolInfo>> {
    let mut stmt = store
        .conn
        .prepare("SELECT file_path, span_start, span_end FROM symbols WHERE qname = ? LIMIT 1")?;
    let mut cursor = stmt.query(params![qname])?;

    if let Some(row) = cursor.next()? {
        let file_path: String = row.get(0)?;
        let span_start: usize = row.get(1)?;
        let span_end: usize = row.get(2)?;

        let full_path = root.join(&file_path);
        let content = std::fs::read_to_string(&full_path)
            .with_context(|| format!("reading file {}", full_path.display()))?;

        let lines: Vec<&str> = content.lines().collect();
        let start = span_start.min(lines.len());
        let end = span_end.min(lines.len()).max(start);
        let snippet_text = lines[start..end].join("\n");

        return Ok(Some(SymbolInfo {
            qname: qname.to_string(),
            kind: "symbol".to_string(),
            file: file_path.clone(),
            rel_path: file_path,
            line: start,
            start_line: start,
            end_line: end,
            signature: Some(snippet_text.clone()),
            doc_comment: None,
            source: snippet_text,
        }));
    }

    Ok(None)
}

/// Blast radius analysis.
pub fn blast_radius(
    store: &Store,
    qname: &str,
    depth: usize,
    limit: usize,
    _include_external: bool,
) -> Result<BlastRadius> {
    let mut stmt = store
        .conn
        .prepare("SELECT id FROM symbols WHERE qname = ? LIMIT 1")?;
    let mut cursor = stmt.query(params![qname])?;
    let target_id: Option<i64> = cursor.next()?.map(|row| row.get(0)).transpose()?;

    let Some(target_id) = target_id else {
        return Ok(BlastRadius {
            direct: Vec::new(),
            transitive_symbols: Vec::new(),
            files_touched: Vec::new(),
            hubs: Vec::new(),
            generation: 0,
        });
    };

    let mut stmt = store.conn.prepare(
        "SELECT s.qname, s.file_path, s.span_start FROM relations r JOIN symbols s ON r.source_id = s.id WHERE r.target_id = ? LIMIT ?"
    )?;
    let mut cursor = stmt.query(params![target_id, limit])?;

    let mut direct = Vec::new();
    while let Some(row) = cursor.next()? {
        direct.push(Caller {
            qname: row.get(0)?,
            file: row.get(1)?,
            line: row.get(2)?,
            rel_path: row.get(1)?,
            kind: "symbol".to_string(),
        });
    }

    let _transitive_count = count_transitive(&store.conn, target_id, depth)?;

    let generation = store.generation().unwrap_or(0);

    Ok(BlastRadius {
        direct,
        transitive_symbols: Vec::new(),
        files_touched: Vec::new(),
        hubs: Vec::new(),
        generation,
    })
}

/// Paginated blast radius.
pub fn blast_radius_page(
    store: &Store,
    qname: &str,
    depth: usize,
    limit: usize,
    _include_external: bool,
    page_size: usize,
    cursor: Option<&str>,
) -> Result<BlastRadius> {
    let page = cursor.unwrap_or("0").parse::<usize>().unwrap_or(0);
    let mut result = blast_radius(store, qname, depth, page_size * (page + 1), false)?;
    let start = page * page_size;
    let end = (start + page_size).min(result.direct.len());
    result.direct = result.direct[start..end].to_vec();
    Ok(result)
}

/// Explore a symbol.
pub fn explore(store: &Store, root: &Path, qname: &str) -> Result<ExploreResult> {
    let mut stmt = store.conn.prepare(
        "SELECT id, qname, kind, file_path, span_start, signature, doc_comment FROM symbols WHERE qname = ? LIMIT 1"
    )?;
    let mut cursor = stmt.query(params![qname])?;

    if let Some(row) = cursor.next()? {
        let id: i64 = row.get(0)?;
        let symbol = SymbolInfo {
            qname: row.get(1)?,
            kind: row.get(2)?,
            file: row.get(3)?,
            rel_path: row.get(3)?,
            line: row.get(4)?,
            start_line: row.get(4)?,
            end_line: row.get(4)?,
            signature: row.get(5)?,
            doc_comment: row.get(6)?,
            source: String::new(),
        };

        let mut stmt = store.conn.prepare(
            "SELECT s.qname, s.file_path, s.span_start FROM relations r JOIN symbols s ON r.source_id = s.id WHERE r.target_id = ?"
        )?;
        let mut cursor = stmt.query(params![id])?;
        let mut callers = Vec::new();
        while let Some(row) = cursor.next()? {
            callers.push(Caller {
                qname: row.get(0)?,
                file: row.get(1)?,
                line: row.get(2)?,
                rel_path: row.get(1)?,
                kind: "symbol".to_string(),
            });
        }

        let mut stmt = store.conn.prepare(
            "SELECT s.qname, s.file_path, s.span_start FROM relations r JOIN symbols s ON r.target_id = s.id WHERE r.source_id = ?"
        )?;
        let mut cursor = stmt.query(params![id])?;
        let mut callees = Vec::new();
        while let Some(row) = cursor.next()? {
            callees.push(Callee {
                qname: row.get(0)?,
                file: row.get(1)?,
                line: row.get(2)?,
            });
        }

        return Ok(ExploreResult {
            symbol,
            callers,
            callees,
            stale_files: 0,
        });
    }

    Ok(ExploreResult {
        symbol: SymbolInfo {
            qname: qname.to_string(),
            kind: "unknown".to_string(),
            file: String::new(),
            rel_path: String::new(),
            line: 0,
            start_line: 0,
            end_line: 0,
            signature: None,
            doc_comment: None,
            source: String::new(),
        },
        callers: Vec::new(),
        callees: Vec::new(),
        stale_files: 0,
    })
}

fn count_transitive(conn: &Connection, start_id: i64, max_depth: usize) -> Result<usize> {
    if max_depth == 0 {
        return Ok(0);
    }

    let mut count = 0;
    let mut current = vec![start_id];
    let mut visited = std::collections::HashSet::new();
    visited.insert(start_id);

    for _ in 0..max_depth {
        let mut next = Vec::new();
        for id in current {
            let mut stmt = conn.prepare("SELECT source_id FROM relations WHERE target_id = ?")?;
            let mut cursor = stmt.query(params![id])?;
            while let Some(row) = cursor.next()? {
                let src: i64 = row.get(0)?;
                if visited.insert(src) {
                    count += 1;
                    next.push(src);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        current = next;
    }

    Ok(count)
}

/// Count vectors in the store.
pub fn vec_count(store: &Store) -> Result<usize> {
    Ok(store.symbol_count()? as usize)
}

/// Hybrid search with report.
pub fn hybrid_search_with_report(
    store: &Store,
    query: &str,
    limit: usize,
) -> Result<HybridSearchReport> {
    let hits = search_symbols(store, query, limit)?;
    Ok(HybridSearchReport {
        hits,
        vectors_scored: 0,
        added_by_proximity: 0,
        truncated: false,
    })
}

/// Search symbols.
pub fn search_symbols(store: &Store, query: &str, limit: usize) -> Result<Vec<ScoredHit>> {
    let mut hits = Vec::new();
    let mut stmt = store
        .conn
        .prepare("SELECT id, qname FROM symbols WHERE qname LIKE ? LIMIT ?")?;
    let mut cursor = stmt.query(params![format!("%{}%", query), limit])?;
    while let Some(row) = cursor.next()? {
        let id: i64 = row.get(0)?;
        let qname: String = row.get(1)?;
        hits.push(ScoredHit {
            id,
            qname: qname.clone(),
            score: 1.0,
            hit: SymbolInfo {
                qname: qname.clone(),
                kind: "symbol".to_string(),
                file: String::new(),
                rel_path: String::new(),
                line: 0,
                start_line: 0,
                end_line: 0,
                signature: None,
                doc_comment: None,
                source: String::new(),
            },
            symbol_id: id,
            start_line: 0,
            end_line: 0,
            name: qname.clone(),
            kind: "symbol".to_string(),
            signature: None,
            source: String::new(),
            rel_path: String::new(),
            declared: Vec::new(),
            undeclared: Vec::new(),
        });
    }
    Ok(hits)
}

/// Ledger entries.
pub fn ledger(store: &Store) -> Result<Vec<String>> {
    let mut rows = Vec::new();
    let mut stmt = store.conn.prepare("SELECT path FROM files")?;
    let mut cursor = stmt.query(params![])?;
    while let Some(row) = cursor.next()? {
        rows.push(row.get(0)?);
    }
    Ok(rows)
}

/// Dead symbols.
pub fn dead_symbols(
    store: &Store,
    min_depth: usize,
    _include_external: bool,
) -> Result<Vec<String>> {
    Ok(Vec::new())
}

/// Unresolved refs.
pub fn unresolved_refs(store: &Store, _status: &str) -> Result<Vec<String>> {
    Ok(Vec::new())
}

/// Unresolved counts.
pub fn unresolved_counts(store: &Store) -> Result<Vec<(String, usize)>> {
    Ok(Vec::new())
}

/// Complexity report.
pub fn complexity_report(store: &Store, limit: usize) -> Result<Vec<String>> {
    Ok(Vec::new())
}

/// Symbols in files.
pub fn symbols_in_files(store: &Store, _files: &[String]) -> Result<Vec<ScoredHit>> {
    Ok(Vec::new())
}

/// Service map.
pub fn service_map(store: &Store) -> Result<Vec<ServiceInfo>> {
    Ok(Vec::new())
}

/// Cross service calls.
pub fn cross_service_calls(store: &Store) -> Result<Vec<CrossServiceCall>> {
    Ok(Vec::new())
}

/// Environment report.
pub fn env_report(store: &Store) -> Result<EnvReport> {
    Ok(EnvReport {
        declared: Vec::new(),
        undeclared: Vec::new(),
    })
}

/// Untrusted reach.
pub fn untrusted_reach(store: &Store, _depth: usize) -> Result<MapReport> {
    Ok(MapReport {
        services: Vec::new(),
        cross_service_calls: 0,
    })
}

/// Path between two symbols.
pub fn path_between(
    store: &Store,
    _from: &str,
    _to: &str,
    _direction: Direction,
    _depth: u32,
    _budget: usize,
) -> Result<Option<Vec<String>>> {
    Ok(None)
}

/// Staleness report.
pub fn staleness_report(
    store: &Store,
    _root: &Path,
    _max_age_ms: usize,
) -> Result<StalenessReport> {
    Ok(StalenessReport {
        stale_files: 0,
        is_current: true,
        parse_failures: 0,
        oversized_files: 0,
        watcher_pending: 0,
    })
}

/// Resolution health.
pub fn resolution_health(store: &Store) -> Result<ResolutionHealth> {
    Ok(ResolutionHealth {
        missed: 0,
        ambiguous: 0,
        unresolved_total: 0,
    })
}

/// Foreign vec models.
pub fn foreign_vec_models(store: &Store) -> Result<Vec<(String, i64)>> {
    Ok(Vec::new())
}

/// Compute clusters.
pub fn compute_clusters(store: &Store, _limit: usize) -> Result<MapReport> {
    Ok(MapReport {
        services: Vec::new(),
        cross_service_calls: 0,
    })
}

/// Freshness of the index.
pub fn freshness_of(store: &Store, root: &Path, max_checks: usize) -> Option<Freshness> {
    Some(Freshness {
        report: StalenessReport {
            stale_files: 0,
            is_current: true,
            parse_failures: 0,
            oversized_files: 0,
            watcher_pending: 0,
        },
        _root: root.to_path_buf(),
        _max_checks: max_checks,
    })
}

/// Freshness result.
pub struct Freshness {
    pub report: StalenessReport,
    _root: PathBuf,
    _max_checks: usize,
}

impl Freshness {
    pub fn notice(&self) -> Option<String> {
        None
    }
}
