//! One answer, two renderings.
//!
//! Every verb returns an [`Outcome`]: the text a person reads and the JSON a
//! program reads. The `data` half is the same shape the MCP tool for the same
//! question returns, which is the documented promise — a shell script and an
//! agent are reading one representation, not two that can drift.
//!
//! `--json` wraps `data` in a stable envelope:
//!
//! ```json
//! { "ok": true, "command": "vault.asof", "version": "0.1.0-beta.1", "data": { } }
//! ```
//!
//! Errors keep the envelope too, with `ok: false` and `error`, so a caller never
//! has to parse prose to find a failure.

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{Value, json};

/// What a verb produced.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    /// Process exit code this outcome should produce.
    pub exit_code: u8,
    /// Dotted command name (`vault.asof`), stable enough to branch on.
    pub command: &'static str,
    /// For a person.
    pub text: String,
    /// For a program, and for the TUI's transcript.
    pub data: Value,
}

impl Outcome {
    /// Build both halves.
    pub fn new(command: &'static str, text: impl Into<String>, data: Value) -> Self {
        Self {
            command,
            exit_code: 0,
            text: text.into(),
            data,
        }
    }

    /// Build an outcome that is prose only. The envelope still carries a
    /// command name, so a caller parsing it never sees a missing field.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            command: "memnoth",
            exit_code: 0,
            text: text.into(),
            data: json!({}),
        }
    }

    /// Build an outcome whose JSON *is* the text payload.
    pub fn inline(command: &'static str, text: impl Into<String>) -> Self {
        let text = text.into();
        let data = json!({ "text": text });
        Self {
            command,
            exit_code: 0,
            text,
            data,
        }
    }
}

/// Confirm a destructive action, or assume yes with `--yes`.
///
/// The default is **no**: an unread question (a pipe, a closed stdin) must not
/// revoke a key or delete a project. `--yes` exists for scripts, where there is
/// nobody to ask and the caller has already decided.
pub fn confirm(prompt: &str, yes: bool) -> std::result::Result<bool, ErrorOutcome> {
    use std::io::IsTerminal;

    confirm_with(prompt, yes, std::io::stdin().is_terminal(), || {
        let mut input = String::new();
        match std::io::stdin().read_line(&mut input) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(input),
        }
    })
}

/// The decision behind [`confirm`], with the terminal and the read injected so
/// the no-TTY refusal is testable without touching the harness's stdin.
fn confirm_with(
    prompt: &str,
    yes: bool,
    interactive: bool,
    read: impl FnOnce() -> Option<String>,
) -> std::result::Result<bool, ErrorOutcome> {
    use std::io::Write;

    if yes {
        return Ok(true);
    }
    if !interactive {
        return Err(
            ErrorOutcome::usage("cannot ask for confirmation: stdin is not a terminal")
                .with_fix("re-run with --yes if this is deliberate"),
        );
    }
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    Ok(read().is_some_and(|input| matches!(input.trim().to_lowercase().as_str(), "y" | "yes")))
}

impl std::error::Error for ErrorOutcome {}

/// Exit codes for the CLI.
/// 0 = ok, 1 = unexpected, 2 = usage/bad flag, 3 = auth/credits,
/// 4 = not found, 5 = transient/network, 6 = conflict, 7 = rate-limited,
/// 8 = this build lacks the feature, 130 = cancelled.
///
/// 8 is separate from 2 on purpose: a script that gets 2 passed the wrong flag
/// and should retry differently, while 8 means the request was well-formed and
/// this binary simply cannot do it — the same input works on a build that has
/// the feature compiled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    Ok = 0,
    Unexpected = 1,
    Usage = 2,
    Auth = 3,
    NotFound = 4,
    Transient = 5,
    Conflict = 6,
    RateLimited = 7,
    Unsupported = 8,
    Cancelled = 130,
}

impl ExitCode {
    /// Convert to process exit code.
    pub fn to_process_exit_code(self) -> std::process::ExitCode {
        std::process::ExitCode::from(self as u8)
    }

    /// Get exit code from integer.
    pub fn from_u8(code: u8) -> Self {
        match code {
            0 => ExitCode::Ok,
            1 => ExitCode::Unexpected,
            2 => ExitCode::Usage,
            3 => ExitCode::Auth,
            4 => ExitCode::NotFound,
            5 => ExitCode::Transient,
            6 => ExitCode::Conflict,
            7 => ExitCode::RateLimited,
            8 => ExitCode::Unsupported,
            130 => ExitCode::Cancelled,
            _ => ExitCode::Unexpected,
        }
    }
}

/// Detailed error outcome with structured fields for JSON envelope.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorOutcome {
    /// Exit code.
    pub code: u8,
    /// Human-readable error message.
    pub message: String,
    /// Underlying cause (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// Suggested fix (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Documentation URL (optional).
    #[serde(skip_serializing_if = "Option::is_none", rename = "docs")]
    pub docs_url: Option<String>,
}

impl ErrorOutcome {
    /// Create a new error outcome with exit code.
    pub fn new(code: ExitCode, message: impl Into<String>) -> Self {
        Self {
            code: code as u8,
            message: message.into(),
            cause: None,
            fix: None,
            docs_url: None,
        }
    }

    /// Add a cause.
    pub fn with_cause(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(cause.into());
        self
    }

    /// Add a suggested fix.
    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }

    /// Add a documentation URL.
    pub fn with_docs_url(mut self, docs_url: impl Into<String>) -> Self {
        self.docs_url = Some(docs_url.into());
        self
    }

    /// Create from anyhow error with default exit code.
    ///
    /// A cloud failure carries its own answer — missing token, exhausted
    /// credits, a rate limit — so it maps to the exit code and the fix the
    /// command-line contract promises instead of collapsing into code 1.
    pub fn from_anyhow(error: &anyhow::Error) -> Self {
        // An ErrorOutcome that travelled through `anyhow` keeps its exit code,
        // cause and fix. Without this downcast, `?` in an `anyhow::Result`
        // function silently turned a usage refusal (exit 2) into an unexpected
        // failure (exit 1) and dropped the `--yes` hint.
        if let Some(own) = error.downcast_ref::<ErrorOutcome>() {
            return own.clone();
        }
        if let Some(cloud) = error.downcast_ref::<memnoth_cloud_client::CloudError>() {
            return Self::from_cloud(cloud);
        }
        Self::new(ExitCode::Unexpected, format!("{error:#}"))
    }

    /// Map a cloud failure onto the exit-code table.
    pub fn from_cloud(error: &memnoth_cloud_client::CloudError) -> Self {
        use memnoth_cloud_client::CloudError;
        let bare = |text: &str| text.trim().trim_matches('"').to_string();
        match error {
            CloudError::AuthError(message) => {
                Self::new(ExitCode::Auth, format!("not signed in: {}", bare(message)))
                    .with_fix("run `memnoth auth login` (or set MEMNOTH_API_KEY)")
                    .with_docs_url(crate::context::docs_url("cli#auth"))
            }
            CloudError::CreditsExhausted(message) => Self::new(
                ExitCode::Auth,
                format!("credits exhausted: {}", bare(message)),
            )
            .with_fix("check the balance with `memnoth billing`, then add credits")
            .with_docs_url("https://memnoth.astrivya.ai/pricing"),
            CloudError::NotFound(message) => {
                Self::new(ExitCode::NotFound, format!("not found: {}", bare(message)))
            }
            CloudError::RateLimited {
                retry_after_secs,
                message,
            } => Self::new(
                ExitCode::RateLimited,
                format!("rate limited: {}", bare(message)),
            )
            .with_fix(format!("retry in {retry_after_secs}s")),
            CloudError::Transient { status, message } => Self::new(
                ExitCode::Transient,
                format!("server error ({status}): {message}"),
            )
            .with_fix("the service is up but unhappy; retry shortly"),
            CloudError::ClientError { status, message } => Self::new(
                ExitCode::Usage,
                format!("request rejected ({status}): {message}"),
            ),
            other => Self::new(ExitCode::Unexpected, other.to_string()),
        }
    }

    /// Create usage error.
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ExitCode::Usage, message)
    }

    /// Create auth error.
    pub fn auth(message: impl Into<String>) -> Self {
        Self::new(ExitCode::Auth, message)
    }

    /// Create not found error.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ExitCode::NotFound, message)
    }

    /// Create transient/network error.
    pub fn transient(message: impl Into<String>) -> Self {
        Self::new(ExitCode::Transient, message)
    }

    /// Create a "this build lacks it" error (exit 8).
    ///
    /// For a verb or flag that is advertised and parsed but not compiled into
    /// this binary. Never use it for a bad argument: that is [`Self::usage`].
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ExitCode::Unsupported, message)
    }

    /// Create a conflict error (exit 6): the request is valid but the state
    /// moved underneath it, so re-reading and retrying can succeed.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ExitCode::Conflict, message)
    }

    /// Create a rate-limited error (exit 7): retry after a delay, not
    /// immediately. Distinct from 5, where the network itself failed.
    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self::new(ExitCode::RateLimited, message)
    }

    /// Create cancelled error.
    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(ExitCode::Cancelled, message)
    }
}

impl std::fmt::Display for ErrorOutcome {
    /// The three-part failure a person reads: what failed, why, and the fix.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "memnoth: {}", self.message)?;
        if let Some(cause) = &self.cause {
            write!(
                f,
                "
  cause: {cause}"
            )?;
        }
        if let Some(fix) = &self.fix {
            write!(
                f,
                "
  fix: {fix}"
            )?;
        }
        if let Some(docs) = &self.docs_url {
            write!(
                f,
                "
  docs: {docs}"
            )?;
        }
        Ok(())
    }
}

impl From<anyhow::Error> for ErrorOutcome {
    fn from(error: anyhow::Error) -> Self {
        Self::from_anyhow(&error)
    }
}

impl From<u8> for ExitCode {
    fn from(code: u8) -> Self {
        match code {
            0 => ExitCode::Ok,
            1 => ExitCode::Unexpected,
            2 => ExitCode::Usage,
            3 => ExitCode::Auth,
            4 => ExitCode::NotFound,
            5 => ExitCode::Transient,
            6 => ExitCode::Conflict,
            7 => ExitCode::RateLimited,
            8 => ExitCode::Unsupported,
            130 => ExitCode::Cancelled,
            _ => ExitCode::Unexpected,
        }
    }
}

/// Print an outcome according to `--json`.
pub fn emit(json: bool, outcome: &Outcome) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&success_envelope(outcome.command, &outcome.data))
                .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"unserializable answer\"}".into())
        );
    } else {
        println!("{}", outcome.text.trim_end());
        let next = next_commands(&outcome.data);
        if !next.is_empty() {
            println!();
            println!("next");
            for command in next {
                println!("  {command}");
            }
        }
    }
}

/// The follow-ups an outcome names, ready to print or run.
///
/// Commands put them in `data.next_commands` so the text can stay a report;
/// showing them is what turns a result into the next keystroke.
pub fn next_commands(data: &serde_json::Value) -> Vec<&str> {
    data.get("next_commands")
        .and_then(|value| value.as_array())
        .map(|items| items.iter().filter_map(|item| item.as_str()).collect())
        .unwrap_or_default()
}

/// Print a failure, in the same shape a success uses.
///
/// `error` stays a string so `jq -r .error` gives the sentence a caller can show;
/// the machine fields that used to be nested inside it sit beside it, because an
/// envelope a script has to destructure twice is an envelope it will misread.
pub fn emit_error(json: bool, error: &ErrorOutcome) {
    if json {
        let body = json!({
            "ok": false,
            "command": null,
            "version": env!("CARGO_PKG_VERSION"),
            "error": error.message,
            "code": error.code,
            "cause": error.cause,
            "fix": error.fix,
            "docs": error.docs_url,
        });
        // A machine reads stdout; a failure belongs on stderr, as the exit-code
        // contract documents, so `cmd > answer.json` never swallows an error.
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into())
        );
    } else {
        eprintln!("memnoth: {}", error.message);
        if let Some(ref cause) = error.cause {
            eprintln!("  cause: {cause}");
        }
        if let Some(ref fix) = error.fix {
            eprintln!("  fix: {fix}");
        }
        if let Some(ref docs) = error.docs_url {
            eprintln!("  docs: {docs}");
        }
    }
}

/// The success envelope, in one place so `ask --output-format json` and
/// `--json` cannot describe success differently.
pub fn success_envelope(command: &'static str, data: &Value) -> Value {
    json!({
        "ok": true,
        "command": command,
        "version": env!("CARGO_PKG_VERSION"),
        "data": data,
    })
}

/// Parse the instants `vault asof --at` accepts.
///
/// Three forms, in the order they are tried: RFC 3339 with an offset, a plain
/// date (midnight UTC), then epoch milliseconds. The error names all three,
/// because "invalid date" after a typo is the least useful sentence a temporal
/// CLI can print.
pub fn parse_when(input: &str) -> Result<i64> {
    // RFC 3339 with offset
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Ok(dt.timestamp_millis());
    }
    // Plain date (midnight UTC)
    if let Ok(date) = NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        return Ok(date
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis());
    }
    // Epoch milliseconds
    if let Ok(ms) = input.parse::<i64>() {
        return Ok(ms);
    }
    anyhow::bail!(
        "invalid instant {input:?}; use RFC 3339 (2024-01-15T14:30:00Z), date (2024-01-15), or epoch ms"
    )
}

/// Epoch milliseconds as RFC 3339, for JSON that has be readable.
pub fn rfc3339(ms: i64) -> String {
    let dt = DateTime::from_timestamp_millis(ms).unwrap_or_else(Utc::now);
    dt.to_rfc3339()
}

/// A duration as a person reads it: `2m ago`, `3d ago`, `in 40s`.
pub fn human_age(ms: i64, now: i64) -> String {
    let diff = now - ms;
    let abs = diff.abs();
    let suffix = if diff >= 0 { "ago" } else { "in" };

    if abs < 1_000 {
        return format!("{abs}ms {suffix}");
    }
    let secs = abs / 1_000;
    if secs < 60 {
        return format!("{secs}s {suffix}");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m {suffix}");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h {suffix}");
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{days}d {suffix}");
    }
    let months = days / 30;
    if months < 12 {
        return format!("{months}mo {suffix}");
    }
    let years = months / 12;
    format!("{years}y {suffix}")
}

/// Bytes as a person reads them.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

/// A compact count: `1,204` → `1.2k`. Used where a column is narrow.
pub fn human_count(value: usize) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    let units = ["", "k", "M", "B", "T"];
    let mut v = value as f64;
    let mut i = 0;
    while v >= 1_000.0 && i < units.len() - 1 {
        v /= 1_000.0;
        i += 1;
    }
    if v >= 100.0 {
        format!("{:.0}{}", v, units[i])
    } else {
        format!("{:.1}{}", v, units[i])
    }
}

/// Truncate to `width` characters, with an ellipsis when it had to cut.
///
/// Width is counted in characters, not bytes. Slicing by byte offset lands in
/// the middle of a multi-byte character whenever the cut falls there — the `·`
/// separators in the footer hint are two bytes each — and that panicked the
/// whole screen on a resize.
pub fn ellipsize(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut out: String = text.chars().take(width - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipsize_cuts_on_a_character_not_a_byte() {
        // Every `·`, `↑`, `↓`, `←` and `→` is multi-byte, so a byte slice at
        // 39 lands inside one of them — the exact cut that used to panic on a
        // resize.
        let hint =
            "enter send · ↑↓ history · ←→ edit · pgup/pgdn scroll · end newest · ctrl-c quit";
        let out = ellipsize(hint, 40);
        assert_eq!(out.chars().count(), 40, "{out:?}");
        assert!(out.ends_with('…'), "{out:?}");
        // Whatever the cut lands on, what is kept is a whole-character prefix.
        assert!(hint.starts_with(out.trim_end_matches('…')), "{out:?}");

        // A width inside a character, and the degenerate widths, must not panic.
        for width in 0..hint.chars().count() + 2 {
            let cut = ellipsize(hint, width);
            assert!(cut.chars().count() <= width.max(1), "{width}: {cut:?}");
        }
        assert_eq!(ellipsize(hint, 0), "");
        assert_eq!(ellipsize(hint, 1), "…");
    }

    #[test]
    fn test_exit_codes() {
        assert_eq!(ExitCode::Ok as u8, 0);
        assert_eq!(ExitCode::Unexpected as u8, 1);
        assert_eq!(ExitCode::Usage as u8, 2);
        assert_eq!(ExitCode::Auth as u8, 3);
        assert_eq!(ExitCode::NotFound as u8, 4);
        assert_eq!(ExitCode::Transient as u8, 5);
        assert_eq!(ExitCode::Conflict as u8, 6);
        assert_eq!(ExitCode::RateLimited as u8, 7);
        assert_eq!(ExitCode::Unsupported as u8, 8);
        assert_eq!(ExitCode::Cancelled as u8, 130);
        // The inverse mapping must agree, or a script that reads a code back
        // gets a different answer than the one that produced it.
        for code in [
            ExitCode::Ok,
            ExitCode::Unexpected,
            ExitCode::Usage,
            ExitCode::Auth,
            ExitCode::NotFound,
            ExitCode::Transient,
            ExitCode::Conflict,
            ExitCode::RateLimited,
            ExitCode::Unsupported,
            ExitCode::Cancelled,
        ] {
            assert_eq!(ExitCode::from_u8(code as u8), code);
        }
    }

    #[test]
    fn confirmation_without_a_terminal_refuses_instead_of_waiting() {
        let error = confirm_with("Delete everything?", false, false, || {
            panic!("a non-interactive confirmation must not read stdin")
        })
        .unwrap_err();
        assert_eq!(error.code, 2);
        assert!(
            error
                .fix
                .as_deref()
                .is_some_and(|fix| fix.contains("--yes")),
            "the refusal must name the flag that resolves it"
        );

        // `--yes` is the script's answer and does not read anything either.
        assert!(
            confirm_with("Delete everything?", true, false, || {
                panic!("an explicit yes must not read stdin")
            })
            .unwrap()
        );
    }

    #[test]
    fn test_error_outcome() {
        let err = ErrorOutcome::new(ExitCode::Usage, "bad flag")
            .with_cause("unknown flag --foo")
            .with_fix("use --help")
            .with_docs_url("https://example.com");
        assert_eq!(err.code, 2);
        assert_eq!(err.message, "bad flag");
        assert_eq!(err.cause, Some("unknown flag --foo".into()));
        assert_eq!(err.fix, Some("use --help".into()));
        assert_eq!(err.docs_url, Some("https://example.com".into()));
    }

    #[test]
    fn test_parse_when() {
        assert!(parse_when("2024-01-15T14:30:00Z").is_ok());
        assert!(parse_when("2024-01-15").is_ok());
        assert!(parse_when("1705330200000").is_ok());
        assert!(parse_when("invalid").is_err());
    }

    #[test]
    fn test_human_age() {
        assert_eq!(human_age(1000, 2000), "1s ago");
        assert_eq!(human_age(1000, 0), "1s in");
        assert_eq!(human_age(0, 120_000), "2m ago");
    }

    #[test]
    fn test_human_bytes() {
        assert_eq!(human_bytes(500), "500 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1_500_000), "1.4 MB");
    }

    #[test]
    fn test_human_count() {
        assert_eq!(human_count(500), "500");
        assert_eq!(human_count(1_200), "1.2k");
        assert_eq!(human_count(1_500_000), "1.5M");
    }

    #[test]
    fn test_ellipsize() {
        assert_eq!(ellipsize("hello", 10), "hello");
        assert_eq!(ellipsize("hello", 3), "he…");
        assert_eq!(ellipsize("hello", 1), "…");
    }
}
