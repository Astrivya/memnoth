//! Local import gate for a decrypted, staged snapshot database.
//!
//! A pulled snapshot is authenticated and decrypted by the CLI before its
//! database bytes are written to a staging file. This module is the last gate
//! before that staged file is installed: it opens the SQLite file read-only,
//! runs the engine's read-only memory-invariant proofs, and refuses a memory
//! vault that violates any modelled rule.
//!
//! A database that is *not* a memory vault (the project code graph, for
//! example) is outside the memory rules. It is reported as [`GateOutcome::NotAVault`]
//! rather than failed for tables the verifier never claimed to check. This
//! keeps the gate honest: it never reports a pass it did not prove, and it
//! never reports a violation for a schema it does not govern.
//!
//! This is the **local import gate only**. Passing it does not prove a
//! production restore, Cloud-side storage, or transport integrity.
//!
//! Wiring: this file is compiled into the CLI crate through `#[path]` from
//! `src/project_graph_sync.rs` (which keeps the engine's `store.rs` untouched).
//! It deliberately refers to the engine by its external crate name; registering
//! it a second time inside the engine crate would require changing that path.

use std::path::Path;

use anyhow::{Context, Result};
use memnoth_engine::store::invariants::{self, InvariantReport};
use rusqlite::{Connection, OpenFlags};

/// Outcome of running the memory-invariant gate on a staged database.
#[derive(Debug)]
pub enum GateOutcome {
    /// A memory vault whose modelled invariants all passed.
    CleanVault(InvariantReport),
    /// The database is not a memory vault; the memory rules do not apply.
    NotAVault,
    /// A memory vault violates at least one modelled invariant.
    Violations(Vec<&'static str>),
}

impl GateOutcome {
    /// True only when a memory vault was actually checked and found violating.
    pub fn is_violation(&self) -> bool {
        matches!(self, Self::Violations(_))
    }
}

/// Run the memory-invariant gate on a staged SQLite database.
///
/// The connection is read-only: a check can never mutate the staged bytes.
/// Detection is by schema (`entities` table), never by filename, so a renamed
/// code graph cannot be mistaken for a vault or vice versa.
pub fn check_staged_invariants(path: &Path) -> Result<GateOutcome> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| {
            format!(
                "opening staged snapshot {} for invariant checks",
                path.display()
            )
        })?;
    let is_vault = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'entities'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !is_vault {
        return Ok(GateOutcome::NotAVault);
    }
    let report = invariants::check_invariants(&connection);
    if report.has_violations() {
        let ids = report.violations().into_iter().map(|row| row.id).collect();
        Ok(GateOutcome::Violations(ids))
    } else {
        Ok(GateOutcome::CleanVault(report))
    }
}
