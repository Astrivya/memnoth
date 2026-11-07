//! SQLite persistence for the v1 code graph.
//!
//! Four tables, no foreign keys, no triggers: `files` (what was walked),
//! `symbols` (what was declared), `refs` (who mentions a name), and `meta`
//! (when it was built and from which root). The graph is a derived artifact —
//! deleting the file is always safe — so the schema favours cheap rebuilds over
//! migrations: `SCHEMA_VERSION` is written to `meta`, and a mismatch is a full
//! rebuild rather than an in-place upgrade.
//!
//! Paths are workspace-relative with forward slashes (`normalize_display_path`'s
//! rule), which is what makes output comparable across platforms and what lets a
//! reference row join a symbol row by string equality.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, params, params_from_iter};

/// Bumped when the table shape changes; a different value forces a full rebuild.
///
/// 2: reference rows are written only for files this build can read declarations
/// from, and not for generated modules. A graph built under v1 carries mentions
/// from Markdown, CSS and lockfiles, so it is rebuilt rather than trusted.
/// 3: `symbols.exported`, which is what makes `dead-code` and `unused-exports`
/// mean anything outside Rust.
pub(crate) const SCHEMA_VERSION: i64 = 3;

/// The columns the incremental diff needs from a file row.
///
/// Language and symbol count live in the same table but are not part of "did
/// this file change", so they are not carried here.
#[derive(Debug, Clone)]
pub(crate) struct FileRow {
    /// Workspace-relative path.
    pub path: String,
    /// Size in bytes when indexed.
    pub bytes: i64,
    /// Modification time in milliseconds when indexed.
    pub mtime_ms: i64,
}

/// Whole-graph counters.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Counts {
    /// Files in the graph.
    pub files: usize,
    /// Declarations found.
    pub symbols: usize,
    /// Mention rows.
    pub references: usize,
}

/// One symbol row.
#[derive(Debug, Clone)]
pub(crate) struct SymbolRow {
    /// Declared name.
    pub name: String,
    /// Declaration kind.
    pub kind: String,
    /// Defining file.
    pub path: String,
    /// Declaration line.
    pub line: usize,
    /// End line.
    pub end_line: usize,
    /// Enclosing declaration.
    pub container: Option<String>,
    /// Whether the declaration is exported from its file.
    pub exported: bool,
}
/// One reference row.
#[derive(Debug, Clone)]
pub(crate) struct RefRow {
    /// Name mentioned.
    pub name: String,
    /// File the mention is in.
    pub path: String,
    /// Line the mention is on.
    pub line: usize,
    /// Enclosing declaration at that line, when known.
    pub container: Option<String>,
}

/// A ranked-name match from `search`.
#[derive(Debug, Clone)]
pub(crate) struct SearchRow {
    /// Declared name.
    pub name: String,
    /// Declaration kind.
    pub kind: String,
    /// Defining file.
    pub path: String,
    /// Declaration line.
    pub line: usize,
    /// Enclosing declaration.
    pub container: Option<String>,
}

/// An open graph database.
pub(crate) struct Graph {
    conn: Connection,
}

impl Graph {
    /// Open (creating) the graph and ensure the schema is present.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("cannot open the graph at {}", path.display()))?;
        let graph = Self { conn };
        graph.conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;
        graph.init_schema()?;
        Ok(graph)
    }

    /// Open an existing graph read-only. Fails when the file is absent, which is
    /// `status`'s "not indexed" signal rather than an error.
    pub fn open_readonly(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("cannot open the graph at {}", path.display()))?;
        Ok(Self { conn })
    }

    fn init_schema(&self) -> Result<()> {
        // A version bump can add a column, and `CREATE TABLE IF NOT EXISTS`
        // cannot: an older file keeps its old shape and every write that names
        // the new column fails. The graph is a derived artifact, so a mismatch
        // is a drop and a rebuild, not a migration.
        // The stamp alone is not proof: a build that wrote the new version and
        // then failed on the new column leaves a file that claims to be current
        // while having the old shape, and every later write fails the same way.
        let stored = self.get_meta("schema").ok().flatten();
        let stamped_current = stored
            .as_deref()
            .and_then(|value| value.parse::<i64>().ok())
            == Some(SCHEMA_VERSION);
        if !stamped_current || !self.column_exists("symbols", "exported") {
            self.conn.execute_batch(
                "DROP TABLE IF EXISTS refs;
                 DROP TABLE IF EXISTS symbols;
                 DROP TABLE IF EXISTS files;
                 DROP TABLE IF EXISTS meta;",
            )?;
        }
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS files (
                 path      TEXT PRIMARY KEY,
                 language  TEXT NOT NULL,
                 bytes     INTEGER NOT NULL,
                 mtime_ms  INTEGER NOT NULL,
                 symbols   INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS symbols (
                 id        INTEGER PRIMARY KEY AUTOINCREMENT,
                 path      TEXT NOT NULL,
                 name      TEXT NOT NULL,
                 kind      TEXT NOT NULL,
                 line      INTEGER NOT NULL,
                 end_line  INTEGER NOT NULL,
                 container TEXT,
                 exported  INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS symbols_name ON symbols(name);
             CREATE INDEX IF NOT EXISTS symbols_path ON symbols(path);
             CREATE TABLE IF NOT EXISTS refs (
                 id        INTEGER PRIMARY KEY AUTOINCREMENT,
                 name      TEXT NOT NULL,
                 path      TEXT NOT NULL,
                 line      INTEGER NOT NULL,
                 container TEXT
             );
             CREATE INDEX IF NOT EXISTS refs_name ON refs(name);
             CREATE INDEX IF NOT EXISTS refs_path ON refs(path);",
        )?;
        self.set_meta("schema", &SCHEMA_VERSION.to_string())
    }

    /// Whether `table` has a column called `column`.
    ///
    /// Used to decide a rebuild, so it has to answer `false` rather than fail
    /// when the table does not exist yet.
    fn column_exists(&self, table: &str, column: &str) -> bool {
        let Ok(mut statement) = self.conn.prepare(&format!("PRAGMA table_info({table})")) else {
            return false;
        };
        let columns: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        columns.iter().any(|name| name == column)
    }

    /// Whether this file is a graph this build understands.
    pub fn schema_present(&self) -> bool {
        let present: Option<i64> = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
                [],
                |row| row.get(0),
            )
            .ok();
        if present.unwrap_or(0) == 0 {
            return false;
        }
        match self.get_meta("schema") {
            Ok(Some(version)) => version.parse::<i64>().ok() == Some(SCHEMA_VERSION),
            _ => false,
        }
    }

    /// Run `f` inside one write transaction.
    ///
    /// The index writes tens of thousands of rows; without a transaction each
    /// insert is its own durable commit and the build is dominated by fsync.
    pub fn transaction<T>(&mut self, f: impl FnOnce(&Graph) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f(self) {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Remove every row, keeping the schema.
    pub fn clear(&self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM refs;
             DELETE FROM symbols;
             DELETE FROM files;
             DELETE FROM meta WHERE key <> 'schema';",
        )?;
        Ok(())
    }

    /// Every file row, for the incremental diff.
    pub fn existing_files(&self) -> Result<Vec<FileRow>> {
        let mut statement = self
            .conn
            .prepare("SELECT path, bytes, mtime_ms FROM files")?;
        let rows = statement.query_map([], |row| {
            Ok(FileRow {
                path: row.get(0)?,
                bytes: row.get(1)?,
                mtime_ms: row.get(2)?,
            })
        })?;
        let mut files = Vec::new();
        for row in rows {
            files.push(row?);
        }
        Ok(files)
    }

    /// Every symbol name currently stored, for the reference-rescan decision.
    pub fn symbol_names(&self) -> Result<BTreeSet<String>> {
        let mut statement = self.conn.prepare("SELECT DISTINCT name FROM symbols")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut names = BTreeSet::new();
        for row in rows {
            names.insert(row?);
        }
        Ok(names)
    }

    /// Delete a file and everything derived from it.
    pub fn delete_file(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM refs WHERE path = ?1", params![path])?;
        self.conn
            .execute("DELETE FROM symbols WHERE path = ?1", params![path])?;
        self.conn
            .execute("DELETE FROM files WHERE path = ?1", params![path])?;
        Ok(())
    }

    /// Delete only a file's mention rows (used when its declarations are kept
    /// but the reference scan has to run again).
    pub fn delete_refs_for(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM refs WHERE path = ?1", params![path])?;
        Ok(())
    }

    /// Insert or replace a file row.
    pub fn upsert_file(
        &self,
        path: &str,
        language: &str,
        bytes: i64,
        mtime_ms: i64,
        symbols: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO files (path, language, bytes, mtime_ms, symbols)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET
                 language = excluded.language,
                 bytes    = excluded.bytes,
                 mtime_ms = excluded.mtime_ms,
                 symbols  = excluded.symbols",
            params![path, language, bytes, mtime_ms, symbols],
        )?;
        Ok(())
    }

    /// Store one file's declarations.
    pub fn insert_symbols(
        &self,
        path: &str,
        symbols: &[super::extract::ExtractedSymbol],
    ) -> Result<()> {
        let mut statement = self.conn.prepare(
            "INSERT INTO symbols (path, name, kind, line, end_line, container, exported)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for symbol in symbols {
            statement.execute(params![
                path,
                symbol.name,
                symbol.kind,
                symbol.line as i64,
                symbol.end_line as i64,
                symbol.container,
                symbol.exported as i64,
            ])?;
        }
        Ok(())
    }

    /// Store one file's mention rows.
    pub fn insert_refs(&self, path: &str, refs: &[(String, usize, Option<String>)]) -> Result<()> {
        let mut statement = self
            .conn
            .prepare("INSERT INTO refs (name, path, line, container) VALUES (?1, ?2, ?3, ?4)")?;
        for (name, line, container) in refs {
            statement.execute(params![name, path, *line as i64, container])?;
        }
        Ok(())
    }

    /// Whole-graph counters.
    pub fn counts(&self) -> Result<Counts> {
        let count = |sql: &str| -> Result<usize> {
            let value: i64 = self.conn.query_row(sql, [], |row| row.get(0))?;
            Ok(value.max(0) as usize)
        };
        Ok(Counts {
            files: count("SELECT COUNT(*) FROM files")?,
            symbols: count("SELECT COUNT(*) FROM symbols")?,
            references: count("SELECT COUNT(*) FROM refs")?,
        })
    }

    /// Files and symbols per language, alphabetically.
    pub fn language_counts(&self) -> Result<Vec<super::LanguageCount>> {
        let mut statement = self.conn.prepare(
            "SELECT language, COUNT(*), COALESCE(SUM(symbols), 0)
             FROM files GROUP BY language ORDER BY language",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(super::LanguageCount {
                language: row.get(0)?,
                files: row.get::<_, i64>(1)?.max(0) as usize,
                symbols: row.get::<_, i64>(2)?.max(0) as usize,
            })
        })?;
        let mut counts = Vec::new();
        for row in rows {
            counts.push(row?);
        }
        Ok(counts)
    }

    /// Read a `meta` value.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .ok();
        Ok(value)
    }

    /// Write a `meta` value.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Definitions for any of `names`.
    pub fn definitions_for(&self, names: &[String]) -> Result<Vec<SymbolRow>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT name, kind, path, line, end_line, COALESCE(container, ''), exported
             FROM symbols WHERE name IN ({}) ORDER BY path, line",
            placeholders(names.len())
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(names.iter()), |row| {
            Ok(SymbolRow {
                name: row.get(0)?,
                kind: row.get(1)?,
                path: row.get(2)?,
                line: row.get::<_, i64>(3)?.max(0) as usize,
                end_line: row.get::<_, i64>(4)?.max(0) as usize,
                container: empty_to_none(row.get(5)?),
                exported: row.get::<_, i64>(6).unwrap_or(0) != 0,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Every declaration in the graph, ordered by file then line.
    ///
    /// `definitions_for(&[])` deliberately answers nothing — an empty name set
    /// is not a wildcard — so the whole-graph reports (`dead-code`,
    /// `unused-exports`, `query kind:`) ask for this instead of passing an
    /// empty list and silently getting zero rows back.
    pub fn all_symbols(&self) -> Result<Vec<SymbolRow>> {
        self.symbols_where("1 = 1", params![])
    }

    /// Declarations in the given files.
    ///
    /// File paths are workspace-relative with forward slashes, the same form
    /// `files.path` stores. A lookup by *name* cannot answer an outline, which
    /// is why this exists next to [`Self::definitions_for`].
    pub fn symbols_in_paths(&self, paths: &[String]) -> Result<Vec<SymbolRow>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        self.symbols_where(
            &format!("path IN ({})", placeholders(paths.len())),
            params_from_iter(paths.iter()),
        )
    }

    /// Declarations under a directory prefix (or in one exact file).
    ///
    /// `prefix` may be empty for the whole graph. The directory boundary is
    /// matched with a `/` so `src/app` cannot also match `src/apple`.
    pub fn symbols_under(&self, prefix: &str) -> Result<Vec<SymbolRow>> {
        let prefix = prefix.trim_matches('/');
        if prefix.is_empty() {
            return self.all_symbols();
        }
        self.symbols_where(
            "(path = ?1 OR path LIKE ?2)",
            params![prefix, format!("{prefix}/%")],
        )
    }

    /// Shared SELECT behind the path-shaped lookups above.
    fn symbols_where(
        &self,
        clause: &str,
        parameters: impl rusqlite::Params,
    ) -> Result<Vec<SymbolRow>> {
        let sql = format!(
            "SELECT name, kind, path, line, end_line, COALESCE(container, ''), exported
             FROM symbols WHERE {clause} ORDER BY path, line"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(parameters, |row| {
            Ok(SymbolRow {
                name: row.get(0)?,
                kind: row.get(1)?,
                path: row.get(2)?,
                line: row.get::<_, i64>(3)?.max(0) as usize,
                end_line: row.get::<_, i64>(4)?.max(0) as usize,
                container: empty_to_none(row.get(5)?),
                exported: row.get::<_, i64>(6).unwrap_or(0) != 0,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Mentions of any of `names`, capped at `limit`.
    pub fn refs_for(&self, names: &[String], limit: usize) -> Result<Vec<RefRow>> {
        if names.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        // One row per place, not per mention: `add(add(a, b), b)` mentions
        // `add` twice on the same line, and the report listed the same line
        // twice. DISTINCT collapses those without hiding a second call site.
        let sql = format!(
            "SELECT DISTINCT name, path, line, COALESCE(container, '')
             FROM refs WHERE name IN ({})
             ORDER BY path, line LIMIT {}",
            placeholders(names.len()),
            limit
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(names.iter()), |row| {
            Ok(RefRow {
                name: row.get(0)?,
                path: row.get(1)?,
                line: row.get::<_, i64>(2)?.max(0) as usize,
                container: empty_to_none(row.get(3)?),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Names declared in the given files (for the second impact hop).
    pub fn names_in_files(&self, paths: &[String]) -> Result<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT DISTINCT name FROM symbols WHERE path IN ({})",
            placeholders(paths.len())
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(paths.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// What one declaration's body mentions that is itself declared: the
    /// caller → callee edges of the textual graph.
    ///
    /// `refs.container` is the innermost declaration around a mention, so this
    /// answers "what does this function call" without a resolved call graph.
    /// A mention that happens to sit inside a comment counts the same as code;
    /// the reports that use this say so.
    pub fn callees_of(&self, container: &str) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare(
            "SELECT DISTINCT r.name
             FROM refs r JOIN symbols s ON s.name = r.name
             WHERE r.container = ?1
             ORDER BY r.name",
        )?;
        let rows = statement.query_map([container], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Names ending in `::segment` or `.segment` — how a qualified lookup
    /// (`store::MemoryEngine`) reaches a declaration without guessing.
    pub fn names_end_with(&self, segment: &str) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare("SELECT DISTINCT name FROM symbols")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let colon = format!("::{segment}");
        let dot = format!(".{segment}");
        let mut out: Vec<String> = Vec::new();
        for row in rows {
            let name = row?;
            if (name.ends_with(&colon) || name.ends_with(&dot)) && !out.contains(&name) {
                out.push(name);
            }
        }
        Ok(out)
    }

    /// Name matches for `search`, already narrowed by SQL.
    pub fn like_name(&self, query: &str, scan_limit: usize) -> Result<Vec<SearchRow>> {
        let mut statement = self.conn.prepare(
            "SELECT name, kind, path, line, COALESCE(container, '')
             FROM symbols
             WHERE name LIKE '%' || ?1 || '%'
             ORDER BY name, path, line
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![query, scan_limit as i64], |row| {
            Ok(SearchRow {
                name: row.get(0)?,
                kind: row.get(1)?,
                path: row.get(2)?,
                line: row.get::<_, i64>(3)?.max(0) as usize,
                container: empty_to_none(row.get(4)?),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Distinct paths that contain any mention of `names` (for the test list).
    pub fn ref_paths_for(&self, names: &[String], limit: usize) -> Result<Vec<String>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT DISTINCT path FROM refs WHERE name IN ({}) ORDER BY path LIMIT {}",
            placeholders(names.len()),
            limit
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(names.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(",")
}

fn empty_to_none(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_graph_reports_its_schema_and_empty_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let graph = Graph::open(&path).unwrap();
        assert!(graph.schema_present());
        let counts = graph.counts().unwrap();
        assert_eq!(counts.files, 0);
        assert_eq!(counts.symbols, 0);
        assert_eq!(counts.references, 0);
        drop(graph);

        let readonly = Graph::open_readonly(&path).unwrap();
        assert!(readonly.schema_present());
    }

    #[test]
    fn names_are_matched_only_against_the_asked_set() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(&dir.path().join("graph.db")).unwrap();
        let symbols = vec![
            super::super::extract::ExtractedSymbol {
                name: "alpha".into(),
                kind: "fn",
                line: 1,
                end_line: 1,
                container: None,
                exported: true,
            },
            super::super::extract::ExtractedSymbol {
                name: "beta".into(),
                kind: "fn",
                line: 2,
                end_line: 2,
                container: None,
                exported: false,
            },
        ];
        graph.insert_symbols("a.rs", &symbols).unwrap();
        let found = graph.definitions_for(&["alpha".to_string()]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "alpha");
        // `exported` survives the round trip: reports depend on it.
        assert!(found[0].exported);
        let beta = graph.definitions_for(&["beta".to_string()]).unwrap();
        assert!(!beta[0].exported);
        assert!(graph.definitions_for(&[]).unwrap().is_empty());
    }

    #[test]
    fn an_older_table_shape_is_dropped_rather_than_written_into() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        // A graph as an earlier build wrote it: no `exported` column.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE files (path TEXT PRIMARY KEY, language TEXT NOT NULL,
                                     bytes INTEGER NOT NULL, mtime_ms INTEGER NOT NULL,
                                     symbols INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE symbols (id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL,
                                       name TEXT NOT NULL, kind TEXT NOT NULL, line INTEGER NOT NULL,
                                       end_line INTEGER NOT NULL, container TEXT);
                 CREATE TABLE refs (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                                    path TEXT NOT NULL, line INTEGER NOT NULL, container TEXT);
                 INSERT INTO meta VALUES ('schema', '2');",
            )
            .unwrap();
        }

        let graph = Graph::open(&path).unwrap();
        assert_eq!(
            graph.get_meta("schema").unwrap(),
            Some(SCHEMA_VERSION.to_string())
        );
        // The write names `exported`, so it only succeeds if the table was rebuilt.
        graph
            .insert_symbols(
                "a.ts",
                &[super::super::extract::ExtractedSymbol {
                    name: "Hero".into(),
                    kind: "function",
                    line: 1,
                    end_line: 4,
                    container: None,
                    exported: true,
                }],
            )
            .unwrap();
        assert_eq!(graph.counts().unwrap().symbols, 1);
    }

    #[test]
    fn whole_graph_and_path_lookups_answer_where_name_lookup_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(&dir.path().join("graph.db")).unwrap();
        let symbol = |name: &str, line: usize| super::super::extract::ExtractedSymbol {
            name: name.into(),
            kind: "fn",
            line,
            end_line: line,
            container: None,
            exported: true,
        };
        graph
            .insert_symbols("src/app.ts", &[symbol("a", 1), symbol("b", 2)])
            .unwrap();
        graph
            .insert_symbols("src/apple.ts", &[symbol("c", 1)])
            .unwrap();

        assert_eq!(graph.all_symbols().unwrap().len(), 3);
        assert_eq!(
            graph
                .symbols_in_paths(&["src/app.ts".to_string()])
                .unwrap()
                .len(),
            2
        );
        // A directory prefix stops at the separator: `src/app` is not `src/apple`.
        assert_eq!(graph.symbols_under("src/app.ts").unwrap().len(), 2);
        assert_eq!(graph.symbols_under("src").unwrap().len(), 3);
        assert_eq!(graph.symbols_under("").unwrap().len(), 3);
    }
}
