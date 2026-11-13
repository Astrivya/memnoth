//! `/recap` — what this session actually did, from its own record.
//!
//! The recap is a provider turn over the session's own messages, returned to the
//! screen and never written back. With no model configured it refuses rather
//! than inventing a sentence, and it reports the real turn count it summarised.

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::checkpoint::latest_session;
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};
use crate::provider;
use memnoth_session::{Role, Session};

/// `/recap` — one-paragraph summary of this session.
pub fn run(ctx: &AppContext) -> Result<Outcome, ErrorOutcome> {
    recap(ctx).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn recap(ctx: &AppContext) -> Result<Outcome> {
    let session_name = latest_session(&ctx.sessions_dir)?;
    let path = ctx.sessions_dir.join(format!("{session_name}.jsonl"));
    let session =
        Session::load(&path).with_context(|| format!("cannot read session {}", path.display()))?;
    if session.messages.is_empty() {
        bail!("{session_name} has no messages to recap");
    }

    let settings = provider::Settings::load(&ctx.config)?;
    if !settings.ready() {
        bail!(
            "no model configured — /recap needs one; export {} or run `mn connect add`",
            settings.key_hint()
        );
    }

    let mut tools = provider::Toolbox::new(ctx);
    let mut silent = |_line: &str| {};
    let answer = provider::run_turn(
        &settings,
        &ctx.root,
        &[],
        &recap_prompt(&session),
        &mut tools,
        &mut silent,
    )?;

    Ok(Outcome::new(
        "recap",
        format!(
            "recap of {session_name} ({} messages):\n{}",
            session.messages.len(),
            answer.text.trim()
        ),
        json!({
            "verb": "recap",
            "available": true,
            "session": session_name,
            "messages": session.messages.len(),
            "summary": answer.text,
        }),
    ))
}

/// The recap prompt: the transcript, with an explicit length limit.
pub(crate) fn recap_prompt(session: &Session) -> String {
    let mut prompt = String::from(
        "Summarise this coding session in one short paragraph: what was asked, what changed, and \
         what is still open. Do not call tools. Answer with the paragraph only.\n\n",
    );
    for message in &session.messages {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Tool => "tool",
        };
        prompt.push_str(&format!("{role}: {}\n\n", message.content));
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_carries_the_transcript_and_asks_for_one_paragraph() {
        let mut session = Session::new("memnoth-local");
        session.push(Role::User, "fix the redirect");
        session.push(Role::Assistant, "edited src/login.rs");
        let prompt = recap_prompt(&session);
        assert!(prompt.contains("one short paragraph"));
        assert!(prompt.contains("user: fix the redirect"));
        assert!(prompt.contains("assistant: edited src/login.rs"));
    }
}
