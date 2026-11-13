//! `/brief` — facts and next actions, derived from what is on disk.
//!
//! Every line is a read: the newest session's message and receipt counts, the
//! vault file, the graph file, and whether this workspace is trusted. A missing
//! file is reported as missing rather than as a zero.

use anyhow::Result;
use serde_json::json;

use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};
use crate::session::SessionStore;
use memnoth_session::Session;

/// `/brief` — one screen of real state.
pub fn run(ctx: &AppContext) -> Result<Outcome, ErrorOutcome> {
    brief(ctx).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn brief(ctx: &AppContext) -> Result<Outcome> {
    let sessions = SessionStore::new(&ctx.sessions_dir).list();
    let newest = sessions.first();

    let mut lines = Vec::new();
    match newest {
        Some(session) => {
            lines.push(format!(
                "brief — {} ({} messages, last used {} ms)",
                session.name, session.messages, session.modified_ms
            ));
            if let Ok(loaded) = Session::load(std::path::Path::new(&session.path)) {
                lines.push(format!(
                    "  receipts: {} · edits: {} · checkpoints: {}",
                    loaded.receipts.len(),
                    loaded.writes.len(),
                    loaded.checkpoints.len()
                ));
            }
        }
        None => lines.push("brief — no conversations in this workspace yet".to_string()),
    }

    lines.push(format!(
        "  vault: {} ({})",
        ctx.vault_path.display(),
        state(&ctx.vault_path)
    ));
    lines.push(format!(
        "  graph: {} ({})",
        ctx.graph_path.display(),
        state(&ctx.graph_path)
    ));
    lines.push(format!(
        "  workspace: {} — {}",
        ctx.root.display(),
        if crate::trust::is_trusted(&ctx.root) {
            "trusted"
        } else {
            "not trusted"
        }
    ));
    let (week_lines, week_data) = super::week::render(ctx);
    lines.extend(week_lines);
    lines.push("  next: /status · /receipt · /why <symbol>".to_string());

    let text = lines.join("\n");

    let week = week_data;
    Ok(Outcome::new(
        "brief",
        text,
        json!({
            "verb": "brief",
            "available": true,
            "sessions": sessions.len(),
            "session": newest.map(|session| session.name.clone()),
            "messages": newest.map(|session| session.messages),
            "vault": ctx.vault_path.display().to_string(),
            "graph": ctx.graph_path.display().to_string(),
            "trusted": crate::trust::is_trusted(&ctx.root),
            "week": week,
        }),
    ))
}

fn state(path: &std::path::Path) -> &'static str {
    if path.is_file() {
        "present"
    } else {
        "not created yet"
    }
}
