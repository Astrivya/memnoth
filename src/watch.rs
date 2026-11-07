//! `mn watch` — incremental code graph indexing and vault sync on file changes.

use anyhow::{Context, Result};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::context::AppContext;
use crate::kg;
use crate::output::{ErrorOutcome, ExitCode, Outcome};
/// Run `mn watch`.
pub fn run(ctx: &AppContext, args: &crate::cli::WatchArgs) -> Result<Outcome, ErrorOutcome> {
    let start = Instant::now();
    let mut events = Vec::new();
    let mut processed = 0usize;

    // Index once up front: `--once` stops here, and a continuous watch starts
    // from a fresh graph instead of serving stale results until the first edit.
    {
        let mut progress = |line: &str| {
            if !args.json {
                eprintln!("  {line}");
            }
        };
        let report = kg::index(ctx, false, false, &mut progress)
            .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
        if !args.json {
            eprintln!(
                "Indexed {} files, {} symbols in {:.2}s",
                report.files,
                report.symbols,
                start.elapsed().as_secs_f64()
            );
        }
    }

    if args.once {
        let data = serde_json::json!({
            "watched": false,
            "once": true,
            "duration_ms": start.elapsed().as_millis(),
            "events_processed": 0,
        });
        return Ok(Outcome::new(
            "watch",
            "Indexed once; no watcher started.",
            data,
        ));
    }

    // Set up file watcher
    let (tx, rx) = mpsc::channel::<Result<Event, notify::Error>>();
    let mut watcher = RecommendedWatcher::new(
        tx.clone(),
        Config::default().with_poll_interval(Duration::from_millis(args.debounce)),
    )
    .context("Failed to create file watcher")?;

    // Watch workspace root for code graph changes
    watcher
        .watch(&ctx.root, RecursiveMode::Recursive)
        .with_context(|| format!("Failed to watch {}", ctx.root.display()))?;

    // Optionally watch vault
    if args.vault_changes
        && let Some(vault_parent) = ctx.vault_path.parent()
    {
        watcher
            .watch(vault_parent, RecursiveMode::NonRecursive)
            .with_context(|| format!("Failed to watch vault dir {}", vault_parent.display()))?;
    }

    // The stream is bound before anything is printed: the banner this file used
    // to print named a port nothing listened on, which is the whole class of bug
    // that a real bind removes. Port 0 asks the OS for a free port, and the
    // banner names the one that was actually bound.
    let event_server = match args.port {
        Some(port) => {
            let log: SharedLog = Arc::new(Mutex::new(EventLog::default()));
            let (bound, handle) =
                start_event_server(port, ctx.root.display().to_string(), log.clone()).map_err(
                    |error| {
                        ErrorOutcome::new(ExitCode::Unexpected, "cannot start the event server")
                            .with_cause(format!("{error:#}"))
                            .with_fix(
                                "pick another port with --port, or leave --port off to watch \
                                 without the event stream",
                            )
                    },
                )?;
            Some((bound, handle, log))
        }
        None => None,
    };

    if !args.json {
        eprintln!("Watching for changes (debounce: {}ms)...", args.debounce);
        if let Some((port, _, _)) = &event_server {
            eprintln!(
                "Event server on http://127.0.0.1:{port} — GET /health, GET /events (ndjson)"
            );
        }
        if args.vault_changes {
            // Said once, at startup, instead of a per-event "synced" that was
            // `open_existing` returning `Ok(())` for doing nothing.
            eprintln!("vault sync is not wired; --vault-changes only watches the vault file");
        }
    }

    // Main event loop
    let mut last_index = Instant::now();
    let debounce = Duration::from_millis(args.debounce);

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                let relevant =
                    is_relevant_event(&event, args.vault_changes, &ctx.root, &ctx.vault_path);
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
            // The vault file is not source. Counting it as a changed file made
            // a vault write report a re-index of one file with no language.
            let changed_files: Vec<PathBuf> = events
                .drain(..)
                .flat_map(|e| extract_paths(&e))
                .filter(|path| path != &ctx.vault_path)
                .collect();

            if !changed_files.is_empty() {
                if !args.json {
                    eprintln!(
                        "\nChanges detected in {} file(s), re-indexing...",
                        changed_files.len()
                    );
                }

                let mut progress = |line: &str| {
                    if !args.json {
                        eprintln!("  {}", line);
                    }
                };

                for path in &changed_files {
                    let relative = path.strip_prefix(&ctx.root).unwrap_or(path);
                    progress(&format!(
                        "changed {} ({})",
                        relative.display(),
                        detect_language(path)
                    ));
                }
                processed += changed_files.len();

                // Incremental index only changed files
                let report = incremental_index(ctx, &changed_files, &mut progress)
                    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

                if !args.json {
                    eprintln!(
                        "Re-indexed {} files, {} symbols in {:.2}s",
                        report.files,
                        report.symbols,
                        last_index.elapsed().as_secs_f64()
                    );
                }

                // The index pass is the event: the same numbers the progress
                // lines just carried, so a subscriber and the terminal agree
                // about what happened and when.
                if let Some((_, _, log)) = &event_server
                    && let Ok(mut log) = log.lock()
                {
                    log.push(
                        IndexedEvent {
                            files: changed_files
                                .iter()
                                .map(|path| {
                                    path.strip_prefix(&ctx.root)
                                        .unwrap_or(path)
                                        .display()
                                        .to_string()
                                })
                                .collect(),
                            symbols: report.symbols,
                        }
                        .line(),
                    );
                }

                // A vault change is watched and reported by nothing: there is no
                // vault-to-cloud sync in this build, and the startup line says
                // so once instead of a per-event success that did nothing.
            }
            last_index = Instant::now();
        }
    }

    let data = serde_json::json!({
        "watched": true,
        "duration_ms": start.elapsed().as_millis(),
        "events_processed": processed,
    });

    Ok(Outcome::new("watch", "Watch completed", data))
}

/// Check if an event is relevant for indexing/syncing.
fn is_relevant_event(event: &Event, watch_vault: bool, root: &Path, vault_path: &Path) -> bool {
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
        _ => return false,
    }

    for path in &event.paths {
        // Check if path is within workspace root
        if path.starts_with(root)
            && let Ok(rel) = path.strip_prefix(root)
        {
            let rel_str = rel.to_string_lossy();
            // Skip build/cache directories
            if skip_relative(&rel_str) {
                continue;
            }
            // Only watch source files
            if is_source_file(path) {
                return true;
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
            "rs" | "ts"
                | "tsx"
                | "js"
                | "jsx"
                | "py"
                | "go"
                | "java"
                | "kt"
                | "swift"
                | "cpp"
                | "cc"
                | "cxx"
                | "c"
                | "h"
                | "hpp"
                | "cs"
                | "rb"
                | "php"
                | "scala"
                | "clj"
        )
    } else {
        false
    }
}

/// Extract relevant paths from an event.
fn extract_paths(event: &Event) -> Vec<PathBuf> {
    event
        .paths
        .iter()
        .filter(|p| p.is_file())
        .cloned()
        .collect()
}

/// Incrementally index only the changed files.
///
/// `kg::index` already walks the graph incrementally when `full` is false. The
/// per-file extractor internals (`scan_refs`, `mtime_ms`, `root_label`) are
/// private to `kg`, so going through the public entry point avoids a second
/// copy of the extractor that could silently drift from the real one.
fn incremental_index(
    ctx: &AppContext,
    _changed_files: &[PathBuf],
    progress: &mut dyn FnMut(&str),
) -> Result<kg::IndexReport> {
    kg::index(ctx, false, false, progress)
}

/// Directories that never change the graph, checked per path component.
fn skip_relative(relative_path: &str) -> bool {
    relative_path.split('/').any(|part| {
        matches!(
            part,
            "target"
                | "node_modules"
                | ".git"
                | ".memnoth"
                | "dist"
                | "build"
                | "vendor"
                | "__pycache__"
        )
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

/// One re-index, as the event server reports it.
#[derive(Debug, Clone)]
struct IndexedEvent {
    /// Paths relative to the watched root, in the order they were seen.
    files: Vec<String>,
    /// Declarations the pass stored, the same number the progress line says.
    symbols: usize,
}

impl IndexedEvent {
    /// One newline-terminated JSON object, because a client reading the stream
    /// line by line should never have to buffer a whole response to find out
    /// where one event ends.
    fn line(&self) -> String {
        serde_json::json!({
            "event": "indexed",
            "files": self.files,
            "symbols": self.symbols,
            "at_ms": now_ms(),
        })
        .to_string()
            + "\n"
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// How many lines a subscriber may be behind before the oldest are dropped. A
/// watcher can run for hours; a client that has been gone for one of them does
/// not need the backlog it missed.
const EVENT_BACKLOG: usize = 256;

/// How long a quiet stream waits before sending a line that is only there to
/// prove the connection is still open.
const EVENT_PING: Duration = Duration::from_secs(15);

/// The events this run has produced, numbered so a subscriber can ask for what
/// it has not seen yet.
#[derive(Debug, Default)]
struct EventLog {
    lines: VecDeque<(u64, String)>,
    total: u64,
}

impl EventLog {
    fn push(&mut self, line: String) {
        self.total += 1;
        self.lines.push_back((self.total, line));
        while self.lines.len() > EVENT_BACKLOG {
            self.lines.pop_front();
        }
    }

    /// Every line numbered after `cursor`, and the number to continue from.
    fn after(&self, cursor: u64) -> (Vec<String>, u64) {
        let lines = self
            .lines
            .iter()
            .filter(|(number, _)| *number > cursor)
            .map(|(_, line)| line.clone())
            .collect();
        (lines, self.total)
    }
}

type SharedLog = Arc<Mutex<EventLog>>;

/// Bind the event server and serve it from its own thread.
///
/// The bind happens here, on the caller's thread, so a port that cannot be
/// taken is an error the command reports rather than a banner followed by a
/// thread that dies quietly. Returns the port that was actually bound, which is
/// how `--port 0` names a free one.
fn start_event_server(
    port: u16,
    root: String,
    log: SharedLog,
) -> Result<(u16, std::thread::JoinHandle<()>)> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("cannot bind 127.0.0.1:{port}"))?;
    let bound = listener
        .local_addr()
        .map(|address| address.port())
        .unwrap_or(port);

    let handle = std::thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(stream) = connection else { continue };
            let log = log.clone();
            let root = root.clone();
            std::thread::spawn(move || serve_connection(stream, &root, &log));
        }
    });
    Ok((bound, handle))
}

/// Read one request and answer it. A connection is one request: the stream
/// endpoints hold their own connection open and never come back here.
fn serve_connection(stream: TcpStream, root: &str, log: &SharedLog) {
    // A client that opens a socket and says nothing must not keep a thread.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Headers are read to their blank line and otherwise ignored: nothing here
    // needs a header, and half-reading them would answer the next request with
    // the previous one's path.
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) => break,
            Ok(_) if header.trim().is_empty() => break,
            Ok(_) => {}
            Err(_) => return,
        }
    }

    let request_path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();

    let mut stream = stream;
    match request_path.as_str() {
        "/health" => {
            let body = serde_json::json!({
                "status": "ok",
                "watching": root,
                "events": log.lock().map(|log| log.total).unwrap_or(0),
            })
            .to_string();
            let _ = write_response(&mut stream, "200 OK", "application/json", &body);
            // A response with no length on the wire is delimited by close, and a
            // close with a second handle still open is a reset on Windows often
            // enough to fail the handshake one run in three. Half-closing says
            // "that was all" while the socket is still alive to say it.
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
        "/events" => stream_events(stream, root, log),
        _ => {
            let body = serde_json::json!({
                "error": "no such route",
                "routes": ["/health", "/events"],
            })
            .to_string();
            let _ = write_response(&mut stream, "404 Not Found", "application/json", &body);
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    }
}

/// One complete response, closed by `Connection: close`.
fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Keep one connection open and write each event as it happens.
///
/// The body is delimited by close (there is no length to announce), so the
/// headers say `Connection: close` and each line ends where an event ends.
fn stream_events(mut stream: TcpStream, root: &str, log: &SharedLog) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\n\
                Cache-Control: no-store\r\nConnection: close\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    // A first line immediately: a client that connects only to prove the server
    // is live must not have to wait for somebody to save a file.
    let ready = serde_json::json!({
        "event": "ready",
        "watching": root,
        "events": log.lock().map(|log| log.total).unwrap_or(0),
    })
    .to_string()
        + "\n";
    if stream.write_all(ready.as_bytes()).is_err() || stream.flush().is_err() {
        return;
    }

    let mut cursor = log.lock().map(|log| log.total).unwrap_or(0);
    let mut last_write = Instant::now();
    loop {
        let (lines, next) = match log.lock() {
            Ok(log) => log.after(cursor),
            // A poisoned log is a panic elsewhere; the stream ends rather than
            // spinning on a lock that cannot be taken.
            Err(_) => return,
        };
        cursor = next;

        let mut wrote = false;
        for line in lines {
            if stream.write_all(line.as_bytes()).is_err() {
                return;
            }
            wrote = true;
        }
        if wrote {
            if stream.flush().is_err() {
                return;
            }
            last_write = Instant::now();
        } else if last_write.elapsed() >= EVENT_PING {
            // Silence is not the same as a live connection. A ping both proves
            // the stream is still there and turns a client that went away into
            // a write error, which is the only way this thread learns of it.
            if stream.write_all(b"{\"event\":\"ping\"}\n").is_err() || stream.flush().is_err() {
                return;
            }
            last_write = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn event() -> IndexedEvent {
        IndexedEvent {
            files: vec!["src/lib.rs".to_string()],
            symbols: 3,
        }
    }

    #[test]
    fn an_indexed_event_is_one_json_line() {
        let line = event().line();
        assert!(line.ends_with('\n'), "{line:?}");
        assert_eq!(line.matches('\n').count(), 1, "{line:?}");

        let value: serde_json::Value =
            serde_json::from_str(line.trim_end()).expect("one json object per line");
        assert_eq!(value["event"], "indexed");
        assert_eq!(value["files"][0], "src/lib.rs");
        assert_eq!(value["symbols"], 3);
        assert!(value["at_ms"].as_i64().is_some(), "{value}");
    }

    #[test]
    fn a_late_subscriber_starts_at_the_live_edge() {
        let mut log = EventLog::default();
        log.push(event().line());
        let cursor = log.total;
        log.push(
            IndexedEvent {
                files: vec!["src/main.rs".to_string()],
                symbols: 1,
            }
            .line(),
        );

        let (lines, next) = log.after(cursor);
        assert_eq!(lines.len(), 1, "only what happened after the connect");
        assert!(lines[0].contains("src/main.rs"), "{lines:?}");
        assert_eq!(next, 2);
    }

    #[test]
    fn the_backlog_is_bounded_but_the_count_is_not() {
        let mut log = EventLog::default();
        for _ in 0..(EVENT_BACKLOG + 10) {
            log.push("x\n".to_string());
        }
        assert_eq!(log.lines.len(), EVENT_BACKLOG);
        assert_eq!(log.total as usize, EVENT_BACKLOG + 10);
    }

    #[test]
    fn the_event_server_answers_health_and_streams_a_real_event() {
        let log: SharedLog = Arc::new(Mutex::new(EventLog::default()));
        // Port 0: the OS picks one, so the test never races another process for
        // a fixed port and the returned port is the one the banner would name.
        let (port, _handle) = start_event_server(0, "workspace".to_string(), log.clone())
            .expect("an ephemeral port always binds");

        let mut health = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        health
            .write_all(b"GET /health HTTP/1.1\r\nHost: test\r\n\r\n")
            .expect("write request");
        let mut answer = String::new();
        health.read_to_string(&mut answer).expect("read response");
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(answer.contains("\"status\":\"ok\""), "{answer}");
        assert!(answer.contains("workspace"), "{answer}");

        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .write_all(b"GET /events HTTP/1.1\r\nHost: test\r\n\r\n")
            .expect("write request");
        // The stream is a live one: it sends keep-alive pings until the event
        // arrives. A read timeout turns a missing event into a failure instead
        // of a test that hangs the suite.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).expect("read header");
            if header.trim().is_empty() {
                break;
            }
        }
        let mut first = String::new();
        reader.read_line(&mut first).expect("the ready line");
        assert!(first.contains("\"ready\""), "{first}");

        // A real event, pushed where the watcher pushes it, reaches the open
        // connection without a reconnect.
        log.lock().expect("log").push(event().line());
        // `{"event":"ping"}` is the keep-alive a quiet stream sends, and a
        // loaded machine can take longer than the ping interval to get here — so
        // the assertion is that the event arrives, not that it is the very next
        // line. Pings are skipped; anything else that is not the event fails.
        let mut found = String::new();
        // Skip pings until the event shows up. A fixed number of lines was the
        // old bound, and a loaded machine can emit that many pings before the
        // pushed event is read — the assertion is that the event arrives, so
        // the bound is time, not line count.
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let mut line = String::new();
            reader.read_line(&mut line).expect("a line from the stream");
            if line.contains("src/lib.rs") {
                found = line;
                break;
            }
            assert!(line.contains("\"ping\""), "unexpected line: {line}");
        }
        assert!(found.contains("src/lib.rs"), "the event never arrived");
    }

    #[test]
    fn an_unknown_route_says_which_routes_exist() {
        let log: SharedLog = Arc::new(Mutex::new(EventLog::default()));
        let (port, _handle) = start_event_server(0, "workspace".to_string(), log).expect("bind");
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .write_all(b"GET /nope HTTP/1.1\r\nHost: test\r\n\r\n")
            .expect("write request");
        let mut answer = String::new();
        stream.read_to_string(&mut answer).expect("read response");
        assert!(answer.starts_with("HTTP/1.1 404"), "{answer}");
        assert!(answer.contains("/events"), "{answer}");
    }
}
