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
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
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

/// Exit codes for the CLI.
/// 0 = ok, 1 = unexpected, 2 = usage/bad flag, 3 = auth/credits,
/// 4 = not found, 5 = transient/network, 130 = cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    Ok = 0,
    Unexpected = 1,
    Usage = 2,
    Auth = 3,
    NotFound = 4,
    Transient = 5,
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
    pub fn from_anyhow(error: &anyhow::Error) -> Self {
        Self::new(ExitCode::Unexpected, format!("{error:#}"))
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
            write!(f, "
  cause: {cause}")?;
        }
        if let Some(fix) = &self.fix {
            write!(f, "
  fix: {fix}")?;
        }
        if let Some(docs) = &self.docs_url {
            write!(f, "
  docs: {docs}")?;
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
            130 => ExitCode::Cancelled,
            _ => ExitCode::Unexpected,
        }
    }
}

impl From<ErrorOutcome> for anyhow::Error {
    fn from(error: ErrorOutcome) -> Self {
        anyhow::anyhow!("{}", error.message)
    }
}

/// Print an outcome according to `--json`.
pub fn emit(json: bool, outcome: &Outcome) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope(outcome.command, &outcome.data))
                .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"unserializable answer\"}".into())
        );
    } else {
        println!("{}", outcome.text.trim_end());
    }
}

/// Print a failure, in the same shape a success uses.
pub fn emit_error(json: bool, error: &ErrorOutcome) {
    if json {
        let body = json!({
            "ok": false,
            "command": null,
            "version": env!("CARGO_PKG_VERSION"),
            "error": {
                "code": error.code,
                "message": error.message,
                "cause": error.cause,
                "fix": error.fix,
                "docs": error.docs_url,
            },
        });
        println!(
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

fn envelope(command: &'static str, data: &Value) -> Value {
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
    let dt = DateTime::from_timestamp_millis(ms).unwrap_or_else(|| Utc::now());
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
pub fn ellipsize(text: &str, width: usize) -> String {
    if text.len() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    format!("{}…", &text[..width - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_codes() {
        assert_eq!(ExitCode::Ok as u8, 0);
        assert_eq!(ExitCode::Unexpected as u8, 1);
        assert_eq!(ExitCode::Usage as u8, 2);
        assert_eq!(ExitCode::Auth as u8, 3);
        assert_eq!(ExitCode::NotFound as u8, 4);
        assert_eq!(ExitCode::Transient as u8, 5);
        assert_eq!(ExitCode::Cancelled as u8, 130);
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
