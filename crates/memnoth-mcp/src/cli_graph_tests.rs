//! Tests for the adapter that lets the server read the graph the CLI writes.
//!
//! Split out of `cli_graph.rs` only for size; the module under test is private
//! to this crate, so these exercise the real SQL against fixture files rather
//! than a mock.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::json;

use super::*;

/// A scratch workspace with a `.memnoth` directory and nothing in it.
fn scratch(name: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("memnoth-cli-graph-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".memnoth")).unwrap();
    root
}

fn graph_path(root: &Path) -> PathBuf {
    root.join(".memnoth").join("graph.db")
}

/// The schema `memnoth kg` writes today.
fn create_current(root: &Path) -> Connection {
    let conn = Connection::open(graph_path(root)).unwrap();
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
         INSERT INTO meta VALUES ('built_at_ms', '1700000000000');
         INSERT INTO meta VALUES ('root', '/repo');",
    )
    .unwrap();
    conn
}

/// A graph as an older build wrote it: no `container`, no `end_line`, and no
/// `exported`. It must still answer what it can instead of failing every query.
fn create_legacy(root: &Path) -> Connection {
    let conn = Connection::open(graph_path(root)).unwrap();
    conn.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE files (path TEXT PRIMARY KEY, language TEXT NOT NULL,
                             bytes INTEGER NOT NULL, mtime_ms INTEGER NOT NULL,
                             symbols INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE symbols (id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL,
                               name TEXT NOT NULL, kind TEXT NOT NULL, line INTEGER NOT NULL);
         CREATE TABLE refs (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                            path TEXT NOT NULL, line INTEGER NOT NULL);
         INSERT INTO meta VALUES ('schema', '1');",
    )
    .unwrap();
    conn
}

#[test]
fn a_workspace_with_no_graph_has_no_graph() {
    let root = scratch("missing");
    assert!(CliGraph::open(&root).is_none());
}

#[test]
fn a_database_that_is_not_a_graph_has_no_graph() {
    let root = scratch("foreign");
    // A real SQLite file, but another tool's: answering "0 symbols" from it
    // would look like an indexed workspace with nothing in it.
    Connection::open(graph_path(&root))
        .unwrap()
        .execute_batch("CREATE TABLE notes (body TEXT);")
        .unwrap();
    assert!(CliGraph::open(&root).is_none());
}

#[test]
fn the_current_schema_answers_search_and_provenance() {
    let root = scratch("current");
    let conn = create_current(&root);
    conn.execute_batch(
        "INSERT INTO files VALUES ('src/a.rs', 'rust', 10, 0, 2);
         INSERT INTO symbols (path, name, kind, line, end_line, container, exported)
              VALUES ('src/a.rs', 'parse_config', 'function', 3, 9, 'Config', 1),
                     ('src/a.rs', 'Config', 'struct', 1, 12, NULL, 1);",
    )
    .unwrap();
    drop(conn);

    let graph = CliGraph::open(&root).expect("the graph opens");
    let answer = graph.search("parse", 10).unwrap();
    assert_eq!(answer["count"], 1);
    assert_eq!(answer["hits"][0]["name"], "parse_config");
    assert_eq!(answer["hits"][0]["container"], "Config");
    assert_eq!(answer["indexed"]["schema"], "3");
    assert!(
        answer["indexed"]["age"].as_str().is_some(),
        "every answer says how old the snapshot is: {answer}"
    );
}

#[test]
fn search_ranks_an_exact_name_before_a_longer_containing_one() {
    let root = scratch("rank");
    let conn = create_current(&root);
    conn.execute_batch(
        "INSERT INTO symbols (path, name, kind, line, end_line, container, exported)
              VALUES ('src/a.rs', 'parse_config', 'function', 1, 2, NULL, 1),
                     ('src/b.rs', 'parse', 'function', 5, 8, NULL, 1);",
    )
    .unwrap();
    drop(conn);

    let graph = CliGraph::open(&root).unwrap();
    let hits = graph.search("parse", 10).unwrap();
    assert_eq!(hits["hits"][0]["name"], "parse", "{hits}");
}

#[test]
fn a_legacy_graph_answers_instead_of_erroring_on_a_column_it_lacks() {
    let root = scratch("legacy");
    let conn = create_legacy(&root);
    conn.execute_batch(
        "INSERT INTO symbols (path, name, kind, line)
              VALUES ('src/a.rs', 'run_tests', 'function', 7);
         INSERT INTO refs (name, path, line) VALUES ('run_tests', 'src/b.rs', 2);",
    )
    .unwrap();
    drop(conn);

    let graph = CliGraph::open(&root).expect("a legacy graph still opens");
    let answer = graph.search("run_tests", 5).unwrap();
    assert_eq!(answer["count"], 1);
    assert_eq!(
        answer["hits"][0]["container"], "",
        "a missing column reads as empty, not as a failure"
    );
    // `end_line` is absent too, so a definition reports its own line rather
    // than an empty range.
    let impact = graph.impact("run_tests", 1).unwrap();
    assert_eq!(impact["definitions"][0]["end_line"], 7);
}

#[test]
fn a_graph_without_exports_refuses_a_dead_code_report() {
    let root = scratch("no-exports");
    let conn = create_legacy(&root);
    conn.execute_batch(
        "INSERT INTO symbols (path, name, kind, line)
              VALUES ('src/a.rs', 'helper', 'function', 1);",
    )
    .unwrap();
    drop(conn);

    let graph = CliGraph::open(&root).unwrap();
    let answer = graph.dead(100).unwrap();
    assert_eq!(answer["supported"], false, "{answer}");
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .contains("before exports were recorded"),
        "the refusal says why: {answer}"
    );
}

#[test]
fn dead_lists_an_export_no_other_file_mentions() {
    let root = scratch("dead");
    let conn = create_current(&root);
    conn.execute_batch(
        "INSERT INTO symbols (path, name, kind, line, end_line, container, exported)
              VALUES ('src/a.rs', 'orphan_export', 'function', 1, 4, NULL, 1),
                     ('src/a.rs', 'used_export', 'function', 6, 9, NULL, 1),
                     ('src/a.rs', 'private_helper', 'function', 11, 12, NULL, 0);
         -- One mention from another file, and one from the declaring file only.
         INSERT INTO refs (name, path, line, container)
              VALUES ('used_export', 'src/b.rs', 3, NULL),
                     ('orphan_export', 'src/a.rs', 20, NULL);",
    )
    .unwrap();
    drop(conn);

    let graph = CliGraph::open(&root).unwrap();
    let answer = graph.dead(100).unwrap();
    let names: Vec<&str> = answer["dead_exports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["orphan_export"],
        "a mention inside its own file is not a use, and a private declaration is not an export"
    );
    assert_eq!(answer["total"], 1);
}

#[test]
fn staleness_names_changed_and_deleted_files_and_nothing_else() {
    let root = scratch("stale");
    let conn = create_current(&root);
    conn.execute_batch(
        "INSERT INTO files VALUES ('src/fresh.rs', 'rust', 4, 0, 1),
                                   ('src/gone.rs', 'rust', 4, 0, 1);",
    )
    .unwrap();
    drop(conn);
    // 'fresh.rs' exists with a current mtime, which is not the recording `0`.
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/fresh.rs"), "fn a() {}\n").unwrap();

    let graph = CliGraph::open(&root).unwrap();
    let stale = graph.staleness();
    assert_eq!(stale["changed"], 1, "{stale}");
    assert_eq!(stale["deleted"], 1, "{stale}");
    assert_eq!(stale["stale"], true);
    assert_eq!(stale["changed_examples"][0], "src/fresh.rs");
    assert_eq!(stale["deleted_examples"][0], "src/gone.rs");
    assert!(
        stale["hint"].as_str().unwrap().contains("memnoth kg index"),
        "a stale answer says what to do: {stale}"
    );
}

#[test]
fn a_current_graph_says_so_rather_than_staying_silent() {
    let root = scratch("current-files");
    let conn = create_current(&root);
    let file = root.join("src/ok.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "fn a() {}\n").unwrap();
    let mtime = std::fs::metadata(&file)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    conn.execute(
        "INSERT INTO files VALUES ('src/ok.rs', 'rust', 10, ?1, 1)",
        rusqlite::params![mtime],
    )
    .unwrap();
    drop(conn);

    let graph = CliGraph::open(&root).unwrap();
    let stale = graph.staleness();
    assert_eq!(stale["stale"], false, "{stale}");
    assert_eq!(stale["changed"], 0);
}

#[test]
fn trace_pages_mentions_and_the_cursor_moves() {
    let root = scratch("trace");
    let conn = create_current(&root);
    conn.execute_batch(
        "INSERT INTO symbols (path, name, kind, line, end_line, container, exported)
              VALUES ('src/a.rs', 'run', 'function', 1, 3, NULL, 1);",
    )
    .unwrap();
    for line in 1..=5 {
        conn.execute(
            "INSERT INTO refs (name, path, line, container) VALUES ('run', ?1, ?2, NULL)",
            rusqlite::params![format!("src/use_{line}.rs"), line as i64],
        )
        .unwrap();
    }
    drop(conn);

    let graph = CliGraph::open(&root).unwrap();
    let first = graph.trace_page("run", 2, None).unwrap();
    assert_eq!(first["references"].as_array().unwrap().len(), 2);
    assert_eq!(first["page"]["total"], 5);
    let cursor = first["next_cursor"].as_str().expect("a second page exists");

    let second = graph.trace_page("run", 2, Some(cursor)).unwrap();
    assert_eq!(second["references"].as_array().unwrap().len(), 2);
    assert_ne!(
        first["references"][0], second["references"][0],
        "the second page continues rather than repeating the first"
    );
}

#[test]
fn the_advertised_page_size_is_the_one_trace_page_honours() {
    // The schema used to promise 500 while the code clamped to 100, so a caller
    // asking for the advertised maximum silently got a fifth of it. Both sides
    // are read here, so the text and the clamp cannot drift apart again.
    let schema = crate::catalog::tool_definitions()
        .into_iter()
        .find(|tool| tool["name"] == "kg_trace")
        .expect("kg_trace is in the catalog");
    let page_size = schema["inputSchema"]["properties"]["page_size"]["description"]
        .as_str()
        .expect("page_size is described");
    assert!(
        page_size.contains(&format!("clamped to {MAX_REFERENCES}")),
        "kg_trace advertises {page_size:?} but the clamp is {MAX_REFERENCES}"
    );

    let root = scratch("page-size");
    let conn = create_current(&root);
    // A name has to resolve before it has mentions, so the declaration comes first.
    conn.execute_batch(
        "INSERT INTO symbols (path, name, kind, line, end_line, container, exported)
              VALUES ('src/wide.rs', 'wide', 'function', 1, 2, NULL, 1);",
    )
    .unwrap();
    for line in 1..=(MAX_REFERENCES + 25) {
        conn.execute(
            "INSERT INTO refs (name, path, line, container) VALUES ('wide', ?1, ?2, NULL)",
            rusqlite::params![format!("src/use_{line}.rs"), line as i64],
        )
        .unwrap();
    }
    drop(conn);

    let graph = CliGraph::open(&root).unwrap();
    let page = graph.trace_page("wide", MAX_REFERENCES * 2, None).unwrap();
    assert_eq!(
        page["references"].as_array().unwrap().len(),
        MAX_REFERENCES,
        "a page larger than the advertised bound must return that bound, not a smaller one"
    );
}

#[test]
fn the_graph_is_opened_read_only() {
    let root = scratch("readonly");
    let conn = create_current(&root);
    conn.execute_batch(
        "INSERT INTO symbols (path, name, kind, line, end_line, container, exported)
              VALUES ('src/a.rs', 'kept', 'function', 1, 2, NULL, 1);",
    )
    .unwrap();
    drop(conn);

    let graph = CliGraph::open(&root).unwrap();
    let refused = graph
        .conn
        .execute("DELETE FROM symbols", [])
        .expect_err("the server must never write the person's graph");
    assert!(
        refused.to_string().to_lowercase().contains("readonly")
            || refused.to_string().to_lowercase().contains("read-only"),
        "{refused}"
    );
}

#[test]
fn coverage_reports_what_the_graph_holds_without_claiming_resolution() {
    let root = scratch("coverage");
    let conn = create_current(&root);
    conn.execute_batch(
        "INSERT INTO files VALUES ('src/a.rs', 'rust', 10, 0, 1);
         INSERT INTO symbols (path, name, kind, line, end_line, container, exported)
              VALUES ('src/a.rs', 'run', 'function', 1, 2, NULL, 1);
         INSERT INTO refs (name, path, line, container) VALUES ('run', 'src/b.rs', 1, NULL);",
    )
    .unwrap();
    drop(conn);

    let graph = CliGraph::open(&root).unwrap();
    let coverage = graph.coverage();
    assert_eq!(coverage["files"], 1);
    assert_eq!(coverage["symbols"], 1);
    assert_eq!(coverage["references"], 1);
    assert_eq!(coverage["languages"][0]["language"], "rust");
    assert!(
        coverage["resolution"]
            .as_str()
            .unwrap()
            .contains("not what it binds"),
        "{coverage}"
    );
}

#[test]
fn the_cli_and_the_server_read_the_same_file() {
    let root = scratch("one-file");
    // The path the CLI writes is the path this adapter reads; a second name for
    // the same concept is what produced two graphs in the first place.
    let graph = CliGraph::open(&root);
    assert!(graph.is_none(), "nothing indexed yet");
    create_current(&root);
    let graph = CliGraph::open(&root).expect("the CLI's graph is the server's graph");
    assert_eq!(
        graph.path(),
        graph_path(&root).display().to_string(),
        "the adapter reads <root>/.memnoth/graph.db and nothing else"
    );
}

#[test]
fn doctor_passes_a_graph_that_is_current_and_says_what_it_did_not_check() {
    let root = scratch("doctor-clean");
    let conn = create_current(&root);
    conn.execute_batch(
        "INSERT INTO files VALUES ('src/a.rs', 'rust', 10, 0, 1);
         INSERT INTO symbols (path, name, kind, line, end_line, container, exported)
              VALUES ('src/a.rs', 'run', 'function', 1, 2, NULL, 1);",
    )
    .unwrap();
    drop(conn);
    // The file rows carry mtime 0, which is not what the disk says — so write a
    // file whose mtime really is in the same second the row claims, or the
    // doctor would report the fixture as changed rather than the graph as clean.
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "pub fn run() {}\n").unwrap();
    let mtime = std::fs::metadata(root.join("src/a.rs"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    Connection::open(graph_path(&root))
        .unwrap()
        .execute(
            "UPDATE files SET mtime_ms = ?1 WHERE path = 'src/a.rs'",
            rusqlite::params![mtime],
        )
        .unwrap();

    let graph = CliGraph::open(&root).unwrap();
    let report = graph.doctor();
    assert_eq!(
        report["findings"],
        json!(["no problems found — the graph is current and every report it supports will run"]),
        "a clean graph gets a clean verdict: {report}"
    );
    assert_eq!(
        report["not_checked"].as_array().unwrap().len(),
        3,
        "the verdict names its own scope: {report}"
    );
    assert_eq!(report["files"], 1);
    assert_eq!(report["symbols"], 1);
}

#[test]
fn doctor_names_the_columns_an_older_graph_was_built_without() {
    let root = scratch("doctor-legacy");
    let conn = create_legacy(&root);
    conn.execute_batch(
        "INSERT INTO symbols (path, name, kind, line) VALUES ('src/a.rs', 'run', 'function', 1);",
    )
    .unwrap();
    drop(conn);

    let graph = CliGraph::open(&root).unwrap();
    let findings = graph.doctor()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    for expected in [
        "container",
        "end line",
        "exports were recorded",
        "no build time",
    ] {
        assert!(
            findings.contains(expected),
            "an old graph must be diagnosed by column, not vaguely: {expected} missing from {findings}"
        );
    }
    assert!(
        !findings.contains("no problems found"),
        "a graph missing three columns is not healthy: {findings}"
    );
}

#[test]
fn doctor_calls_a_language_that_extracted_nothing_a_problem() {
    let root = scratch("doctor-empty-language");
    let conn = create_current(&root);
    // A file the extractor recorded but found no declarations in: either the
    // tree is prose or the extractor is not covering that language, and either
    // way a search for it will come back empty for a reason worth naming.
    conn.execute_batch("INSERT INTO files VALUES ('docs/a.exotic', 'exotic', 10, 0, 0);")
        .unwrap();
    drop(conn);

    let graph = CliGraph::open(&root).unwrap();
    let findings = graph.doctor()["findings"].to_string();
    assert!(
        findings.contains("exotic") && findings.contains("no declarations"),
        "{findings}"
    );
}

#[test]
fn provenance_names_the_second_graph_when_the_workspace_has_one() {
    let root = scratch("two-graphs");
    let conn = create_current(&root);
    conn.execute_batch(
        "INSERT INTO symbols (path, name, kind, line, end_line, container, exported)
              VALUES ('src/a.rs', 'run', 'function', 1, 2, NULL, 1);",
    )
    .unwrap();
    drop(conn);

    let graph = CliGraph::open(&root).unwrap();
    assert_eq!(
        graph.search("run", 5).unwrap()["indexed"]["shadowed_by"],
        json!(null),
        "one graph means nothing to explain"
    );

    // The bundled indexer's file, from a `kg_index` call that took the scoped
    // path. The answer still comes from graph.db; saying so is what stops a
    // caller concluding their own index run did nothing.
    let second = root.join(".memnoth").join("kg.db");
    std::fs::write(&second, b"not read by this adapter").unwrap();

    let graph = CliGraph::open(&root).unwrap();
    let shadowed = &graph.search("run", 5).unwrap()["indexed"]["shadowed_by"];
    assert_eq!(shadowed["path"], second.display().to_string(), "{shadowed}");
    assert!(
        shadowed["note"].as_str().unwrap().contains("graph.db"),
        "the note names the file that did answer: {shadowed}"
    );
    assert!(
        graph.doctor()["findings"]
            .to_string()
            .contains("second graph"),
        "the health check reports it too"
    );
}

#[test]
fn an_empty_symbol_table_is_still_a_graph() {
    let root = scratch("empty");
    create_current(&root);
    let graph = CliGraph::open(&root).expect("an indexed workspace with no code still has a graph");
    let answer = graph.search("anything", 5).unwrap();
    assert_eq!(answer["count"], 0);
    assert_eq!(
        answer["hits"],
        json!([]),
        "an empty answer from a real graph is honest: {answer}"
    );
}
