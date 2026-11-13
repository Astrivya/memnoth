//! `/receipt [turn]` — per-turn proof, verify against the commit.
//!
//! Receipts are bound to commits, not turns, so the default is `HEAD`: the
//! receipt for the revision the working tree sits on. Verification is the real
//! check from `src/receipt.rs`, so a moved commit fails here with the same
//! named reason `mn receipt verify` gives.

use anyhow::Result;

use crate::cli::{ReceiptArgs, ReceiptCommand, ReceiptShowArgs, ReceiptVerifyArgs};
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

/// `/receipt [turn]` — show and verify the receipt for a turn.
pub fn run(ctx: &AppContext, turn: Option<&str>, verify: bool) -> Result<Outcome, ErrorOutcome> {
    // A turn has no receipt of its own; the commit is the unit, and `HEAD` is
    // the one this session is working on.
    let _ = turn;
    let args = ReceiptArgs {
        action: if verify {
            ReceiptCommand::Verify(ReceiptVerifyArgs {
                commit: None,
                json: ctx.json,
            })
        } else {
            ReceiptCommand::Show(ReceiptShowArgs {
                commit: None,
                json: ctx.json,
            })
        },
    };
    crate::receipt::run(ctx, &args).map_err(|e| ErrorOutcome::from_anyhow(&e))
}
