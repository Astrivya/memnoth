//! `/impact <symbol> [--depth N] [--diff]` — the blast radius of a change.
//!
//! Read-only by construction: it asks the code graph for reverse reachability
//! and the test index for coverage, and renders what those two stores know. It
//! never edits, never writes the vault, and never opens a socket.
//!
//! Every answer carries a `precision` label. The graph is lexical today, so the
//! label is `lexical`; when a tree-sitter or LSP backend answers, the label
//! changes and the shape does not. Both an exact and a likely count are
//! reported separately, because "12 callers" reads as 12 certainties when four
//! of them are name matches in comments.
//!
//! `impacted_tests` is either real data or `null` with a reason. A test list
//! that silently degrades to an empty array is indistinguishable from "no test
//! covers this", which is the answer that gets someone to merge.

use serde_json::{Value, json};

use crate::cli::ImpactArgs;
use crate::context::AppContext;
use crate::kg;
use crate::output::{ErrorOutcome, Outcome, ellipsize};

/// The precision the lexical scanner can honestly claim.
const PRECISION: &str = "lexical";

/// `memnoth impact` / `/impact`.
pub fn run(ctx: &AppContext, args: &ImpactArgs) -> Result<Outcome, ErrorOutcome> {
    let symbol = args.symbol.trim();
    if symbol.is_empty() && !args.diff {
        return Err(ErrorOutcome::usage("the symbol is empty")
            .with_fix("usage: /impact <symbol> [--depth N] [--diff]"));
    }
    let depth = args.depth.clamp(1, 8) as u32;

    // ── The blast radius ────────────────────────────────────────────────
    let (subject, impact) = if args.diff {
        let report = kg::diff_impact(ctx, false).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
        (
            "the working tree diff".to_string(),
            serde_json::to_value(&report).unwrap_or(Value::Null),
        )
    } else {
        let report = kg::impact(ctx, symbol, depth).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
        (
            symbol.to_string(),
            serde_json::to_value(&report).unwrap_or(Value::Null),
        )
    };

    // ── Affected tests: real, or null with a reason ─────────────────────
    let (impacted_tests, tests_reason) = if args.diff {
        (
            Value::Null,
            Some("tests-for resolves one symbol, and --diff names a change set".to_string()),
        )
    } else {
        match kg::tests_for(ctx, symbol) {
            Ok(report) => (serde_json::to_value(&report).unwrap_or(Value::Null), None),
            Err(error) => (
                Value::Null,
                Some(format!("the test index did not answer: {error:#}")),
            ),
        }
    };

    // ── Counts, only where the backend actually said so ─────────────────
    let exact = count_first(&impact, &["exact", "exact_count"]);
    let likely = count_first(&impact, &["likely", "likely_count"]);
    let rows = first_rows(&impact);

    let mut text = format!("impact {subject}\n");
    text.push_str(&format!("  precision: {PRECISION} · depth {depth}\n"));
    match (exact, likely) {
        (Some(exact), Some(likely)) => {
            text.push_str(&format!("  {exact} exact, {likely} likely\n"));
        }
        (Some(exact), None) => text.push_str(&format!("  {exact} exact\n")),
        (None, Some(likely)) => text.push_str(&format!("  {likely} likely\n")),
        (None, None) => text.push_str(&format!("  {} affected symbol(s)\n", rows.len())),
    }

    if rows.is_empty() {
        text.push_str("  no caller refers to it\n");
    } else {
        text.push('\n');
        for row in rows.iter().take(40) {
            let path = row.get("path").and_then(Value::as_str).unwrap_or("?");
            let line = row.get("line").and_then(Value::as_i64);
            let name = row
                .get("symbol")
                .or_else(|| row.get("name"))
                .or_else(|| row.get("function"))
                .and_then(Value::as_str);
            let context = row
                .get("context")
                .or_else(|| row.get("snippet"))
                .and_then(Value::as_str);
            let location = match line {
                Some(line) => format!("{path}:{line}"),
                None => path.to_string(),
            };
            let label = name.unwrap_or_default();
            text.push_str(&format!(
                "  {location}  {}{}\n",
                ellipsize(label, 48),
                match context {
                    Some(context) if !context.is_empty() => format!("  {}", ellipsize(context, 64)),
                    _ => String::new(),
                }
            ));
        }
        if rows.len() > 40 {
            text.push_str(&format!(
                "  … {} more (raise --depth or narrow the symbol)\n",
                rows.len() - 40
            ));
        }
    }

    text.push_str("\ntests\n");
    match &tests_reason {
        Some(reason) => text.push_str(&format!("  unknown: {reason}\n")),
        None => {
            let tests = first_rows(&impacted_tests);
            if tests.is_empty() {
                text.push_str("  no test names this symbol\n");
            } else {
                for test in tests.iter().take(20) {
                    let path = test
                        .get("path")
                        .or_else(|| test.get("file"))
                        .and_then(Value::as_str)
                        .unwrap_or("?");
                    let name = test.get("name").and_then(Value::as_str).unwrap_or("");
                    text.push_str(&format!("  {path}  {name}\n"));
                }
            }
        }
    }

    text.push_str(&format!(
        "\nnext:\n  memnoth kg impact {symbol} --depth {depth}\n  memnoth kg tests-for {symbol}\n"
    ));

    Ok(Outcome::new(
        "impact",
        text.trim_end(),
        json!({
            "subject": subject,
            "precision": PRECISION,
            "depth": depth,
            "exact": exact,
            "likely": likely,
            "impact": impact,
            "impacted_tests": impacted_tests,
            "impacted_tests_reason": tests_reason,
        }),
    ))
}

/// The first numeric field among `keys`, so a renamed count is absent rather
/// than invented.
fn count_first(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
}

/// The first top-level array of objects: the rows the backend returned.
///
/// The graph's reports name that array differently per command
/// (`callers`, `references`, `changes`), so the shape is discovered rather
/// than assumed — an empty answer must mean "no rows", not "a renamed key".
fn first_rows(value: &Value) -> Vec<&Value> {
    let Some(object) = value.as_object() else {
        return value
            .as_array()
            .map(|rows| rows.iter().collect())
            .unwrap_or_default();
    };
    object
        .values()
        .filter_map(Value::as_array)
        .find(|rows| rows.iter().any(|row| row.is_object()))
        .map(|rows| rows.iter().collect())
        .unwrap_or_default()
}
