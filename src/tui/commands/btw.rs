//! `/btw <question>` — side question; not persisted in the transcript.
//!
//! The answer is a one-off provider turn with no history and the session's
//! toolbox: it can read the workspace, it cannot see the conversation, and
//! nothing it produces is written to the session JSONL.

use anyhow::{Result, bail};
use serde_json::json;

use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};
use crate::provider;

/// `/btw <question>` — side question, not persisted.
pub fn run(ctx: &AppContext, question: &str) -> Result<Outcome, ErrorOutcome> {
    btw(ctx, question).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn btw(ctx: &AppContext, question: &str) -> Result<Outcome> {
    if question.trim().is_empty() {
        bail!("btw needs a question — usage: /btw <question>");
    }
    let settings = provider::Settings::load(&ctx.config)?;
    if !settings.ready() {
        bail!(
            "no model configured — /btw needs one; export {} or run `mn connect add`",
            settings.key_hint()
        );
    }

    let mut tools = provider::Toolbox::new(ctx);
    let mut silent = |_line: &str| {};
    let answer = provider::run_turn(
        &settings,
        &ctx.root,
        &[],
        question.trim(),
        &mut tools,
        &mut silent,
    )?;

    Ok(Outcome::new(
        "btw",
        format!("{}\n\n(not saved to this session)", answer.text.trim()),
        json!({
            "verb": "btw",
            "available": true,
            "answer": answer.text,
            "persisted": false,
        }),
    ))
}
