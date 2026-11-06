//! Read-only access to the graph `memnoth kg index` builds.
//!
//! There used to be two graphs per workspace: `.memnoth/graph.db` (written by
//! `memnoth kg`) and `.memnoth/kg.db` (written by the server's own `kg_index`,
//! through `memnoth-codegraph`). They share no schema, so neither could read the
//! other — a person who indexed and then asked their agent about the code got
//! "no code graph at `.memnoth/kg.db`", and an agent that called `kg_index` built
//! a graph the person's own `memnoth kg search` could not see.
//!
//! The CLI is the writer. It is the surface a person uses, it is incremental, and
//! it is where the extraction improvements land. This module is the reader the
//! server needs so both surfaces answer from one file: the same rows, the same
//! numbered lines, no second indexer to drift.
//!
//! Nothing here writes: the connection is opened `READ_ONLY`, and a missing or
//! unreadable graph is `None` rather than an error, so a caller can fall back.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{Connection, OpenFlags, params, params_from_iter};
use serde_json::{Value, json};

/// How many mention rows one `impact` call returns before it reports truncation.
pub(crate) const MAX_REFERENCES: usize = 500;

/// How many rows the optional second hop may add.
const MAX_TRANSITIVE: usize = 400;

/// How many references `explore` prints around a definition.
const MAX_EXPLORE_REFERENCES: usize = 50;

/// How many source lines of a definition `explore` quotes.
const SNIPPET_LINES: usize = 12;

/// How many stored files one staleness check may stat. A monorepo graph is a
/// few thousand rows and a stat is cheap; the cap bounds a pathological one
/// without ever hiding that it applied.
const MAX_STALENESS_STATS: usize = 4000;

/// How many offending paths a staleness answer names before it truncates.
const STALENESS_EXAMPLES: usize = 10;

/// An open handle on `<root>/.memnoth/graph.db`.
pub(crate) struct CliGraph {
    conn: Connection,
    root: PathBuf,
    /// Whether the graph has the `container` column. A file written before it
    /// existed still answers — with an empty container — instead of failing on
    /// every query that names it.
    has_container: bool,
    /// Likewise for `end_line`, which older files do not carry.
    has_end_line: bool,
    /// Whether `exported` is recorded. Without it a dead-code report would list
    /// function-locals as dead exports, so that report refuses instead.
    has_exported: bool,
}

impl CliGraph {
    /// Open the CLI graph for `root`, when one exists and has the tables this
    /// module queries.
    ///
    /// The schema version is deliberately not checked here: a graph built by an
    /// older CLI still has `symbols` and `refs`, and answering approximately is
    /// better than telling an agent there is no graph. A *missing* table is
    /// caught by the first query and reported as an error.
    pub(crate) fn open(root: &Path) -> Option<Self> {
        let path = root.join(".memnoth").join("graph.db");
        if !path.is_file() {
            return None;
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        // A file that is not this schema (an empty one, or another tool's) must
        // not answer "0 symbols" as if it were an indexed workspace.
        conn.query_row("SELECT COUNT(*) FROM symbols", [], |row| {
            row.get::<_, i64>(0)
        })
        .ok()?;
        let has_container = column_exists(&conn, "symbols", "container");
        let has_end_line = column_exists(&conn, "symbols", "end_line");
        let has_exported = column_exists(&conn, "symbols", "exported");
        Some(Self {
            conn,
            root: root.to_path_buf(),
            has_container,
            has_end_line,
            has_exported,
        })
    }

    /// The `container` expression to select: the column, or a constant when the
    /// graph predates it.
    fn container(&self) -> &'static str {
        if self.has_container {
            "COALESCE(container, '')"
        } else {
            "''"
        }
    }

    /// The `end_line` expression to select; a declaration with no recorded end
    /// is its own first line rather than an empty range.
    fn end_line(&self) -> &'static str {
        if self.has_end_line {
            "end_line"
        } else {
            "line"
        }
    }

    /// Where the graph lives, for the answer's provenance.
    fn path(&self) -> String {
        self.root
            .join(".memnoth")
            .join("graph.db")
            .display()
            .to_string()
    }

    fn meta(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .ok()
    }

    fn count(&self, table: &str) -> i64 {
        self.conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap_or(0)
    }

    /// Where an answer came from and how old it is.
    ///
    /// Every answer carries this because a graph is a snapshot: an agent that
    /// cannot see the snapshot's age will treat a miss on a renamed symbol as
    /// proof the symbol is gone.
    fn provenance(&self) -> Value {
        let indexed_at_ms = self
            .meta("built_at_ms")
            .and_then(|value| value.parse::<i64>().ok());
        json!({
            "path": self.path(),
            "schema": self.meta("schema"),
            "indexed_at_ms": indexed_at_ms,
            "age": indexed_at_ms.map(age_label),
            "note": "snapshot: files edited since it was built are not reflected until `memnoth kg index`",
            // Lives in provenance rather than in each tool's answer because
            // provenance is already the sentence "where this came from". One
            // place to put it is one place that cannot be forgotten.
            "shadowed_by": self.second_graph(),
        })
    }

    /// The server's own `.memnoth/kg.db`, when the workspace has both graphs.
    ///
    /// `kg_index` with a `codebases` scope still uses the bundled indexer, so a
    /// workspace can hold two graphs and this reader answers from only one of
    /// them. Picking the CLI's graph is right — it is the one a person sees —
    /// but staying quiet about the other turns "I just indexed" into "my
    /// results are stale" with nothing to explain it.
    fn second_graph(&self) -> Option<Value> {
        let path = self.root.join(".memnoth").join("kg.db");
        if !path.is_file() {
            return None;
        }
        Some(json!({
            "path": path.display().to_string(),
            "note": "a second graph exists at .memnoth/kg.db and was not read; this answer is from .memnoth/graph.db. `memnoth kg index` rebuilds the graph both surfaces use, after which the second file is unreferenced and can be deleted.",
        }))
    }

    /// Which indexed files no longer match what is on disk.
    ///
    /// Statting the indexed paths catches edits and deletions exactly. It does
    /// not catch a *new* file, which needs a walk of the tree; that is what
    /// `memnoth kg status` does, so the answer says so rather than implying the
    /// graph is complete.
    fn staleness(&self) -> Value {
        let Ok(mut statement) = self
            .conn
            .prepare("SELECT path, mtime_ms FROM files ORDER BY path")
        else {
            return json!({"checked": 0, "note": "graph has no file table"});
        };
        let rows: Vec<(String, i64)> = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        let indexed_files = rows.len();

        let mut changed: Vec<String> = Vec::new();
        let mut deleted: Vec<String> = Vec::new();
        for (path, stored_ms) in rows.iter().take(MAX_STALENESS_STATS) {
            match std::fs::metadata(self.root.join(path)) {
                Ok(meta) => {
                    let on_disk = meta
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|delta| delta.as_millis() as i64);
                    // Filesystems round mtimes; a second of slack keeps a file
                    // that was indexed in the same second it was written from
                    // reading as changed forever.
                    if let Some(on_disk) = on_disk
                        && on_disk.saturating_sub(*stored_ms).abs() > 1000
                    {
                        changed.push(path.clone());
                    }
                }
                Err(_) => deleted.push(path.clone()),
            }
        }

        let stale = !changed.is_empty() || !deleted.is_empty();
        json!({
            "indexed_files": indexed_files,
            "checked": indexed_files.min(MAX_STALENESS_STATS),
            "changed": changed.len(),
            "deleted": deleted.len(),
            "changed_examples": examples(&changed),
            "deleted_examples": examples(&deleted),
            "added": "not checked here; `memnoth kg status` walks the tree for new files",
            "stale": stale,
            "hint": if stale {
                "run `memnoth kg index` (incremental) before trusting a miss"
            } else {
                "every indexed file still matches the disk"
            },
        })
    }

    /// What the graph was built from, and how much is in it.
    pub(crate) fn status(&self) -> Value {
        json!({
            "graph": "memnoth kg",
            "path": self.path(),
            "root": self.meta("root"),
            "indexed_at_ms": self.meta("built_at_ms").and_then(|value| value.parse::<i64>().ok()),
            "files": self.count("files"),
            "symbols": self.count("symbols"),
            "references": self.count("refs"),
            "languages": self.languages(),
            // The distinction matters to a model deciding whether to trust a
            // miss: names, not meaning, is why `search` can come back empty.
            "search": "lexical (name matching)",
            "indexed": self.provenance(),
            "stale": self.staleness(),
        })
    }

    /// Declarations whose name contains `query`, ranked by where the match sits.
    pub(crate) fn search(&self, query: &str, limit: usize) -> Result<Value> {
        let limit = limit.clamp(1, 100) as i64;
        let mut statement = self.conn.prepare(&format!(
            "SELECT name, kind, path, line, {}
             FROM symbols
             WHERE name LIKE '%' || ?1 || '%'
             ORDER BY CASE WHEN lower(name) = lower(?1) THEN 0
                           WHEN lower(name) LIKE lower(?1) || '%' THEN 1
                           ELSE 2 END,
                      length(name), path, line
             LIMIT ?2",
            self.container()
        ))?;
        let rows = statement.query_map(params![query, limit], |row| {
            Ok(json!({
                "name": row.get::<_, String>(0)?,
                "kind": row.get::<_, String>(1)?,
                "path": row.get::<_, String>(2)?,
                "line": row.get::<_, i64>(3)?,
                "container": row.get::<_, String>(4)?,
            }))
        })?;
        let hits: Vec<Value> = rows.flatten().collect();
        Ok(json!({
            "graph": "memnoth kg",
            "query": query,
            "hits": hits,
            "count": hits.len(),
            "indexed": self.provenance(),
        }))
    }

    /// Every stored name that `symbol` could mean, exact first.
    fn names(&self, symbol: &str) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT DISTINCT name FROM symbols WHERE name = ?1")?;
        let exact: Vec<String> = statement
            .query_map(params![symbol], |row| row.get::<_, String>(0))?
            .flatten()
            .collect();
        if !exact.is_empty() {
            return Ok(exact);
        }
        // A qualified name (`Type::method`) falls back to its last segment, but
        // only through names that actually end with it.
        let segment = symbol
            .rsplit_once("::")
            .map(|(_, last)| last)
            .or_else(|| symbol.rsplit_once('.').map(|(_, last)| last))
            .unwrap_or(symbol);
        let mut statement = self
            .conn
            .prepare("SELECT DISTINCT name FROM symbols WHERE name LIKE '%' || ?1")?;
        Ok(statement
            .query_map(params![segment], |row| row.get::<_, String>(0))?
            .flatten()
            .collect())
    }

    fn placeholders(count: usize) -> String {
        vec!["?"; count].join(",")
    }

    /// Definitions and references for one symbol.
    ///
    /// `depth` above 1 adds the second hop the CLI offers: symbols declared in
    /// the files that mention this one, then whoever mentions *those*. It is
    /// counted separately because it is a much weaker claim.
    pub(crate) fn impact(&self, symbol: &str, depth: u32) -> Result<Value> {
        let names = self.names(symbol)?;
        if names.is_empty() {
            return Ok(json!({
                "graph": "memnoth kg",
                "symbol": symbol,
                "resolved": false,
                "definitions": [],
                "references": [],
                "note": "no declaration by that name; `kg_search` finds the spelling",
                "indexed": self.provenance(),
            }));
        }

        let mut statement = self.conn.prepare(&format!(
            "SELECT name, kind, path, line, {} FROM symbols WHERE name IN ({}) ORDER BY path, line",
            self.end_line(),
            Self::placeholders(names.len())
        ))?;
        let definitions: Vec<Value> = statement
            .query_map(params_from_iter(names.iter()), |row| {
                Ok(json!({
                    "name": row.get::<_, String>(0)?,
                    "kind": row.get::<_, String>(1)?,
                    "path": row.get::<_, String>(2)?,
                    "line": row.get::<_, i64>(3)?,
                    "end_line": row.get::<_, i64>(4)?,
                }))
            })?
            .flatten()
            .collect();

        let mut statement = self.conn.prepare(&format!(
            "SELECT name, path, line, {} FROM refs
             WHERE name IN ({}) ORDER BY path, line LIMIT {}",
            self.container(),
            Self::placeholders(names.len()),
            MAX_REFERENCES
        ))?;
        let mut references: Vec<Value> = statement
            .query_map(params_from_iter(names.iter()), |row| {
                let path: String = row.get(1)?;
                let line: i64 = row.get(2)?;
                let name: String = row.get(0)?;
                Ok(json!({
                    "name": name,
                    "path": path,
                    "line": line,
                    "container": row.get::<_, String>(3)?,
                }))
            })?
            .flatten()
            .collect();
        let truncated = references.len() >= MAX_REFERENCES;
        let direct = references.len();

        // A second hop, off by default: on a component with two real mentions it
        // produced 380 rows that shared no declaration with the symbol.
        if depth > 1 && !references.is_empty() {
            let mut paths: Vec<String> = Vec::new();
            for row in &references {
                if let Some(path) = row["path"].as_str()
                    && !paths.contains(&path.to_string())
                {
                    paths.push(path.to_string());
                }
            }
            paths.truncate(200);
            let mut second_hop: Vec<String> = Vec::new();
            if !paths.is_empty() {
                let sql = format!(
                    "SELECT DISTINCT name FROM symbols WHERE path IN ({})",
                    Self::placeholders(paths.len())
                );
                let mut statement = self.conn.prepare(&sql)?;
                for name in statement
                    .query_map(params_from_iter(paths.iter()), |row| {
                        row.get::<_, String>(0)
                    })?
                    .flatten()
                {
                    if name.len() >= 3 && !names.contains(&name) {
                        second_hop.push(name);
                    }
                }
            }
            second_hop.sort();
            second_hop.dedup();
            if !second_hop.is_empty() {
                let sql = format!(
                    "SELECT name, path, line FROM refs WHERE name IN ({}) LIMIT {}",
                    Self::placeholders(second_hop.len()),
                    MAX_TRANSITIVE
                );
                let mut statement = self.conn.prepare(&sql)?;
                for row in statement
                    .query_map(params_from_iter(second_hop.iter()), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    })?
                    .flatten()
                {
                    references.push(json!({
                        "name": row.0,
                        "path": row.1,
                        "line": row.2,
                        "horn": "depth 2",
                    }));
                }
            }
        }

        // Source text for each row, read once per file.
        let mut cache: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for row in references.iter_mut() {
            let Some(path) = row["path"].as_str().map(str::to_string) else {
                continue;
            };
            let line = row["line"].as_i64().unwrap_or(0).max(1) as usize;
            let lines = cache.entry(path.clone()).or_insert_with(|| {
                std::fs::read_to_string(self.root.join(&path))
                    .map(|text| text.lines().map(|line| line.trim().to_string()).collect())
                    .unwrap_or_default()
            });
            if let Some(source) = lines.get(line.saturating_sub(1)) {
                row["context"] = json!(source);
            }
            if row.get("horn").is_some() {
                row["reference_kind"] = json!("transitive");
            } else {
                row["reference_kind"] = json!("direct");
            }
        }

        let mut files: Vec<String> = Vec::new();
        for row in definitions.iter().chain(references.iter()) {
            if let Some(path) = row["path"].as_str()
                && !files.contains(&path.to_string())
            {
                files.push(path.to_string());
            }
        }
        let tests: Vec<String> = files
            .iter()
            .filter(|path| looks_like_test(path))
            .cloned()
            .collect();

        Ok(json!({
            "graph": "memnoth kg",
            "symbol": symbol,
            "resolved": true,
            "definitions": definitions,
            "references": references,
            "references_direct": direct,
            "references_transitive": references.len().saturating_sub(direct),
            "files": files.len(),
            "tests": tests,
            "truncated": truncated || references.len() - direct >= MAX_TRANSITIVE,
            // Same caveat the CLI prints: a mention is a name in a file, not a
            // resolved call edge.
            "note": "references are textual; a shadowed local with the same name is counted",
            "indexed": self.provenance(),
        }))
    }

    /// A definition with its snippet, its mentions, and the call sites among them.
    pub(crate) fn explore(&self, symbol: &str) -> Result<Value> {
        let names = self.names(symbol)?;
        if names.is_empty() {
            return Ok(json!({
                "graph": "memnoth kg",
                "symbol": symbol,
                "resolved": false,
                "note": "no declaration by that name; `kg_search` finds the spelling",
                "indexed": self.provenance(),
            }));
        }
        let mut statement = self.conn.prepare(&format!(
            "SELECT name, kind, path, line, {}, {}
             FROM symbols WHERE name IN ({}) ORDER BY path, line LIMIT 5",
            self.end_line(),
            self.container(),
            Self::placeholders(names.len())
        ))?;
        let definitions: Vec<Value> = statement
            .query_map(params_from_iter(names.iter()), |row| {
                let path: String = row.get(2)?;
                let line: i64 = row.get(3)?;
                let end_line: i64 = row.get(4)?;
                Ok(json!({
                    "name": row.get::<_, String>(0)?,
                    "kind": row.get::<_, String>(1)?,
                    "path": path,
                    "line": line,
                    "end_line": end_line,
                    "container": row.get::<_, String>(5)?,
                }))
            })?
            .flatten()
            .collect();

        let mut definitions = definitions;
        for definition in definitions.iter_mut() {
            let path = definition["path"].as_str().unwrap_or_default().to_string();
            let line = definition["line"].as_i64().unwrap_or(1).max(1) as usize;
            definition["snippet"] = json!(self.snippet(&path, line));
        }

        let impact = self.impact(symbol, 1)?;
        let references: Vec<Value> = impact["references"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(MAX_EXPLORE_REFERENCES)
            .collect();
        let callers: Vec<Value> = references
            .iter()
            .filter(|row| is_call_site(row["context"].as_str().unwrap_or_default()))
            .cloned()
            .collect();

        Ok(json!({
            "graph": "memnoth kg",
            "symbol": symbol,
            "resolved": !definitions.is_empty(),
            "definitions": definitions,
            "references": references,
            "callers": callers,
            "files": impact["files"],
            "note": "callers are mention lines whose shape looks like an import, JSX use or call",
            "indexed": self.provenance(),
        }))
    }

    /// `memnoth kg dead-code`'s rule, in SQL.
    ///
    /// Duplicated from the CLI deliberately, with the same constants: an agent
    /// and a person asking the same checkout the same question must get the
    /// same list, and this is the only way to guarantee it without one calling
    /// the other. An export mentioned only inside its own file is still dead —
    /// nothing outside can reach it.
    pub(crate) fn dead(&self, limit: usize) -> Result<Value> {
        /// Declarations that can be dead in the sense the report means.
        const KINDS: [&str; 10] = [
            "function",
            "class",
            "interface",
            "struct",
            "enum",
            "trait",
            "type",
            "const",
            "static",
            "mod",
        ];
        /// Below this a name matches too much incidental text to judge.
        const MIN_NAME: usize = 3;

        if !self.has_exported {
            return Ok(json!({
                "graph": "memnoth kg",
                "supported": false,
                "reason": "this graph was built before exports were recorded, so every function-local would read as dead",
                "fix": "run `memnoth kg index` to rebuild it",
                "indexed": self.provenance(),
            }));
        }

        let mut statement = self.conn.prepare(
            "SELECT DISTINCT r.name FROM refs r
             WHERE NOT EXISTS (
                 SELECT 1 FROM symbols s WHERE s.name = r.name AND s.path = r.path
             )",
        )?;
        let used: HashSet<String> = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .flatten()
            .collect();

        let mut statement = self.conn.prepare(
            "SELECT name, kind, path, line FROM symbols WHERE exported = 1 ORDER BY path, line",
        )?;
        let mut dead: Vec<Value> = Vec::new();
        let mut total = 0usize;
        for row in statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })? {
            let (name, kind, path, line) = row?;
            if KINDS.contains(&kind.as_str())
                && name.len() >= MIN_NAME
                && !name.starts_with('_')
                && !used.contains(&name)
            {
                total += 1;
                if dead.len() < limit {
                    dead.push(json!({
                        "name": name,
                        "kind": kind,
                        "file": path,
                        "line": line,
                    }));
                }
            }
        }

        Ok(json!({
            "graph": "memnoth kg",
            "dead_exports": dead,
            "total": total,
            "truncated": total > dead.len(),
            "rule": "exported declarations of a public kind that no other file mentions",
            "caveat": "a mention is textual: a dynamic import or a string-built name does not count as a use",
            "indexed": self.provenance(),
        }))
    }

    /// `memnoth kg hotspots`: the files whose churn and size make them risky.
    ///
    /// The score is the CLI's own — the square root of commits times the number
    /// of declarations — so the two lists rank the same checkout the same way.
    pub(crate) fn hotspots(&self, limit: usize) -> Result<Value> {
        let Some(churn) = self.churn() else {
            return Ok(json!({
                "graph": "memnoth kg",
                "supported": false,
                "reason": "git could not read this checkout, so churn has no value to report",
                "fix": "run `memnoth kg index` and this tool from inside the repository",
                "indexed": self.provenance(),
            }));
        };

        let mut statement = self.conn.prepare("SELECT path, symbols FROM files")?;
        let files: Vec<(String, i64)> = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .flatten()
            .collect();

        let mut rows: Vec<(f64, Value)> = Vec::new();
        for (path, symbols) in files {
            let commits = churn.get(&path).copied().unwrap_or(0);
            if commits == 0 || symbols <= 0 {
                continue;
            }
            let score = ((commits as f64) * (symbols as f64)).sqrt();
            rows.push((
                score,
                json!({
                    "file": path,
                    "commits": commits,
                    "symbols": symbols,
                    "score": (score * 100.0).round() / 100.0,
                }),
            ));
        }
        rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let total = rows.len();
        let hotspots: Vec<Value> = rows.into_iter().take(limit).map(|(_, row)| row).collect();
        Ok(json!({
            "graph": "memnoth kg",
            "hotspots": hotspots,
            "total": total,
            "truncated": total > hotspots.len(),
            "metric": "sqrt(commits x declarations)",
            // Said plainly: the graph records no branch counts, so this is a
            // risk ranking, not cyclomatic complexity.
            "note": "churn and size, not control-flow complexity",
            "indexed": self.provenance(),
        }))
    }

    /// Commits per file in this checkout, or `None` when git cannot answer.
    fn churn(&self) -> Option<std::collections::HashMap<String, usize>> {
        let output = std::process::Command::new("git")
            .current_dir(&self.root)
            .args([
                "log",
                "--pretty=format:",
                "--name-only",
                "--max-count=5000",
                "--",
                ".",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let mut churn: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let path = line.trim();
            if !path.is_empty() {
                *churn.entry(path.to_string()).or_insert(0) += 1;
            }
        }
        Some(churn)
    }

    /// Declarations in the given files, for the "what did I just touch" report.
    pub(crate) fn symbols_in(&self, paths: &[String]) -> Result<Vec<Value>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut statement = self.conn.prepare(&format!(
            "SELECT name, kind, path, line, {}
             FROM symbols WHERE path IN ({}) ORDER BY path, line LIMIT 500",
            self.end_line(),
            Self::placeholders(paths.len())
        ))?;
        Ok(statement
            .query_map(params_from_iter(paths.iter()), |row| {
                Ok(json!({
                    "name": row.get::<_, String>(0)?,
                    "kind": row.get::<_, String>(1)?,
                    "path": row.get::<_, String>(2)?,
                    "line": row.get::<_, i64>(3)?,
                    "end_line": row.get::<_, i64>(4)?,
                }))
            })?
            .flatten()
            .collect())
    }

    /// `impact`, one page at a time, so a symbol with hundreds of mentions does
    /// not arrive as one unreadable answer.
    pub(crate) fn trace_page(
        &self,
        symbol: &str,
        page_size: usize,
        cursor: Option<&str>,
    ) -> Result<Value> {
        let impact = self.impact(symbol, 1)?;
        let references = impact["references"].as_array().cloned().unwrap_or_default();
        // A page larger than `impact` can ever return is pointless, but a page
        // smaller than it is a silent lie: the schema advertises this bound, so
        // the clamp is the same number the mention list itself is capped at.
        let size = page_size.clamp(1, MAX_REFERENCES);
        let start = cursor
            .and_then(|cursor| cursor.parse::<usize>().ok())
            .unwrap_or(0)
            .min(references.len());
        let end = (start + size).min(references.len());
        let page: Vec<Value> = references[start..end].to_vec();

        Ok(json!({
            "graph": "memnoth kg",
            "symbol": symbol,
            "resolved": impact["resolved"],
            "definitions": impact["definitions"],
            "references": page,
            "page": { "start": start, "end": end, "total": references.len() },
            // At the cap, `total` means "at least this many": `impact` stops
            // collecting at MAX_REFERENCES, so the page count is not a census.
            "truncated": impact["truncated"],
            "next_cursor": (end < references.len()).then(|| end.to_string()),
            "note": "references are textual mentions, not resolved call edges",
            "indexed": self.provenance(),
        }))
    }

    /// A health check that answers the same question `kg_doctor` asks of the
    /// bundled store: can I trust a miss?
    ///
    /// It checks what this graph can actually be wrong about — files that moved
    /// since the build, extractors that recorded a file but found nothing in it,
    /// a schema older than this reader, a build that never finished — and it
    /// names the one thing that is not checked instead of implying a verdict.
    pub(crate) fn doctor(&self) -> Value {
        let files = self.count("files");
        let symbols = self.count("symbols");
        let references = self.count("refs");
        let staleness = self.staleness();
        let languages = self.languages();
        let mut findings: Vec<String> = Vec::new();

        if files == 0 {
            findings.push(
                "the graph holds no files — `memnoth kg index` never finished one; run it again"
                    .to_string(),
            );
        } else if symbols == 0 {
            findings.push(format!(
                "{files} file(s) are indexed but no declarations were extracted — either the tree is all prose, or the extractors do not cover these languages"
            ));
        }
        let changed = staleness["changed"].as_i64().unwrap_or(0);
        let deleted = staleness["deleted"].as_i64().unwrap_or(0);
        if changed > 0 {
            findings.push(format!(
                "{changed} indexed file(s) changed on disk — their rows describe an older version; run `memnoth kg index`"
            ));
        }
        if deleted > 0 {
            findings.push(format!(
                "{deleted} indexed file(s) no longer exist — their rows still answer queries; run `memnoth kg index`"
            ));
        }
        // Each of these is a column this reader needs and an older build did
        // not write. Saying which one is missing is the difference between "the
        // graph is old" and "one report cannot run".
        if !self.has_container {
            findings.push(
                "built before declarations recorded their container — enclosing type or module is blank"
                    .to_string(),
            );
        }
        if !self.has_end_line {
            findings.push(
                "built before declarations recorded their end line — every definition reads as one line"
                    .to_string(),
            );
        }
        if !self.has_exported {
            findings.push(
                "built before exports were recorded — `kg_dead` refuses on this graph rather than calling every local dead"
                    .to_string(),
            );
        }
        if self.meta("built_at_ms").is_none() {
            findings.push(
                "the graph records no build time — an answer's age cannot be stated, so treat every hit as of unknown freshness"
                    .to_string(),
            );
        }
        for language in &languages {
            let empty = language["symbols"].as_i64().unwrap_or(0) == 0;
            let count = language["files"].as_i64().unwrap_or(0);
            if empty && count > 0 {
                let name = language["language"].as_str().unwrap_or("unknown");
                findings.push(format!(
                    "{count} {name} file(s) contributed no declarations — the extractor for that language is not recording them"
                ));
            }
        }
        if let Some(second) = self.second_graph() {
            findings.push(format!(
                "a second graph exists at {} — this build answers from .memnoth/graph.db only, so anything indexed there is invisible here",
                second["path"].as_str().unwrap_or(".memnoth/kg.db")
            ));
        }
        if findings.is_empty() {
            findings.push(
                "no problems found — the graph is current and every report it supports will run"
                    .to_string(),
            );
        }

        json!({
            "graph": "memnoth kg",
            "path": self.path(),
            "files": files,
            "symbols": symbols,
            "references": references,
            "languages": languages,
            "staleness": staleness,
            "findings": findings,
            // The honest scope of the verdict, beside the verdict.
            "not_checked": [
                "new files: a file added since the build is not in the graph and is not counted as stale here — `memnoth kg status` walks the tree",
                "call edges: this graph records where a name appears, so there is no resolution rate to report",
                "semantic search: the CLI graph is lexical, so vectors are not part of it",
            ],
            "indexed": self.provenance(),
        })
    }

    /// What the graph covers, and what it could not read.
    pub(crate) fn coverage(&self) -> Value {
        json!({
            "graph": "memnoth kg",
            "path": self.path(),
            "files": self.count("files"),
            "symbols": self.count("symbols"),
            "references": self.count("refs"),
            "languages": self.languages(),
            "stale": self.staleness(),
            "resolution": "mentions only: this graph records where a name appears, not what it binds to",
            "indexed": self.provenance(),
        })
    }

    /// File and declaration counts per language.
    fn languages(&self) -> Vec<Value> {
        let mut languages: Vec<Value> = Vec::new();
        if let Ok(mut statement) = self.conn.prepare(
            "SELECT language, COUNT(*), COALESCE(SUM(symbols), 0)
             FROM files GROUP BY language ORDER BY language",
        ) && let Ok(rows) = statement.query_map([], |row| {
            Ok(json!({
                "language": row.get::<_, String>(0)?,
                "files": row.get::<_, i64>(1)?,
                "symbols": row.get::<_, i64>(2)?,
            }))
        }) {
            languages.extend(rows.flatten());
        }
        languages
    }

    /// The declaration's source, `SNIPPET_LINES` lines from its first line.
    fn snippet(&self, path: &str, line: usize) -> Vec<String> {
        let Ok(text) = std::fs::read_to_string(self.root.join(path)) else {
            return Vec::new();
        };
        text.lines()
            .skip(line.saturating_sub(1))
            .take(SNIPPET_LINES)
            .map(|line| line.trim_end().to_string())
            .collect()
    }
}

/// Whether `table` has a column called `column`.
///
/// Done once at open so a graph written by an older CLI can still answer: the
/// column is missing, not the graph.
fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let Ok(mut statement) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .map(|rows| rows.flatten().any(|name| name == column))
        .unwrap_or(false)
}

/// An age in the words a person would use, for a snapshot's provenance.
fn age_label(indexed_at_ms: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_millis() as i64)
        .unwrap_or(indexed_at_ms);
    let seconds = (now - indexed_at_ms).max(0) / 1000;
    match seconds {
        0..=90 => format!("{seconds}s ago"),
        91..=5400 => format!("{}m ago", seconds / 60),
        5401..=172_800 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

/// At most `STALENESS_EXAMPLES` of `paths`, so a large answer stays readable.
fn examples(paths: &[String]) -> Vec<String> {
    paths.iter().take(STALENESS_EXAMPLES).cloned().collect()
}

#[cfg(test)]
#[path = "cli_graph_tests.rs"]
mod tests;

/// Write an empty graph at the current schema, for tests outside this module
/// that need a workspace the CLI has indexed.
///
/// Kept here rather than duplicated in each test module because a fixture that
/// drifts from the real schema stops being a test of the real schema.
#[cfg(test)]
pub(crate) fn write_test_graph(root: &Path) {
    let path = root.join(".memnoth").join("graph.db");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE files (path TEXT PRIMARY KEY, language TEXT NOT NULL,
                             bytes INTEGER NOT NULL, mtime_ms INTEGER NOT NULL,
                             symbols INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE symbols (id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL,
                               name TEXT NOT NULL, kind TEXT NOT NULL, line INTEGER NOT NULL,
                               end_line INTEGER NOT NULL, container TEXT,
                               exported INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE refs (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                            path TEXT NOT NULL, line INTEGER NOT NULL, container TEXT);
         INSERT INTO meta VALUES ('schema', '3');
         INSERT INTO meta VALUES ('built_at_ms', '1700000000000');",
    )
    .unwrap();
}

/// A file whose path follows one of the usual test conventions.
fn looks_like_test(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("__tests__")
        || name.starts_with("test_")
        || name.ends_with("_test.rs")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".spec.ts")
        || name.ends_with(".spec.tsx")
}

/// Whether a source line looks like it *uses* a symbol rather than mentioning it
/// in prose: an import, a JSX tag, or a call.
fn is_call_site(context: &str) -> bool {
    context.starts_with("import ")
        || context.contains("require(")
        || context.contains("=> ")
        || context.ends_with(')')
        || context.ends_with(");")
        || context.contains('<')
}
