//! `/`-command implementations for the TUI.
//!
//! Each file here is one slash command: argument parsing, backend calls, and
//! text rendering for the transcript. The command registry
//! (`memnoth-command`) is the CLI's source of truth; these are the TUI's own
//! verbs that either have no CLI counterpart or need a session-specific shape.
//!
//! A verb is registered before its runtime exists so the palette can list it.
//! Until the backend lands, [`not_wired`] is the answer: it says what is
//! missing and names the command that works today. A canned number would be a
//! claim nothing can prove, which is worse than an honest gap.

use crate::output::Outcome;

pub mod brief;
pub mod btw;
pub mod checkpoint;
pub mod compact;
pub mod delegate;
pub mod drift;
pub mod export;
pub mod goal;
pub mod handoff;
pub mod impact;
pub mod keymap;
pub mod lineage;
pub mod merge;
pub mod onboarding;
pub mod plan;
pub mod recap;
pub mod receipt;
pub mod redo;
pub mod rename;
pub mod simulate;
pub mod status;
pub mod undo;
pub mod week;
pub mod why;

/// Answer for a registered verb whose runtime is not wired yet.
///
/// `needs` names the missing piece; `alternative` is the command that does the
/// job today. Both are part of the JSON so a script sees the same gap a person
/// reads in the transcript.
pub(crate) fn not_wired(verb: &'static str, needs: &str, alternative: &str) -> Outcome {
    let text = format!("{verb}\n  status: not wired\n  needs: {needs}\n  meanwhile: {alternative}");
    Outcome::new(
        verb,
        text,
        serde_json::json!({
            "verb": verb,
            "available": false,
            "needs": needs,
            "alternative": alternative,
        }),
    )
}
