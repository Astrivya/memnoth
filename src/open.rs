//! `mn open` — open deep-links to the memnoth cloud console.

use anyhow::{Context, Result};
use std::process::Command;

use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

/// Run `mn open`.
pub fn run(ctx: &AppContext, args: &crate::cli::OpenArgs) -> Result<Outcome, ErrorOutcome> {
    let url = build_console_url(args, ctx)?;

    if args.json {
        let data = serde_json::json!({
            "url": url,
            "page": args.page,
            "project_id": args.project_id,
            "session_id": args.session_id,
        });
        return Ok(Outcome::new("open", url.clone(), data));
    }

    if args.no_browser {
        // Not printed here: the outcome's message *is* the URL, and every verb's
        // renderer prints that once. Printing it here put it on screen twice.
        return Ok(Outcome::new(
            "open",
            url,
            serde_json::json!({ "opened": false }),
        ));
    }

    open_browser(&url).context("Failed to open browser")?;

    Ok(Outcome::new(
        "open",
        format!("Opened {}", url),
        serde_json::json!({ "opened": true, "url": url }),
    ))
}

/// Build the console URL based on page and parameters.
///
/// The mount lives in [`crate::context::console_url`]; this function only decides
/// which console route a verb means.
fn build_console_url(args: &crate::cli::OpenArgs, _ctx: &AppContext) -> Result<String> {
    let page = args.page.to_lowercase();
    // Decided before the match because the catch-all arm moves `page`.
    let is_sessions = page.starts_with("session");
    let route = match page.as_str() {
        "dashboard" | "dash" => "dashboard".to_string(),
        "projects" | "project" => match &args.project_id {
            Some(id) => format!("projects/{id}"),
            None => "projects".to_string(),
        },
        // The console has no per-session route: a session is a row in a list,
        // and its drawer is opened by `?session=`. A path segment here 404s.
        "sessions" | "session" => "sessions".to_string(),
        "settings" | "config" => "settings".to_string(),
        "usage" | "costs" | "billing" => "usage".to_string(),
        "org" | "organization" => "org".to_string(),
        "teams" | "team" => "teams".to_string(),
        "api-keys" | "apikeys" | "keys" => "api-keys".to_string(),
        "workspaces" | "workspace" => "workspaces".to_string(),
        "connectors" | "integrations" => "connectors".to_string(),
        "sharing" | "share" => "sharing".to_string(),
        // An arbitrary path is still a console route, never an origin path.
        _ => page,
    };
    let mut url = crate::context::console_url(&route);

    // The session a reader asked for, as the drawer the console actually has.
    //
    // No `?workspace=`: every link used to carry one, and no console route reads
    // it — the sessions page validates `day`, `session` and `project` and drops
    // anything else. A parameter nothing consumes is a promise to the reader that
    // the page is filtered by this directory, which it is not.
    let query = if is_sessions && let Some(id) = args.session_id.as_deref() {
        format!("?session={}", encode_query_component(id))
    } else {
        String::new()
    };
    url.push_str(&query);

    Ok(url)
}

/// Percent-encode one query-string value (RFC 3986 unreserved set kept literal).
///
/// A session id comes from a file name, so it can hold a space or an `&`. The
/// console would read either as a different id, so the value is encoded here
/// rather than trusted.
fn encode_query_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Open a URL in the default browser.
fn open_browser(url: &str) -> Result<()> {
    // One statement per platform and a single `Ok(())` at the end. A `return`
    // inside whichever block is last on this host is needless there, and only
    // a clippy run on that host says so.
    #[cfg(target_os = "macos")]
    Command::new("open").arg(url).spawn()?;

    #[cfg(target_os = "windows")]
    Command::new("cmd").args(["/c", "start", "", url]).spawn()?;

    #[cfg(target_os = "linux")]
    Command::new("xdg-open").arg(url).spawn()?;

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        anyhow::bail!("Unsupported platform for opening browser");
    }

    Ok(())
}
