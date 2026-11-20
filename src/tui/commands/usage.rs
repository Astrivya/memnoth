//! `/usage` — measured session and provider usage, with explicit unknowns.

use std::collections::BTreeMap;

use memnoth_session::{Role, Session, TurnReceipt};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct Measurement<T> {
    status: &'static str,
    value: Option<T>,
    reported_turns: usize,
    total_turns: usize,
}

#[derive(Debug, Serialize)]
struct ModelUsage {
    provider: Option<String>,
    model: Option<String>,
    turns: usize,
    input_tokens: Measurement<u64>,
    output_tokens: Measurement<u64>,
    reasoning_output_tokens: Measurement<u64>,
    cache_read_tokens: Measurement<u64>,
    cache_write_tokens: Measurement<u64>,
    efforts: Vec<String>,
}

#[derive(Debug, Serialize)]
struct UsageSnapshot {
    session_id: String,
    session_model: String,
    active_provider: Option<String>,
    active_model: String,
    session_age_ms: u64,
    messages: MessageCounts,
    completed_turns: usize,
    latest_turn: Option<TurnUsage>,
    provider_usage: UsageCoverage,
    input_tokens: Measurement<u64>,
    output_tokens: Measurement<u64>,
    reasoning_output_tokens: Measurement<u64>,
    cache_read_tokens: Measurement<u64>,
    cache_write_tokens: Measurement<u64>,
    provider_requests: Measurement<u64>,
    tool_calls: Measurement<u64>,
    latency: Latency,
    models: Vec<ModelUsage>,
    context_window: ContextWindow,
    next_turn_effort: String,
    cost: Cost,
    rate_limits: ProviderRateLimits,
    assembled_prompt: AssembledPrompt,
}

#[derive(Debug, Serialize)]
struct ProviderRateLimits {
    status: &'static str,
    detail: &'static str,
}

#[derive(Debug, Serialize)]
struct UsageCoverage {
    status: &'static str,
    reported_turns: usize,
    total_turns: usize,
}

#[derive(Debug, Serialize)]
struct TurnUsage {
    turn_id: Option<usize>,
    completed_at_ms: i64,
    provider: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    input_tokens: Measurement<u64>,
    output_tokens: Measurement<u64>,
    reasoning_output_tokens: Measurement<u64>,
    cache_read_tokens: Measurement<u64>,
    cache_write_tokens: Measurement<u64>,
    provider_requests: Measurement<u64>,
    tool_calls: Measurement<u64>,
    latency_ms: Measurement<u64>,
    context_occupancy_tokens: Measurement<u64>,
    cost_estimate_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
struct MessageCounts {
    user: usize,
    assistant: usize,
    tool: usize,
    system: usize,
}

#[derive(Debug, Serialize)]
struct Latency {
    status: &'static str,
    measured_turns: usize,
    average_ms: Option<u64>,
    median_ms: Option<u64>,
    p95_ms: Option<u64>,
    last_ms: Option<u64>,
    output_tokens_per_second_wall_time: Option<f64>,
}

#[derive(Debug, Serialize)]
struct ContextWindow {
    model: Option<String>,
    provider: Option<String>,
    occupancy_tokens: Option<u64>,
    limit_tokens: Option<u64>,
    remaining_tokens: Option<u64>,
    over_limit_tokens: Option<u64>,
    limit_source: Option<String>,
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct Cost {
    status: &'static str,
    estimate_usd: Option<f64>,
    priced_turns: usize,
    total_turns: usize,
    source: &'static str,
}

#[derive(Debug, Serialize)]
struct AssembledPrompt {
    status: &'static str,
    tokens: Measurement<u64>,
    global_tokens: Measurement<u64>,
    project_tokens: Measurement<u64>,
    session_tokens: Measurement<u64>,
    turn_tokens: Measurement<u64>,
    pieces: Measurement<u64>,
}

/// Render the same snapshot as readable text or machine-readable JSON.
pub(crate) fn render(
    session: &Session,
    context_window_size: Option<u64>,
    context_model: &str,
    context_provider: &str,
    context_window_source: Option<&str>,
    next_turn_effort: Option<&str>,
    as_json: bool,
) -> String {
    let snapshot = snapshot(
        session,
        context_window_size,
        context_model,
        context_provider,
        context_window_source,
        next_turn_effort,
    );
    if as_json {
        return serde_json::to_string_pretty(&snapshot)
            .unwrap_or_else(|error| format!("{{\"error\":\"cannot encode usage: {error}\"}}"));
    }
    render_text(&snapshot)
}

fn snapshot(
    session: &Session,
    context_window_size: Option<u64>,
    context_model: &str,
    context_provider: &str,
    context_window_source: Option<&str>,
    next_turn_effort: Option<&str>,
) -> UsageSnapshot {
    let receipts = &session.receipts;
    let total_turns = receipts.len();
    let receipt_refs = receipts.iter().collect::<Vec<_>>();
    let usage_receipts = receipts
        .iter()
        .filter(|receipt| usage_was_reported(receipt))
        .collect::<Vec<_>>();
    let input_receipts = receipts
        .iter()
        .filter(|receipt| input_was_reported(receipt))
        .collect::<Vec<_>>();
    let output_receipts = receipts
        .iter()
        .filter(|receipt| output_was_reported(receipt))
        .collect::<Vec<_>>();
    let input_sum = sum(input_receipts.iter().map(|receipt| receipt.input_tokens));
    let output_sum = sum(output_receipts.iter().map(|receipt| receipt.output_tokens));
    let usage_coverage = UsageCoverage {
        status: if usage_receipts.is_empty() {
            "not reported"
        } else if usage_receipts.len() == total_turns {
            "reported"
        } else {
            "partially reported"
        },
        reported_turns: usage_receipts.len(),
        total_turns,
    };
    let mut models = BTreeMap::<(Option<String>, Option<String>), Vec<&TurnReceipt>>::new();
    for receipt in receipts {
        models
            .entry((
                receipt.provider_id.clone(),
                receipt.context_window_model.clone(),
            ))
            .or_default()
            .push(receipt);
    }
    let models = models
        .into_iter()
        .map(|((provider, model), rows)| model_usage(provider, model, &rows))
        .collect();

    let latest = receipts.last();
    let context_matches = latest.is_some_and(|receipt| {
        receipt.context_window_model.as_deref() == Some(context_model)
            && receipt
                .context_window_provider
                .as_deref()
                .is_some_and(|provider| provider.eq_ignore_ascii_case(context_provider))
    });
    let limit_tokens = context_matches.then_some(context_window_size).flatten();
    let limit_source = limit_tokens.and(context_window_source.map(str::to_owned));
    let occupancy_tokens = latest.and_then(|receipt| receipt.context_window_used);
    let context_window = ContextWindow {
        model: latest.and_then(|receipt| receipt.context_window_model.clone()),
        provider: latest.and_then(|receipt| receipt.provider_id.clone()),
        occupancy_tokens,
        limit_tokens,
        remaining_tokens: occupancy_tokens
            .zip(limit_tokens)
            .map(|(used, limit)| limit.saturating_sub(used)),
        over_limit_tokens: occupancy_tokens
            .zip(limit_tokens)
            .and_then(|(used, limit)| (used > limit).then(|| used - limit)),
        limit_source,
        status: if latest
            .and_then(|receipt| receipt.context_window_used)
            .is_none()
        {
            "provider did not report request occupancy"
        } else if limit_tokens.is_some() {
            "measured occupancy and matched model limit"
        } else {
            "occupancy reported; matching model limit unavailable"
        },
    };

    let durations = receipts
        .iter()
        .filter_map(|receipt| receipt.duration_ms)
        .collect::<Vec<_>>();
    let throughput_samples = receipts
        .iter()
        .filter(|receipt| output_was_reported(receipt) && receipt.duration_ms.is_some())
        .collect::<Vec<_>>();
    let output_for_throughput = sum(throughput_samples
        .iter()
        .map(|receipt| receipt.output_tokens));
    let elapsed_for_throughput = sum(throughput_samples
        .iter()
        .filter_map(|receipt| receipt.duration_ms));
    let latency = Latency {
        status: if durations.len() == total_turns && total_turns > 0 {
            "complete"
        } else if durations.is_empty() {
            "not recorded"
        } else {
            "partial"
        },
        measured_turns: durations.len(),
        average_ms: (!durations.is_empty())
            .then(|| durations.iter().sum::<u64>() / durations.len() as u64),
        median_ms: percentile(&durations, 0.50),
        p95_ms: percentile(&durations, 0.95),
        last_ms: latest.and_then(|receipt| receipt.duration_ms),
        output_tokens_per_second_wall_time: (elapsed_for_throughput > 0
            && output_for_throughput > 0)
            .then(|| output_for_throughput as f64 / (elapsed_for_throughput as f64 / 1000.0)),
    };

    let (priced, priced_turns) = receipts
        .iter()
        .fold((0u64, 0usize), |(sum, count), receipt| {
            match receipt.cost_micros {
                Some(value) => (sum.saturating_add(value), count + 1),
                None => (sum, count),
            }
        });
    let cost = Cost {
        status: if priced_turns == 0 {
            "not priced"
        } else if priced_turns == total_turns {
            "estimate"
        } else {
            "partial estimate"
        },
        estimate_usd: (priced_turns > 0).then_some(priced as f64 / 1_000_000.0),
        priced_turns,
        total_turns,
        source: "local model/config price table; not provider-billed cost",
    };

    let assembled_prompt = assembled_prompt(receipts);
    let messages = MessageCounts {
        user: session
            .messages
            .iter()
            .filter(|m| m.role == Role::User)
            .count(),
        assistant: session
            .messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .count(),
        tool: session
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .count(),
        system: session
            .messages
            .iter()
            .filter(|m| m.role == Role::System)
            .count(),
    };
    let elapsed_ms = memnoth_session::now_ms()
        .saturating_sub(session.created_ms)
        .max(0) as u64;
    let request_receipts = receipts
        .iter()
        .filter_map(|receipt| receipt.provider_requests)
        .collect::<Vec<_>>();
    let tool_receipts = receipts
        .iter()
        .filter_map(|receipt| receipt.tool_calls)
        .collect::<Vec<_>>();

    UsageSnapshot {
        session_id: session.id.clone(),
        session_model: session.model.clone(),
        active_provider: (!context_provider.is_empty()).then(|| context_provider.to_string()),
        active_model: if context_model.is_empty() {
            session.model.clone()
        } else {
            context_model.to_string()
        },
        session_age_ms: elapsed_ms,
        messages,
        completed_turns: total_turns,
        latest_turn: latest.map(turn_usage),
        provider_usage: usage_coverage,
        input_tokens: measurement(
            input_receipts.len(),
            total_turns,
            (!input_receipts.is_empty()).then_some(input_sum),
        ),
        output_tokens: measurement(
            output_receipts.len(),
            total_turns,
            (!output_receipts.is_empty()).then_some(output_sum),
        ),
        reasoning_output_tokens: optional_category(&receipt_refs, |receipt| {
            receipt.provider_reasoning_tokens
        }),
        cache_read_tokens: optional_category(&receipt_refs, |receipt| {
            receipt.provider_cache_read_tokens
        }),
        cache_write_tokens: optional_category(&receipt_refs, |receipt| {
            receipt.provider_cache_write_tokens
        }),
        provider_requests: measurement(
            request_receipts.len(),
            total_turns,
            (!request_receipts.is_empty())
                .then(|| request_receipts.iter().map(|count| u64::from(*count)).sum()),
        ),
        tool_calls: measurement(
            tool_receipts.len(),
            total_turns,
            (!tool_receipts.is_empty())
                .then(|| tool_receipts.iter().map(|count| u64::from(*count)).sum()),
        ),
        latency,
        models,
        context_window,
        next_turn_effort: next_turn_effort.unwrap_or("provider default").to_string(),
        cost,
        rate_limits: ProviderRateLimits {
            status: "not reported",
            detail: "provider rate-limit headers are not persisted in session receipts",
        },
        assembled_prompt,
    }
}

fn model_usage(
    provider: Option<String>,
    model: Option<String>,
    rows: &[&TurnReceipt],
) -> ModelUsage {
    let input_known = rows
        .iter()
        .filter(|receipt| input_was_reported(receipt))
        .copied()
        .collect::<Vec<_>>();
    let output_known = rows
        .iter()
        .filter(|receipt| output_was_reported(receipt))
        .copied()
        .collect::<Vec<_>>();
    ModelUsage {
        provider,
        model,
        turns: rows.len(),
        input_tokens: measurement(
            input_known.len(),
            rows.len(),
            (!input_known.is_empty())
                .then(|| sum(input_known.iter().map(|receipt| receipt.input_tokens))),
        ),
        output_tokens: measurement(
            output_known.len(),
            rows.len(),
            (!output_known.is_empty())
                .then(|| sum(output_known.iter().map(|receipt| receipt.output_tokens))),
        ),
        reasoning_output_tokens: optional_category(rows, |receipt| {
            receipt.provider_reasoning_tokens
        }),
        cache_read_tokens: optional_category(rows, |receipt| receipt.provider_cache_read_tokens),
        cache_write_tokens: optional_category(rows, |receipt| receipt.provider_cache_write_tokens),
        efforts: rows
            .iter()
            .filter_map(|receipt| receipt.reasoning_effort.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
    }
}

fn assembled_prompt(receipts: &[TurnReceipt]) -> AssembledPrompt {
    let count = receipts.len();
    let sum_layer = |layer: usize| -> Option<u64> {
        (!receipts.is_empty()).then(|| {
            receipts
                .iter()
                .map(|receipt| receipt.layer_tokens[layer] as u64)
                .sum()
        })
    };
    let measured = |value| measurement(count, count, value);
    AssembledPrompt {
        status: if count == 0 {
            "not recorded"
        } else {
            "estimated"
        },
        tokens: measured((count > 0).then(|| {
            receipts
                .iter()
                .map(|receipt| receipt.total_tokens as u64)
                .sum()
        })),
        global_tokens: measured(sum_layer(0)),
        project_tokens: measured(sum_layer(1)),
        session_tokens: measured(sum_layer(2)),
        turn_tokens: measured(sum_layer(3)),
        pieces: measured((count > 0).then(|| receipts.iter().map(|r| r.pieces as u64).sum())),
    }
}

fn optional_category(
    receipts: &[&TurnReceipt],
    value: impl Fn(&TurnReceipt) -> Option<u64>,
) -> Measurement<u64> {
    let values = receipts
        .iter()
        .filter_map(|receipt| value(receipt))
        .collect::<Vec<_>>();
    measurement(
        values.len(),
        receipts.len(),
        (!values.is_empty()).then(|| sum(values.into_iter())),
    )
}

fn measurement(reported_turns: usize, total_turns: usize, value: Option<u64>) -> Measurement<u64> {
    Measurement {
        status: if total_turns == 0 || reported_turns == 0 {
            "not reported"
        } else if reported_turns == total_turns {
            "reported"
        } else {
            "partially reported"
        },
        value,
        reported_turns,
        total_turns,
    }
}

fn input_was_reported(receipt: &TurnReceipt) -> bool {
    receipt.provider_input_reported.unwrap_or_else(|| {
        receipt
            .provider_usage_reported
            .unwrap_or(receipt.input_tokens > 0 || receipt.output_tokens > 0)
    })
}

fn output_was_reported(receipt: &TurnReceipt) -> bool {
    receipt.provider_output_reported.unwrap_or_else(|| {
        receipt
            .provider_usage_reported
            .unwrap_or(receipt.input_tokens > 0 || receipt.output_tokens > 0)
    })
}

fn turn_usage(receipt: &TurnReceipt) -> TurnUsage {
    let measured =
        |known: bool, value: u64| measurement(usize::from(known), 1, known.then_some(value));
    TurnUsage {
        turn_id: receipt.turn_id,
        completed_at_ms: receipt.at_ms,
        provider: receipt.provider_id.clone(),
        model: receipt.context_window_model.clone(),
        effort: receipt.reasoning_effort.clone(),
        input_tokens: measured(input_was_reported(receipt), receipt.input_tokens),
        output_tokens: measured(output_was_reported(receipt), receipt.output_tokens),
        reasoning_output_tokens: measured(
            receipt.provider_reasoning_tokens.is_some(),
            receipt.provider_reasoning_tokens.unwrap_or_default(),
        ),
        cache_read_tokens: measured(
            receipt.provider_cache_read_tokens.is_some(),
            receipt.provider_cache_read_tokens.unwrap_or_default(),
        ),
        cache_write_tokens: measured(
            receipt.provider_cache_write_tokens.is_some(),
            receipt.provider_cache_write_tokens.unwrap_or_default(),
        ),
        provider_requests: measured(
            receipt.provider_requests.is_some(),
            u64::from(receipt.provider_requests.unwrap_or_default()),
        ),
        tool_calls: measured(
            receipt.tool_calls.is_some(),
            u64::from(receipt.tool_calls.unwrap_or_default()),
        ),
        latency_ms: measured(
            receipt.duration_ms.is_some(),
            receipt.duration_ms.unwrap_or_default(),
        ),
        context_occupancy_tokens: measured(
            receipt.context_window_used.is_some(),
            receipt.context_window_used.unwrap_or_default(),
        ),
        cost_estimate_usd: receipt.cost_micros.map(|value| value as f64 / 1_000_000.0),
    }
}

fn usage_was_reported(receipt: &TurnReceipt) -> bool {
    receipt
        .provider_usage_reported
        .unwrap_or(input_was_reported(receipt) || output_was_reported(receipt))
}

fn percentile(values: &[u64], fraction: f64) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() - 1) as f64 * fraction).ceil() as usize;
    sorted.get(index).copied()
}

fn sum(values: impl Iterator<Item = u64>) -> u64 {
    values.fold(0, u64::saturating_add)
}

fn render_text(snapshot: &UsageSnapshot) -> String {
    let mut out = format!(
        "usage · {}\nsession  {} · age {}\nactive  {} · {}\ncompleted turns  {} · messages {} user / {} assistant / {} tool\n",
        snapshot.session_id,
        snapshot.session_model,
        duration(snapshot.session_age_ms),
        snapshot
            .active_provider
            .as_deref()
            .unwrap_or("provider not selected"),
        snapshot.active_model,
        snapshot.completed_turns,
        snapshot.messages.user,
        snapshot.messages.assistant,
        snapshot.messages.tool,
    );
    out.push_str("\nprovider usage\n");
    render_measurement(&mut out, "input", &snapshot.input_tokens, "tokens");
    render_measurement(&mut out, "output", &snapshot.output_tokens, "tokens");
    render_measurement(
        &mut out,
        "reasoning output",
        &snapshot.reasoning_output_tokens,
        "tokens",
    );
    render_measurement(
        &mut out,
        "cache read",
        &snapshot.cache_read_tokens,
        "tokens",
    );
    render_measurement(
        &mut out,
        "cache write",
        &snapshot.cache_write_tokens,
        "tokens",
    );

    out.push_str("\nlatest completed turn\n");
    if let Some(turn) = &snapshot.latest_turn {
        out.push_str(&format!(
            "turn {} · {} · {} · effort {}\n",
            turn.turn_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "index not recorded".to_string()),
            turn.provider.as_deref().unwrap_or("provider not recorded"),
            turn.model.as_deref().unwrap_or("model not recorded"),
            turn.effort.as_deref().unwrap_or("not recorded"),
        ));
        render_measurement(&mut out, "turn input", &turn.input_tokens, "tokens");
        render_measurement(&mut out, "turn output", &turn.output_tokens, "tokens");
        render_measurement(&mut out, "turn requests", &turn.provider_requests, "");
        render_measurement(&mut out, "turn tools", &turn.tool_calls, "");
        render_measurement(&mut out, "turn latency", &turn.latency_ms, "ms");
        render_measurement(
            &mut out,
            "turn reasoning",
            &turn.reasoning_output_tokens,
            "tokens",
        );
        render_measurement(
            &mut out,
            "turn cache read",
            &turn.cache_read_tokens,
            "tokens",
        );
        render_measurement(
            &mut out,
            "turn cache write",
            &turn.cache_write_tokens,
            "tokens",
        );
        render_measurement(
            &mut out,
            "turn context",
            &turn.context_occupancy_tokens,
            "tokens",
        );
        if let Some(estimate) = turn.cost_estimate_usd {
            out.push_str(&format!("turn cost  ~${estimate:.6} estimate\n"));
        } else {
            out.push_str("turn cost  not priced\n");
        }
    } else {
        out.push_str("no completed turn receipt\n");
    }

    out.push_str("\nmodel and provider\n");
    for model in &snapshot.models {
        out.push_str(&format!(
            "{} · {} · {} turn(s) · effort {}\n",
            model.provider.as_deref().unwrap_or("provider not recorded"),
            model.model.as_deref().unwrap_or("model not recorded"),
            model.turns,
            if model.efforts.is_empty() {
                "not recorded".to_string()
            } else {
                model.efforts.join(", ")
            }
        ));
    }
    if snapshot.models.is_empty() {
        out.push_str("not recorded\n");
    }
    out.push_str(&format!(
        "next turn effort  {}\n",
        snapshot.next_turn_effort
    ));

    out.push_str("\nactivity and latency\n");
    render_measurement(
        &mut out,
        "provider requests",
        &snapshot.provider_requests,
        "",
    );
    render_measurement(&mut out, "tool calls", &snapshot.tool_calls, "");
    if let Some(avg) = snapshot.latency.average_ms {
        out.push_str(&format!(
            "turn latency  avg {} · median {} · p95 {} · last {} · {} measured turn(s)\n",
            duration(avg),
            duration(snapshot.latency.median_ms.unwrap_or(avg)),
            duration(snapshot.latency.p95_ms.unwrap_or(avg)),
            duration(snapshot.latency.last_ms.unwrap_or(avg)),
            snapshot.latency.measured_turns,
        ));
    } else {
        out.push_str("turn latency  not recorded\n");
    }
    if let Some(rate) = snapshot.latency.output_tokens_per_second_wall_time {
        out.push_str(&format!(
            "output throughput  {rate:.1} tokens/s (turn wall time)\n"
        ));
    } else {
        out.push_str("output throughput  unavailable\n");
    }

    out.push_str("\ncontext window\n");
    out.push_str(&format!(
        "{} · {}\n",
        snapshot
            .context_window
            .provider
            .as_deref()
            .unwrap_or("provider not recorded"),
        snapshot
            .context_window
            .model
            .as_deref()
            .unwrap_or("model not recorded")
    ));
    match (
        snapshot.context_window.occupancy_tokens,
        snapshot.context_window.limit_tokens,
    ) {
        (Some(used), Some(limit)) if limit > 0 => out.push_str(&format!(
            "last request  {} / {} tokens · {:.1}% · {} remaining{} · {}\n",
            grouped(used),
            grouped(limit),
            used as f64 * 100.0 / limit as f64,
            grouped(snapshot.context_window.remaining_tokens.unwrap_or_default()),
            snapshot
                .context_window
                .over_limit_tokens
                .map_or_else(String::new, |over| format!(" · {} over", grouped(over))),
            snapshot
                .context_window
                .limit_source
                .as_deref()
                .unwrap_or("source unavailable")
        )),
        (Some(used), _) => out.push_str(&format!(
            "last request  {} tokens · model limit unavailable\n",
            grouped(used)
        )),
        _ => out.push_str("last request  occupancy not reported\n"),
    }

    out.push_str("\nMemnoth prompt assembly (token estimates)\n");
    render_measurement(
        &mut out,
        "total",
        &snapshot.assembled_prompt.tokens,
        "tokens",
    );
    render_measurement(
        &mut out,
        "global",
        &snapshot.assembled_prompt.global_tokens,
        "tokens",
    );
    render_measurement(
        &mut out,
        "project",
        &snapshot.assembled_prompt.project_tokens,
        "tokens",
    );
    render_measurement(
        &mut out,
        "session",
        &snapshot.assembled_prompt.session_tokens,
        "tokens",
    );
    render_measurement(
        &mut out,
        "turn",
        &snapshot.assembled_prompt.turn_tokens,
        "tokens",
    );
    render_measurement(
        &mut out,
        "context pieces",
        &snapshot.assembled_prompt.pieces,
        "",
    );

    out.push_str("\ncost\n");
    if let Some(value) = snapshot.cost.estimate_usd {
        out.push_str(&format!(
            "~${value:.6} {} · {}/{} priced turns · {}\n",
            snapshot.cost.status,
            snapshot.cost.priced_turns,
            snapshot.cost.total_turns,
            snapshot.cost.source
        ));
    } else {
        out.push_str(&format!("not available · {}\n", snapshot.cost.source));
    }
    out.push_str(&format!(
        "rate limits  {} · {}\n",
        snapshot.rate_limits.status, snapshot.rate_limits.detail
    ));
    out
}

fn render_measurement(out: &mut String, label: &str, value: &Measurement<u64>, unit: &str) {
    match value.value {
        Some(number) => {
            let suffix = if unit.is_empty() {
                String::new()
            } else {
                format!(" {unit}")
            };
            out.push_str(&format!(
                "{label:<19} {}{suffix} · {}/{} turns · {}\n",
                grouped(number),
                value.reported_turns,
                value.total_turns,
                value.status
            ));
        }
        None => out.push_str(&format!("{label:<19} not reported\n")),
    }
}

fn duration(ms: u64) -> String {
    if ms < 10_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 60_000 {
        format!("{}s", (ms + 500) / 1000)
    } else {
        format!("{}m {:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt() -> TurnReceipt {
        TurnReceipt {
            at_ms: 1,
            total_tokens: 12,
            prefix_hash: "hash".into(),
            layer_tokens: [3, 3, 3, 3],
            pieces: 4,
            input_tokens: 12_345,
            context_window_used: Some(10_000),
            context_window_model: Some("vendor/model:free".into()),
            context_window_provider: Some("gateway".into()),
            provider_id: Some("kilo".into()),
            reasoning_effort: Some("high".into()),
            duration_ms: Some(1_200),
            provider_requests: Some(2),
            tool_calls: Some(1),
            provider_reasoning_tokens: Some(23),
            provider_cache_read_tokens: Some(0),
            provider_cache_write_tokens: Some(18),
            provider_usage_reported: Some(true),
            provider_input_reported: Some(true),
            provider_output_reported: Some(true),
            output_tokens: 678,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            prompt: String::new(),
            cost_micros: Some(1_234),
            turn_id: Some(1),
        }
    }

    fn session_with(receipts: Vec<TurnReceipt>) -> Session {
        let mut session = Session::new("vendor/model:free");
        session.id = "usage-fixture".into();
        session.created_ms = 1;
        session.receipts = receipts;
        session
    }

    fn render_text(session: &Session) -> String {
        render(
            session,
            Some(100_000),
            "vendor/model:free",
            "gateway",
            Some("provider model catalog"),
            Some("medium"),
            false,
        )
    }

    #[test]
    fn text_includes_exact_identity_effort_usage_activity_latency_and_source() {
        let text = render_text(&session_with(vec![receipt()]));
        for expected in [
            "active  gateway · vendor/model:free",
            "kilo · vendor/model:free",
            "effort high",
            "latest completed turn",
            "turn 1 · kilo · vendor/model:free · effort high",
            "turn requests       2",
            "turn tools          1",
            "12,345",
            "reasoning output",
            "cache read",
            "cache write",
            "provider requests",
            "tool calls",
            "turn latency  avg 1.2s",
            "output throughput",
            "provider model catalog",
            "90,000 remaining",
            "~$0.001234 estimate",
            "not provider-billed cost",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
        assert!(text.contains("cache read          0 tokens"), "{text}");
    }

    #[test]
    fn provider_reported_zero_is_distinct_from_missing_categories() {
        let mut row = receipt();
        row.provider_cache_read_tokens = Some(0);
        row.provider_cache_write_tokens = None;
        let text = render_text(&session_with(vec![row]));
        assert!(text.contains("cache read          0 tokens"), "{text}");
        assert!(text.contains("cache write         not reported"), "{text}");

        let mut unknown = receipt();
        unknown.provider_cache_read_tokens = None;
        unknown.provider_usage_reported = Some(false);
        unknown.provider_input_reported = Some(false);
        unknown.provider_output_reported = Some(false);
        unknown.input_tokens = 0;
        unknown.output_tokens = 0;
        let text = render_text(&session_with(vec![unknown]));
        assert!(text.contains("input               not reported"), "{text}");
        assert!(text.contains("cache read          not reported"), "{text}");
    }

    #[test]
    fn json_has_the_same_known_values_and_explicit_unknown_statuses() {
        let json_text = render(
            &session_with(vec![receipt()]),
            Some(100_000),
            "vendor/model:free",
            "gateway",
            Some("provider model catalog"),
            Some("medium"),
            true,
        );
        let value: serde_json::Value = serde_json::from_str(&json_text).unwrap();
        assert_eq!(value["provider_requests"]["value"], 2);
        assert_eq!(value["tool_calls"]["value"], 1);
        assert_eq!(value["latest_turn"]["provider_requests"]["value"], 2);
        assert_eq!(value["latest_turn"]["tool_calls"]["value"], 1);
        assert_eq!(value["latest_turn"]["latency_ms"]["value"], 1_200);
        assert_eq!(value["active_provider"], "gateway");
        assert_eq!(value["active_model"], "vendor/model:free");
        assert_eq!(value["reasoning_output_tokens"]["value"], 23);
        assert_eq!(value["cache_read_tokens"]["value"], 0);
        assert_eq!(value["context_window"]["remaining_tokens"], 90_000);
        assert!(value["context_window"]["over_limit_tokens"].is_null());
        assert_eq!(value["cost"]["status"], "estimate");
        assert_eq!(value["rate_limits"]["status"], "not reported");
        assert_eq!(
            value["context_window"]["limit_source"],
            "provider model catalog"
        );

        let mut unknown = receipt();
        unknown.provider_usage_reported = Some(false);
        unknown.provider_input_reported = Some(false);
        unknown.provider_output_reported = Some(false);
        unknown.provider_cache_read_tokens = None;
        unknown.input_tokens = 0;
        unknown.output_tokens = 0;
        let value: serde_json::Value = serde_json::from_str(&render(
            &session_with(vec![unknown]),
            None,
            "vendor/model:free",
            "gateway",
            None,
            None,
            true,
        ))
        .unwrap();
        assert_eq!(value["input_tokens"]["status"], "not reported");
        assert_eq!(value["input_tokens"]["value"], serde_json::Value::Null);
        assert_eq!(value["cache_read_tokens"]["status"], "not reported");
    }

    #[test]
    fn partial_category_reports_keep_unknown_zero_and_positive_values_distinct() {
        let mut input_only = receipt();
        input_only.input_tokens = 0;
        input_only.provider_input_reported = Some(true);
        input_only.provider_output_reported = Some(false);
        input_only.provider_reasoning_tokens = None;
        input_only.provider_cache_read_tokens = Some(0);
        input_only.provider_cache_write_tokens = None;

        let mut output_only = receipt();
        output_only.provider_input_reported = Some(false);
        output_only.provider_output_reported = Some(true);
        output_only.provider_reasoning_tokens = Some(0);
        output_only.provider_cache_read_tokens = None;
        output_only.provider_cache_write_tokens = Some(7);

        let session = session_with(vec![input_only, output_only]);
        let value: serde_json::Value = serde_json::from_str(&render(
            &session,
            Some(100_000),
            "vendor/model:free",
            "gateway",
            Some("provider model catalog"),
            None,
            true,
        ))
        .unwrap();

        for (field, expected_status, expected_value, expected_coverage) in [
            ("input_tokens", "partially reported", 0, 1),
            ("output_tokens", "partially reported", 678, 1),
            ("reasoning_output_tokens", "partially reported", 0, 1),
            ("cache_read_tokens", "partially reported", 0, 1),
            ("cache_write_tokens", "partially reported", 7, 1),
        ] {
            assert_eq!(value[field]["status"], expected_status, "{field}");
            assert_eq!(value[field]["value"], expected_value, "{field}");
            assert_eq!(value[field]["reported_turns"], expected_coverage, "{field}");
        }

        let mut none = receipt();
        none.provider_input_reported = Some(false);
        none.provider_output_reported = Some(false);
        none.provider_reasoning_tokens = None;
        none.provider_cache_read_tokens = None;
        none.provider_cache_write_tokens = None;
        let unknown: serde_json::Value = serde_json::from_str(&render(
            &session_with(vec![none]),
            None,
            "vendor/model:free",
            "gateway",
            None,
            None,
            true,
        ))
        .unwrap();
        for field in [
            "input_tokens",
            "output_tokens",
            "reasoning_output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
        ] {
            assert_eq!(unknown[field]["status"], "not reported", "{field}");
            assert!(unknown[field]["value"].is_null(), "{field}");
        }
    }

    #[test]
    fn provider_only_switch_does_not_reuse_the_previous_providers_context_limit() {
        let mut direct = receipt();
        direct.provider_id = Some("openai".into());
        direct.context_window_provider = Some("openai".into());
        direct.context_window_model = Some("shared-model-id".into());
        direct.context_window_used = Some(8_000);

        let mut gateway = direct.clone();
        gateway.provider_id = Some("kilo".into());
        gateway.context_window_provider = Some("kilo".into());
        gateway.context_window_used = Some(9_000);

        let value: serde_json::Value = serde_json::from_str(&render(
            &session_with(vec![direct, gateway]),
            Some(128_000),
            "shared-model-id",
            "openai",
            Some("OpenAI catalog"),
            None,
            true,
        ))
        .unwrap();
        assert_eq!(value["context_window"]["provider"], "kilo");
        assert_eq!(value["context_window"]["model"], "shared-model-id");
        assert_eq!(value["context_window"]["occupancy_tokens"], 9_000);
        assert!(value["context_window"]["limit_tokens"].is_null());
        assert!(value["context_window"]["limit_source"].is_null());
        assert!(value["context_window"]["remaining_tokens"].is_null());
    }

    #[test]
    fn latest_effort_reset_does_not_inherit_the_previous_turn_effort() {
        let mut explicit = receipt();
        explicit.reasoning_effort = Some("high".into());
        let mut default = receipt();
        default.turn_id = Some(2);
        default.reasoning_effort = None;

        let session = session_with(vec![explicit, default]);
        let json: serde_json::Value = serde_json::from_str(&render(
            &session,
            Some(100_000),
            "vendor/model:free",
            "gateway",
            Some("provider model catalog"),
            None,
            true,
        ))
        .unwrap();
        assert!(json["latest_turn"]["effort"].is_null());
        assert_eq!(json["next_turn_effort"], "provider default");
        assert_eq!(json["models"][0]["efforts"], serde_json::json!(["high"]));

        let text = render(
            &session,
            Some(100_000),
            "vendor/model:free",
            "gateway",
            Some("provider model catalog"),
            None,
            false,
        );
        assert!(text.contains("turn 2 · kilo · vendor/model:free · effort not recorded"));
        assert!(text.contains("next turn effort  provider default"));
    }

    #[test]
    fn text_and_json_are_rendered_from_the_same_semantic_snapshot() {
        let session = session_with(vec![receipt()]);
        let mut expected = serde_json::to_value(snapshot(
            &session,
            Some(100_000),
            "vendor/model:free",
            "gateway",
            Some("provider model catalog"),
            Some("medium"),
        ))
        .unwrap();
        let mut actual: serde_json::Value = serde_json::from_str(&render(
            &session,
            Some(100_000),
            "vendor/model:free",
            "gateway",
            Some("provider model catalog"),
            Some("medium"),
            true,
        ))
        .unwrap();

        // Session age is sampled when each rendering is created, so the two
        // snapshots can legitimately differ by a few milliseconds. Compare
        // every stable semantic field and validate the sampled age separately.
        let actual_age = actual["session_age_ms"].as_u64().unwrap();
        let expected_age = expected["session_age_ms"].as_u64().unwrap();
        assert!(actual_age > 0);
        assert!(expected_age > 0);
        actual.as_object_mut().unwrap().remove("session_age_ms");
        expected.as_object_mut().unwrap().remove("session_age_ms");
        assert_eq!(actual, expected);

        let text = render_text(&session);
        for semantic in [
            expected["active_provider"].as_str().unwrap(),
            expected["active_model"].as_str().unwrap(),
            expected["next_turn_effort"].as_str().unwrap(),
            expected["context_window"]["limit_source"].as_str().unwrap(),
            expected["cost"]["status"].as_str().unwrap(),
        ] {
            assert!(text.contains(semantic), "missing {semantic:?}: {text}");
        }
        assert!(text.contains("12,345 tokens"), "{text}");
        assert!(text.contains("90,000 remaining"), "{text}");
        assert!(text.contains("~$0.001234 estimate"), "{text}");
        assert!(text.contains("not provider-billed cost"), "{text}");
    }

    #[test]
    fn context_limit_is_not_reused_for_a_different_model_or_provider() {
        let text = render(
            &session_with(vec![receipt()]),
            Some(100_000),
            "other/model",
            "gateway",
            Some("provider model catalog"),
            None,
            false,
        );
        assert!(
            text.contains("10,000 tokens · model limit unavailable"),
            "{text}"
        );
        assert!(!text.contains("/ 100,000 tokens"), "{text}");
    }

    #[test]
    fn context_overflow_reports_zero_remaining_and_the_overage_without_underflow() {
        let mut row = receipt();
        row.context_window_used = Some(100_050);
        let text = render_text(&session_with(vec![row.clone()]));
        assert!(text.contains("0 remaining · 50 over"), "{text}");

        let value: serde_json::Value = serde_json::from_str(&render(
            &session_with(vec![row]),
            Some(100_000),
            "vendor/model:free",
            "gateway",
            Some("provider model catalog"),
            Some("medium"),
            true,
        ))
        .unwrap();
        assert_eq!(value["context_window"]["remaining_tokens"], 0);
        assert_eq!(value["context_window"]["over_limit_tokens"], 50);
    }

    #[test]
    fn empty_session_reports_unknown_metrics_without_fabricated_zero_cost() {
        let text = render_text(&session_with(Vec::new()));
        assert!(text.contains("input               not reported"), "{text}");
        assert!(text.contains("provider requests   not reported"), "{text}");
        assert!(text.contains("turn latency  not recorded"), "{text}");
        assert!(
            text.contains("not available · local model/config price table"),
            "{text}"
        );
        assert!(!text.contains("$0.000000"), "{text}");
    }

    #[test]
    fn model_rows_group_exact_provider_and_model_pairs() {
        let first = receipt();
        let mut second = receipt();
        second.provider_id = Some("openai".into());
        second.context_window_model = Some("gpt-exact-2026-01".into());
        let value: serde_json::Value = serde_json::from_str(&render(
            &session_with(vec![first, second]),
            None,
            "",
            "",
            None,
            None,
            true,
        ))
        .unwrap();
        assert_eq!(value["models"].as_array().unwrap().len(), 2);
    }
}
