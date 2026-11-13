//! Benchmark harness for the code graph (W7-14).
//!
//! Full index on 100k files must complete in under 60 seconds; incremental
//! re-index of a single edited file must complete in under 200 ms. These are
//! budgets, not guarantees — the benchmark reports measured numbers so the
//! claim can be checked rather than asserted.
//!
//! The benchmarks are intentionally *not* wired into `cargo test` (they touch
//! the filesystem and the store); they are run manually via `mn bench run
//! --suite kg`.

use crate::open_store;
use anyhow::Result;
use std::path::Path;
use std::time::Instant;

/// Result of one benchmark run.
#[derive(Debug, Clone)]
pub struct BenchResult {
    pub suite: String,
    pub case: String,
    pub elapsed_ms: u64,
    pub budget_ms: u64,
    pub met_budget: bool,
}

/// Run the full-index benchmark: index a directory tree and report elapsed time.
pub fn bench_full_index(root: &Path) -> Result<BenchResult> {
    let start = Instant::now();
    let mut store = open_store(&root.join(".memnoth/graph.db"))?;
    let _report = crate::index_full(&mut store, root)?;
    let elapsed = start.elapsed();
    Ok(BenchResult {
        suite: "kg".into(),
        case: "full_index".into(),
        elapsed_ms: elapsed.as_millis() as u64,
        budget_ms: 60_000,
        met_budget: elapsed.as_millis() <= 60_000,
    })
}

/// Run the incremental benchmark: re-index one file and report elapsed time.
pub fn bench_incremental(root: &Path, file: &str) -> Result<BenchResult> {
    let mut store = open_store(&root.join(".memnoth/graph.db"))?;
    let start = Instant::now();
    let _report = crate::update_file(&mut store, root, file)?;
    let elapsed = start.elapsed();
    Ok(BenchResult {
        suite: "kg".into(),
        case: "incremental".into(),
        elapsed_ms: elapsed.as_millis() as u64,
        budget_ms: 200,
        met_budget: elapsed.as_millis() <= 200,
    })
}
