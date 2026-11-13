//! `/why <symbol> [--at WHEN]` — belief, code, and supersession chain for one symbol.
//!
//! Combines three backends into a single answer:
//!
//! * **belief** — what the vault believed about the symbol at an instant
//!   (`vault asof --at WHEN --name <symbol>`),
//! * **code** — the symbol's declaration, snippet, references, callers and callees
//!   (`kg explore <symbol>`),
//! * **supersession chain** — every version of the belief and the beliefs
//!   explicitly revoked or superseded (`vault history <symbol>`,
//!   `vault contradictions --name <symbol>`).
//!
//! Dates and sources are carried on every row so a reader can trace each claim
//! back to the observation that made it.

use anyhow::Context;
use serde_json::{Value, json};

use crate::context::AppContext;
use crate::kg;
use crate::output::{ErrorOutcome, ExitCode, Outcome, parse_when, rfc3339};
use crate::vault;

/// Why one symbol is believed, what code declares it, and how the belief has
/// been superseded.
pub fn run(ctx: &AppContext, symbol: &str, at: Option<&str>) -> Result<Outcome, ErrorOutcome> {
    let symbol = symbol.trim();
    if symbol.is_empty() {
        return Err(ErrorOutcome::usage("the symbol is empty")
            .with_cause("no symbol provided")
            .with_fix("usage: /why <symbol> [--at WHEN]"));
    }

    // Instant to reconstruct the belief at. Defaults to now.
    let instant = match at {
        Some(when) => parse_when(when).map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, format!("invalid --at value: {e}"))
                .with_cause("unrecognized date format")
                .with_fix("use RFC 3339 (2026-09-01T00:00:00Z), a date (2026-09-01), or epoch milliseconds")
                .with_docs_url(crate::context::docs_url("vault/asof"))
        })?,
        None => memnoth_engine::now_ms(),
    };

    // ── Code: the symbol in the graph ───────────────────────────────────
    let explore = kg::explore(ctx, symbol).map_err(|e| {
        ErrorOutcome::from_anyhow(&e)
            .with_cause(format!("failed to explore symbol {symbol:?}"))
            .with_fix("run `memnoth kg index` if the workspace changed since the graph was built")
    })?;
    let explore_data = serde_json::to_value(&explore)
        .context("serialize kg explore report")
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    // ── Belief: what was believed about this symbol at the instant ──────
    let asof_outcome =
        vault::asof(ctx, Some(&rfc3339(instant)), Some(symbol), 1).map_err(|e| e.clone());
    let asof_data = asof_outcome
        .as_ref()
        .map(|o| o.data.clone())
        .unwrap_or_else(|_| json!({ "at": instant, "at_iso": rfc3339(instant), "name": symbol, "entities": [] }));

    // ── Supersession chain: every version ever recorded ─────────────────
    let history_outcome = vault::history(ctx, symbol).map_err(|e| e.clone());
    let history_data = history_outcome
        .as_ref()
        .map(|o| o.data.clone())
        .unwrap_or_else(|_| json!({ "name": symbol, "versions": [] }));

    // ── Superseded beliefs: explicitly revoked / contradicted ───────────
    let contradictions_outcome = vault::contradictions(ctx, Some(symbol)).map_err(|e| e.clone());
    let contradictions_data = contradictions_outcome
        .as_ref()
        .map(|o| o.data.clone())
        .unwrap_or_else(
            |_| json!({ "scope": symbol, "contradictions": [], "current_belief": None::<Value> }),
        );

    // ── Compose the text answer ─────────────────────────────────────────
    let stamp = rfc3339(instant);
    let mut text = format!("why {symbol}\n");
    text.push_str(&format!("  at: {stamp}\n\n"));

    // Belief section
    text.push_str("belief\n");
    if let Some(entities) = asof_data.get("entities").and_then(Value::as_array) {
        if entities.is_empty() {
            text.push_str(&format!(
                "  nothing was believed about {symbol} at {stamp}\n"
            ));
        } else {
            for entity in entities {
                let name = entity.get("name").and_then(Value::as_str).unwrap_or(symbol);
                let kind = entity
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("concept");
                let confidence = entity
                    .get("confidence")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let summary = entity
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("");
                text.push_str(&format!(
                    "  {name:<28} [{kind}] {confidence:.2}  {}\n",
                    ellipsize(summary, 96)
                ));
                if let Some(facts) = entity.get("facts").and_then(Value::as_array) {
                    for fact in facts {
                        let content = fact.get("content").and_then(Value::as_str).unwrap_or("");
                        let kind = fact.get("kind").and_then(Value::as_str).unwrap_or("fact");
                        let author = fact
                            .get("provenance")
                            .and_then(|p| p.get("user").and_then(Value::as_str))
                            .unwrap_or("unknown");
                        let recorded = fact.get("recorded_at").and_then(Value::as_i64);
                        let date = recorded.map(rfc3339).unwrap_or_else(|| rfc3339(instant));
                        text.push_str(&format!(
                            "    [{kind}] {}\n      source: {}\n      date:   {}\n      author: {}\n",
                            ellipsize(content, 120),
                            content,
                            date,
                            author
                        ));
                    }
                }
            }
        }
    } else {
        text.push_str(&format!(
            "  nothing was believed about {symbol} at {stamp}\n"
        ));
    }

    // Code section
    text.push_str("\ncode\n");
    if !explore.resolved {
        text.push_str(&format!(
            "  no symbol named {symbol:?} in the graph\n  run `memnoth kg index` if the workspace changed since it was built\n"
        ));
    } else {
        if let Some(def) = &explore.definition {
            text.push_str(&format!("  {}:{}  [{}]\n", def.path, def.line, def.kind));
            if let Some(container) = &def.container {
                text.push_str(&format!("  in {container}\n"));
            }
            text.push_str("  snippet:\n");
            for (i, line) in def.snippet.iter().enumerate() {
                let line_num = def.line.saturating_sub(3) + i + 1;
                let marker = if line_num == def.line { "→" } else { " " };
                text.push_str(&format!(
                    "  {}{} {:>4} | {}\n",
                    marker,
                    if marker == "→" { "" } else { " " },
                    line_num,
                    line
                ));
            }
        }
        if !explore.references.is_empty() {
            text.push_str(&format!("\n  references ({})\n", explore.references.len()));
            for hit in &explore.references {
                text.push_str(&format!(
                    "    {}:{}  {}\n",
                    hit.path,
                    hit.line,
                    ellipsize(&hit.context, 100)
                ));
            }
        }
        if !explore.callers.is_empty() {
            text.push_str(&format!("\n  callers ({})\n", explore.callers.len()));
            for hit in &explore.callers {
                text.push_str(&format!(
                    "    {}:{}  {}\n",
                    hit.path,
                    hit.line,
                    ellipsize(&hit.context, 100)
                ));
            }
        }
        if !explore.callees.is_empty() {
            text.push_str(&format!("\n  callees ({})\n", explore.callees.len()));
            for hit in &explore.callees {
                text.push_str(&format!(
                    "    {}:{}  {}\n",
                    hit.path,
                    hit.line,
                    ellipsize(&hit.context, 100)
                ));
            }
        }
        if explore.truncated {
            text.push_str("\n  lists truncated; narrow the name or the change set.\n");
        }
    }

    // Supersession chain section
    text.push_str("\nsupersession chain\n");
    if let Some(versions) = history_data.get("versions").and_then(Value::as_array) {
        if versions.is_empty() {
            text.push_str(&format!(
                "  no belief about {symbol} has ever been recorded\n"
            ));
        } else {
            for version in versions {
                let entity_id = version
                    .get("entity_id")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let from = version
                    .get("valid_from")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let to = version.get("valid_to").and_then(Value::as_i64);
                let via = version.get("via").and_then(Value::as_str).unwrap_or("name");
                let window = match to {
                    Some(to) => format!("{} → {}", rfc3339(from), rfc3339(to)),
                    None => format!("{} → now", rfc3339(from)),
                };
                text.push_str(&format!("  #{entity_id}  {window}  ({via})\n"));
                if let Some(facts) = version.get("facts").and_then(Value::as_array) {
                    for fact in facts {
                        let content = fact.get("content").and_then(Value::as_str).unwrap_or("");
                        let author = fact
                            .get("provenance")
                            .and_then(|p| p.get("user").and_then(Value::as_str))
                            .unwrap_or("unknown");
                        let recorded = fact.get("recorded_at").and_then(Value::as_i64);
                        let date = recorded.map(rfc3339).unwrap_or_else(|| rfc3339(from));
                        text.push_str(&format!(
                            "      source: {}\n      date:   {}\n      author: {}\n",
                            ellipsize(content, 120),
                            date,
                            author
                        ));
                    }
                }
                if let Some(reason) = version.get("change_reason").and_then(Value::as_str) {
                    text.push_str(&format!("      reason: {reason}\n"));
                }
            }
        }
    } else {
        text.push_str(&format!(
            "  no belief about {symbol} has ever been recorded\n"
        ));
    }

    // Contradictions / superseded beliefs
    if let Some(contradictions) = contradictions_data
        .get("contradictions")
        .and_then(Value::as_array)
        && !contradictions.is_empty()
    {
        text.push_str("\nsuperseded (explicitly revoked)\n");
        for row in contradictions {
            if let (Some(source), Some(target)) = (
                row.get("source").and_then(Value::as_str),
                row.get("target").and_then(Value::as_str),
            ) {
                let from = row
                    .get("valid_from")
                    .and_then(Value::as_i64)
                    .map(rfc3339)
                    .unwrap_or_default();
                let until = row
                    .get("revoked_at")
                    .and_then(Value::as_i64)
                    .map(rfc3339)
                    .unwrap_or_default();
                let kind = row
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("relation");
                let reason = row
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("(none recorded)");
                text.push_str(&format!(
                        "  {source} --{kind}--> {target}\n    believed {from} → revoked {until}\n    reason: {reason}\n"
                    ));
            } else {
                let previous_name = row
                    .get("previous_name")
                    .and_then(Value::as_str)
                    .unwrap_or("(unnamed)");
                let from = row
                    .get("previous_valid_from")
                    .and_then(Value::as_i64)
                    .map(rfc3339)
                    .unwrap_or_default();
                let until = row
                    .get("valid_to")
                    .and_then(Value::as_i64)
                    .map(rfc3339)
                    .unwrap_or_default();
                let summary = row
                    .get("previous_summary")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let reason = row
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("(none recorded)");
                text.push_str(&format!("  previous: {previous_name}  {from} → {until}\n",));
                if !summary.is_empty() {
                    text.push_str(&format!("    said: {summary}\n"));
                }
                text.push_str(&format!("    reason: {reason}\n"));
            }
        }
    }

    // Next commands
    text.push_str(&format!("\nnext:\n  memnoth vault asof --name {symbol} --at {stamp}\n  memnoth vault history {symbol}\n  memnoth vault contradictions --name {symbol}\n  memnoth kg explore {symbol}\n"));

    // ── Compose the data envelope ───────────────────────────────────────
    let mut next_commands = vec![
        format!("memnoth vault asof --name {symbol} --at {stamp}"),
        format!("memnoth vault history {symbol}"),
        format!("memnoth vault contradictions --name {symbol}"),
        format!("memnoth kg explore {symbol}"),
    ];
    if !explore.resolved {
        next_commands.push("memnoth kg index".to_string());
    }

    let data = json!({
        "symbol": symbol,
        "at": instant,
        "at_iso": stamp,
        "code": explore_data,
        "belief": asof_data,
        "history": history_data,
        "superseded": contradictions_data,
        "next_commands": next_commands,
    });

    Ok(Outcome::new("why", text.trim_end(), data))
}

/// Ellipsize a string to a maximum length, mirroring the vault's own helper.
fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out = String::new();
        for (i, ch) in text.char_indices() {
            if i >= max.saturating_sub(1) {
                out.push('…');
                break;
            }
            out.push(ch);
        }
        out
    }
}
