//! Every registry row must say where it runs, what it risks, and what to run
//! when it cannot.
//!
//! This is the test behind W13-27. A row without that metadata cannot be
//! projected into the palette, and the failure it prevents is specific: a
//! terminal-only verb (`mcp add`, `serve`) rendered as if Enter would run it,
//! or a destructive verb with no risk chip. Both are promises the session
//! cannot keep, and neither produces a compile error.

use memnoth_command::build_p0_registry;
use serde_json::Value;

/// The one projection every surface reads.
fn projection() -> Value {
    build_p0_registry().build().build_command_rows()
}

/// Every row in the projection, borrowed.
fn rows(projection: &Value) -> &[Value] {
    projection["commands"]
        .as_array()
        .expect("build_command_rows must return a commands array")
}

/// One row by id, or a panic naming what was missing.
fn row<'a>(projection: &'a Value, id: &str) -> &'a Value {
    rows(projection)
        .iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("no registry row named {id}"))
}

#[test]
fn projection_covers_every_registry_row() {
    let registry = build_p0_registry().build();
    let projection = projection();
    assert_eq!(
        rows(&projection).len(),
        registry.len(),
        "the projection dropped a row; a command that exists in the registry must \
         still appear (hidden, with a reason), or no surface can explain it"
    );
    assert_eq!(projection["command_count"], registry.len());
}

#[test]
fn every_row_has_a_description_and_a_headless_path() {
    let projection = projection();
    for row in rows(&projection) {
        let id = row["id"].as_str().unwrap_or("?");
        assert!(
            !row["description"].as_str().unwrap_or("").trim().is_empty(),
            "{id} has no description; a palette row without one is noise"
        );
        let headless = row["headless"].as_str().unwrap_or("");
        assert!(
            headless.starts_with("memnoth "),
            "{id} has no headless equivalent (got {headless:?})"
        );
        assert!(
            !row["failure_hint"].as_str().unwrap_or("").trim().is_empty(),
            "{id} names no next command when it fails"
        );
    }
}

#[test]
fn palette_rows_are_a_line_that_can_be_typed() {
    let projection = projection();
    for row in rows(&projection) {
        let Some(line) = row["palette"].as_str() else {
            continue;
        };
        let id = row["id"].as_str().unwrap_or("?");
        assert!(!line.trim().is_empty(), "{id} has an empty palette line");
        assert!(
            !line.starts_with('/'),
            "{id} stores a leading slash in its palette line"
        );
        assert!(
            !line.contains("  "),
            "{id} has a doubled space in its palette line: {line:?}"
        );
    }
}

#[test]
fn terminal_only_rows_say_so_before_selection() {
    let projection = projection();
    let mut terminals = 0;
    for row in rows(&projection) {
        if row["availability"] != "terminal" {
            continue;
        }
        terminals += 1;
        let id = row["id"].as_str().unwrap_or("?");
        assert_eq!(
            row["screen"], "terminal",
            "{id} is terminal-only but claims another screen"
        );
        assert_eq!(
            row["failure_hint"], "run it in another terminal",
            "{id} is terminal-only but does not say where to run it"
        );
    }
    assert!(
        terminals > 0,
        "no row is labelled terminal-only; `/serve` and `/watch` would look runnable"
    );
}

#[test]
fn auth_gated_rows_point_at_login() {
    let projection = projection();
    for row in rows(&projection) {
        if row["availability"] != "auth" {
            continue;
        }
        let id = row["id"].as_str().unwrap_or("?");
        assert_eq!(
            row["failure_hint"], "run `memnoth auth login` first",
            "{id} needs auth but does not name the command that grants it"
        );
    }
}

#[test]
fn no_destructive_row_is_labelled_read() {
    let projection = projection();
    // The words that make a verb destructive. A new verb with one of these in
    // its id must declare `DangerLevel::Destructive`, not inherit `Safe`.
    const DESTRUCTIVE: &[&str] = &["forget", "revoke", "clean", "uninstall", "remove", "delete"];
    for row in rows(&projection) {
        let id = row["id"].as_str().unwrap_or("");
        let head = id.rsplit('.').next().unwrap_or(id);
        if DESTRUCTIVE.contains(&head) {
            assert_ne!(
                row["risk"], "read",
                "{id} deletes or revokes something but is labelled read-only"
            );
        }
    }
}

#[test]
fn the_signature_verbs_have_palette_rows() {
    let projection = projection();
    for id in ["why", "impact", "lineage", "drift", "simulate"] {
        let row = row(&projection, id);
        assert!(
            row["palette"].is_string(),
            "{id} is a signature verb with no palette row; it stays buried"
        );
        assert_eq!(
            row["risk"], "read",
            "{id} must not write: it is an explanation, not a mutation"
        );
    }
}
