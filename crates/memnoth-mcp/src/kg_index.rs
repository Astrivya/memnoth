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
    // The registry dispatches this tool directly rather than through
    // `kg_tools::call`, so the shared `supported` envelope is stamped here too.
    run_kg_index_inner(args).map(|mut answer| {
        if let Some(object) = answer.as_object_mut() {
            object.entry("supported").or_insert(serde_json::json!(true));
        }
        answer
    })
}

fn run_kg_index_inner(args: &Value) -> Result<Value> {
    let workspace = workspace_arg(args)?;
    // `kg_index` used to build a second graph (`kg.db`) beside the one
    // `memnoth kg index` writes (`graph.db`), so indexing from an agent left the
    // person's own `memnoth kg search` unchanged and vice versa. The CLI owns the
    // index; when it is findable, this runs it. The richer arguments below
    // (a codebase scope, a single-file update) are this server's own and keep
    // using the bundled indexer.
    // A client that opted into progress gets the bundled indexer, whose per-file
    // snapshots are real; delegating would leave it with only a start/finish
    // pair, and inventing counts to fill the gap is the one thing that stream
    // must never do.
    let plain_run = args.get("codebases").is_none()
        && args.get("incremental").is_none()
        && !crate::kg_index_progress_armed();
    if plain_run
        && let Some(binary) = cli_binary()
        && let Some(value) = run_cli_index(&binary, std::path::Path::new(&workspace))?
    {
        return Ok(value);
    }
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
    // A workspace that has never been indexed has no `.memnoth` directory yet;
    // creating it here is what makes the first `kg_index` call succeed instead of
    // failing with SQLite's "unable to open database file".
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
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

/// The `memnoth` CLI, when it can be found.
///
/// The server is normally started by that CLI (or shipped beside it), so a
/// sibling binary is the common case; `MEMNOTH_CLI_BIN` is the escape hatch for
/// a deployment that separates the two.
fn cli_binary() -> Option<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("MEMNOTH_CLI_BIN") {
        let path = std::path::PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    let directory = std::env::current_exe().ok()?.parent()?.to_path_buf();
    ["mn.exe", "mn", "memnoth.exe", "memnoth"]
        .iter()
        .map(|name| directory.join(name))
        .find(|candidate| candidate.is_file())
}

/// Build the workspace's one graph with the CLI, and answer with its counts.
///
/// `None` means "fall back to the bundled indexer": a CLI that is missing,
/// fails, or cannot answer is not an error the caller should see instead of a
/// graph.
fn run_cli_index(binary: &Path, root: &Path) -> Result<Option<Value>> {
    let output = std::process::Command::new(binary)
        .arg("--root")
        .arg(root)
        .args(["kg", "index", "--json"])
        .current_dir(root)
        .output();
    let Ok(output) = output else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(match crate::cli_graph::CliGraph::open(root) {
        Some(graph) => {
            let mut status = graph.status();
            status["indexed"] = serde_json::json!(true);
            status["engine"] = serde_json::json!("memnoth kg index");
            status
        }
        None => serde_json::json!({
            "indexed": true,
            "engine": "memnoth kg index",
        }),
    }))
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
