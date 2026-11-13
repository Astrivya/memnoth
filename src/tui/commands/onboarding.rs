//! `/onboarding` — one SessionAction overlay, reusing the W5 card.
//!
//! The card itself is real (`tui::overlays::onboarding`), but this verb is a
//! registry `Run` verb, not a session action, so it cannot open an overlay. It
//! names the path that does instead of describing the card as if it had opened.

use anyhow::Result;

use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

use super::not_wired;

/// `/onboarding` — replayable first-run card.
pub fn run(_ctx: &AppContext) -> Result<Outcome, ErrorOutcome> {
    Ok(not_wired(
        "onboarding",
        "a session action that opens the first-run card",
        "run `mn session` with a fresh MEMNOTH_HOME; the card is the first screen",
    ))
}
