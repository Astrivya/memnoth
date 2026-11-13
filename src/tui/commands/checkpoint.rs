//! `/checkpoint [name]` — snapshot code, conversation, and vault metadata.
//!
//! The snapshot is a copy of the session's JSONL plus the commit the workspace
//! was on ([`crate::checkpoint::CheckpointStore`]). Nothing is held in memory:
//! the row points at a file that `/undo` can restore.

use anyhow::Result;
use serde_json::json;

use crate::checkpoint::{CheckpointStore, latest_session};
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

/// `/checkpoint [name]` — snapshot the newest conversation on disk.
pub fn run(ctx: &AppContext, name: Option<&str>) -> Result<Outcome, ErrorOutcome> {
    checkpoint(ctx, name).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn checkpoint(ctx: &AppContext, name: Option<&str>) -> Result<Outcome> {
    let session = latest_session(&ctx.sessions_dir)?;
    let store = CheckpointStore::new(&ctx.home);
    let label = name.map(str::trim).filter(|label| !label.is_empty());
    let checkpoint = store.create(
        &ctx.sessions_dir,
        &session,
        label.unwrap_or("checkpoint"),
        Some(&ctx.root),
    )?;

    let code = checkpoint
        .git_head
        .as_deref()
        .map(|head| head[..head.len().min(12)].to_string())
        .unwrap_or_else(|| "not a git repository".to_string());

    let text = format!(
        "checkpoint {} \"{}\" — {} ({} messages)\n  snapshot: {}\n  code: {}\n  /undo restores it as {}-rewind; /redo moves forward",
        checkpoint.seq,
        checkpoint.name,
        session,
        checkpoint.messages,
        checkpoint.snapshot,
        code,
        session
    );

    Ok(Outcome::new(
        "checkpoint",
        text,
        json!({
            "verb": "checkpoint",
            "available": true,
            "session": session,
            "seq": checkpoint.seq,
            "name": checkpoint.name,
            "messages": checkpoint.messages,
            "snapshot": checkpoint.snapshot,
            "git_head": checkpoint.git_head,
        }),
    ))
}
