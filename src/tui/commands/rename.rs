//! `/rename <name>` — give the newest conversation a different name.
//!
//! The rename is a file move through [`SessionStore::rename`], which refuses an
//! existing target rather than overwriting a conversation. In a session the
//! dispatch layer also updates the live `Session` id and the app's name, so the
//! next turn is written to the new file.

use anyhow::Result;
use serde_json::json;

use crate::checkpoint::latest_session;
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};
use crate::session::SessionStore;

/// `/rename <name>` — rename the newest session file.
pub fn run(ctx: &AppContext, name: &str) -> Result<Outcome, ErrorOutcome> {
    rename(ctx, name).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn rename(ctx: &AppContext, name: &str) -> Result<Outcome> {
    let from = latest_session(&ctx.sessions_dir)?;
    let store = SessionStore::new(&ctx.sessions_dir);
    let to = store.rename(&from, name)?;

    Ok(Outcome::new(
        "rename",
        format!("renamed {from} → {to}\n  {to}.jsonl is the conversation from now on"),
        json!({
            "verb": "rename",
            "available": true,
            "from": from,
            "to": to,
        }),
    ))
}
