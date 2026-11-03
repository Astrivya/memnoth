//! `mn watch` — incremental code graph indexing and vault sync on file changes.

use anyhow::{Context, Result};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::context::AppContext;
use crate::output::{Outcome, ErrorOutcome};
/// Run `mn watch`.
pub fn run(ctx: &AppContext, args: &crate::cli::WatchArgs) -> Result<Outcome, ErrorOutcome> {
    let start = Instant::now();
    let mut events = Vec::new();

    // Initial index
    if !args.once {
        eprintln!("Initial indexing...");
        let mut progress = |line: &str| {
            if !args.json {
                eprintln!("  {}", line);
            }
        };
        let report = kg::index(ctx, false, false, &mut progress).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
        if !args.json {
            eprintln!("Indexed {} files, {} symbols in {:.2}s", report.files, report.symbols, start.elapsed().as_secs_f64());
        }
    }

    // Set up file watcher
    let (tx, rx) = mpsc::channel::<Result<Event, notify::Error>>();
    let mut watcher = RecommendedWatcher::new(
        tx,
        Config::default().with_poll_interval(Duration::from_millis(args.debounce)),
    ).context("Failed to create file watcher")?;

    // Watch workspace root for code graph changes
    watcher.watch(&ctx.root, RecursiveMode::Recursive)
        .with_context(|| format!("Failed to watch {}", ctx.root.display()))?;

    // Optionally watch vault
    if args.vault {
        if let Some(vault_parent) = ctx.vault_path.parent() {
            watcher.watch(vault_parent, RecursiveMode::NonRecursive)
                .with_context(|| format!("Failed to watch vault dir {}", vault_parent.display()))?;
        }
    }

    if !args.json {
        eprintln!("Watching for changes (debounce: {}ms)...", args.debounce);
        if args.port.is_some() {
            eprintln!("Event server on port {}", args.port.unwrap());
        }
    }

    // Start optional event server
    let _server_handle = if let Some(port) = args.port {
        Some(start_event_server(port, tx.clone()))
    } else {
        None
    };

    // Main event loop
    let mut last_index = Instant::now();
    let debounce = Duration::from_millis(args.debounce);

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                let relevant = is_relevant_event(&event, args.vault, &ctx.root, &ctx.vault_path);
                if relevant {
                    events.push(event);
                }
            }
            Ok(Err(e)) => {
                eprintln!("Watch error: {}", e);
            }
            Err(_) => break, // Channel closed
        }

        // Debounced processing
        if !events.is_empty() && last_index.elapsed() >= debounce {
            let changed_files: Vec<PathBuf> = events.drain(..)
                .filter_map(|e| extract_paths(&e))
                .collect();

            if !changed_files.is_empty() {
                if !args.json {
                    eprintln!("\nChanges detected in {} file(s), re-indexing...", changed_files.len());
                }

                let mut progress = |line: &str| {
                    if !args.json {
                        eprintln!("  {}", line);
                    }
                };

                // Incremental index only changed files
                let report = incremental_index(ctx, &changed_files, &mut progress)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

                if !args.json {
                    eprintln!("Re-indexed {} files, {} symbols in {:.2}s",
                        report.files, report.symbols, last_index.elapsed().as_secs_f64());
                }

                // If vault watching enabled, sync vault
                if args.vault {
                    sync_vault(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
                }
            }
            last_index = Instant::now();
        }

        if args.once {
            break;
        }
    }

    let data = serde_json::json!({
        "watched": true,
        "duration_ms": start.elapsed().as_millis(),
        "events_processed": events.len(),
    });

    Ok(Outcome {
        text: "Watch completed".to_string(),
        data,
    })
}

/// Check if an event is relevant for indexing/syncing.
fn is_relevant_event(event: &Event, watch_vault: bool, root: &Path, vault_path: &Path) -> bool {
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
        _ => return false,
    }

    for path in &event.paths {
        // Check if path is within workspace root
        if path.starts_with(root) {
            if let Ok(rel) = path.strip_prefix(root) {
                let rel_str = rel.to_string_lossy();
                // Skip build/cache directories
                if crate::kg::should_skip(&rel_str) {
                    continue;
                }
                // Only watch source files
                if is_source_file(path) {
                    return true;
                }
            }
        }

        // Check if path is the vault file
        if watch_vault && path == vault_path {
            return true;
        }
    }
    false
}

/// Check if a file is a source file we should index.
fn is_source_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        matches!(
            ext.to_lowercase().as_str(),
            "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "kt" | "swift" |
            "cpp" | "cc" | "cxx" | "c" | "h" | "hpp" | "cs" | "rb" | "php" | "scala" | "clj"
        )
    } else {
        false
    }
}

/// Extract relevant paths from an event.
fn extract_paths(event: &Event) -> Vec<PathBuf> {
    event.paths.iter()
        .filter(|p| p.is_file())
        .cloned()
        .collect()
}

/// Incrementally index only the changed files.
fn incremental_index(
    ctx: &AppContext,
    changed_files: &[PathBuf],
    progress: &mut dyn FnMut(&str),
) -> Result<kg::IndexReport> {
    use crate::kg::store::{Graph, FileRow};
    use crate::kg::extract::extract_symbols;
    use std::collections::HashSet;

    let mut graph = Graph::open(&ctx.graph_path)?;
    let mut counts = crate::kg::Counts::default();
    let mut changed = 0;

    for file_path in changed_files {
        let rel_path = file_path.strip_prefix(&ctx.root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .replace('\\', "/");

        progress(&format!("Indexing {}", rel_path));

        // Read file
        let text = match std::fs::read_to_string(file_path) {
            Ok(t) => t,
            Err(_) => {
                // File deleted, remove from graph
                graph.remove_file(&rel_path)?;
                continue;
            }
        };

        // Extract symbols
        let symbols = extract_symbols(&rel_path, &text, &mut counts);

        // Update file entry
        let file_row = FileRow {
            path: rel_path.clone(),
            bytes: text.len() as i64,
            mtime_ms: std::fs::metadata(file_path)
                .map(|m| crate::kg::mtime_ms(&m))
                .unwrap_or(0),
        };
        graph.upsert_file(&file_row)?;

        // Update symbols
        for sym in symbols {
            graph.upsert_symbol(&sym)?;
        }

        // Update references (re-extract for this file)
        let refs = crate::kg::scan_refs(&text, &[], &HashSet::new(), crate::kg::MAX_REFS);
        graph.replace_refs(&rel_path, &refs)?;

        changed += 1;
    }

    // Update meta
    graph.update_meta(&ctx.root)?;

    Ok(kg::IndexReport {
        root: crate::kg::root_label(&ctx.root),
        files: changed,
        symbols: counts.symbols,
        references: counts.references,
        languages: counts.languages.into_iter().collect(),
        elapsed_ms: 0,
        full: false,
        embedded: false,
        built_at_ms: chrono::Utc::now().timestamp_millis(),
    })
}

/// Detect language from file extension.
fn detect_language(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| match ext.to_lowercase().as_str() {
            "rs" => "rust",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" => "javascript",
            "py" => "python",
            "go" => "go",
            "java" => "java",
            "kt" => "kotlin",
            "swift" => "swift",
            "cpp" | "cc" | "cxx" => "cpp",
            "c" => "c",
            "h" | "hpp" => "cpp",
            "cs" => "csharp",
            "rb" => "ruby",
            "php" => "php",
            "scala" => "scala",
            "clj" => "clojure",
            _ => "unknown",
        })
        .unwrap_or("unknown")
        .to_string()
}

/// Sync vault to cloud (placeholder for actual sync logic).
fn sync_vault(ctx: &AppContext) -> Result<()> {
    // For now, just verify vault is accessible
    let _engine = vault::open_existing(ctx)?;
    Ok(())
}

/// Start a simple HTTP server for event streaming (placeholder).
fn start_event_server(port: u16, _tx: mpsc::Sender<Result<Event, notify::Error>>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // Placeholder for event server
        eprintln!("Event server started on port {}", port);
    })
}