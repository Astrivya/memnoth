//! `/compact [focus]` — summarise the transcript through the configured model.
//!
//! Compaction is destructive to a conversation, so it never touches the
//! original: the live session is checkpointed first
//! ([`CheckpointStore`]), the model writes a summary, and the result is saved as
//! `<session>-compact.jsonl`. The original stays exactly as it was.

use anyhow::{Result, bail};
use serde_json::json;

use crate::checkpoint::{CheckpointStore, latest_session};
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};
use crate::provider;
use memnoth_session::{Role, Session};

/// Messages kept verbatim after the summary. The last exchange is what a
/// follow-up question refers to, so dropping it makes the compaction useless.
const KEEP_TAIL: usize = 2;

/// `/compact [focus]` — compact the newest conversation.
pub fn run(ctx: &AppContext, focus: Option<&str>) -> Result<Outcome, ErrorOutcome> {
    compact(ctx, focus).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn compact(ctx: &AppContext, focus: Option<&str>) -> Result<Outcome> {
    let session_name = latest_session(&ctx.sessions_dir)?;
    let source = ctx.sessions_dir.join(format!("{session_name}.jsonl"));
    let session = Session::load(&source)?;
    if session.messages.is_empty() {
        bail!("{session_name} has no messages to compact");
    }

    let settings = provider::Settings::load(&ctx.config)?;
    if !settings.ready() {
        bail!(
            "no model configured — compaction needs a summariser; export {} or run `mn connect add`",
            settings.key_hint()
        );
    }

    // Checkpoint first. A summary that loses the thread must not be the only
    // copy of the conversation.
    let checkpoint = CheckpointStore::new(&ctx.home).create(
        &ctx.sessions_dir,
        &session_name,
        "before compact",
        Some(&ctx.root),
    )?;

    let prompt = summary_prompt(&session, focus);
    let mut tools = provider::Toolbox::new(ctx);
    let mut silent = |_line: &str| {};
    let answer = provider::run_turn(&settings, &ctx.root, &[], &prompt, &mut tools, &mut silent)?;

    let compacted = build_compacted(&session, &answer.text, KEEP_TAIL);
    let path = compacted.save(&ctx.sessions_dir)?;

    let kept = compacted.messages.len().saturating_sub(1);
    let text = format!(
        "compacted {session_name}: {} messages → 1 summary + {kept} kept\n  checkpoint {} \"{}\": {}\n  compacted file: {}\n  resume with: mn --resume {session_name}-compact\n  {session_name} itself is unchanged",
        session.messages.len(),
        checkpoint.seq,
        checkpoint.name,
        checkpoint.snapshot,
        path.display(),
    );

    Ok(Outcome::new(
        "compact",
        text,
        json!({
            "verb": "compact",
            "available": true,
            "session": session_name,
            "original_messages": session.messages.len(),
            "kept_messages": kept,
            "summary_bytes": answer.text.len(),
            "checkpoint": checkpoint.snapshot,
            "checkpoint_seq": checkpoint.seq,
            "path": path.display().to_string(),
            "resume": format!("mn --resume {session_name}-compact"),
        }),
    ))
}

/// The prompt a summariser is asked for.
///
/// It names what must survive: a summary that drops a file path or an error is
/// worse than no compaction, because the next turn will act on it.
pub(crate) fn summary_prompt(session: &Session, focus: Option<&str>) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "Summarise this coding session for a continuation. Preserve, verbatim: file paths, \
         commands run, errors seen, decisions made, and anything still unresolved. Do not call \
         tools; answer with the summary only.\n",
    );
    if let Some(focus) = focus.map(str::trim).filter(|focus| !focus.is_empty()) {
        prompt.push_str(&format!("Focus on: {focus}\n"));
    }
    prompt.push_str(&format!(
        "\n{} messages follow.\n\n",
        session.messages.len()
    ));
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

/// The compacted conversation: one summary message, then the last `keep`
/// messages verbatim.
pub(crate) fn build_compacted(original: &Session, summary: &str, keep: usize) -> Session {
    let mut compacted = Session::new(&original.model);
    compacted.id = format!("{}-compact", original.id);
    compacted.created_ms = crate::session::now_ms();
    compacted.push(
        Role::System,
        format!(
            "Compacted summary of {} messages:\n\n{}",
            original.messages.len(),
            summary.trim()
        ),
    );
    let tail = original.messages.len().saturating_sub(keep);
    for message in &original.messages[tail..] {
        compacted.messages.push(message.clone());
    }
    compacted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut session = Session::new("memnoth-local");
        session.id = "payments".to_string();
        session.push(Role::User, "fix the redirect");
        session.push(Role::Assistant, "edited src/login.rs");
        session.push(Role::User, "now add a test");
        session.push(Role::Assistant, "added tests/login.rs");
        session
    }

    #[test]
    fn the_prompt_carries_every_message_and_the_focus() {
        let prompt = summary_prompt(&session(), Some("the redirect"));
        assert!(prompt.contains("user: fix the redirect"));
        assert!(prompt.contains("assistant: added tests/login.rs"));
        assert!(prompt.contains("Focus on: the redirect"));
        assert!(prompt.contains("4 messages follow"));
    }

    #[test]
    fn the_compacted_session_is_a_summary_plus_the_tail() {
        let compacted = build_compacted(&session(), "  the redirect was fixed  ", KEEP_TAIL);
        assert_eq!(compacted.id, "payments-compact");
        assert_eq!(compacted.messages.len(), 3, "summary + two kept");
        assert_eq!(compacted.messages[0].role, Role::System);
        assert!(
            compacted.messages[0]
                .content
                .contains("the redirect was fixed")
        );
        assert!(compacted.messages[0].content.contains("4 messages"));
        assert_eq!(compacted.messages[1].content, "now add a test");
        assert_eq!(compacted.messages[2].content, "added tests/login.rs");
    }

    #[test]
    fn a_short_session_keeps_everything() {
        let mut short = Session::new("memnoth-local");
        short.id = "one".to_string();
        short.push(Role::User, "hello");
        let compacted = build_compacted(&short, "said hello", KEEP_TAIL);
        assert_eq!(compacted.messages.len(), 2);
        assert_eq!(compacted.messages[1].content, "hello");
    }
}
