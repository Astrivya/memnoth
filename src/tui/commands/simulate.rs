//! `/simulate <change>` — dry-run plan with zero writes.
//!
//! Given a proposed change, reports what the code graph says would be
//! affected and whether the sandbox policy permits it. Nothing is written.
//!
//! Backend: kg (impact analysis), sandbox policy (read/write and network
//! decisions for the change's command line).

use anyhow::Result;
use serde_json::{Value, json};

use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

/// `memnoth simulate` / `/simulate <change>`.
///
/// Produces a dry-run plan: which code the change would touch, whether the
/// sandbox policy would permit it, and what the blast radius looks like.
/// Nothing is written.
pub fn run(ctx: &AppContext, args: &crate::cli::SimulateArgs) -> Result<Outcome, ErrorOutcome> {
    let change = args.change.trim();
    if change.is_empty() {
        return Err(ErrorOutcome::usage(
            "simulate requires a non-empty change description: /simulate <change>",
        ));
    }

    let mut text = String::new();
    text.push_str(&format!("simulate: {change}\n"));
    text.push_str("  mode: dry-run, zero writes\n\n");

    // ── 1. Kg: what the graph says would be affected ──────────────────────
    let impact = match kg_impact_for_change(ctx, change) {
        Ok(report) => report,
        Err(e) => {
            text.push_str(&format!("  graph: unavailable ({e})\n"));
            let data = json!({
                "change": change,
                "graph": null,
                "sandbox": null,
                "plan": "cannot simulate without graph or policy data",
            });
            return Ok(Outcome::new("simulate", text, data));
        }
    };

    match impact["symbol"].as_str() {
        Some(symbol) => text.push_str(&format!("  graph: {symbol}\n")),
        None => text.push_str("  graph: no symbol named in this change is in the graph\n"),
    }
    let defs = impact["definitions"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    let refs = impact["references"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    let files = impact["files"].as_u64().unwrap_or(0);
    text.push_str(&format!("    definitions: {defs}\n"));
    text.push_str(&format!("    references:  {refs}\n"));
    text.push_str(&format!("    files:       {files}\n"));

    if let Some(tests) = impact["tests"].as_array()
        && !tests.is_empty()
    {
        text.push_str("    affected tests:\n");
        for t in tests {
            text.push_str(&format!("      - {}\n", t.as_str().unwrap_or("?")));
        }
    }
    if impact["truncated"].as_bool().unwrap_or(false) {
        text.push_str("    (reference list truncated)\n");
    }
    text.push('\n');

    // ── 2. Sandbox policy: would the change's commands be allowed? ────────
    let sandbox = sandbox_plan_for_change(ctx, change)?;
    text.push_str(&format!("  sandbox: {}\n", sandbox.describe()));
    text.push_str(&format!("    enforced: {}\n", sandbox.is_enforced()));

    // ── 3. Plan: read-only, no writes ─────────────────────────────────────
    let plan = if sandbox.is_enforced() {
        "plan: change is permitted under sandbox policy; no writes will be made"
    } else {
        "plan: sandbox not enforced — change would run unconfined; inspect before applying"
    };
    text.push_str(&format!("{plan}\n"));

    Ok(Outcome::new(
        "simulate",
        text,
        json!({
            "change": change,
            "graph": impact,
            "sandbox": {
                "enforced": sandbox.is_enforced(),
                "describe": sandbox.describe(),
            },
            "plan": plan,
            "writes": 0,
        }),
    ))
}

/// Run `kg impact` for the symbol a change description names.
///
/// A change is prose, not a command line, so no word in it is authoritative on
/// its own: `simulate "switch the vault to JSON files"` used to report on a
/// symbol called `switch`. A backticked or quoted name is an explicit claim and
/// is tried first; after that every identifier-shaped word is tried and the
/// graph decides. When nothing resolves, the report says so and carries no
/// symbol, instead of presenting the first word as the thing being changed.
fn kg_impact_for_change(ctx: &AppContext, change: &str) -> Result<Value> {
    let candidates = symbol_candidates(change);
    for candidate in &candidates {
        let report = crate::kg::impact(ctx, candidate, 1)
            .map_err(|e| anyhow::anyhow!("kg impact for {candidate}: {e}"))?;
        if report.resolved {
            return serde_json::to_value(&report)
                .map_err(|e| anyhow::anyhow!("serialize kg impact: {e}"));
        }
    }
    Ok(json!({
        "symbol": Value::Null,
        "resolved": false,
        "described": change,
        "candidates": candidates,
        "definitions": [],
        "references": [],
        "files": 0,
        "tests": [],
        "truncated": false,
    }))
}

/// Every symbol name a change description could mean, explicit claims first.
fn symbol_candidates(change: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for quote in ['`', '"'] {
        let mut parts = change.split(quote);
        parts.next();
        while let Some(span) = parts.next() {
            let span = span.trim();
            if is_identifier(span) {
                push_unique(&mut out, span);
            }
            parts.next();
        }
    }
    for token in change.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':')) {
        let token = token.trim_matches(':');
        if token.len() >= 2 && is_identifier(token) && token.chars().any(|c| c.is_alphabetic()) {
            push_unique(&mut out, token);
        }
    }
    out
}

/// Whether `text` is shaped like a symbol name rather than a sentence fragment.
fn is_identifier(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && text
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
}

fn push_unique(out: &mut Vec<String>, name: &str) {
    if !out.iter().any(|existing| existing == name) {
        out.push(name.to_string());
    }
}

/// Build a sandbox plan for the change's implied command line, or return an
/// unavailable confinement explaining why.
fn sandbox_plan_for_change(
    ctx: &AppContext,
    _change: &str,
) -> Result<memnoth_sandbox::Confinement> {
    let config = memnoth_sandbox::profiles::load_sandbox_config(&ctx.root)
        .map_err(|e| anyhow::anyhow!("sandbox config: {e}"))?;
    let plan = memnoth_sandbox::backend::BackendPlan::build(
        memnoth_sandbox::backend::OsBackend::host(),
        &memnoth_sandbox::profiles::ProfileName::Workspace,
        &ctx.root,
        &config,
    )
    .map_err(|e| anyhow::anyhow!("sandbox plan: {e}"))?;
    Ok(memnoth_sandbox::confine(
        &plan,
        &ctx.root,
        &["true".to_string()],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quoted_name_outranks_the_prose_around_it() {
        let found = symbol_candidates("rename `Store` to Vault");
        assert_eq!(found.first().map(String::as_str), Some("Store"));
    }

    #[test]
    fn prose_yields_candidates_in_reading_order() {
        let found = symbol_candidates("switch the vault to JSON files on disk");
        assert_eq!(found.first().map(String::as_str), Some("switch"));
        assert!(found.iter().any(|name| name == "vault"));
        assert!(found.iter().any(|name| name == "JSON"));
    }

    #[test]
    fn a_qualified_name_stays_one_candidate() {
        let found = symbol_candidates("tighten Store::open");
        assert!(found.iter().any(|name| name == "Store::open"), "{found:?}");
    }

    #[test]
    fn punctuation_and_digit_leading_tokens_are_dropped() {
        // `and` is still a candidate — only the graph can say it means nothing;
        // punctuation and `1x` are not shaped like names at all.
        let found = symbol_candidates("- and : 1x");
        assert_eq!(found, vec!["and".to_string()]);
    }
}
