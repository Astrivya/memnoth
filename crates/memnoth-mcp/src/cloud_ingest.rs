//! Cloud ingest: pushes session lifecycle events to the memnoth control plane.
//!
//! This is what makes the console's Sessions page and its usage charts show
//! something. It was written against `MEMNOTH_CLOUD_INGEST_URL` (e.g.
//! `https://memnoth.astrivya.ai`) and had four defects that all failed
//! *silently*, which is why the page stayed empty and nothing said so:
//!
//! 1. **No TLS.** The request was hand-written over a raw `TcpStream` and
//!    defaulted to port 443, so an `https://` base wrote cleartext HTTP into a
//!    TLS socket. It could never succeed against the documented endpoint.
//! 2. **No status check.** The response was drained and discarded, so a 401 was
//!    indistinguishable from a 200.
//! 3. **The wrong credential.** It sent the MCP server's *static* shared bearer,
//!    which the control plane could not verify (it has no such verifier
//!    configured). It now sends the caller's account key — the credential the
//!    device sign-in mints — and falls back to the shared bearer only when no
//!    account key is present.
//! 4. **Off unless someone remembered an env var.** The base URL is now derived
//!    from the account API base the CLI already configures, so signing in is what
//!    turns reporting on. No credential, no ingest.
//!
//! All functions stay fire-and-forget: a failed report is logged, never
//! propagated. Cloud ingest must never block or fail a session operation.
//!
//! Environment:
//! - `MEMNOTH_CLOUD_API_KEY` / `MEMNOTH_API_KEY` — the account key (preferred).
//! - `MEMNOTH_MCP_HTTP_TOKEN` — fallback bearer for deployments that still use
//!   the shared static token.
//! - `MEMNOTH_CLOUD_INGEST_URL` — explicit base URL. Unset, it is derived from
//!   `MEMNOTH_API_BASE` (dropping a trailing `/api/v1`), and finally defaults to
//!   the hosted console.
//! - `MEMNOTH_PROJECT_ID` — file every session into this cloud project. A
//!   gateway serves a deployment, not one directory, so its project is a
//!   setting rather than a repository link. Unset, the control plane falls back
//!   to the reporting key's own project — a project-scoped service-account
//!   token attributes correctly with no gateway configuration at all.

use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

/// How long one report may take before it is abandoned. Ingest is not on any
/// critical path, so this is deliberately short.
const INGEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The hosted control plane, used when nothing else names one.
const DEFAULT_INGEST_BASE: &str = "https://memnoth.astrivya.ai";

/// The control plane's CSRF marker, which every non-browser mutating request
/// must carry. The server half of this contract is not part of this repository.
///
/// A named constant rather than a literal, and deliberately duplicated rather
/// than imported: this crate must not depend on control-plane internals, and the
/// dependency-boundaries gate is right to ask why a machine-surface crate would.
/// The pair is two strings the server hardcodes too — `1` is the whole protocol.
const CSRF_HEADER: &str = "x-memnoth-csrf";
/// The only value the guard accepts, alongside `true`.
const CSRF_VALUE: &str = "1";

/// Returns the cloud ingest base URL, or `None` if none can be determined.
///
/// Derivation order: explicit env → the account API base the CLI is already
/// configured with → the hosted default. The last step is not a guess: nothing is
/// sent unless a credential is present (see [`ingest_token`]), so a deployment
/// that never signed in stays silent.
pub fn ingest_url() -> Option<&'static str> {
    static URL: OnceLock<Option<String>> = OnceLock::new();
    URL.get_or_init(|| {
        if let Some(explicit) = std::env::var("MEMNOTH_CLOUD_INGEST_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            return Some(explicit.trim().trim_end_matches('/').to_string());
        }
        if let Some(api_base) = std::env::var("MEMNOTH_API_BASE")
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            let base = api_base.trim().trim_end_matches('/');
            // `https://host/api/v1` → `https://host`; anything else is used as
            // written, because only the `/api/v1` suffix is this crate's business.
            let base = base.strip_suffix("/api/v1").unwrap_or(base);
            if !base.is_empty() {
                return Some(base.to_string());
            }
        }
        Some(DEFAULT_INGEST_BASE.to_string())
    })
    .as_deref()
}

/// The bearer used for cloud API auth.
///
/// The account key first: it is per-user, revocable, and the one the control
/// plane actually verifies. The MCP server's static token is the fallback for
/// older setups — never the other way round, because a shared secret cannot say
/// which user a session belongs to.
fn ingest_token() -> Option<String> {
    for var in [
        "MEMNOTH_CLOUD_API_KEY",
        "MEMNOTH_API_KEY",
        "MEMNOTH_MCP_HTTP_TOKEN",
    ] {
        if let Some(value) = std::env::var(var).ok().filter(|v| !v.trim().is_empty()) {
            return Some(value);
        }
    }
    None
}

/// The project every report from this gateway should name, when configured.
///
/// Read per call rather than cached: the tests set and clear it, and the
/// control plane makes the final attribution decision anyway (a key-scoped
/// project wins when this is unset).
fn project_id() -> Option<String> {
    std::env::var("MEMNOTH_PROJECT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Shared client. Built once: a `reqwest::Client` owns a connection pool, and
/// building one per report throws that away.
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(INGEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// POST a session creation event to the cloud API.
pub fn session_created(session_id: &str, workspace: &str, model: &str) {
    let body = json!({
        "id": session_id,
        "workspace": workspace,
        "model": model,
        "status": "running",
        "started_at": now_rfc3339(),
        "project_id": project_id(),
    });
    fire_post(body);
}

/// POST a session close event to the cloud API.
pub fn session_closed(session_id: &str, tokens: i64, files: i64, cost_micros: i64) {
    let body = json!({
        "id": session_id,
        "status": "finished",
        "tokens": tokens,
        "files": files,
        "cost_micros": cost_micros,
        "ended_at": now_rfc3339(),
        "project_id": project_id(),
    });
    fire_post(body);
}

/// POST a session progress update (after prompt completion).
///
/// Reports are cumulative totals, so the control plane only rolls a session into
/// its usage ledger once it is closed — an update cannot double-count.
pub fn session_updated(session_id: &str, tokens: i64, files: i64, cost_micros: i64) {
    let body = json!({
        "id": session_id,
        "tokens": tokens,
        "files": files,
        "cost_micros": cost_micros,
        "project_id": project_id(),
    });
    fire_post(body);
}

/// Fire-and-forget POST. Never blocks the caller.
///
/// Every failure path logs, because the whole reason this went unnoticed for so
/// long is that it used to say nothing at all.
fn fire_post(body: Value) {
    let Some(base) = ingest_url() else {
        return;
    };
    let Some(token) = ingest_token() else {
        return;
    };
    let url = format!("{}/api/v1/sessions", base.trim_end_matches('/'));

    tokio::spawn(async move {
        match client()
            .post(&url)
            // Without this the guard answers 403 and the bearer is never read, so
            // nothing this module reports has ever reached the control plane.
            .header(CSRF_HEADER, CSRF_VALUE)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                let status = response.status();
                let detail = response.text().await.unwrap_or_default();
                eprintln!(
                    "[cloud_ingest] {url} returned {status}: {}",
                    truncate(&detail, 200)
                );
            }
            Err(error) => eprintln!("[cloud_ingest] POST {url} failed: {error}"),
        }
    });
}

/// A bounded slice of a response body for a log line. Truncating on a char
/// boundary keeps a multi-byte character from panicking the logger.
fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Current UTC time as RFC 3339.
fn now_rfc3339() -> String {
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let ms = dur.subsec_millis();
    let days = (secs / 86400) as i64;
    let (year, month, day) = civil_from_days(days);
    let tod = secs % 86400;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year,
        month,
        day,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        ms,
    )
}

/// Simple Gregorian calendar from days since Unix epoch.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundaries() {
        // A slice at a byte index inside a multi-byte character must not panic.
        let text = "é".repeat(200);
        let cut = truncate(&text, 5);
        assert!(text.starts_with(cut));
        assert!(cut.len() <= 5);
    }

    #[test]
    fn truncate_leaves_short_text_alone() {
        assert_eq!(truncate("ok", 200), "ok");
    }

    #[test]
    fn rfc3339_shape_is_stable() {
        // The control plane parses these with `parse_from_rfc3339`; the shape is
        // the contract, not the value.
        let stamp = now_rfc3339();
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert_eq!(stamp.len(), 24, "{stamp}");
        assert!(chrono_like(&stamp), "{stamp}");
    }

    /// `YYYY-MM-DDTHH:MM:SS.mmmZ` without pulling a date library into this crate.
    fn chrono_like(stamp: &str) -> bool {
        let bytes = stamp.as_bytes();
        let digits = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18, 20, 21, 22];
        digits.iter().all(|i| bytes[*i].is_ascii_digit())
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes[10] == b'T'
            && bytes[13] == b':'
            && bytes[16] == b':'
            && bytes[19] == b'.'
    }
}
