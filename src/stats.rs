//! `mn stats` — what this machine spent, on what, and when it did the work.
//!
//! The report is insight-first: a developer wants "what does a turn cost me and
//! when am I most expensive", not a reconciliation ledger with the interesting
//! numbers buried at the bottom. The cloud is the authority for org-wide spend,
//! the receipts under `~/.memnoth/sessions` are the authority for this machine
//! (cache reads, per-turn cost, the hours worked), and the two are compared in
//! one line rather than two tables.
//!
//! When the cloud is unreachable or this machine is not signed in, the local
//! receipts still answer the question. That is a state, not a failure: the old
//! version returned a sign-in error instead of the numbers it already had.

use std::collections::{BTreeSet, HashMap};

use anyhow::Result;
use serde_json::json;

use crate::AppContext;
use crate::output::Outcome;
use memnoth_cloud_client::{
    LocalReceipt, LocalTotals, Reconciliation, UsageDelta, UsageParams, UsageResponse, UsageTotals,
};
use memnoth_session::Session;

const DAY_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;

/// Execute `mn stats`.
pub fn run(ctx: &AppContext, args: &crate::cli::StatsArgs) -> Result<Outcome> {
    let params = UsageParams {
        range: Some(args.range.clone()),
        by: args.by.clone(),
        since: args.since.clone(),
    };

    let sessions = count_session_files(ctx);
    let local = insights(&load_local_receipts(ctx)?, sessions);
    // Per-project spend is read from the session headers, not from today's
    // config: a receipt belongs to the project it was spent under. Historical
    // sessions written before the header carried a project are "unassigned",
    // which is a state the console already shows rather than a guess.
    let projects = project_spend(ctx)?;

    // Offline is a normal state on a laptop. The reason is kept, because "no
    // cloud" and "not signed in" are different fixes.
    let mut offline: Option<String> = None;
    let mut cloud: Option<(UsageResponse, Reconciliation)> = None;
    match crate::cloud::signed_in_client(ctx) {
        Ok(client) => {
            let usage = client.usage(params.clone());
            let reconciliation = client.reconcile(params, load_local_receipts(ctx)?);
            match (usage, reconciliation) {
                (Ok(usage), Ok(reconciliation)) => cloud = Some((usage, reconciliation)),
                (Err(error), _) | (_, Err(error)) => offline = Some(format!("{error}")),
            }
        }
        Err(error) => offline = Some(format!("{error}")),
    }

    let data = match &cloud {
        Some((usage, reconciliation)) => json!({
            "range": usage.range,
            "demo": usage.demo,
            "totals": usage.totals,
            "series": usage.series,
            "leaderboard": usage.leaderboard,
            "models": usage.models,
            "reconciliation": reconciliation,
            "insights": insight_json(&local),
            "projects": project_rows_json(&projects),
        }),
        None => json!({
            "range": "local",
            "demo": false,
            "totals": local_totals_json(&local),
            "series": [],
            "leaderboard": [],
            "models": local_models_json(&local),
            "reconciliation": local_only(&local),
            "insights": insight_json(&local),
            "projects": project_rows_json(&projects),
            "offline": offline.as_deref(),
        }),
    };

    let mut text = if args.json {
        serde_json::to_string_pretty(&data)?
    } else {
        render(&local, cloud.as_ref(), offline.as_deref())
    };
    if !args.json && !projects.is_empty() {
        text.push_str(&render_projects(&projects));
    }

    Ok(Outcome::new("stats.show", text, data))
}

// ── local receipts ───────────────────────────────────────────────────────

/// What the receipts on this machine say, in the shapes the report asks for.
#[derive(Debug, Default)]
struct Insights {
    turns: usize,
    sessions: usize,
    tokens_in: i64,
    tokens_out: i64,
    cost_usd: f64,
    cache_read: i64,
    cache_write: i64,
    /// `(days ago, tokens, turns)` for the busiest day.
    busiest: Option<(i64, i64, usize)>,
    /// `(hour UTC, turns)` for the busiest hour.
    peak_hour: Option<(u32, usize)>,
    /// Consecutive days with at least one turn, ending at the last active day.
    streak: usize,
    /// `(model, tokens, turns)`, most used first.
    models: Vec<(String, i64, usize)>,
}

fn insights(receipts: &[LocalReceipt], sessions: usize) -> Insights {
    let mut out = Insights {
        sessions,
        ..Default::default()
    };
    if receipts.is_empty() {
        return out;
    }

    let today = memnoth_engine::now_ms().div_euclid(DAY_MS);
    let mut days: HashMap<i64, (i64, usize)> = HashMap::new();
    let mut hours = [0usize; 24];
    let mut models: HashMap<String, (i64, usize)> = HashMap::new();
    let mut active_days = BTreeSet::new();

    for receipt in receipts {
        let tokens = receipt.total_tokens as i64;
        out.turns += 1;
        out.tokens_in += receipt.input_tokens as i64;
        out.tokens_out += receipt.output_tokens as i64;
        out.cache_read += receipt.cache_read_tokens as i64;
        out.cache_write += receipt.cache_write_tokens as i64;
        // An unpriced model reports no cost rather than a zero: the sum stays a
        // sum of what was actually priced.
        out.cost_usd += receipt.cost_micros.unwrap_or(0) as f64 / 1_000_000.0;

        let day = receipt.at_ms.div_euclid(DAY_MS);
        active_days.insert(day);
        let bucket = days.entry(day).or_insert((0, 0));
        bucket.0 += tokens;
        bucket.1 += 1;

        let hour = receipt.at_ms.rem_euclid(DAY_MS) / HOUR_MS;
        hours[hour as usize] += 1;

        let model = if receipt.model.trim().is_empty() {
            "unknown".to_string()
        } else {
            receipt.model.clone()
        };
        let entry = models.entry(model).or_insert((0, 0));
        entry.0 += tokens;
        entry.1 += 1;
    }

    out.busiest = days
        .iter()
        .max_by_key(|(_, (tokens, turns))| (*tokens, *turns))
        .map(|(day, (tokens, turns))| (today - day, *tokens, *turns));

    out.peak_hour = hours
        .iter()
        .enumerate()
        .max_by_key(|(hour, turns)| (**turns, 24 - *hour))
        .filter(|(_, turns)| **turns > 0)
        .map(|(hour, turns)| (hour as u32, *turns));

    // A streak counted from the last active day reads as "how long have I been
    // on this", which is the interesting question; counting from today would
    // reset it every morning before the first turn.
    let mut streak = 0usize;
    if let Some(last) = active_days.iter().next_back().copied() {
        let mut day = last;
        while active_days.contains(&day) {
            streak += 1;
            day -= 1;
        }
    }
    out.streak = streak;

    let mut models: Vec<(String, (i64, usize))> = models.into_iter().collect();
    models.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(b.1.1.cmp(&a.1.1)));
    out.models = models
        .into_iter()
        .map(|(model, (tokens, turns))| (model, tokens, turns))
        .collect();

    out
}

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
            && let Ok(session) = Session::load(&path)
        {
            for receipt in session.receipts {
                receipts.push(LocalReceipt {
                    at_ms: receipt.at_ms,
                    total_tokens: receipt.total_tokens,
                    input_tokens: receipt.input_tokens,
                    output_tokens: receipt.output_tokens,
                    cache_read_tokens: receipt.cache_read_tokens,
                    cache_write_tokens: receipt.cache_write_tokens,
                    cost_micros: receipt.cost_micros,
                    model: session.model.clone(),
                });
            }
        }
    }
    Ok(receipts)
}

/// Spend grouped by the project a session recorded when it started.
#[derive(Debug, Default)]
struct ProjectSpend {
    project: String,
    tokens: i64,
    turns: usize,
    cost_usd: f64,
}

fn project_spend(ctx: &AppContext) -> Result<Vec<ProjectSpend>> {
    let Ok(entries) = std::fs::read_dir(&ctx.sessions_dir) else {
        return Ok(Vec::new());
    };
    let mut by_project: HashMap<String, ProjectSpend> = HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(session) = Session::load(&path) else {
            continue;
        };
        let project = session
            .project_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "unassigned".to_string());
        let row = by_project
            .entry(project.clone())
            .or_insert_with(|| ProjectSpend {
                project,
                ..Default::default()
            });
        for receipt in &session.receipts {
            row.tokens += receipt.total_tokens as i64;
            row.turns += 1;
            row.cost_usd += receipt.cost_micros.unwrap_or(0) as f64 / 1_000_000.0;
        }
    }
    let mut rows: Vec<ProjectSpend> = by_project
        .into_values()
        .filter(|row| row.turns > 0)
        .collect();
    rows.sort_by(|a, b| {
        b.cost_usd
            .total_cmp(&a.cost_usd)
            .then(b.tokens.cmp(&a.tokens))
            .then(a.project.cmp(&b.project))
    });
    Ok(rows)
}

fn project_rows_json(rows: &[ProjectSpend]) -> serde_json::Value {
    json!(
        rows.iter()
            .map(|row| json!({
                "project": row.project,
                "tokens": row.tokens,
                "turns": row.turns,
                "cost_usd": row.cost_usd,
            }))
            .collect::<Vec<_>>()
    )
}

/// The project block, appended to the human report. Attribution is visibility,
/// not savings — the heading says "spend", never "saved".
fn render_projects(rows: &[ProjectSpend]) -> String {
    let mut out = vec![String::new(), "projects".to_string()];
    for row in rows.iter().take(8) {
        out.push(format!(
            "  {:<28} {:>4} turn(s)  {:>8}  {}",
            clip(&row.project, 28),
            row.turns,
            compact(row.tokens),
            money(row.cost_usd)
        ));
    }
    if rows.len() > 8 {
        out.push(format!("  … {} more", rows.len() - 8));
    }
    out.join("\n")
}

fn count_session_files(ctx: &AppContext) -> usize {
    let Ok(entries) = std::fs::read_dir(&ctx.sessions_dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .count()
}

// ── rendering ────────────────────────────────────────────────────────────

fn render(
    local: &Insights,
    cloud: Option<&(UsageResponse, Reconciliation)>,
    offline: Option<&str>,
) -> String {
    let mut out: Vec<String> = Vec::new();

    // The cloud is the org's number and the receipts are this machine's. The
    // headline prefers the cloud when it answered, because the dashboard shows
    // the same figure and two surfaces disagreeing is worse than two scopes.
    let cloud_totals = cloud.map(|(usage, _)| &usage.totals);
    let turns = cloud_totals.map(|t| t.tasks).unwrap_or(local.turns as i64);
    let tokens_in = cloud_totals.map(|t| t.tokens_in).unwrap_or(local.tokens_in);
    let tokens_out = cloud_totals
        .map(|t| t.tokens_out)
        .unwrap_or(local.tokens_out);
    let tokens = tokens_in + tokens_out;

    let range = cloud
        .map(|(usage, _)| usage.range.as_str())
        .unwrap_or("local");
    out.push(format!(
        "stats · {range} · {} turns · {} tokens",
        thousands(turns),
        compact(tokens)
    ));
    if let Some(note) = offline {
        out.push(format!("  cloud unavailable — {}", first_line(note)));
        out.push("  the numbers below are the receipts on this machine".to_string());
    }
    if cloud.map(|(usage, _)| usage.demo).unwrap_or(false) {
        out.push("  demo data".to_string());
    }
    out.push(String::new());

    let (cost, from_cloud) = match cloud {
        Some((usage, _)) => (usage.totals.cost_usd, true),
        None => (local.cost_usd, false),
    };
    out.push("spend".to_string());
    out.push(format!(
        "  {} total{}",
        money(cost),
        if from_cloud { "" } else { " (this machine)" }
    ));
    if turns > 0 {
        out.push(format!("  {} per turn", money(cost / turns as f64)));
        if local.sessions > 0 {
            out.push(format!(
                "  across {} sessions on this machine",
                local.sessions
            ));
        }
    }

    out.push(String::new());
    out.push("tokens".to_string());
    out.push(format!(
        "  {} in · {} out · {} per turn",
        compact(tokens_in),
        compact(tokens_out),
        compact(if turns == 0 { 0 } else { tokens / turns })
    ));
    if local.turns == 0 {
        out.push(
            "  no local receipts yet — cache and rhythm fill in after a turn here".to_string(),
        );
    } else if local.cache_read + local.cache_write > 0 {
        let billed = local.tokens_in + local.cache_read;
        let share = if billed == 0 {
            0
        } else {
            local.cache_read * 100 / billed
        };
        out.push(format!(
            "  cache {share}% of input · {} read · {} written",
            compact(local.cache_read),
            compact(local.cache_write)
        ));
    } else {
        out.push("  cache no reads yet — long turns are paying full price".to_string());
    }

    if let Some((ago, day_tokens, day_turns)) = local.busiest {
        out.push(String::new());
        out.push("rhythm".to_string());
        out.push(format!(
            "  busiest day {} · {} · {} turn(s)",
            day_label(ago),
            compact(day_tokens),
            day_turns
        ));
        if let Some((hour, hour_turns)) = local.peak_hour {
            out.push(format!(
                "  peak hour {hour:02}:00–{:02}:00 UTC · {hour_turns} turn(s)",
                (hour + 1) % 24
            ));
        }
        if local.streak > 1 {
            out.push(format!("  streak {} days in a row", local.streak));
        }
    }

    let models: Vec<(String, i64)> = match cloud {
        Some((usage, _)) if !usage.models.is_empty() => usage
            .models
            .iter()
            .map(|row| {
                // Providers that already prefix the id (`kilo/...` with
                // provider `kilo`) read twice when both are printed.
                let name = if row.provider.is_empty() || row.model.starts_with(&row.provider) {
                    row.model.clone()
                } else {
                    format!("{}/{}", row.provider, row.model)
                };
                (name, row.tokens)
            })
            .collect(),
        _ => local
            .models
            .iter()
            .map(|(model, tokens, _)| (model.clone(), *tokens))
            .collect(),
    };
    if !models.is_empty() {
        let total: i64 = models.iter().map(|(_, value)| *value).sum();
        out.push(String::new());
        out.push("models".to_string());
        for (model, value) in models.iter().take(3) {
            let share = if total == 0 { 0 } else { value * 100 / total };
            out.push(format!(
                "  {:<30} {:>3}%  {}",
                clip(model, 30),
                share,
                compact(*value)
            ));
        }
    }

    if let Some((usage, reconciliation)) = cloud {
        out.push(String::new());
        out.push("cloud vs this machine".to_string());
        out.push(format!(
            "  cloud {} tasks · {} tokens · {}",
            thousands(usage.totals.tasks),
            compact(usage.totals.tokens_in + usage.totals.tokens_out),
            money(usage.totals.cost_usd)
        ));
        out.push(format!(
            "  this machine {} turns · {} receipts matched",
            local.turns, reconciliation.matched
        ));
        out.push(format!(
            "  delta {} tokens · {}",
            signed(reconciliation.delta.tokens_in + reconciliation.delta.tokens_out),
            money(reconciliation.delta.cost_usd)
        ));
    }

    out.join("\n")
}

fn local_totals_json(local: &Insights) -> UsageTotals {
    UsageTotals {
        tokens_in: local.tokens_in,
        tokens_out: local.tokens_out,
        tasks: local.turns as i64,
        cost_usd: local.cost_usd,
    }
}

fn local_models_json(local: &Insights) -> serde_json::Value {
    json!(
        local
            .models
            .iter()
            .map(|(model, tokens, turns)| json!({
                "provider": "",
                "model": model,
                "tokens": tokens,
                "cost_usd": 0.0,
                "turns": turns,
            }))
            .collect::<Vec<_>>()
    )
}

fn local_only(local: &Insights) -> Reconciliation {
    Reconciliation {
        cloud_totals: local_totals_json(local),
        local_totals: LocalTotals {
            tokens_in: local.tokens_in.max(0) as u64,
            tokens_out: local.tokens_out.max(0) as u64,
            tasks: local.turns,
            cost_usd: local.cost_usd,
        },
        delta: UsageDelta {
            tokens_in: local.tokens_in,
            tokens_out: local.tokens_out,
            tasks: local.turns as i64,
            cost_usd: local.cost_usd,
        },
        matched: 0,
        unmatched_cloud: 0,
        unmatched_local: local.turns,
    }
}

fn insight_json(local: &Insights) -> serde_json::Value {
    json!({
        "turns": local.turns,
        "sessions": local.sessions,
        "tokens_in": local.tokens_in,
        "tokens_out": local.tokens_out,
        "cache_read": local.cache_read,
        "cache_write": local.cache_write,
        "cost_usd": local.cost_usd,
        "cost_per_turn_usd": if local.turns == 0 { 0.0 } else { local.cost_usd / local.turns as f64 },
        "busiest": local.busiest.map(|(ago, tokens, turns)| json!({
            "days_ago": ago, "tokens": tokens, "turns": turns
        })),
        "peak_hour_utc": local.peak_hour.map(|(hour, turns)| json!({
            "hour": hour, "turns": turns
        })),
        "streak_days": local.streak,
        "models": local.models.iter().map(|(model, tokens, turns)| json!({
            "model": model, "tokens": tokens, "turns": turns
        })).collect::<Vec<_>>(),
    })
}

fn money(value: f64) -> String {
    if value != 0.0 && value.abs() < 0.01 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}

fn thousands(value: i64) -> String {
    let value = value.max(0);
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn compact(value: i64) -> String {
    let value = value.max(0);
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn signed(value: i64) -> String {
    if value > 0 {
        format!("+{}", compact(value))
    } else if value < 0 {
        format!("-{}", compact(-value))
    } else {
        "0".to_string()
    }
}

fn day_label(days_ago: i64) -> String {
    match days_ago {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        n if n < 0 => "in the future".to_string(),
        n => format!("{n} days ago"),
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(
        at_ms: i64,
        tokens_in: u64,
        tokens_out: u64,
        cache_read: u64,
        micros: u64,
        model: &str,
    ) -> LocalReceipt {
        LocalReceipt {
            at_ms,
            total_tokens: (tokens_in + tokens_out) as usize,
            input_tokens: tokens_in,
            output_tokens: tokens_out,
            cache_read_tokens: cache_read,
            cache_write_tokens: 0,
            cost_micros: Some(micros),
            model: model.to_string(),
        }
    }

    #[test]
    fn insight_counts_and_per_turn_cost_are_measured_from_receipts() {
        let now = memnoth_engine::now_ms();
        let receipts = vec![
            receipt(now, 1_000, 500, 750, 2_000, "kilo/reflection"),
            receipt(now - DAY_MS, 3_000, 1_000, 0, 4_000, "kilo/reflection"),
        ];
        let local = insights(&receipts, 1);
        assert_eq!(local.turns, 2);
        assert_eq!(local.tokens_in, 4_000);
        assert_eq!(local.tokens_out, 1_500);
        assert_eq!(local.cache_read, 750);
        assert!((local.cost_usd - 0.006).abs() < 1e-9);
        // The heavier day is yesterday, not the turn that just ran.
        assert_eq!(local.busiest.map(|(ago, _, _)| ago), Some(1));
        assert_eq!(local.streak, 2, "two consecutive days is a two-day streak");
        assert_eq!(local.models[0].0, "kilo/reflection");
    }

    #[test]
    fn the_cache_share_is_of_what_was_billed_not_of_the_output() {
        let now = memnoth_engine::now_ms();
        let receipts = vec![receipt(now, 1_000, 200, 3_000, 0, "m")];
        let local = insights(&receipts, 1);
        let billed = local.tokens_in + local.cache_read;
        assert_eq!(local.cache_read * 100 / billed, 75);
    }

    #[test]
    fn a_report_with_no_receipts_says_zero_without_inventing_a_rhythm() {
        let local = insights(&[], 0);
        let text = render(&local, None, Some("not signed in"));
        assert!(text.contains("0 turns"), "{text}");
        assert!(text.contains("cloud unavailable"), "{text}");
        assert!(!text.contains("busiest day"), "{text}");
        assert!(!text.contains("peak hour"), "{text}");
    }

    #[test]
    fn offline_still_reconciles_against_this_machine() {
        let local = insights(&[], 0);
        let reconciliation = local_only(&local);
        assert_eq!(reconciliation.unmatched_local, 0);
        assert_eq!(reconciliation.matched, 0);
    }

    #[test]
    fn project_spend_is_rendered_as_visibility_not_savings() {
        let rows = vec![
            ProjectSpend {
                project: "payments".into(),
                tokens: 12_500,
                turns: 3,
                cost_usd: 0.42,
            },
            ProjectSpend {
                project: "unassigned".into(),
                tokens: 900,
                turns: 1,
                cost_usd: 0.03,
            },
        ];
        let text = render_projects(&rows);
        assert!(text.contains("projects"), "{text}");
        assert!(text.contains("payments"), "{text}");
        assert!(text.contains("12.5k"), "{text}");
        assert!(
            !text.contains("saved"),
            "attribution must not claim savings"
        );
        let rows = project_rows_json(&rows);
        assert_eq!(rows[0]["project"], "payments");
        assert_eq!(rows[0]["turns"], 3);
    }

    #[test]
    fn compact_keeps_small_numbers_exact_and_rounds_the_rest() {
        assert_eq!(compact(950), "950");
        assert_eq!(compact(1_500), "1.5k");
        assert_eq!(compact(1_240_000), "1.2M");
        assert_eq!(thousands(12_624), "12,624");
    }
}
