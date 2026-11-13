//! `mn budget` — what the next request may spend, read from the assembler.
//!
//! A status line can say "context: 18%" without saying what the percentage is
//! of, and a compaction counter can say "dropped four" without naming one. This
//! verb answers the question a person has before a long turn: which slots are
//! filled, what was refused and why, and whether the prefix the provider is
//! about to cache is the one it cached last turn.
//!
//! Nothing here is estimated twice. The window comes from
//! `memnoth-provider-registry`, the ceiling comes from `Settings::prompt_budget`,
//! and the accounting comes from `assemble`'s own receipt — so the number a
//! person reads is the number the turn will send.

use anyhow::Result;
use serde_json::json;

use crate::cli::BudgetArgs;
use crate::context::AppContext;
use crate::output::Outcome;
use crate::provider;

/// The share of the request budget at which `budget` warns.
///
/// Compaction starts dropping narrative around this point, so the warning is
/// the last moment to shorten the task rather than discover the loss later.
pub const WARN_AT: f64 = 0.8;

/// Layer ordinals, as `assemble` records them.
const LAYERS: [&str; 4] = ["Global", "Project", "Session", "Turn"];

/// Run `mn budget`.
pub fn run(ctx: &AppContext, _args: &BudgetArgs) -> Result<Outcome> {
    let settings = provider::Settings::load(&ctx.config)?;
    let window = settings.context_window();
    let budget = settings.prompt_budget();
    let assembly = provider::assemble_system_prompt_for(&ctx.root, Some(budget));

    let receipt = &assembly.receipt;
    let used = receipt.total_tokens;
    let ratio = if budget == 0 {
        0.0
    } else {
        used as f64 / budget as f64
    };
    let percent = (ratio * 100.0).round() as usize;
    let refused = ratio >= 1.0;
    let warn = ratio >= WARN_AT;
    let prefix_hash = memnoth_context::assemble::hash_label(receipt.prefix_hash);

    let slots: Vec<serde_json::Value> = receipt
        .pieces
        .iter()
        .map(|piece| {
            json!({
                "slot": piece.slot,
                "tokens": piece.tokens,
                "layer": LAYERS.get(piece.layer as usize).copied().unwrap_or("unknown"),
                "hash": format!("{:016x}", piece.hash),
            })
        })
        .collect();

    let dropped: Vec<serde_json::Value> = receipt
        .dropped
        .iter()
        .map(|(slot, reason)| json!({ "slot": slot, "reason": format!("{reason:?}") }))
        .collect();

    let mut text = format!(
        "memnoth {} · {} · {}\nwindow {} · request {} · committed {} ({}%) · {} {}\n\n",
        env!("CARGO_PKG_VERSION"),
        settings.kind.name(),
        settings.model,
        window,
        budget,
        used,
        percent,
        receipt.pieces.len(),
        if receipt.pieces.len() == 1 {
            "piece"
        } else {
            "pieces"
        },
    );

    text.push_str("slots\n");
    if receipt.pieces.is_empty() {
        text.push_str("  none\n");
    } else {
        for piece in &receipt.pieces {
            text.push_str(&format!(
                "  {:<14} {:>6}  {}\n",
                piece.slot,
                piece.tokens,
                LAYERS
                    .get(piece.layer as usize)
                    .copied()
                    .unwrap_or("unknown")
            ));
        }
    }

    text.push_str("\ndropped\n");
    if receipt.dropped.is_empty() {
        text.push_str("  none\n");
    } else {
        for (slot, reason) in &receipt.dropped {
            text.push_str(&format!("  {slot:<14} {reason:?}\n"));
        }
    }

    text.push_str(&format!("\nprefix hash {prefix_hash}\n"));

    if refused {
        text.push_str(&format!(
            "\nrefused: {used} of {budget} request tokens committed ({percent}%) — \
             compaction must run before the next turn\n"
        ));
    } else if warn {
        text.push_str(&format!(
            "\nwarning: {percent}% of the request budget is committed; compaction starts \
             summarising narrative\n"
        ));
    }

    let mut outcome = Outcome::new(
        "budget",
        text,
        json!({
            "provider": settings.kind.name(),
            "model": settings.model,
            "window": window,
            "budget": budget,
            "used": used,
            "percent": percent,
            "pieces": receipt.pieces.len(),
            "prefix_hash": prefix_hash,
            "slots": slots,
            "dropped": dropped,
            "warn": warn,
            "refused": refused,
        }),
    );
    // A refusal is an answer with a non-zero status, not a diagnostic exit 0:
    // a script that reads the status must see that the next turn cannot fit.
    if refused {
        outcome.exit_code = 2;
    }
    Ok(outcome)
}
