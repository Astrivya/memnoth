//! The repo map: a ranked, token-budgeted index of the symbols a reader needs
//! first.
//!
//! Ranking is `centrality × recency × proximity` over the **file** graph, not
//! the symbol graph:
//!
//! - **centrality** is the file's PageRank over `source file → target file`
//!   edges, so a module everything imports outranks a leaf that imports one
//!   thing;
//! - **recency** is the file's `last_indexed` stamp, `0` when the index never
//!   recorded one — a peer, never "new";
//! - **proximity** prefers shallower paths, because those are the entry points.
//!
//! The render is estimated at four characters per token and stops **before** the
//! budget, so a 1,024-token map never arrives as 1,100 tokens. Every entry keeps
//! its file, degree and score, so an impact answer can say which slice of the
//! map it used instead of pasting prose.

use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{GraphAnswer, Precision, Store, best_available};

/// The default render budget, in estimated tokens (W7-09 / G-04).
pub const DEFAULT_MAP_TOKENS: usize = 1024;

/// Characters per estimated token. Deliberately conservative.
pub const CHARS_PER_TOKEN: usize = 4;

/// PageRank damping factor.
pub const DAMPING: f64 = 0.85;

/// PageRank iterations. Fixed, so the same index ranks the same way.
pub const ITERATIONS: usize = 20;

/// Estimate the token cost of a rendered line.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(CHARS_PER_TOKEN)
}

/// One symbol in the map, with the inputs to its rank kept beside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoMapEntry {
    /// Qualified symbol name.
    pub qname: String,
    /// File the symbol lives in.
    pub file: String,
    /// Resolved edge degree of the symbol.
    pub degree: usize,
    /// PageRank of the symbol's file, in `[0, 1]`.
    pub file_rank: f64,
    /// Final ranking score.
    pub score: f64,
}

impl RepoMapEntry {
    /// One rendered line: `file: qname (degree N)`.
    pub fn render_line(&self) -> String {
        format!("{}: {} (degree {})", self.file, self.qname, self.degree)
    }
}

/// A rendered repo map and the slice it actually used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoMap {
    /// Ranked entries, best first, clipped to the budget.
    pub entries: Vec<RepoMapEntry>,
    /// Rendered lines, one per entry.
    pub lines: Vec<String>,
    /// Estimated tokens actually rendered.
    pub tokens_used: usize,
    /// The budget the render was asked to fit.
    pub token_budget: usize,
    /// Whether symbols were withheld to honour the budget.
    pub truncated: bool,
    /// How the edges behind the ranking were produced.
    pub precision: Precision,
    /// The store generation the map was computed from.
    pub generation: u64,
}

impl RepoMap {
    /// The map as text, one line per entry.
    pub fn render(&self) -> String {
        self.lines.join("\n")
    }

    /// Whether the map has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries whose file appears in `files`, in map order.
    ///
    /// This is the slice an impact answer cites: the part of the map that covers
    /// the files it returned, not the whole map.
    pub fn slice_for_files<S: AsRef<str>>(&self, files: &[S]) -> Vec<RepoMapEntry> {
        let wanted: std::collections::BTreeSet<&str> = files.iter().map(|f| f.as_ref()).collect();
        self.entries
            .iter()
            .filter(|entry| wanted.contains(entry.file.as_str()))
            .cloned()
            .collect()
    }
}

impl GraphAnswer for RepoMap {
    fn precision(&self) -> Precision {
        self.precision
    }
}

/// Build a repo map that fits `token_budget` estimated tokens.
///
/// A budget of zero returns an empty map and says it was truncated, which is the
/// honest answer to "show me nothing".
pub fn build(store: &Store, token_budget: usize) -> Result<RepoMap> {
    let conn = store.conn();

    let mut stmt = conn.prepare(
        "SELECT s.qname, s.file_path,
                (SELECT COUNT(*) FROM relations r WHERE r.source_id = s.id)
              + (SELECT COUNT(*) FROM relations r WHERE r.target_id = s.id) AS degree,
                COALESCE(f.last_indexed, 0) AS last_indexed
         FROM symbols s LEFT JOIN files f ON f.path = s.file_path",
    )?;
    let mut rows: Vec<(String, String, i64, i64)> = stmt
        .query_map(params![], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .filter_map(Result::ok)
        .collect();
    rows.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

    let mut edges_stmt = conn.prepare(
        "SELECT s.file_path, t.file_path FROM relations r
         JOIN symbols s ON s.id = r.source_id
         JOIN symbols t ON t.id = r.target_id",
    )?;
    let edges: Vec<(String, String)> = edges_stmt
        .query_map(params![], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(Result::ok)
        .collect();

    let ranks = file_pagerank(&rows, &edges);

    let mut entries: Vec<RepoMapEntry> = rows
        .into_iter()
        .map(|(qname, file, degree, last_indexed)| {
            let file_rank = ranks.get(&file).copied().unwrap_or(0.0);
            let proximity = 1.0 / (1.0 + file.matches('/').count() as f64);
            let centrality = 1.0 + 9.0 * file_rank;
            let degree_boost = 1.0 + (degree.max(0) as f64).ln_1p();
            let recency = 1.0 + (last_indexed.max(0) as f64).ln_1p();
            let score = centrality * degree_boost * recency * (0.5 + proximity);
            RepoMapEntry {
                qname,
                file,
                degree: degree.max(0) as usize,
                file_rank,
                score,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.qname.cmp(&b.qname))
            .then_with(|| a.file.cmp(&b.file))
    });

    let mut lines = Vec::new();
    let mut kept = Vec::new();
    let mut used = 0usize;
    for entry in entries {
        let line = entry.render_line();
        let cost = estimate_tokens(&line);
        if used + cost > token_budget {
            break;
        }
        used += cost;
        lines.push(line);
        kept.push(entry);
    }

    let generation = store.generation()?;
    let total = store.symbol_count()?;
    Ok(RepoMap {
        truncated: (kept.len() as u64) < total,
        entries: kept,
        lines,
        tokens_used: used,
        token_budget,
        precision: best_available(),
        generation,
    })
}

/// PageRank over the file graph, deterministic and dependency-free.
///
/// Dangling files (no outgoing edge) distribute their rank across every file,
/// which keeps the total at 1 and stops a leaf from absorbing rank it never
/// passed on.
fn file_pagerank(
    symbols: &[(String, String, i64, i64)],
    edges: &[(String, String)],
) -> BTreeMap<String, f64> {
    let mut files: Vec<String> = symbols.iter().map(|(_, file, _, _)| file.clone()).collect();
    files.sort();
    files.dedup();
    let n = files.len();
    if n == 0 {
        return BTreeMap::new();
    }

    let index: BTreeMap<&str, usize> = files
        .iter()
        .enumerate()
        .map(|(position, file)| (file.as_str(), position))
        .collect();

    let mut out_degree = vec![0usize; n];
    let mut incoming: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (source, target) in edges {
        let (Some(&from), Some(&to)) = (index.get(source.as_str()), index.get(target.as_str()))
        else {
            continue;
        };
        if from == to {
            continue;
        }
        out_degree[from] += 1;
        incoming[to].push(from);
    }

    let mut rank = vec![1.0 / n as f64; n];
    let teleport = (1.0 - DAMPING) / n as f64;
    for _ in 0..ITERATIONS {
        let dangling: f64 = (0..n)
            .filter(|position| out_degree[*position] == 0)
            .map(|position| rank[position])
            .sum();
        let dangling_share = DAMPING * dangling / n as f64;
        let mut next = vec![teleport + dangling_share; n];
        for (to, sources) in incoming.iter().enumerate() {
            let mut sum = 0.0;
            for from in sources {
                if out_degree[*from] > 0 {
                    sum += rank[*from] / out_degree[*from] as f64;
                }
            }
            next[to] += DAMPING * sum;
        }
        rank = next;
    }

    // Normalise so the largest rank is 1.0: the score is a ranking, and an
    // absolute PageRank value would drift with the file count.
    let max = rank.iter().copied().fold(0.0_f64, f64::max);
    let scale = if max > 0.0 { 1.0 / max } else { 0.0 };
    files
        .into_iter()
        .zip(rank)
        .map(|(file, value)| (file, value * scale))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let path = std::env::temp_dir().join(format!(
            "mn-repomap-{}-{:?}.db",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        store
            .conn()
            .execute_batch(
                "INSERT INTO symbols (id, qname, kind, file_path, span_start, span_end)
                    VALUES (1, 'core::parse', 'function', 'src/core.rs', 1, 5),
                           (2, 'app::main',   'function', 'src/main.rs', 1, 9),
                           (3, 'app::run',    'function', 'src/main.rs', 10, 19),
                           (4, 'tests::parse_works', 'function', 'tests/core_test.rs', 1, 9);
                 INSERT INTO relations (source_id, target_id, kind)
                    VALUES (2, 1, 'calls'), (3, 1, 'calls'), (4, 1, 'calls');
                 INSERT INTO files (path, language, hash, last_indexed)
                    VALUES ('src/core.rs', 'rust', '10', 5),
                           ('src/main.rs', 'rust', '20', 9),
                           ('tests/core_test.rs', 'rust', '30', 1);",
            )
            .unwrap();
        store
    }

    #[test]
    fn the_map_fits_its_budget_and_orders_the_hub_first() {
        let store = store();
        let map = build(&store, 60).unwrap();
        assert!(!map.is_empty());
        let used: usize = map.lines.iter().map(|line| estimate_tokens(line)).sum();
        assert_eq!(used, map.tokens_used);
        assert!(used <= 60, "used {used} tokens: {:?}", map.lines);
        // `core::parse` has degree 3, and its file is imported by two others.
        assert_eq!(map.entries[0].qname, "core::parse");
        assert_eq!(map.precision_label(), "tree-sitter");
    }

    #[test]
    fn a_zero_budget_says_truncated_rather_than_lying() {
        let store = store();
        let map = build(&store, 0).unwrap();
        assert!(map.is_empty());
        assert!(map.lines.is_empty());
        assert_eq!(map.tokens_used, 0);
        assert!(map.truncated);
    }

    #[test]
    fn the_slice_names_only_the_files_asked_for() {
        let store = store();
        let map = build(&store, 1024).unwrap();
        let slice = map.slice_for_files(&["src/main.rs"]);
        assert!(!slice.is_empty());
        assert!(slice.iter().all(|entry| entry.file == "src/main.rs"));
    }

    #[test]
    fn pagerank_is_symmetric_and_deterministic() {
        let symbols = vec![
            ("a::one".to_string(), "a.rs".to_string(), 1, 0),
            ("b::two".to_string(), "b.rs".to_string(), 1, 0),
        ];
        let edges = vec![
            ("b.rs".to_string(), "a.rs".to_string()),
            ("a.rs".to_string(), "b.rs".to_string()),
        ];
        let ranks = file_pagerank(&symbols, &edges);
        let total: f64 = ranks.values().sum();
        // A symmetric pair, normalised by the maximum: both files rank 1.0, so
        // the sum is 2.0 and neither file wins on structure alone.
        assert!((total - 2.0).abs() < 1e-9, "{ranks:?}");
        assert_eq!(ranks.get("a.rs"), ranks.get("b.rs"));
        assert_eq!(file_pagerank(&symbols, &edges), ranks);
    }
}
