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
pub(crate) const SCHEMA_VERSION: i64 = 1;

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
                 container TEXT
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
            "INSERT INTO symbols (path, name, kind, line, end_line, container)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for symbol in symbols {
            statement.execute(params![
                path,
                symbol.name,
                symbol.kind,
                symbol.line as i64,
                symbol.end_line as i64,
                symbol.container,
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
            "SELECT name, kind, path, line, end_line, COALESCE(container, '')
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
        let sql = format!(
            "SELECT name, path, line, COALESCE(container, '')
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
            },
            super::super::extract::ExtractedSymbol {
                name: "beta".into(),
                kind: "fn",
                line: 2,
                end_line: 2,
                container: None,
            },
        ];
        graph.insert_symbols("a.rs", &symbols).unwrap();
        let found = graph.definitions_for(&["alpha".to_string()]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "alpha");
        assert!(graph.definitions_for(&[]).unwrap().is_empty());
    }
}
