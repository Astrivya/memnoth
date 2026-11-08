//! `mn usage` — show usage and costs, with local reconciliation.
use anyhow::Result;
use serde_json::json;
use std::path::PathBuf;

use crate::AppContext;
use crate::output::Outcome;
use memnoth_cloud_client::{LocalReceipt, UsageParams};
use memnoth_session::{Session, TurnReceipt};

/// Execute `mn usage`.
pub fn run(ctx: &AppContext, args: &crate::cli::UsageArgs) -> Result<Outcome> {
    // The one client every command that needs an account uses: it reads the
    // token this machine stored at `/login`, not only the environment, so
    // `mn usage` works from the same sign-in the session already has.
    let client = crate::cloud::signed_in_client(ctx)?;

    // Build usage params
    let params = UsageParams {
        range: Some(args.range.clone()),
        by: args.by.clone(),
        since: args.since.clone(),
    };

    // Fetch cloud usage
    let cloud = client.usage(params.clone())?;

    // Load local receipts for reconciliation
    let local_receipts = load_local_receipts(ctx)?;
    let reconciliation = client.reconcile(params, local_receipts)?;

    let text = if args.json {
        serde_json::to_string_pretty(&reconciliation)?
    } else {
        render_usage(&cloud, &reconciliation)
    };

    let data = json!({
        "range": cloud.range,
        "demo": cloud.demo,
        "totals": cloud.totals,
        "series": cloud.series,
        "leaderboard": cloud.leaderboard,
        "models": cloud.models,
        "reconciliation": reconciliation,
    });

    Ok(Outcome::new("usage.show", text, data))
}

/// Load local receipts from session files.
fn load_local_receipts(ctx: &AppContext) -> Result<Vec<LocalReceipt>> {
    let mut receipts = Vec::new();

    let sessions_dir = &ctx.sessions_dir;
    if !sessions_dir.exists() {
        return Ok(receipts);
    }

    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("jsonl")
            && let Ok(session) = load_session(&path)
        {
            for r in session.receipts {
                receipts.push(LocalReceipt {
                    at_ms: r.at_ms,
                    total_tokens: r.total_tokens,
                    input_tokens: r.input_tokens,
                    output_tokens: r.output_tokens,
                    cache_read_tokens: r.cache_read_tokens,
                    cache_write_tokens: r.cache_write_tokens,
                    cost_micros: r.cost_micros,
                    model: session.model.clone(),
                });
            }
        }
    }

    Ok(receipts)
}

/// Load a session from a JSONL file.
fn load_session(path: &PathBuf) -> Result<Session> {
    let content = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        anyhow::bail!("empty session file");
    }

    // Parse header
    let header: memnoth_session::Header = serde_json::from_str(lines[0])?;
    if header.version != memnoth_session::FORMAT_VERSION {
        anyhow::bail!("unsupported session format version {}", header.version);
    }

    let mut session = Session {
        id: header.id,
        model: header.model,
        created_ms: header.created_ms,
        messages: Vec::new(),
        receipts: Vec::new(),
        writes: Vec::new(),
        checkpoints: Vec::new(),
        approvals: Vec::new(),
        input_tokens: 0,
        output_tokens: 0,
        project_id: header.project_id.clone(),
    };

    // Parse messages and receipts
    for line in &lines[1..] {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line)?;
        if let Some(kind) = value.get("kind").and_then(|v| v.as_str()) {
            match kind {
                "message" => {
                    if let Ok(msg) =
                        serde_json::from_value::<memnoth_session::Message>(value.clone())
                    {
                        session.messages.push(msg);
                    }
                }
                "receipt" => {
                    if let Ok(rcpt) = serde_json::from_value::<TurnReceipt>(value.clone()) {
                        session.receipts.push(rcpt);
                    }
                }
                "write" => {
                    if let Ok(write) =
                        serde_json::from_value::<memnoth_session::WriteReceipt>(value.clone())
                    {
                        session.writes.push(write);
                    }
                }
                "checkpoint" => {
                    if let Ok(cp) =
                        serde_json::from_value::<memnoth_session::Checkpoint>(value.clone())
                    {
                        session.checkpoints.push(cp);
                    }
                }
                _ => {}
            }
        }
    }

    Ok(session)
}

/// Render usage for human consumption.
fn render_usage(
    cloud: &memnoth_cloud_client::UsageResponse,
    reconciliation: &memnoth_cloud_client::Reconciliation,
) -> String {
    let mut out = Vec::new();

    out.push(format!("Usage: {}", cloud.range));
    if cloud.demo {
        out.push("  (demo data)".to_string());
    }
    out.push("".to_string());

    // Totals
    out.push("Totals:".to_string());
    out.push(format!(
        "  Input tokens:      {:>15}",
        format_number(cloud.totals.tokens_in)
    ));
    out.push(format!(
        "  Output tokens:     {:>15}",
        format_number(cloud.totals.tokens_out)
    ));
    out.push(format!(
        "  Tasks (turns):     {:>15}",
        format_number(cloud.totals.tasks)
    ));
    out.push(format!(
        "  Cost:              ${:>14.4}",
        cloud.totals.cost_usd
    ));
    out.push("".to_string());

    // Reconciliation
    out.push("Reconciliation (cloud vs local):".to_string());
    out.push(format!(
        "  Cloud input tokens:   {:>15}",
        format_number(cloud.totals.tokens_in)
    ));
    out.push(format!(
        "  Local input tokens:   {:>15}",
        format_number(reconciliation.local_totals.tokens_in as i64)
    ));
    out.push(format!(
        "  Delta input tokens:   {:>15}",
        format_number(reconciliation.delta.tokens_in)
    ));
    out.push("".to_string());
    out.push(format!(
        "  Cloud output tokens:  {:>15}",
        format_number(cloud.totals.tokens_out)
    ));
    out.push(format!(
        "  Local output tokens:  {:>15}",
        format_number(reconciliation.local_totals.tokens_out as i64)
    ));
    out.push(format!(
        "  Delta output tokens:  {:>15}",
        format_number(reconciliation.delta.tokens_out)
    ));
    out.push("".to_string());
    out.push(format!(
        "  Cloud tasks:          {:>15}",
        format_number(cloud.totals.tasks)
    ));
    out.push(format!(
        "  Local tasks:          {:>15}",
        format_number(reconciliation.local_totals.tasks as i64)
    ));
    out.push(format!(
        "  Delta tasks:          {:>15}",
        format_number(reconciliation.delta.tasks)
    ));
    out.push("".to_string());
    out.push(format!(
        "  Cloud cost:           ${:>14.4}",
        cloud.totals.cost_usd
    ));
    out.push(format!(
        "  Local cost:           ${:>14.4}",
        reconciliation.local_totals.cost_usd
    ));
    out.push(format!(
        "  Delta cost:           ${:>14.4}",
        reconciliation.delta.cost_usd
    ));
    out.push("".to_string());
    out.push(format!(
        "  Matched:              {}",
        reconciliation.matched
    ));
    out.push(format!(
        "  Unmatched (cloud):    {}",
        reconciliation.unmatched_cloud
    ));
    out.push(format!(
        "  Unmatched (local):    {}",
        reconciliation.unmatched_local
    ));
    out.push("".to_string());

    // Series (by day)
    if !cloud.series.is_empty() {
        out.push("Daily series:".to_string());
        out.push("  Date        Input       Output      Cost      Tasks".to_string());
        out.push("  ──────────  ─────────  ─────────  ───────  ──────".to_string());
        for p in &cloud.series {
            out.push(format!(
                "  {:<10}  {:>9}  {:>9}  ${:>6.2}  {:>6}",
                p.day,
                format_number(p.tokens_in),
                format_number(p.tokens_out),
                p.cost_usd,
                p.tasks
            ));
        }
        out.push("".to_string());
    }

    // By model breakdown
    if !cloud.models.is_empty() {
        out.push("By model:".to_string());
        out.push("  Provider          Model                    Tokens      Cost".to_string());
        out.push("  ────────────────  ──────────────────────  ─────────  ───────".to_string());
        for m in &cloud.models {
            out.push(format!(
                "  {:<16}  {:<22}  {:>9}  ${:>6.2}",
                truncate(&m.provider, 16),
                truncate(&m.model, 22),
                format_number(m.tokens),
                m.cost_usd
            ));
        }
        out.push("".to_string());
    }

    // Leaderboard
    if !cloud.leaderboard.is_empty() {
        out.push("Top users:".to_string());
        out.push(
            "  Email                    Name              Spend      Tokens  Tasks".to_string(),
        );
        out.push("  ──────────────────────  ────────────────  ───────  ───────  ─────".to_string());
        for l in &cloud.leaderboard {
            out.push(format!(
                "  {:<22}  {:<16}  ${:>6.2}  {:>7}  {:>5}",
                truncate(&l.email, 22),
                truncate(&l.name, 16),
                l.spend_usd,
                format_number(l.tokens),
                l.tasks
            ));
        }
    }

    out.join("\n")
}

fn format_number(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Truncate to `max` display columns, ending in `…` when it had to cut.
///
/// The ellipsis is inside the budget, so a truncated value never overflows the
/// column it is drawn in, and cutting on a character boundary cannot panic on
/// multi-byte text.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(500), "500");
        assert_eq!(format_number(1_500), "1.5k");
        assert_eq!(format_number(1_500_000), "1.5M");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this is a very long string", 10), "this is a…");
    }
}
