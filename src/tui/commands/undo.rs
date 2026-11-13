//! `/undo` — move back one checkpoint and restore that conversation.
//!
//! The restore is written to `<session>-rewind.jsonl`, not over the live
//! conversation: the process is still appending to the live file, and a restore
//! underneath it would be overwritten on the next turn.

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::checkpoint::{CheckpointStore, latest_session};
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

/// `/undo` — restore the checkpoint before the current one.
pub fn run(ctx: &AppContext) -> Result<Outcome, ErrorOutcome> {
    undo(ctx).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn undo(ctx: &AppContext) -> Result<Outcome> {
    let session = latest_session(&ctx.sessions_dir)?;
    let store = CheckpointStore::new(&ctx.home);
    let rows = store.list(&session)?;
    if rows.is_empty() {
        bail!("no checkpoints for {session} — /checkpoint writes one first");
    }

    let cursor = store.cursor(&session).unwrap_or(rows.len());
    if cursor <= 1 {
        bail!(
            "already at checkpoint 1 of {} for {session} — nothing earlier to undo into",
            rows.len()
        );
    }

    let target = rows
        .iter()
        .find(|row| row.seq == cursor - 1)
        .with_context(|| format!("checkpoint {} is missing from the store", cursor - 1))?;
    let path = store.restore(&ctx.sessions_dir, &session, target)?;
    store.set_cursor(&session, cursor - 1)?;

    let text = format!(
        "undo → checkpoint {} \"{}\" ({} messages; {} of {})\n  written to: {}\n  resume with: mn --resume {}-rewind\n  {session} itself was not modified",
        target.seq,
        target.name,
        target.messages,
        target.seq,
        rows.len(),
        path.display(),
        session
    );

    Ok(Outcome::new(
        "undo",
        text,
        json!({
            "verb": "undo",
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
