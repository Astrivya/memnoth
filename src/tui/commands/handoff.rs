//! `/handoff [path]` — redacted bundle; import on another machine.
//!
//! The bundle is written by `session attach`, the same writer
//! `memnoth session attach` uses, so the two doors cannot produce two formats.
//! Vault and graph snapshots are deliberately not embedded: a handoff that
//! carries a whole vault is a copy of the person's memory, not a transcript.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use crate::checkpoint::latest_session;
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

/// `/handoff [path]` — bundle the newest conversation for transfer.
pub fn run(ctx: &AppContext, path: Option<&str>) -> Result<Outcome, ErrorOutcome> {
    handoff(ctx, path).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn handoff(ctx: &AppContext, path: Option<&str>) -> Result<Outcome> {
    let session = latest_session(&ctx.sessions_dir)?;
    let bundle = path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{session}.memnoth-bundle")));
    let bundle_arg = bundle.display().to_string();

    let attached =
        crate::session_cmd::attach(ctx, &session, Some(&bundle_arg), false, false, false)?;
    let size = attached.data["size_bytes"].as_u64().unwrap_or(0);
    let next = format!("mn session handoff {bundle_arg} --name {session}");

    let text = format!(
        "handoff bundle for {session} → {bundle_arg} ({size} bytes)\n  vault and graph are not embedded\n  import with: {next}"
    );

    Ok(Outcome::new(
        "handoff",
        text,
        json!({
            "verb": "handoff",
            "available": true,
            "session": session,
            "bundle_path": bundle_arg,
            "size_bytes": size,
            "include_vault": false,
            "include_kg": false,
            "next": next,
        }),
    ))
}
