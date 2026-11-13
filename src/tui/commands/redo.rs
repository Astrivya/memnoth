//! `/redo` — move forward one checkpoint and restore that conversation.
//!
//! `/redo` is the inverse of `/undo` over the same cursor, so the pair cannot
//! disagree about where in the checkpoint list the session stands.

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::checkpoint::{CheckpointStore, latest_session};
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

/// `/redo` — restore the checkpoint after the current one.
pub fn run(ctx: &AppContext) -> Result<Outcome, ErrorOutcome> {
    redo(ctx).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn redo(ctx: &AppContext) -> Result<Outcome> {
    let session = latest_session(&ctx.sessions_dir)?;
    let store = CheckpointStore::new(&ctx.home);
    let rows = store.list(&session)?;
    if rows.is_empty() {
        bail!("no checkpoints for {session} — /checkpoint writes one first");
    }

    let cursor = store.cursor(&session).unwrap_or(rows.len());
    if cursor >= rows.len() {
        bail!(
            "already at the newest checkpoint ({} of {}) for {session}",
            rows.len(),
            rows.len()
        );
    }

    let target = rows
        .iter()
        .find(|row| row.seq == cursor + 1)
        .with_context(|| format!("checkpoint {} is missing from the store", cursor + 1))?;
    let path = store.restore(&ctx.sessions_dir, &session, target)?;
    store.set_cursor(&session, cursor + 1)?;

    let text = format!(
        "redo → checkpoint {} \"{}\" ({} messages; {} of {})\n  written to: {}\n  resume with: mn --resume {}-rewind",
        target.seq,
        target.name,
        target.messages,
        target.seq,
        rows.len(),
        path.display(),
        session
    );

    Ok(Outcome::new(
        "redo",
        text,
        json!({
            "verb": "redo",
            "available": true,
            "session": session,
            "seq": target.seq,
            "of": rows.len(),
            "messages": target.messages,
            "restored_path": path.display().to_string(),
            "resume": format!("mn --resume {session}-rewind"),
        }),
    ))
}
