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
fn build_console_url(args: &crate::cli::OpenArgs, ctx: &AppContext) -> Result<String> {
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

    // Add workspace context as query parameter if available
    let workspace_name = ctx
        .root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");
    let mut query = format!("?workspace={}", encode_query_component(workspace_name));

    // The session a reader asked for, as the drawer the console actually has.
    if is_sessions && let Some(id) = args.session_id.as_deref() {
        query.push_str(&format!("&session={}", encode_query_component(id)));
    }
    url.push_str(&query);

    Ok(url)
}

/// Percent-encode one query-string value (RFC 3986 unreserved set kept literal).
///
/// A workspace name is a directory name, so it can hold a space or an `&`. The
/// console would read either as a different workspace, so the value is encoded
/// here rather than trusted.
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
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn()?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        // Use cmd /c start to open in default browser
        Command::new("cmd").args(["/c", "start", "", url]).spawn()?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open").arg(url).spawn()?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        anyhow::bail!("Unsupported platform for opening browser");
    }
}
