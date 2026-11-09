//! `mn ask` — one question, one answer, no screen.
//!
//! The session is the product's face, but a pipe cannot use a face. This verb
//! is the same turn without the TUI: it resolves one prompt from the command
//! line or stdin, runs the provider with the same toolbox and system prompt,
//! and renders the answer as text, one JSON envelope, or one event per line.
//!
//! Two rules are deliberate:
//!
//! * **No prompt, no hang.** With no argument and a terminal on stdin, the verb
//!   refuses and names the three forms. It only reads stdin when it is a pipe,
//!   so a script with a closed stdin gets a typed usage error rather than a wait.
//! * **No approver, no writes.** The toolbox refuses every write when nobody can
//!   approve it, so an unattended `ask` can read the vault and the graph but
//!   cannot record a belief or edit a file.

use std::io::{IsTerminal, Read};

use anyhow::Result;
use serde_json::json;

use crate::cli::AskArgs;
use crate::context::AppContext;
use crate::output::{ErrorOutcome, ExitCode, Outcome};
use crate::provider;

/// The three rendering shapes are distinct products, not formatting flags:
/// text for a person, one envelope for a parser, and a stream for a reader that
/// wants to see a tool round before the answer arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Json,
    Jsonl,
}

pub fn run(ctx: &AppContext, args: &AskArgs) -> Result<Outcome, ErrorOutcome> {
    let format = format_of(ctx, args)?;
    let prompt = read_prompt(args)?;

    let settings = provider::Settings::load(&ctx.config).map_err(|error| {
        ErrorOutcome::usage(format!("model config: {error:#}"))
            .with_fix("run `mn doctor` to see the provider settings")
    })?;
    if !settings.ready() {
        return Err(ErrorOutcome::auth("no model configured").with_fix(format!(
            "export {} or run `mn connect add`",
            settings.key_hint()
        )));
    }

    let streaming = format == Format::Jsonl;
    let mut tools = provider::Toolbox::new(ctx);
    let mut on_note = |line: &str| {
        if streaming {
            println!("{}", json!({ "type": "note", "text": line }));
        } else if !ctx.quiet {
            eprintln!("· {line}");
        }
    };

    let answer = provider::run_turn(&settings, &ctx.root, &[], &prompt, &mut tools, &mut on_note)
        .map_err(|error| {
        let outcome = failure(&error);
        if streaming {
            let mut event = json!({
                "type": "error",
                "ok": false,
                "code": outcome.code,
                "message": outcome.message,
                "cause": outcome.cause,
                "fix": outcome.fix,
                "docs": outcome.docs_url,
            });
            if let Some(object) = event.as_object_mut() {
                object.retain(|_, value| !value.is_null());
            }
            println!("{event}");
        }
        outcome
    })?;

    let data = json!({
        "text": answer.text,
        "model": settings.model,
        "provider": settings.kind.name(),
        "usage": {
            "input_tokens": answer.input_tokens,
            "output_tokens": answer.output_tokens,
        },
        "rounds": answer.rounds,
    });

    let text = match format {
        Format::Text => answer.text,
        Format::Json => {
            serde_json::to_string_pretty(&crate::output::success_envelope("ask", &data))
                .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"unserializable answer\"}".into())
        }
        Format::Jsonl => {
            let mut event = data.clone();
            event["type"] = json!("answer");
            event["ok"] = json!(true);
            event.to_string()
        }
    };

    Ok(Outcome::new("ask", text, data))
}

/// `--json` is the shared envelope switch and wins over the format, except for
/// `jsonl`: one envelope and a stream of events cannot both own stdout.
fn format_of(ctx: &AppContext, args: &AskArgs) -> Result<Format, ErrorOutcome> {
    if ctx.json {
        if args.output_format == "jsonl" {
            return Err(ErrorOutcome::usage(
                "`--json` and `--output-format jsonl` both claim stdout; choose one",
            ));
        }
        return Ok(Format::Json);
    }
    match args.output_format.as_str() {
        "text" => Ok(Format::Text),
        "json" => Ok(Format::Json),
        "jsonl" => Ok(Format::Jsonl),
        other => Err(ErrorOutcome::usage(format!(
            "unknown output format {other:?}; use text, json, or jsonl"
        ))),
    }
}

/// One prompt, from the flag, the positional words, or a pipe.
fn read_prompt(args: &AskArgs) -> Result<String, ErrorOutcome> {
    let words = args.words.join(" ");
    let words = words.trim();
    let flag = args
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());

    if !words.is_empty() && flag.is_some() {
        return Err(ErrorOutcome::usage(
            "the prompt was given twice; use the positional form or `-p`, not both",
        ));
    }

    let prompt = if let Some(prompt) = flag {
        prompt.to_string()
    } else if !words.is_empty() {
        words.to_string()
    } else {
        if std::io::stdin().is_terminal() {
            return Err(no_prompt());
        }
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| {
                ErrorOutcome::usage(format!("cannot read the prompt from stdin: {error}"))
            })?;
        input.trim().to_string()
    };

    if prompt.is_empty() {
        return Err(no_prompt());
    }
    Ok(prompt)
}

fn no_prompt() -> ErrorOutcome {
    ErrorOutcome::usage("no prompt").with_fix(
        "pass it as `mn ask \"…\"`, `mn ask -p \"…\"`, or pipe it: `printf '…\\n' | mn ask`",
    )
}

/// Map a turn failure onto the documented exit table. The provider layer writes
/// prose, not a typed error, so the status it already parsed is recovered here;
/// everything else stays a typed code rather than collapsing into 1.
fn failure(error: &anyhow::Error) -> ErrorOutcome {
    let message = format!("{error:#}");
    let status = provider_status(&message);
    let lower = message.to_ascii_lowercase();

    if message.contains("no model configured") || message.contains("provider.model is empty") {
        return ErrorOutcome::auth(message)
            .with_fix("run `mn connect add`, or export MEMNOTH_API_KEY and MEMNOTH_MODEL");
    }
    match status {
        Some(401 | 403) => {
            return ErrorOutcome::auth(message)
                .with_fix("check the provider key, then run `mn ask` again");
        }
        Some(402) => {
            return ErrorOutcome::auth(message)
                .with_fix("check the plan in the console, then retry");
        }
        Some(429) => {
            return ErrorOutcome::rate_limited(message)
                .with_fix("retry after the window the provider names");
        }
        Some(500..=599) => {
            return ErrorOutcome::transient(message)
                .with_fix("the provider is up but unhappy; retry shortly");
        }
        _ => {}
    }
    if message.contains("provider.base_url is not allowed")
        || message.contains("refusing the provider request")
        || message.contains("URLs with embedded credentials")
    {
        return ErrorOutcome::usage(message).with_fix("run `mn doctor` and fix provider.base_url");
    }
    if lower.contains("cannot reach")
        || lower.contains("timed out")
        || lower.contains("connection refused")
    {
        return ErrorOutcome::transient(message)
            .with_fix("check the endpoint and the network, then retry");
    }
    ErrorOutcome::new(ExitCode::Unexpected, message)
}

/// The provider says `"<url> answered <status>: …"`. Three digits after that
/// word are the status; nothing else in the sentence is trusted.
fn provider_status(message: &str) -> Option<u16> {
    let rest = message.split("answered ").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() == 3 {
        digits.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(flag: Option<&str>, words: &[&str]) -> AskArgs {
        AskArgs {
            words: words.iter().map(|word| word.to_string()).collect(),
            prompt: flag.map(str::to_string),
            output_format: "text".into(),
        }
    }

    #[test]
    fn a_word_prompt_and_a_flag_prompt_are_both_refused_when_both_arrive() {
        let error = read_prompt(&args(Some("one"), &["two"])).unwrap_err();
        assert_eq!(error.code, 2);
        assert!(error.message.contains("twice"), "{}", error.message);
    }

    #[test]
    fn a_status_is_read_only_from_the_answered_sentence() {
        assert_eq!(
            provider_status("https://api.example.com answered 429: slow down"),
            Some(429)
        );
        assert_eq!(provider_status("no status here"), None);
        assert_eq!(
            provider_status("answered 40: truncated"),
            None,
            "only a complete three-digit status counts"
        );
    }

    #[test]
    fn a_rate_limit_is_not_a_transient_network_failure() {
        let error = anyhow::anyhow!(
            "https://api.example.com/chat/completions answered 429 Too Many Requests: slow down"
        );
        assert_eq!(failure(&error).code, 7);
    }

    #[test]
    fn a_missing_key_is_an_auth_failure_with_a_fix() {
        let error = anyhow::anyhow!(
            "no model configured: export MEMNOTH_API_KEY (or ANTHROPIC_API_KEY) and run again"
        );
        let outcome = failure(&error);
        assert_eq!(outcome.code, 3);
        assert!(outcome.fix.unwrap().contains("mn connect add"));
        assert!(!outcome.message.contains("sk-"), "no key in the sentence");
    }

    #[test]
    fn a_5xx_is_transient_and_a_400_is_not_mistaken_for_one() {
        let server = failure(&anyhow::anyhow!("https://x.test answered 503: down"));
        assert_eq!(server.code, 5);
        let bad = failure(&anyhow::anyhow!("https://x.test answered 400: bad body"));
        assert_eq!(bad.code, 1);
    }
}
