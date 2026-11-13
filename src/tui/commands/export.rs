//! `/export [path]` — Markdown transcript with tool calls and receipts.
//!
//! The writer reads the session's own file and renders it: messages first, then
//! the edits the session recorded and the receipts its turns produced. A file
//! that says "0 edits" because the writer never looked would be worse than no
//! export at all.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::json;

use crate::checkpoint::latest_session;
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};
use memnoth_session::Session;

/// `/export [path]` — write the newest conversation to a Markdown file.
pub fn run(ctx: &AppContext, path: Option<&str>) -> Result<Outcome, ErrorOutcome> {
    export(ctx, path).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn export(ctx: &AppContext, path: Option<&str>) -> Result<Outcome> {
    let session_name = latest_session(&ctx.sessions_dir)?;
    let source = ctx.sessions_dir.join(format!("{session_name}.jsonl"));
    let session = Session::load(&source)
        .with_context(|| format!("cannot read session {}", source.display()))?;

    let output = path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{session_name}.md")));
    let markdown = render(&session_name, &session);
    std::fs::write(&output, &markdown)
        .with_context(|| format!("cannot write {}", output.display()))?;

    let text = format!(
        "exported {session_name} → {} ({} messages, {} edits, {} receipts, {} bytes)",
        output.display(),
        session.messages.len(),
        session.writes.len(),
        session.receipts.len(),
        markdown.len()
    );

    Ok(Outcome::new(
        "export",
        text,
        json!({
            "verb": "export",
            "available": true,
            "session": session_name,
            "path": output.display().to_string(),
            "messages": session.messages.len(),
            "edits": session.writes.len(),
            "receipts": session.receipts.len(),
            "bytes": markdown.len(),
        }),
    ))
}

/// The Markdown a transcript export is made of.
///
/// Kept public to this crate so a test can assert the body without a workspace.
pub(crate) fn render(name: &str, session: &Session) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Session {name}\n\n"));
    out.push_str(&format!("- model: {}\n", session.model));
    out.push_str(&format!("- created: {} ms\n", session.created_ms));
    out.push_str(&format!("- messages: {}\n", session.messages.len()));
    out.push_str(&format!("- edits: {}\n", session.writes.len()));
    out.push_str(&format!("- receipts: {}\n", session.receipts.len()));
    out.push_str(&format!(
        "- tokens: {} in / {} out\n",
        session.input_tokens, session.output_tokens
    ));

    for message in &session.messages {
        let role = match message.role {
            memnoth_session::Role::User => "user",
            memnoth_session::Role::Assistant => "assistant",
            memnoth_session::Role::System => "system",
            memnoth_session::Role::Tool => "tool",
        };
        out.push_str(&format!("\n## {role}\n\n{}\n", message.content));
    }

    if !session.writes.is_empty() {
        out.push_str("\n## Edits\n\n");
        for write in &session.writes {
            let turn = write
                .turn_id
                .map(|turn| format!(" (turn {turn})"))
                .unwrap_or_default();
            out.push_str(&format!("- `{}`{turn}\n", write.path));
        }
    }

    if !session.receipts.is_empty() {
        out.push_str("\n## Receipts\n\n");
        for receipt in &session.receipts {
            let turn = receipt
                .turn_id
                .map(|turn| format!("turn {turn}"))
                .unwrap_or_else(|| "turn ?".to_string());
            let prompt = receipt.prompt_bytes().map(str::len).unwrap_or(0);
            let cost = receipt
                .cost_micros
                .map(|micros| format!(" · ${:.6}", micros as f64 / 1_000_000.0))
                .unwrap_or_default();
            out.push_str(&format!("- {turn} · {prompt} prompt bytes{cost}\n"));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use memnoth_session::Role;

    #[test]
    fn the_transcript_names_every_role_and_counts_what_it_wrote() {
        let mut session = Session::new("memnoth-local");
        session.push(Role::User, "fix the redirect");
        session.push(Role::Assistant, "done: two files");
        let markdown = render("payments", &session);
        assert!(markdown.starts_with("# Session payments"));
        assert!(markdown.contains("## user\n\nfix the redirect"));
        assert!(markdown.contains("## assistant\n\ndone: two files"));
        assert!(markdown.contains("- messages: 2"));
        assert!(markdown.contains("- edits: 0"));
    }
}
