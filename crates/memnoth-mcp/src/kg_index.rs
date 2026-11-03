//! The `kg_index` tool body, split out of `lib.rs` (which is over the size
//! ratchet) and cohesive: everything that decides *what* an index run reads.

use std::path::Path;

use anyhow::{Context, Result};
use memnoth_codegraph::{
    IndexScope, codebases_under, index_full_scoped_with_progress, open_store, resolve_codebases,
    update_file, with_embedding,
};
use serde_json::Value;

use crate::kg_tools::workspace_arg;
use crate::report_kg_index_progress;

/// `kg_index` body, shared by the synchronous path and the task worker.
///
/// It needs nothing but a workspace path — it opens its own code-graph store —
/// so moving it off the request thread crosses no borrow at all: not of
/// `MemoryEngine` (which is `Send` but not `Sync`) and not of `Coord`.
/// `mem_ingest` *does* need the vault, which is why its worker opens a second
/// connection to the same file instead of borrowing the server's.
pub(crate) fn run_kg_index(args: &Value) -> Result<Value> {
    let workspace = workspace_arg(args)?;
    let incremental = args
        .get("incremental")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // `embed` defaults to true: the server's own embedding runtime decides
    // whether that means anything, and an explicit `false` is the caller
    // buying speed for a structural query. Thread-scoped so one client's
    // choice cannot silence vectors for the others this server is serving.
    let embed = args.get("embed").and_then(|v| v.as_bool()).unwrap_or(true);
    let root = std::path::Path::new(&workspace);
    let db_path = root.join(".memnoth/kg.db");
    let mut store = open_store(&db_path)?;
    if let Some(file) = incremental {
        let upd = update_file(&mut store, root, &file)?;
        Ok(serde_json::to_value(upd)?)
    } else {
        // Narrated when the client opted into progress and silent otherwise —
        // `report_kg_index_progress` is a no-op with no sink armed, and the
        // payload is identical either way, because the callback only observes.
        let detected = codebases_under(root);
        let scope = kg_index_scope(root, args)?;
        let report = with_embedding(embed, || {
            index_full_scoped_with_progress(&mut store, root, &scope, report_kg_index_progress)
        })?;
        let mut payload = serde_json::to_value(report)?;
        // The inventory is reported on every run, scoped or not. A client that
        // asked for one codebase needs to see the others to change its mind, and
        // a client that indexed all of them needs to know there *was* a choice —
        // that is the only way `codebases` is discoverable from here.
        if let Some(object) = payload.as_object_mut() {
            object.insert("codebases".to_string(), serde_json::to_value(detected)?);
        }
        Ok(payload)
    }
}

/// Which codebases a `kg_index` call asked for, as an index scope.
///
/// Absent, empty or `["all"]` means the whole workspace; anything else is
/// resolved by name, directory or path segment, and an unknown name fails the
/// call with the candidates listed. It is never quietly ignored — a run that
/// indexed everything because it did not understand the request is the failure
/// mode this exists to prevent.
pub(crate) fn kg_index_scope(root: &Path, args: &Value) -> Result<IndexScope> {
    let mut wanted: Vec<String> = Vec::new();
    if let Some(list) = args.get("codebases").and_then(|value| value.as_array()) {
        for value in list {
            if let Some(name) = value.as_str() {
                wanted.push(name.to_string());
            }
        }
    }
    if wanted.is_empty() {
        return Ok(IndexScope::all());
    }
    let dirs = resolve_codebases(root, &wanted)
        .with_context(|| format!("resolving codebases for {}", root.display()))?;
    Ok(IndexScope::from_dirs(dirs))
}
