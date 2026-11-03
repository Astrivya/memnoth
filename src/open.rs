//! `mn open` — open deep-links to the memnoth cloud console.

use anyhow::{Context, Result};
use std::process::Command;

use crate::context::AppContext;
use crate::output::{Outcome, ErrorOutcome};

/// Run `mn open`.
pub fn run(ctx: &AppContext, args: &crate::cli::OpenArgs) -> Result<Outcome, ErrorOutcome> {
    let base_url = get_console_base_url()?;
    let url = build_console_url(&base_url, args, ctx)?;

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
        println!("{}", url);
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

/// Get the console base URL from config or default.
fn get_console_base_url() -> Result<String> {
    // Check environment variable first
    if let Ok(url) = std::env::var("MEMNOTH_CONSOLE_URL") {
        return Ok(url.trim_end_matches('/').to_string());
    }

    // Check config for custom endpoint
    // Default to the production console
    Ok("https://console.memnoth.astrivya.ai".to_string())
}

/// Build the console URL based on page and parameters.
fn build_console_url(base_url: &str, args: &crate::cli::OpenArgs, ctx: &AppContext) -> Result<String> {
    let page = args.page.to_lowercase();
    let mut url = match page.as_str() {
        "dashboard" | "dash" => format!("{}/dashboard", base_url),
        "projects" | "project" => {
            if let Some(id) = &args.project_id {
                format!("{}/projects/{}", base_url, id)
            } else {
                format!("{}/projects", base_url)
            }
        }
        "sessions" | "session" => {
            if let Some(id) = &args.session_id {
                format!("{}/sessions/{}", base_url, id)
            } else {
                format!("{}/sessions", base_url)
            }
        }
        "settings" | "config" => format!("{}/settings", base_url),
        "usage" | "costs" | "billing" => format!("{}/usage", base_url),
        "org" | "organization" => format!("{}/org", base_url),
        "teams" | "team" => format!("{}/teams", base_url),
        "api-keys" | "apikeys" | "keys" => format!("{}/api-keys", base_url),
        "workspaces" | "workspace" => format!("{}/workspaces", base_url),
        "connectors" | "integrations" => format!("{}/connectors", base_url),
        "sharing" | "share" => format!("{}/sharing", base_url),
        _ => {
            // Allow arbitrary paths
            format!("{}/{}", base_url.trim_end_matches('/'), page)
        }
    };

    // Add workspace context as query parameter if available
    let workspace_name = ctx.root.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");
    url.push_str(&format!("?workspace={}", encode_query_component(workspace_name)));

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
        return Ok(());
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