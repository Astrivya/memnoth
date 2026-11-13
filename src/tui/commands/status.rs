//! `/status` — one answer from real state; no secrets.
//!
//! What the session can see for itself is the workspace, its trust state, the
//! vault and session directories. Model, context usage, goal, approvals and
//! sync live in a saved session, so the verb names `mn session status` for
//! those rather than printing numbers it did not measure.

use anyhow::Result;

use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

/// `/status` — redacted session status, in three lines.
///
/// Three, not six: the plan's 80x24 pass asks that the answer fit on one small
/// screen without pushing the prompt off it. Nothing is dropped — the paths are
/// still here, the saved-session fact is still here, and the pointer to the verb
/// that owns the numbers this one cannot measure is still here.
pub fn run(ctx: &AppContext) -> Result<Outcome, ErrorOutcome> {
    let trusted = crate::trust::is_trusted(&ctx.root);
    let saved_session = ctx.sessions_dir.join("session.json").exists();

    let text = render_status(
        &ctx.root.display().to_string(),
        trusted,
        &ctx.vault_path.display().to_string(),
        &ctx.sessions_dir.display().to_string(),
        saved_session,
    );

    let data = serde_json::json!({
        "workspace": ctx.root.display().to_string(),
        "trusted": trusted,
        "vault": ctx.vault_path.display().to_string(),
        "sessions_dir": ctx.sessions_dir.display().to_string(),
        "saved_session": saved_session,
        "alternative": "mn session status",
    });

    Ok(Outcome::new("status", text, data))
}

/// The three lines, separated from the context so a test can hold the shape.
pub fn render_status(
    workspace: &str,
    trusted: bool,
    vault: &str,
    sessions: &str,
    saved_session: bool,
) -> String {
    format!(
        "status · workspace {workspace} · trusted {}
vault {vault} · sessions {sessions}
saved session {} · model, context, goal, approvals, sync: `mn session status`",
        if trusted { "yes" } else { "no" },
        if saved_session { "yes" } else { "none yet" },
    )
}

#[cfg(test)]
mod tests {
    use super::render_status;

    #[test]
    fn the_status_answer_fits_three_lines_at_eighty_by_twenty_four() {
        let text = render_status(
            "/repo",
            true,
            "/repo/.memnoth/vault.db",
            "/home/u/.memnoth/sessions",
            true,
        );
        assert_eq!(
            text.lines().count(),
            3,
            "the plan's 80x24 pass asks for three lines:
{text}"
        );
        for line in text.lines() {
            assert!(
                line.chars().count() <= 80,
                "a line overflows 80 columns: {line:?}"
            );
        }
    }

    #[test]
    fn status_is_text_not_colour() {
        // Screen-reader mode and a colour-blind reader get the same answer: both
        // facts are spelled out, never implied by a colour.
        let trusted = render_status("/repo", true, "/v", "/s", false);
        let untrusted = render_status("/repo", false, "/v", "/s", true);
        assert!(trusted.contains("trusted yes"));
        assert!(untrusted.contains("trusted no"));
        assert!(trusted.contains("saved session none yet"));
        assert!(untrusted.contains("saved session yes"));
        assert!(
            !trusted.chars().any(|c| c.is_control() && c as u32 != 10),
            "the text answer carries no terminal control characters beyond its line breaks"
        );
    }
}
