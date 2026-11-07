//! Text renderings for the MCP verbs.
//!
//! Plain text, aligned, no colour — the same choice every other verb makes. The
//! JSON half of each answer is the report struct itself, so a rendering that
//! drops a fact cannot hide it from a script.

use crate::output::{ellipsize, rfc3339};

use super::{
    AddReport, DoctorReport, ExposesReport, ListReport, LogsReport, ProfileSetReport,
    ProfileShowReport, RemoveReport, RestartReport, SelftestReport, TestReport, TokenReport,
};

/// `memnoth mcp doctor`.
pub fn render_doctor(report: &DoctorReport) -> String {
    let mut text = String::new();
    text.push_str("mcp\n");
    text.push_str(&format!("  endpoint   {}\n", report.url));
    text.push_str(&format!(
        "  profile    {} · {} tools\n",
        report.profile, report.profile_tools
    ));
    match &report.server {
        Some(path) => {
            text.push_str(&format!("  server     {path}\n"));
            let verdict = match report.server_ok {
                Some(true) => "answers",
                Some(false) => "FAILED",
                None => "not run",
            };
            text.push_str(&format!(
                "             selftest {verdict} — {}\n",
                ellipsize(&report.server_detail, 120)
            ));
        }
        None => {
            text.push_str("  server     not found\n");
            text.push_str(&format!("             {}\n", report.server_detail));
        }
    }
    text.push_str(&format!(
        "  token      {}\n",
        if report.token_present {
            "MEMNOTH_MCP_TOKEN is set".to_string()
        } else {
            format!(
                "missing — mint one at {}, then export MEMNOTH_MCP_TOKEN",
                crate::context::console_url("api-keys")
            )
        }
    ));
    if let Some(expires_at) = report.token_expires_at_ms {
        text.push_str(&format!(
            "             cloud token expires {}\n",
            rfc3339(expires_at)
        ));
    }

    text.push_str("\nclients\n");
    if report.clients.is_empty() {
        text.push_str("  none readable on this machine\n");
    } else {
        for client in &report.clients {
            text.push_str(&format!(
                "  {:<8} {}\n           {}\n",
                client.client,
                ellipsize(&client.path, 88),
                client.detail
            ));
        }
    }

    text.push_str("\npublished tool counts\n  ");
    let counts: Vec<String> = report
        .tool_counts
        .iter()
        .filter_map(|row| {
            let profile = row.get("profile")?.as_str()?;
            let tools = row.get("tools")?.as_u64()?;
            Some(format!("{profile}={tools}"))
        })
        .collect();
    text.push_str(&counts.join(" · "));
    text.push_str("\n  agent is transport-gated (HTTP only)\n");
    text
}

/// `memnoth mcp add`.
pub fn render_add(report: &AddReport) -> String {
    let mut text = format!(
        "mcp add\n  endpoint   {}\n  profile    {}\n  dry-run    {}\n  token      {}\n",
        report.url,
        report.profile,
        if report.dry_run { "yes" } else { "no" },
        if report.token_used {
            "provided"
        } else {
            "none"
        }
    );
    if !report.token_used {
        text.push_str("           (set MEMNOTH_MCP_TOKEN or pass --token)\n");
    }
    if report.results.is_empty() {
        text.push_str("\n  no client was detected; pass --clients claude,cursor,codex,zed\n");
    } else {
        text.push_str("\nresults\n");
        for status in &report.results {
            let status_str = if status.ok { "ok" } else { "failed" };
            text.push_str(&format!("  {:<8} {}\n", status.client, status_str));
            if !status.path.is_empty() {
                text.push_str(&format!("           {}\n", status.path));
            }
            text.push_str(&format!("           {}\n", status.detail));
        }
    }
    if !report.snippets.is_empty() {
        text.push_str("\nmanual registration required:\n");
        for snippet in &report.snippets {
            text.push_str(&format!("  {}\n", snippet));
        }
    }
    if !report.dry_run && !report.results.is_empty() {
        text.push_str("\nrestart the client to pick up the new server\n");
    }
    text
}

/// `memnoth mcp call`.
pub fn render_call(_tool: &str, value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// `memnoth mcp selftest`.
pub fn render_selftest(report: &SelftestReport) -> String {
    let mut text = format!("mcp selftest\n  server     {}\n", report.server);
    text.push_str(&format!(
        "  result     {}\n",
        if report.ok { "passed" } else { "FAILED" }
    ));
    if let Some(code) = report.exit_code {
        text.push_str(&format!("  exit       {code}\n"));
    }
    let stdout = report.stdout.trim();
    let stderr = report.stderr.trim();
    if !stdout.is_empty() {
        text.push_str("\nstdout\n");
        for line in stdout.lines() {
            text.push_str(&format!("  {line}\n"));
        }
    }
    if !stderr.is_empty() {
        text.push_str("\nstderr\n");
        for line in stderr.lines() {
            text.push_str(&format!("  {line}\n"));
        }
    }
    text
}

/// `memnoth mcp remove`.
pub fn render_remove(report: &RemoveReport) -> String {
    let mut text = String::new();
    if report.results.is_empty() {
        text.push_str("no clients acted on\n");
    } else {
        for status in &report.results {
            let status_str = if status.ok { "ok" } else { "failed" };
            text.push_str(&format!("{}  {}", status.client, status_str));
            if !status.path.is_empty() {
                text.push_str(&format!("  ({})", status.path));
            }
            text.push_str(&format!("  {}\n", status.detail));
        }
    }
    if !report.snippets.is_empty() {
        text.push_str("\nmanual removal required:\n");
        for snippet in &report.snippets {
            text.push_str(&format!("  {}\n", snippet));
        }
    }
    text
}

/// `memnoth mcp list`.
pub fn render_list(report: &ListReport) -> String {
    let mut text = String::new();
    text.push_str("mcp\n");
    text.push_str(&format!("  endpoint   {}\n", report.url));
    text.push_str(&format!(
        "  token      {}\n",
        if report.token_present {
            "set"
        } else {
            "not set"
        }
    ));

    text.push_str("\nclients\n");
    if report.clients.is_empty() {
        text.push_str("  none readable on this machine\n");
    } else {
        for status in &report.clients {
            let status_str = if status.found {
                "configured"
            } else {
                "not configured"
            };
            text.push_str(&format!("  {}  {}\n", status.client, status_str));
            if !status.path.is_empty() {
                text.push_str(&format!("    {}\n", status.path));
            }
            if !status.detail.is_empty() {
                text.push_str(&format!("    {}\n", status.detail));
            }
        }
    }
    text
}

/// `memnoth mcp test`.
pub fn render_test(report: &TestReport) -> String {
    let mut text = String::new();
    text.push_str("mcp test\n");
    text.push_str(&format!("  endpoint   {}\n", report.url));
    text.push_str(&format!("  profile    {}\n", report.profile));
    text.push_str(&format!(
        "  tools/list {}\n",
        if report.tools_list_ok { "ok" } else { "failed" }
    ));
    text.push_str(&format!("  tool count {}\n", report.tool_count));
    if !report.tool_called.is_empty() {
        text.push_str(&format!(
            "  call {}    {}\n",
            report.tool_called,
            if report.call_ok { "ok" } else { "failed" }
        ));
    }

    // A failed probe that prints no reason is the worst of both: the server is
    // blamed and nothing says why. A tool-level failure carries its text, not an
    // `error` key, so both are read.
    if !report.call_ok
        && let Some(why) = report
            .call_result
            .get("error")
            .or_else(|| report.call_result.get("text"))
    {
        let why = match why {
            serde_json::Value::String(text) => text.trim().to_string(),
            other => other.to_string(),
        };
        text.push_str(&format!(
            "  reason     {}\n",
            why.replace('\n', "\n             ")
        ));
    }
    text
}

/// `memnoth mcp token`.
pub fn render_token(report: &TokenReport) -> String {
    let mut text = String::new();
    text.push_str("mcp token\n");
    text.push_str(&format!("  token       {}\n", ellipsize(&report.token, 24)));
    text.push_str(&format!("  type        {}\n", report.token_type));
    text.push_str(&format!("  expires in  {}s\n", report.expires_in));
    text.push_str(&format!("  mcp url     {}\n", report.mcp_url));
    text.push_str(&format!(
        "  expires at  {}\n",
        rfc3339(report.expires_at_ms)
    ));
    text
}

/// `memnoth mcp logs`.
pub fn render_logs(report: &LogsReport) -> String {
    let mut text = String::new();
    text.push_str(&format!(
        "mcp logs (profile: {}, url: {})\n",
        report.profile, report.url
    ));
    if report.sessions.is_empty() {
        text.push_str("  no sessions found\n");
    } else {
        for session in &report.sessions {
            text.push_str(&format!(
                "  session: {} (status: {})\n",
                session.session, session.status
            ));
            if session.events.is_empty() {
                text.push_str("    no events\n");
            } else {
                for event in &session.events {
                    text.push_str(&format!(
                        "    [{}] {}: {}\n",
                        event.at,
                        event.source,
                        event.transcript.as_deref().unwrap_or("")
                    ));
                }
            }
        }
    }
    text
}

/// `memnoth mcp profile show`.
pub fn render_profile_show(report: &ProfileShowReport) -> String {
    format!(
        "mcp profile: {} (source: {})\n",
        report.profile, report.source
    )
}

/// `memnoth mcp profile set`.
pub fn render_profile_set(report: &ProfileSetReport) -> String {
    format!("mcp profile set to: {}\n", report.profile)
}

/// `memnoth mcp exposes`.
pub fn render_exposes(report: &ExposesReport) -> String {
    let mut text = String::new();
    text.push_str("mcp exposes:\n");
    for p in &report.profiles {
        text.push_str(&format!(
            "  {}: {} tools ({} bytes)\n",
            p.profile, p.tool_count, p.tools_list_bytes
        ));
        for tool in &p.tools {
            text.push_str(&format!("    {}\n", tool));
        }
    }
    text
}

/// `memnoth mcp restart`.
pub fn render_restart(report: &RestartReport) -> String {
    let mut text = String::new();
    if report.success {
        text.push_str(&format!("mcp restart: {}\n", report.message));
    } else {
        text.push_str(&format!("mcp restart failed: {}\n", report.message));
    }
    if let Some(server) = &report.server {
        text.push_str(&format!("  server: {}\n", server));
    }
    text
}
