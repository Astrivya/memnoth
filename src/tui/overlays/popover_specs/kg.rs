//! Windows for `kg …` answers.
//!
//! The six KG verbs that already had a window (`refs`, `impact`, `callers`,
//! `callees`, `search`, `dead_code`) stay in `popover.rs`, where they share the
//! hit-row renderer. These are the rest of the graph verbs whose answer is a
//! list: an explore, a tree, a review, the tests that cover a symbol, the people
//! a file belongs to, the import cycles and edges, a DSL query, the call chain
//! between two symbols, a file's outline, the churn rankings, the exports nothing
//! reaches, the blast radius of a diff, and the per-language totals the index
//! reports.
//!
//! `kg def` is the one window built around a single declaration, and it earns the
//! box because the row is not a dead end: Enter opens the full explore and the
//! footer asks the four questions that follow a declaration (refs, callers,
//! callees, impact). `kg status` outside an index is absent for the opposite
//! reason — "not indexed" is a sentence, not a table — while an indexed status
//! does open the language breakdown.
//!
//! Every row's action is a line the TUI really parses (`/kg explore`, `/kg def`,
//! `/kg tests-for`, `/kg tree`, `/kg deps`, `/kg refs`), so Enter is never a
//! promise the slash parser cannot keep.

use serde_json::Value;

use crate::tui::overlays::popover::{
    PopoverButton, PopoverRow, PopoverSpec, follow_buttons, list, present, string,
};

pub(crate) fn spec(command: &str, data: &Value) -> Option<PopoverSpec> {
    match command {
        "kg.explore" => explore(data),
        "kg.tree" => tree(data),
        "kg.tests_for" => tests_for(data),
        "kg.owners" => owners(data),
        "kg.review" => review(data),
        "kg.deps" => deps(data),
        "kg.cycles" => cycles(data),
        "kg.query" => query(data),
        "kg.def" => def(data),
        "kg.callpath" => callpath(data),
        "kg.outline" => outline(data),
        "kg.hotspots" => hotspots(data),
        "kg.unused_exports" => unused_exports(data),
        "kg.diff_impact" => diff_impact(data),
        "kg.status" | "kg.index" => languages(command, data),
        _ => None,
    }
}

/// `path:line`, the address every graph verb agrees on.
fn location(hit: &Value) -> String {
    let path = string(hit, "path");
    let line = hit.get("line").and_then(Value::as_i64).unwrap_or(0);
    if line > 0 {
        format!("{path}:{line}")
    } else {
        path
    }
}

/// One `ImpactHit`: a file, a line, the source line, and whether it is call-shaped.
///
/// The title is the file because an `ImpactHit` carries no symbol name — the
/// mention is the row, not the thing mentioned — and the tag is the relationship,
/// which is the part a reader is filtering on.
fn hit_row(hit: &Value) -> Option<PopoverRow> {
    let path = present(hit, "path")?;
    let line = hit.get("line").and_then(Value::as_i64).unwrap_or(0);
    let context = present(hit, "context").unwrap_or_default();
    let mut detail = String::new();
    if line > 0 {
        detail.push_str(&format!("line {line}"));
    }
    if !context.is_empty() {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        detail.push_str(&context);
    }
    if detail.is_empty() {
        detail.push_str("no source line");
    }
    let row = PopoverRow::new(path, detail);
    Some(
        if hit
            .get("call_shaped")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            row.tagged("call")
        } else {
            row.tagged("mention")
        },
    )
}

/// The symbol a verb was asked about, for the buttons that ask the next question.
fn symbol_buttons(spec: PopoverSpec, symbol: &str) -> PopoverSpec {
    if symbol.is_empty() {
        return spec;
    }
    spec.button(PopoverButton::run('d', "def", format!("/kg def {symbol}")))
        .button(PopoverButton::run(
            'i',
            "impact",
            format!("/kg impact {symbol}"),
        ))
        .button(PopoverButton::run(
            't',
            "tests",
            format!("/kg tests-for {symbol}"),
        ))
}

fn truncation_note(mut note: String, data: &Value) -> String {
    if data
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        if !note.is_empty() {
            note.push_str(" · ");
        }
        note.push_str("truncated");
    }
    note
}

fn explore(data: &Value) -> Option<PopoverSpec> {
    let symbol = string(data, "symbol");
    let mut rows = Vec::new();

    if let Some(definition) = data.get("definition").filter(|value| value.is_object()) {
        rows.push(PopoverRow::section("definition".to_string()));
        let prefix = present(definition, "container")
            .map(|container| format!("{container}::"))
            .unwrap_or_default();
        let mut detail = location(definition);
        let kind = string(definition, "kind");
        if !kind.is_empty() {
            detail.push_str(&format!(" · {kind}"));
        }
        rows.push(PopoverRow::new(format!("{prefix}{symbol}"), detail));
    }

    for (key, label) in [
        ("callers", "callers"),
        ("callees", "callees"),
        ("references", "references"),
    ] {
        let hits = list(data, key);
        if hits.is_empty() {
            continue;
        }
        rows.push(PopoverRow::section(format!("{label} ({})", hits.len())));
        rows.extend(hits.iter().filter_map(hit_row));
    }

    if !rows.iter().any(|row| !row.header) {
        // An unresolved symbol has nothing to browse: the verb's own text says
        // which name missed.
        return None;
    }

    let note = truncation_note(String::new(), data);
    let mut spec = PopoverSpec::new(
        format!("Explore: {symbol}"),
        "one declaration, then the three lists that point at it",
    )
    .rows(rows);
    if !note.is_empty() {
        spec = spec.note(note);
    }
    Some(symbol_buttons(spec, &symbol))
}

fn tree(data: &Value) -> Option<PopoverSpec> {
    let dir = string(data, "dir");
    let entries = list(data, "entries");
    let mut rows = Vec::new();
    let mut symbols = 0usize;

    for entry in entries {
        let path = string(entry, "path");
        let language = string(entry, "language");
        let declared = list(entry, "symbols");
        symbols += declared.len();
        rows.push(PopoverRow::section(format!(
            "{path} · {language} · {} symbol(s)",
            declared.len()
        )));
        for symbol in declared {
            let name = string(symbol, "name");
            let mut detail = format!("line {}", string(symbol, "line"));
            if let Some(container) = present(symbol, "container") {
                detail.push_str(&format!(" · in {container}"));
            }
            rows.push(
                PopoverRow::new(name.clone(), detail)
                    .tagged(string(symbol, "kind"))
                    .running(format!("/kg explore {name}")),
            );
        }
    }

    if symbols == 0 {
        return None;
    }

    Some(
        PopoverSpec::new(format!("Tree: {dir}"), "enter explores one symbol")
            .rows(rows)
            .note(format!("{symbols} symbols in {} files", entries.len())),
    )
}

fn tests_for(data: &Value) -> Option<PopoverSpec> {
    let symbol = string(data, "symbol");
    let tests = list(data, "tests");
    if tests.is_empty() {
        return None;
    }

    let rows: Vec<PopoverRow> = tests
        .iter()
        .map(|test| {
            let path = string(test, "path");
            let name = present(test, "test_name").unwrap_or_else(|| path.clone());
            PopoverRow::new(name, location(test)).tagged(string(test, "coverage_type"))
        })
        .collect();

    let inline = tests
        .iter()
        .filter(|test| string(test, "coverage_type") == "inline")
        .count();
    let mut note = format!("{} covering test(s)", rows.len());
    if inline > 0 {
        note.push_str(&format!(" · {inline} inline"));
    }

    Some(
        PopoverSpec::new(
            format!("Tests for {symbol}"),
            "inline = `#[cfg(test)] mod tests` in the declaring file",
        )
        .rows(rows)
        .note(note)
        .button(PopoverButton::run(
            'e',
            "explore",
            format!("/kg explore {symbol}"),
        )),
    )
}

fn owners(data: &Value) -> Option<PopoverSpec> {
    let path = string(data, "path");
    let mut rows = Vec::new();

    let codeowners = list(data, "codeowners");
    if !codeowners.is_empty() {
        rows.push(PopoverRow::section(format!(
            "codeowners ({})",
            codeowners.len()
        )));
        for entry in codeowners {
            let owners: Vec<String> = entry
                .get("owners")
                .and_then(Value::as_array)
                .map(|owners| {
                    owners
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            rows.push(PopoverRow::new(
                string(entry, "pattern"),
                if owners.is_empty() {
                    "no owner listed".to_string()
                } else {
                    owners.join(", ")
                },
            ));
        }
    }

    let blame = list(data, "blame");
    if !blame.is_empty() {
        rows.push(PopoverRow::section(format!("blame ({})", blame.len())));
        for entry in blame {
            let commit = string(entry, "commit");
            let short: String = commit.chars().take(7).collect();
            let mut detail = format!("line {}", string(entry, "line"));
            if let Some(date) = present(entry, "date") {
                detail.push_str(&format!(" · {date}"));
            }
            if !short.is_empty() {
                detail.push_str(&format!(" · {short}"));
            }
            rows.push(PopoverRow::new(string(entry, "author"), detail));
        }
    }

    if !rows.iter().any(|row| !row.header) {
        return None;
    }

    Some(
        PopoverSpec::new(format!("Owners: {path}"), "who to ask about this file")
            .rows(rows)
            .button(PopoverButton::run('f', "deps", format!("/kg deps {path}"))),
    )
}

fn review(data: &Value) -> Option<PopoverSpec> {
    let changes = list(data, "changes");
    if changes.is_empty() {
        return None;
    }

    let mut rows = Vec::new();
    let mut impacted = 0usize;
    for change in changes {
        let file = string(change, "file");
        let change_type = string(change, "change_type");
        rows.push(PopoverRow::section(format!("{file} · {change_type}")));
        for symbol in list(change, "symbols") {
            let name = string(symbol, "name");
            let mut detail = format!("line {}", string(symbol, "line"));
            if let Some(references) = symbol.get("references").and_then(Value::as_i64) {
                detail.push_str(&format!(" · {references} ref(s)"));
            }
            rows.push(
                PopoverRow::new(name.clone(), detail)
                    .tagged(string(symbol, "kind"))
                    .running(format!("/kg explore {name}")),
            );
        }
        if let Some(tests) = change.get("impacted_tests").and_then(Value::as_array) {
            impacted += tests.len();
        }
    }

    if !rows.iter().any(|row| !row.header) {
        return None;
    }

    // The range is part of the answer: the same counts under `--base main` and in
    // the working tree are different claims.
    let range = match (
        present(data, "base"),
        data.get("staged").and_then(Value::as_bool).unwrap_or(false),
    ) {
        (Some(base), _) => format!("since {base}"),
        (None, true) => "staged".to_string(),
        (None, false) => "unstaged".to_string(),
    };

    let mut note = String::new();
    if let Some(summary) = data.get("summary").filter(|value| value.is_object()) {
        let number = |key: &str| summary.get(key).and_then(Value::as_i64).unwrap_or(0);
        note = format!(
            "{} file(s) · {} symbol(s) · {} ref(s)",
            number("files_changed"),
            number("symbols_changed"),
            number("references_affected")
        );
        let delta = number("test_coverage_delta");
        if delta != 0 {
            note.push_str(&format!(" · tests {delta:+}"));
        }
    }
    if impacted > 0 {
        if !note.is_empty() {
            note.push_str(" · ");
        }
        note.push_str(&format!("{impacted} impacted test(s)"));
    }

    let mut spec = PopoverSpec::new(
        format!("Review: {range}"),
        "enter explores a changed symbol",
    )
    .rows(rows);
    if !note.is_empty() {
        spec = spec.note(note);
    }
    Some(spec.button(PopoverButton::run('x', "diff-impact", "/kg diff-impact")))
}

fn deps(data: &Value) -> Option<PopoverSpec> {
    let file = string(data, "file");
    let mut rows = Vec::new();

    for (key, label) in [("imports", "imports"), ("imported_by", "imported by")] {
        let edges = list(data, key);
        if edges.is_empty() {
            continue;
        }
        rows.push(PopoverRow::section(format!("{label} ({})", edges.len())));
        for edge in edges {
            // One heading is this file; the other end is what the row is about.
            let other = if string(edge, "from") == file {
                string(edge, "to")
            } else {
                string(edge, "from")
            };
            let mut detail = string(edge, "kind");
            let line = edge.get("line").and_then(Value::as_i64).unwrap_or(0);
            if line > 0 {
                if detail.is_empty() {
                    detail.push_str(&format!("line {line}"));
                } else {
                    detail.push_str(&format!(" · line {line}"));
                }
            }
            rows.push(PopoverRow::new(other, detail));
        }
    }

    if !rows.iter().any(|row| !row.header) {
        return None;
    }

    // The follow-ups first, then the same file's other questions: a dependency
    // list is read next to the tests and the owners, not instead of them.
    let mut spec = PopoverSpec::new(
        format!("Deps: {file}"),
        "one direction per section, as the imports are written",
    )
    .rows(rows);
    for button in follow_buttons(data) {
        spec = spec.button(button);
    }
    if !file.is_empty() {
        spec = spec
            .button(PopoverButton::run(
                'o',
                "owners",
                format!("/kg owners {file}"),
            ))
            .button(PopoverButton::run('c', "cycles", "/kg cycles".to_string()));
    }
    Some(spec)
}

fn cycles(data: &Value) -> Option<PopoverSpec> {
    let cycles = list(data, "cycles");
    if cycles.is_empty() {
        return None;
    }

    let rows: Vec<PopoverRow> = cycles
        .iter()
        .map(|cycle| {
            let files: Vec<String> = cycle
                .get("files")
                .and_then(Value::as_array)
                .map(|files| {
                    files
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let first = files
                .first()
                .cloned()
                .unwrap_or_else(|| "cycle".to_string());
            let rest: Vec<String> = files.iter().skip(1).cloned().collect();
            let mut row = PopoverRow::new(
                first.clone(),
                if rest.is_empty() {
                    "one file".to_string()
                } else {
                    rest.join(" → ")
                },
            )
            .tagged(format!("{} file(s)", files.len()));
            if !first.is_empty() {
                row = row.running(format!("/kg deps {first}"));
            }
            row
        })
        .collect();

    Some(
        PopoverSpec::new(
            format!("Import cycles ({})", rows.len()),
            "enter reads one file's edges",
        )
        .rows(rows)
        .note("a cycle is a set of files that import each other, not a call chain"),
    )
}

fn query(data: &Value) -> Option<PopoverSpec> {
    let results = list(data, "results");
    if results.is_empty() {
        return None;
    }

    let rows: Vec<PopoverRow> = results
        .iter()
        .map(|result| {
            let symbol = string(result, "symbol");
            let detail = {
                let file = string(result, "file");
                let line = result.get("line").and_then(Value::as_i64).unwrap_or(0);
                if line > 0 {
                    format!("{file}:{line}")
                } else {
                    file
                }
            };
            PopoverRow::new(symbol.clone(), detail)
                .tagged(string(result, "kind"))
                .running(format!("/kg explore {symbol}"))
        })
        .collect();

    let expr = string(data, "expr");
    Some(
        PopoverSpec::new(
            format!("Query: {expr}"),
            "the graph DSL answer, one row per symbol",
        )
        .rows(rows)
        .note(format!("{} result(s)", results.len())),
    )
}

/// The first non-blank line of a snippet, trimmed and capped.
///
/// A window row is one line and a declaration is as long as its author made it,
/// so this is a preview: the verb still prints the whole snippet, and the subtitle
/// says so rather than letting a clipped line read as the source.
fn preview(definition: &Value) -> Option<String> {
    let snippet = definition.get("snippet")?.as_array()?;
    let line = snippet
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    Some(line.chars().take(60).collect())
}

/// `kg def` — one declaration, drawn only when there is a row to act on.
///
/// An unresolved name (or a name with no declaration) is a sentence: the verb
/// prints which name missed and what to do about it, and a window with no rows
/// would be a box that only says "empty".
fn def(data: &Value) -> Option<PopoverSpec> {
    if !data
        .get("resolved")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let symbol = string(data, "symbol");
    let definition = data.get("definition").filter(|value| value.is_object())?;

    let kind = string(definition, "kind");
    let mut detail = vec![location(definition)];
    if !kind.is_empty() {
        detail.push(format!("[{kind}]"));
    }
    if let Some(container) = present(definition, "container") {
        detail.push(format!("in {container}"));
    }
    if let Some(source) = preview(definition) {
        detail.push(source);
    }

    let row = PopoverRow::new(symbol.clone(), detail.join(" · "))
        .running(format!("/kg explore {symbol}"));

    Some(
        PopoverSpec::new(
            format!("Definition: {symbol}"),
            "enter opens the whole picture; the snippet itself is printed by the verb",
        )
        .rows(vec![row])
        .button(PopoverButton::run(
            'r',
            "refs",
            format!("/kg refs {symbol}"),
        ))
        .button(PopoverButton::run(
            'c',
            "callers",
            format!("/kg callers {symbol}"),
        ))
        .button(PopoverButton::run(
            'a',
            "callees",
            format!("/kg callees {symbol}"),
        ))
        .button(PopoverButton::run(
            'i',
            "impact",
            format!("/kg impact {symbol}"),
        )),
    )
}

/// `kg callpath` — the chain, top-down, because a path read out of order is not a
/// path.
fn callpath(data: &Value) -> Option<PopoverSpec> {
    if !data
        .get("resolved")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let nodes = list(data, "path");
    if nodes.is_empty() {
        return None;
    }

    let rows: Vec<PopoverRow> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let symbol = string(node, "symbol");
            let kind = string(node, "kind");
            let file = string(node, "file");
            let line = node.get("line").and_then(Value::as_i64).unwrap_or(0);
            let at = if line > 0 {
                format!("{file}:{line}")
            } else {
                file
            };
            // A hop with no declaration is what the graph saw as a mention — the
            // same word `kg refs` uses, so the two lists cannot disagree.
            let label = if kind.is_empty() {
                "mention".to_string()
            } else {
                kind.clone()
            };
            let row = PopoverRow::new(
                format!("{}. {symbol}", index + 1),
                format!("{at}  [{label}]"),
            )
            .running(format!("/kg explore {symbol}"));
            if kind.is_empty() {
                row
            } else {
                row.tagged(kind)
            }
        })
        .collect();

    let from = string(data, "from");
    let to = string(data, "to");
    let hops = data
        .get("hops")
        .and_then(Value::as_i64)
        .unwrap_or(rows.len().saturating_sub(1) as i64);

    Some(
        PopoverSpec::new(
            format!("Call path: {from} → {to}"),
            "read top-down: each row is one hop, and enter explores that hop",
        )
        .rows(rows)
        .note(format!("{hops} hop(s)")),
    )
}

/// `kg outline` — symbols grouped by the container that holds them.
fn outline(data: &Value) -> Option<PopoverSpec> {
    let symbols = list(data, "symbols");
    if symbols.is_empty() {
        return None;
    }
    let file = string(data, "file");
    let language = string(data, "language");

    let mut rows = Vec::new();
    // `None` is a marker, so the first top-level symbol always opens its section.
    let mut current: Option<Option<String>> = None;
    for symbol in symbols {
        let container = present(symbol, "container");
        if current.as_ref() != Some(&container) {
            let label = container.clone().unwrap_or_else(|| {
                if file.is_empty() {
                    "top level".to_string()
                } else {
                    format!("{file} (top level)")
                }
            });
            rows.push(PopoverRow::section(label));
            current = Some(container);
        }
        push_outline(&mut rows, symbol, 0);
    }

    let mut note = format!("{} symbol(s)", count_outline(symbols));
    if !language.is_empty() {
        note.push_str(&format!(" · {language}"));
    }
    let subtitle = if file.is_empty() {
        "one file, as the graph declares it".to_string()
    } else {
        format!("outline of {file}")
    };

    let mut spec = PopoverSpec::new("Outline", subtitle).rows(rows).note(note);
    // A button is only drawn when there is a line behind it: `/kg deps ` with no
    // file is exactly the kind of promise the slash parser refuses.
    if !file.is_empty() {
        spec = spec.button(PopoverButton::run('d', "deps", format!("/kg deps {file}")));
    }
    Some(spec)
}

/// One outline symbol and its children, indented by nesting depth.
fn push_outline(rows: &mut Vec<PopoverRow>, symbol: &Value, depth: usize) {
    let name = string(symbol, "name");
    let kind = string(symbol, "kind");
    let line = symbol.get("line").and_then(Value::as_i64).unwrap_or(0);
    let indent = "  ".repeat(depth);

    let mut row = PopoverRow::new(format!("{indent}{name}"), format!("line {line}"))
        .running(format!("/kg explore {name}"));
    if !kind.is_empty() {
        row = row.tagged(kind);
    }
    rows.push(row);

    for child in list(symbol, "children") {
        push_outline(rows, child, depth + 1);
    }
}

/// Every symbol in an outline, children included — the count the window claims.
fn count_outline(symbols: &[Value]) -> usize {
    symbols
        .iter()
        .map(|symbol| 1 + count_outline(list(symbol, "children")))
        .sum()
}

/// `kg hotspots` — ranked files. The score is churn times size, not control-flow
/// complexity, and the subtitle says so because "complexity" invites the other
/// reading.
fn hotspots(data: &Value) -> Option<PopoverSpec> {
    let items = list(data, "hotspots");
    if items.is_empty() {
        return None;
    }

    let rows: Vec<PopoverRow> = items
        .iter()
        .map(|hotspot| {
            let file = string(hotspot, "file");
            let churn = hotspot.get("churn").and_then(Value::as_i64).unwrap_or(0);
            let complexity = hotspot
                .get("complexity")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let score = hotspot.get("score").and_then(Value::as_f64).unwrap_or(0.0);
            // `symbols` is a name list; only its length is useful on one row, and
            // the outline the row opens is where the names are worth reading.
            let symbols = hotspot
                .get("symbols")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);

            let mut detail =
                format!("score {score:.2} · churn {churn} · {complexity} declaration(s)");
            if symbols > 0 {
                detail.push_str(&format!(" · {symbols} symbol(s)"));
            }
            PopoverRow::new(file.clone(), detail).running(format!("/kg outline {file}"))
        })
        .collect();

    let total = data
        .get("total")
        .and_then(Value::as_i64)
        .unwrap_or(rows.len() as i64);

    Some(
        PopoverSpec::new(
            format!("Hotspots ({})", rows.len()),
            "sqrt(commits × declarations): churn and size, not branch count",
        )
        .rows(rows)
        .note(format!("{total} file(s) ranked")),
    )
}

/// `kg unused-exports` — the tag is the visibility that made it look reachable,
/// which is the reason it is worth a look.
fn unused_exports(data: &Value) -> Option<PopoverSpec> {
    let exports = list(data, "exports");
    if exports.is_empty() {
        return None;
    }

    let rows: Vec<PopoverRow> = exports
        .iter()
        .map(|export| {
            let name = string(export, "name");
            let kind = string(export, "kind");
            let file = string(export, "file");
            let line = export.get("line").and_then(Value::as_i64).unwrap_or(0);
            let visibility = string(export, "visibility");

            let mut detail = if kind.is_empty() {
                format!("{file}:{line}")
            } else {
                format!("{file}:{line} · [{kind}]")
            };
            if !visibility.is_empty() {
                detail.push_str(&format!(" · {visibility}"));
            }
            let row = PopoverRow::new(name.clone(), detail).running(format!("/kg refs {name}"));
            if visibility.is_empty() {
                row
            } else {
                row.tagged(visibility)
            }
        })
        .collect();

    let total = data
        .get("total")
        .and_then(Value::as_i64)
        .unwrap_or(rows.len() as i64);

    Some(
        PopoverSpec::new(
            format!("Unused exports ({})", rows.len()),
            "declared public, reached by nothing outside its own declaration",
        )
        .rows(rows)
        .note(format!(
            "{total} export(s) · enter runs `kg refs` and shows what the count excluded"
        )),
    )
}

/// `kg diff-impact` — each changed symbol with the number of things pointing at
/// it. The reference rows themselves stay in the printed report: a window that
/// interleaved two kinds of row would be read as one list.
fn diff_impact(data: &Value) -> Option<PopoverSpec> {
    let changes = list(data, "changes");
    if changes.is_empty() {
        return None;
    }

    let rows: Vec<PopoverRow> = changes
        .iter()
        .map(|change| {
            let symbol = string(change, "symbol");
            let file = string(change, "path");
            let line = change.get("line").and_then(Value::as_i64).unwrap_or(0);
            let references = change
                .get("references")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            let change_type = string(change, "change_type");

            let row = PopoverRow::new(
                symbol.clone(),
                format!("{file}:{line} · {references} reference(s)"),
            )
            .running(format!("/kg impact {symbol}"));
            if change_type.is_empty() {
                row
            } else {
                row.tagged(change_type)
            }
        })
        .collect();

    let staged = data.get("staged").and_then(Value::as_bool).unwrap_or(false);
    let label = if staged { "staged" } else { "unstaged" };
    let total = data
        .get("total_references")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    Some(
        PopoverSpec::new(
            format!("Diff impact: {label} ({})", rows.len()),
            "each changed symbol, and how much points at it",
        )
        .rows(rows)
        .note(format!(
            "{total} reference(s) behind {} symbol(s)",
            changes.len()
        )),
    )
}

/// The per-language breakdown, shared by `kg index` and an indexed `kg status`.
fn languages(command: &str, data: &Value) -> Option<PopoverSpec> {
    let status = command == "kg.status";
    if status
        && !data
            .get("indexed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        // Not indexed is a sentence. The verb prints the one line that says so.
        return None;
    }

    let breakdown = list(data, "languages");
    if breakdown.is_empty() {
        return None;
    }

    let rows: Vec<PopoverRow> = breakdown
        .iter()
        .map(|entry| {
            let files = entry.get("files").and_then(Value::as_i64).unwrap_or(0);
            let symbols = entry.get("symbols").and_then(Value::as_i64).unwrap_or(0);
            PopoverRow::new(
                string(entry, "language"),
                format!("{files} file(s) · {symbols} symbol(s)"),
            )
        })
        .collect();

    let mut note = format!(
        "{} file(s) · {} symbol(s) · {} ref(s)",
        data.get("files").and_then(Value::as_i64).unwrap_or(0),
        data.get("symbols").and_then(Value::as_i64).unwrap_or(0),
        data.get("references").and_then(Value::as_i64).unwrap_or(0)
    );
    if status {
        let stale = data.get("stale_files").and_then(Value::as_i64).unwrap_or(0);
        if stale > 0 {
            note.push_str(&format!(" · {stale} file(s) changed since the index"));
        }
    }

    let where_ = present(data, "path").or_else(|| present(data, "root"));
    let subtitle = match where_ {
        Some(path) if status => format!("graph at {path}"),
        Some(path) => format!("indexed {path}"),
        None => "as the graph reports it".to_string(),
    };

    let mut spec = PopoverSpec::new(
        format!(
            "{} ({})",
            if status {
                "Languages"
            } else {
                "Indexed languages"
            },
            rows.len()
        ),
        subtitle,
    )
    .rows(rows)
    .note(note);
    spec = if status {
        spec.button(PopoverButton::run('x', "index", "/kg index"))
    } else {
        spec.button(PopoverButton::run('s', "status", "/kg status"))
    };
    Some(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::overlays::popover::PopoverOverlay;

    fn hit(path: &str, line: i64, call_shaped: bool) -> Value {
        serde_json::json!({
            "path": path, "line": line, "kind": if call_shaped { "call" } else { "reference" },
            "context": "target(1)", "call_shaped": call_shaped
        })
    }

    #[test]
    fn an_explore_is_a_definition_and_three_labelled_lists() {
        let data = serde_json::json!({
            "symbol": "Globe",
            "resolved": true,
            "definition": { "path": "src/globe.rs", "line": 12, "kind": "struct",
                            "snippet": [], "container": "ui" },
            "references": [hit("src/page.rs", 40, false)],
            "callers": [hit("src/app.rs", 7, true)],
            "callees": [],
            "truncated": false
        });
        let spec = PopoverOverlay::for_answer("kg.explore", &data).expect("a window");

        assert_eq!(spec.title, "Explore: Globe");
        // Definition, then only the non-empty lists.
        let headers: Vec<&str> = spec
            .rows
            .iter()
            .filter(|row| row.header)
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(headers, vec!["definition", "callers (1)", "references (1)"]);

        let tags: Vec<&str> = spec
            .rows
            .iter()
            .filter_map(|row| row.tag.as_deref())
            .collect();
        assert_eq!(tags, vec!["call", "mention"]);
        assert!(
            spec.rows[1].title.contains("ui::"),
            "the definition is qualified by its container: {:?}",
            spec.rows[1]
        );
    }

    #[test]
    fn an_unresolved_symbol_is_a_sentence_not_an_empty_window() {
        let data = serde_json::json!({
            "symbol": "nope", "resolved": false, "definition": null,
            "references": [], "callers": [], "callees": [], "truncated": false
        });
        assert!(PopoverOverlay::for_answer("kg.explore", &data).is_none());
    }

    #[test]
    fn a_tree_row_explores_the_symbol_it_names() {
        let data = serde_json::json!({
            "dir": "src/kg",
            "entries": [{
                "path": "src/kg/mod.rs", "language": "rust",
                "symbols": [
                    { "name": "impact", "kind": "fn", "line": 940, "container": null },
                    { "name": "ImpactReport", "kind": "struct", "line": 180,
                      "container": Some("kg".to_string()) }
                ]
            }]
        });
        let spec = PopoverOverlay::for_answer("kg.tree", &data).expect("a window");

        assert_eq!(spec.title, "Tree: src/kg");
        assert!(spec.rows[0].header);
        assert_eq!(
            spec.rows[1].action,
            Some(crate::tui::overlays::popover::PopoverAction::Run(
                "/kg explore impact".into()
            ))
        );
        assert_eq!(spec.note.as_deref(), Some("2 symbols in 1 files"));
    }

    #[test]
    fn a_review_names_its_range_and_counts_the_changed_symbols() {
        let data = serde_json::json!({
            "staged": false,
            "base": "main",
            "changes": [{
                "file": "src/kg/mod.rs", "change_type": "modified",
                "symbols": [{ "name": "impact", "kind": "fn", "line": 940, "references": 7 }],
                "impacted_tests": ["tests/it.rs"]
            }],
            "summary": { "files_changed": 1, "symbols_changed": 1,
                         "references_affected": 7, "test_coverage_delta": -1 }
        });
        let spec = PopoverOverlay::for_answer("kg.review", &data).expect("a window");

        // The range is in the title because the counts only mean something next
        // to the question that produced them.
        assert_eq!(spec.title, "Review: since main");
        let note = spec.note.as_deref().unwrap();
        assert!(
            note.contains("1 file(s) · 1 symbol(s) · 7 ref(s)"),
            "{note}"
        );
        assert!(note.contains("tests -1"), "{note}");
        assert!(note.contains("1 impacted test(s)"), "{note}");
    }

    #[test]
    fn tests_for_says_which_coverage_is_inline() {
        let data = serde_json::json!({
            "symbol": "covered", "resolved": true, "total_coverage": 2,
            "tests": [
                { "path": "tests/it.rs", "line": 1, "test_name": "it_works",
                  "coverage_type": "direct" },
                { "path": "src/lib.rs", "line": 10, "test_name": "it_covers",
                  "coverage_type": "inline" }
            ]
        });
        let spec = PopoverOverlay::for_answer("kg.tests_for", &data).expect("a window");

        let tags: Vec<&str> = spec
            .rows
            .iter()
            .filter_map(|row| row.tag.as_deref())
            .collect();
        assert_eq!(tags, vec!["direct", "inline"]);
        assert_eq!(spec.note.as_deref(), Some("2 covering test(s) · 1 inline"));
    }

    #[test]
    fn status_without_an_index_stays_text_and_an_indexed_one_lists_languages() {
        let not_indexed = serde_json::json!({ "indexed": false, "languages": [] });
        assert!(PopoverOverlay::for_answer("kg.status", &not_indexed).is_none());

        let indexed = serde_json::json!({
            "indexed": true, "root": "/repo", "path": "/repo/.memnoth/graph.db",
            "files": 3, "symbols": 9, "references": 12, "built_at_ms": 0,
            "stale_files": 2,
            "languages": [{ "language": "rust", "files": 3, "symbols": 9 }]
        });
        let spec = PopoverOverlay::for_answer("kg.status", &indexed).expect("a window");
        assert_eq!(spec.rows[0].title, "rust");
        assert!(spec.note.as_deref().unwrap().contains("2 file(s) changed"));
    }

    #[test]
    fn an_id_this_module_does_not_own_is_not_claimed() {
        // `kg.refs` has a window in `popover.rs`; claiming it here would put two
        // renderers behind one command.
        assert!(spec("kg.refs", &serde_json::json!({ "references": [] })).is_none());
        // A `def` with no declaration has no row to act on, so it stays prose.
        assert!(
            spec(
                "kg.def",
                &serde_json::json!({ "symbol": "x", "resolved": false, "definition": null })
            )
            .is_none()
        );
    }

    #[test]
    fn a_def_is_one_row_with_the_questions_that_follow_it() {
        let data = serde_json::json!({
            "symbol": "open_store",
            "resolved": true,
            "definition": {
                "path": "src/kg/store.rs", "line": 42, "kind": "fn",
                "snippet": ["", "pub fn open_store(path: &Path) -> Result<Store> {"],
                "container": "store"
            }
        });
        let spec = PopoverOverlay::for_answer("kg.def", &data).expect("a window");

        assert_eq!(spec.title, "Definition: open_store");
        assert_eq!(spec.rows.len(), 1);
        let detail = &spec.rows[0].detail;
        assert!(detail.contains("src/kg/store.rs:42"), "{detail}");
        assert!(detail.contains("[fn]"), "{detail}");
        assert!(detail.contains("in store"), "{detail}");
        // The blank first snippet line is skipped: a preview of nothing is not a
        // preview.
        assert!(detail.contains("pub fn open_store"), "{detail}");
        assert_eq!(
            spec.rows[0].action,
            Some(crate::tui::overlays::popover::PopoverAction::Run(
                "/kg explore open_store".into()
            ))
        );
        // The box is only worth opening because the footer asks the next four
        // questions about the same name.
        for key in ['r', 'c', 'a', 'i'] {
            assert!(
                spec.buttons.iter().any(|button| button.key == key),
                "missing button {key}"
            );
        }
    }

    #[test]
    fn a_callpath_is_numbered_top_down_and_names_a_declarationless_hop_as_a_mention() {
        let data = serde_json::json!({
            "from": "handle", "to": "write_row", "resolved": true, "hops": 2,
            "path": [
                { "symbol": "handle", "kind": "fn", "file": "src/api.rs", "line": 10 },
                { "symbol": "store", "kind": "", "file": "src/store.rs", "line": 0 },
                { "symbol": "write_row", "kind": "fn", "file": "src/store.rs", "line": 88 }
            ]
        });
        let spec = PopoverOverlay::for_answer("kg.callpath", &data).expect("a window");

        assert_eq!(spec.title, "Call path: handle → write_row");
        assert_eq!(spec.rows[0].title, "1. handle");
        assert_eq!(spec.rows[1].title, "2. store");
        assert!(
            spec.rows[1].detail.contains("[mention]"),
            "a hop with no kind is the word `kg refs` uses: {:?}",
            spec.rows[1]
        );
        // A declarationless hop is not tagged: the tag is a kind, and there is
        // none to print.
        assert_eq!(spec.rows[1].tag, None);
        assert_eq!(spec.rows[2].tag.as_deref(), Some("fn"));
        assert_eq!(
            spec.rows[2].action,
            Some(crate::tui::overlays::popover::PopoverAction::Run(
                "/kg explore write_row".into()
            ))
        );
        assert_eq!(spec.note.as_deref(), Some("2 hop(s)"));
    }

    #[test]
    fn an_outline_groups_by_container_and_indents_children() {
        let data = serde_json::json!({
            "file": "src/kg/mod.rs", "language": "rust",
            "symbols": [
                { "name": "Store", "kind": "struct", "line": 12, "end_line": 40,
                  "container": null, "children": [] },
                { "name": "open", "kind": "fn", "line": 60, "end_line": 80,
                  "container": "impl Store", "children": [
                    { "name": "retry", "kind": "fn", "line": 66, "end_line": 70,
                      "container": "impl Store", "children": [] }
                ] }
            ]
        });
        let spec = PopoverOverlay::for_answer("kg.outline", &data).expect("a window");

        assert_eq!(spec.title, "Outline");
        let headers: Vec<&str> = spec
            .rows
            .iter()
            .filter(|row| row.header)
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(headers, vec!["src/kg/mod.rs (top level)", "impl Store"]);
        assert_eq!(spec.rows[1].title, "Store");
        assert_eq!(spec.rows[3].title, "open");
        assert_eq!(spec.rows[4].title, "  retry", "a child is indented");
        assert_eq!(
            spec.rows[3].action,
            Some(crate::tui::overlays::popover::PopoverAction::Run(
                "/kg explore open".into()
            ))
        );
        // Children are counted: the window says three symbols, not two.
        assert_eq!(spec.note.as_deref(), Some("3 symbol(s) · rust"));
    }

    #[test]
    fn a_hotspot_row_opens_its_file_outline_and_shows_the_score_it_was_ranked_by() {
        let data = serde_json::json!({
            "total": 2,
            "hotspots": [
                { "file": "src/kg/mod.rs", "churn": 9, "complexity": 41, "score": 12.5,
                  "symbols": ["impact", "review"] },
                { "file": "src/tui/app.rs", "churn": 4, "complexity": 12, "score": 6.25,
                  "symbols": [] }
            ]
        });
        let spec = PopoverOverlay::for_answer("kg.hotspots", &data).expect("a window");

        assert_eq!(spec.title, "Hotspots (2)");
        let detail = &spec.rows[0].detail;
        assert!(detail.contains("score 12.50"), "{detail}");
        assert!(detail.contains("churn 9"), "{detail}");
        assert!(detail.contains("2 symbol(s)"), "{detail}");
        assert_eq!(
            spec.rows[0].action,
            Some(crate::tui::overlays::popover::PopoverAction::Run(
                "/kg outline src/kg/mod.rs".into()
            ))
        );
        assert_eq!(spec.note.as_deref(), Some("2 file(s) ranked"));
    }

    #[test]
    fn an_unused_export_is_tagged_by_the_visibility_that_made_it_look_reachable() {
        let data = serde_json::json!({
            "total": 1,
            "exports": [
                { "name": "helper", "kind": "fn", "file": "src/lib.rs", "line": 12,
                  "visibility": "pub" }
            ]
        });
        let spec = PopoverOverlay::for_answer("kg.unused_exports", &data).expect("a window");

        assert_eq!(spec.title, "Unused exports (1)");
        assert_eq!(spec.rows[0].tag.as_deref(), Some("pub"));
        assert!(
            spec.rows[0].detail.contains("src/lib.rs:12"),
            "{:?}",
            spec.rows[0]
        );
        assert_eq!(
            spec.rows[0].action,
            Some(crate::tui::overlays::popover::PopoverAction::Run(
                "/kg refs helper".into()
            ))
        );
        assert!(spec.note.as_deref().unwrap().contains("1 export(s)"));
    }

    #[test]
    fn a_diff_impact_counts_the_references_instead_of_listing_them() {
        let data = serde_json::json!({
            "staged": true,
            "total_references": 7,
            "changes": [{
                "symbol": "impact", "change_type": "modified",
                "path": "src/kg/mod.rs", "line": 940,
                "references": [
                    hit("src/a.rs", 1, true), hit("src/b.rs", 2, true),
                    hit("src/c.rs", 3, true), hit("src/d.rs", 4, true),
                    hit("src/e.rs", 5, true), hit("src/f.rs", 6, true),
                    hit("src/g.rs", 7, true)
                ]
            }]
        });
        let spec = PopoverOverlay::for_answer("kg.diff_impact", &data).expect("a window");

        assert_eq!(spec.title, "Diff impact: staged (1)");
        assert_eq!(spec.rows.len(), 1, "one row per changed symbol");
        assert_eq!(spec.rows[0].tag.as_deref(), Some("modified"));
        assert!(
            spec.rows[0].detail.contains("7 reference(s)"),
            "{:?}",
            spec.rows[0]
        );
        assert_eq!(
            spec.rows[0].action,
            Some(crate::tui::overlays::popover::PopoverAction::Run(
                "/kg impact impact".into()
            ))
        );
        assert_eq!(
            spec.note.as_deref(),
            Some("7 reference(s) behind 1 symbol(s)")
        );
    }

    #[test]
    fn every_new_spec_leaves_an_empty_answer_as_a_sentence() {
        for (command, payload) in [
            (
                "kg.callpath",
                serde_json::json!({ "resolved": false, "path": [] }),
            ),
            (
                "kg.outline",
                serde_json::json!({ "file": "a.rs", "symbols": [] }),
            ),
            (
                "kg.hotspots",
                serde_json::json!({ "hotspots": [], "total": 0 }),
            ),
            (
                "kg.unused_exports",
                serde_json::json!({ "exports": [], "total": 0 }),
            ),
            (
                "kg.diff_impact",
                serde_json::json!({ "staged": false, "changes": [], "total_references": 0 }),
            ),
        ] {
            assert!(
                PopoverOverlay::for_answer(command, &payload).is_none(),
                "{command} opened a window with no rows"
            );
        }
    }
}
