//! `/usage` — provider-reported usage for the active conversation.

use memnoth_session::{Session, TurnReceipt};

/// Render exact session token totals, the latest request's context occupancy,
/// and only the costs explicitly stored on turn receipts.
pub(crate) fn render(
    session: &Session,
    context_window_size: Option<u64>,
    context_model: &str,
    context_provider: &str,
    reasoning_effort: Option<&str>,
) -> String {
    let turns = session.receipts.len();
    let total_tokens = session.input_tokens.saturating_add(session.output_tokens);
    let mut out = format!(
        "usage · {}\nmodel  {}\nturn receipts  {}\ntokens {} in · {} out · {} total\n",
        session.id,
        session.model,
        turns,
        grouped(session.input_tokens),
        grouped(session.output_tokens),
        grouped(total_tokens),
    );

    if let Some(last) = session.last_receipt() {
        out.push_str(&format!(
            "last request  {} · {}\n",
            last.context_window_model
                .as_deref()
                .unwrap_or("model not recorded"),
            last.context_window_provider
                .as_deref()
                .unwrap_or("provider not recorded")
        ));
        let matching_context = context_identity_matches(last, context_model, context_provider);
        match (
            last.context_window_used,
            matching_context.then_some(context_window_size).flatten(),
        ) {
            (Some(used), Some(limit)) if limit > 0 => {
                let percent = ((used as f64 / limit as f64) * 100.0).round() as u64;
                out.push_str(&format!(
                    "last prompt  {} / {} tokens · {}%\n",
                    grouped(used),
                    grouped(limit),
                    percent
                ));
            }
            (Some(used), _) => out.push_str(&format!(
                "last prompt  {} tokens reported · model limit unknown\n",
                grouped(used)
            )),
            (None, _) => out.push_str("last prompt  context usage not reported\n"),
        }
        let (cache_read, cache_write) = session.receipts.iter().fold((0u64, 0u64), |sum, r| {
            (
                sum.0.saturating_add(r.cache_read_tokens),
                sum.1.saturating_add(r.cache_write_tokens),
            )
        });
        if cache_read > 0 || cache_write > 0 {
            out.push_str(&format!(
                "cache      {} read · {} write tokens\n",
                grouped(cache_read),
                grouped(cache_write)
            ));
        } else {
            out.push_str("cache      not reported\n");
        }
    } else {
        out.push_str("last prompt  context not recorded\ncache      not reported\n");
    }

    let (priced_micros, priced_turns) =
        session
            .receipts
            .iter()
            .fold((0u64, 0usize), |acc, r| match r.cost_micros {
                Some(micros) => (acc.0.saturating_add(micros), acc.1 + 1),
                None => acc,
            });
    if priced_turns == turns && turns > 0 {
        out.push_str(&format!(
            "cost     ~${:.6} estimate · {priced_turns}/{turns} priced turns\n",
            priced_micros as f64 / 1_000_000.0
        ));
    } else if priced_turns > 0 {
        out.push_str(&format!(
            "cost     ~${:.6} partial estimate · {priced_turns}/{turns} priced turns\n",
            priced_micros as f64 / 1_000_000.0
        ));
    } else {
        out.push_str("cost     unknown · no priced receipts\n");
    }
    out.push_str(&format!(
        "next turn effort  {}\nrate limits        not reported by provider\n",
        reasoning_effort.unwrap_or("provider default")
    ));
    out
}

fn context_identity_matches(
    receipt: &TurnReceipt,
    context_model: &str,
    context_provider: &str,
) -> bool {
    receipt.context_window_model.as_deref() == Some(context_model)
        && receipt
            .context_window_provider
            .as_deref()
            .is_some_and(|provider| provider.eq_ignore_ascii_case(context_provider))
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

    fn receipt(
        input_tokens: u64,
        output_tokens: u64,
        context_window_used: Option<u64>,
        cost_micros: Option<u64>,
    ) -> TurnReceipt {
        TurnReceipt {
            at_ms: 1,
            total_tokens: 12,
            prefix_hash: "hash".into(),
            layer_tokens: [3, 3, 3, 3],
            pieces: 4,
            input_tokens,
            context_window_used,
            context_window_model: Some("model-a".into()),
            context_window_provider: Some("provider-a".into()),
            output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            prompt: String::new(),
            cost_micros,
            turn_id: None,
        }
    }

    #[test]
    fn usage_shows_measured_totals_matching_context_and_estimated_price() {
        let mut session = Session::new("model-a");
        session.account(12_345, 678);
        session.record_receipt(receipt(12_345, 678, Some(10_000), Some(1_234)));

        let text = render(
            &session,
            Some(100_000),
            "model-a",
            "provider-a",
            Some("high"),
        );
        assert!(
            text.contains("12,345 in · 678 out · 13,023 total"),
            "{text}"
        );
        assert!(text.contains("10,000 / 100,000 tokens · 10%"), "{text}");
        assert!(text.contains("~$0.001234 estimate"), "{text}");
        assert!(text.contains("next turn effort  high"), "{text}");
        assert!(
            text.contains("rate limits        not reported by provider"),
            "{text}"
        );
    }

    #[test]
    fn usage_never_pairs_context_from_a_different_model_or_provider() {
        let mut session = Session::new("model-a");
        session.account(20, 5);
        session.record_receipt(receipt(20, 5, Some(20), None));

        let model_mismatch = render(&session, Some(100), "model-b", "provider-a", None);
        assert!(model_mismatch.contains("20 tokens reported · model limit unknown"));
        let provider_mismatch = render(&session, Some(100), "model-a", "provider-b", None);
        assert!(provider_mismatch.contains("20 tokens reported · model limit unknown"));
        assert!(model_mismatch.contains("cost     unknown · no priced receipts"));
        assert!(model_mismatch.contains("cache      not reported"));
    }

    #[test]
    fn usage_empty_session_does_not_invent_zero_cost_or_request_data() {
        let session = Session::new("model-a");
        let text = render(&session, Some(100), "model-a", "provider-a", None);
        assert!(text.contains("turn receipts  0"), "{text}");
        assert!(text.contains("context not recorded"), "{text}");
        assert!(text.contains("unknown · no priced receipts"), "{text}");
        assert!(!text.contains("$0.000000"), "{text}");
    }
}
