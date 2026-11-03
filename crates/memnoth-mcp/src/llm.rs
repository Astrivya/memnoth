//! Assisted retrieval — an LLM inside the *ranking* loop.
//!
//! # Why this exists, and why it is not the default
//!
//! Every rival in this category that publishes a high number publishes an
//! LLM-assisted one: mempalace ships `--llm-rerank`, MemOS scores with a judge,
//! mem0 extracts facts with a model at ingest. Their raw number is either not
//! published or published beside the assisted one. So a lane that publishes only a
//! raw figure is not being more honest — it is being **less comparable**, and it
//! loses a comparison it could win.
//!
//! This module is the assisted half. Three rules bind it:
//!
//! 1. **Opt-in only.** Nothing here runs unless a caller asks for `rerank=llm`.
//! 2. **Labelled.** The response's `ranking` says `assisted`, so a number produced
//!    with a model can never be quoted as a model-free one.
//! 3. **Fail-open.** A missing key, a timeout, an unparseable answer or a provider
//!    outage returns the fused order unchanged — an optional external service must
//!    never turn a working search into a broken one, and must never silently
//!    reorder a result set into something the caller cannot explain.
//!
//! # Transport
//!
//! `curl.exe` through `std::process`, matching `eval.rs`'s fixed-instrument lanes,
//! and deliberately **not** a new HTTP dependency: the server ships as a single
//! binary and this feature must be removable by deleting one file.

use std::collections::HashSet;

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Which provider answered. Both are read from the environment and neither is
/// required — a server with no key simply cannot serve the assisted mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Provider {
    /// OpenAI-compatible gateway; the same one the E2E lane uses, so the assisted
    /// retrieval number and the assisted answer number share one instrument.
    Kilo,
    /// Google. Used as the second family when the first is unavailable, so the
    /// assisted lane has a fallback the way the judge lane does.
    Gemini,
}

/// The model each provider runs for a listwise rerank.
///
/// Named rather than discovered, and fixed rather than tuned: an assisted number
/// is only comparable if the instrument is. `MEMNOTH_LLM_RERANK_MODEL` overrides
/// it, and the override is disclosed in the `ranking` label so an overridden run
/// cannot be mistaken for a default one.
const KILO_MODEL: &str = "inclusionai/ling-3.0-flash-vl:free";
const GEMINI_MODEL: &str = "gemini-3.6-flash";

/// A resolved provider plus its key. Holding the key here rather than reading it
/// per call makes "is this available?" one decision rather than one per candidate.
pub(crate) struct Client {
    pub provider: Provider,
    key: String,
    model: String,
}

/// The configured client, or `None` when no key is present.
///
/// `MEMNOTH_LLM_RERANK=off` disables it even with a key set, so a deployment can
/// turn the assisted path off without rotating credentials.
pub(crate) fn client() -> Option<Client> {
    if disabled(std::env::var("MEMNOTH_LLM_RERANK").ok().as_deref()) {
        return None;
    }
    let override_model = std::env::var("MEMNOTH_LLM_RERANK_MODEL")
        .ok()
        .filter(|m| !m.trim().is_empty());
    for (provider, vars, default_model) in [
        (
            Provider::Kilo,
            ["KILOCODE_API_KEY", "KILO_API_KEY"].as_slice(),
            KILO_MODEL,
        ),
        (
            Provider::Gemini,
            ["GEMINI_API_KEY"].as_slice(),
            GEMINI_MODEL,
        ),
    ] {
        for var in vars {
            if let Some(key) = std::env::var(var).ok().filter(|k| !k.trim().is_empty()) {
                return Some(Client {
                    provider,
                    key,
                    model: override_model
                        .clone()
                        .unwrap_or_else(|| default_model.to_string()),
                });
            }
        }
    }
    None
}

/// Whether `MEMNOTH_LLM_RERANK` switches the assisted path off.
///
/// A pure predicate rather than a read inside `client()`, for the same reason
/// `cache_ttl` is pure: the decision is then testable without mutating the process
/// environment, and mutating the environment from a test is both `unsafe` in this
/// edition and racy with every other test in the binary. "Is it off?" is a fact about
/// one string, so it is expressed as one.
fn disabled(value: Option<&str>) -> bool {
    matches!(value, Some("off") | Some("0") | Some("false"))
}

/// The system prompt. Constrained hard on purpose: a listwise reranker that
/// *answers the question* instead of *ordering the passages* is the failure mode
/// that makes an assisted number unreproducible, so it is told to emit nothing but
/// a permutation.
const SYSTEM: &str = "You rank retrieved memory passages by how well each one helps answer the question. \
Reply with a single JSON object: {\"order\": [best first list of the passage numbers]}. \
Include every passage number exactly once. Do not answer the question, do not explain, output JSON only.";

/// Re-rank `candidates` (id, passage) best-first, keeping at most `keep`.
///
/// The contract, in order: return a **permutation of the input**, never a subset
/// and never an invented id; put ids the model omitted after the ones it ordered,
/// in their original relative order; and on any failure return the input unchanged.
/// Those three together mean a caller can never lose a candidate to a flaky
/// provider — the assisted pass can only *reorder*.
pub(crate) fn rerank(
    client: &Client,
    query: &str,
    candidates: &[(i64, String)],
    keep: usize,
) -> Result<Vec<i64>> {
    let input: Vec<i64> = candidates.iter().map(|(id, _)| *id).collect();
    if candidates.len() < 2 {
        return Ok(input.into_iter().take(keep).collect());
    }
    let listing: String = candidates
        .iter()
        .enumerate()
        .map(|(index, (_, passage))| format!("{}\t{}", index + 1, truncate(passage, 480)))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!("Question: {query}\n\nPassages:\n{listing}");
    let text = client.complete(SYSTEM, &user)?;
    let order = parse_order(&text).with_context(|| {
        format!(
            "assisted rerank returned no usable order: {}",
            truncate(&text, 200)
        )
    })?;
    let mut seen: HashSet<usize> = HashSet::new();
    let mut ranked: Vec<i64> = Vec::with_capacity(candidates.len());
    for position in order {
        // 1-based, as the prompt asked; out-of-range numbers are ignored rather
        // than remapped, because a mapping guess could move the wrong passage.
        if position >= 1 && position <= candidates.len() && seen.insert(position) {
            ranked.push(candidates[position - 1].0);
        }
    }
    // Anything the model dropped follows, in fused order. It was a candidate on the
    // strength of retrieval evidence; a listwise pass that forgot it does not get
    // to *delete* it.
    for (index, (id, _)) in candidates.iter().enumerate() {
        if seen.insert(index + 1) {
            ranked.push(*id);
        }
    }
    ranked.truncate(keep);
    Ok(ranked)
}

/// The first JSON object/array in `text`, parsed. Models wrap JSON in prose and
/// in markdown fences often enough that a strict `from_str` would report a
/// formatting habit as an outage.
fn parse_order(text: &str) -> Option<Vec<usize>> {
    if let Ok(value) = serde_json::from_str::<Value>(text.trim())
        && let Some(order) = order_from(&value)
    {
        return Some(order);
    }
    let object_start = text.find('{')?;
    let object_text = &text[object_start..];
    let mut depth = 0usize;
    let mut end = None;
    for (index, ch) in object_text.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(index + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    // `end` is deliberately matched rather than `?`-ed: an unterminated object is exactly the
    // truncation case the salvage below exists for, and `?` here would return `None` from the
    // whole function, making that path unreachable. (Its own unit test caught this.)
    if let Some(end_at) = end
        && let Ok(value) = serde_json::from_str::<Value>(&object_text[..end_at])
        && let Some(order) = order_from(&value)
    {
        return Some(order);
    }
    parse_truncated_order(text)
}

/// Last resort: read the numbers out of an `order` array that was **cut off mid-array**.
///
/// WHY: a listwise reply that hits the token cap ends in `{"order": [17, 19, 8, ...` with no
/// closing bracket, so every JSON parse fails and the call reports "no usable order" — while
/// the model had in fact ranked dozens of candidates correctly. Observed on LoCoMo at a
/// 1024-token cap with a ~35-item ordering. `rerank` already appends any candidate the
/// ordering omits, so a *partial* ranking is strictly better than none: it reorders the head,
/// which is what the metric reads. Only numbers inside the array region are scanned, and
/// out-of-range values are ignored by the caller.
fn parse_truncated_order(text: &str) -> Option<Vec<usize>> {
    let start = text
        .find("\"order\"")
        .map(|i| i + "\"order\"".len())
        .or_else(|| text.find('['))?;
    let mut out: Vec<usize> = Vec::new();
    let mut digits = String::new();
    for ch in text[start..].chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if !digits.is_empty() {
            if let Ok(n) = digits.parse::<usize>() {
                out.push(n);
            }
            digits.clear();
        }
    }
    if !digits.is_empty()
        && let Ok(n) = digits.parse::<usize>()
    {
        out.push(n);
    }
    if out.is_empty() { None } else { Some(out) }
}

fn order_from(value: &Value) -> Option<Vec<usize>> {
    let raw = value.get("order").or(Some(value))?;
    let items = raw.as_array()?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        // Accept `3`, `"3"` and `{"index": 3}`. Three shapes because the contract
        // here is "the model's ordering", and rejecting a number that arrived as a
        // string would discard a correct answer over a serialization detail.
        let number = item
            .as_u64()
            .or_else(|| item.as_str().and_then(|s| s.trim().parse().ok()))
            .or_else(|| item.get("index").and_then(Value::as_u64))
            .or_else(|| item.get("id").and_then(Value::as_u64));
        if let Some(number) = number {
            out.push(number as usize);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

impl Client {
    /// The model that answered, for the `ranking` label. An assisted number is only
    /// comparable when the instrument is named beside it.
    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn provider_label(&self) -> &'static str {
        match self.provider {
            Provider::Kilo => "kilo",
            Provider::Gemini => "gemini",
        }
    }

    /// One completion, through `curl.exe`, with a hard timeout.
    fn complete(&self, system: &str, user: &str) -> Result<String> {
        let (url, body) = match self.provider {
            Provider::Kilo => (
                "https://api.kilo.ai/api/gateway/v1/chat/completions".to_string(),
                json!({
                    "model": self.model,
                    // 0 and a hard cap: an assisted *ranking* has to be a function of
                    // its input, and a reranker that samples differently per call
                    // would make the number it produces unreproducible.
                    "temperature": 0,
                    // 2048, not 1024: the model must emit an ordering of up to 80 items
                    // *plus* wrapper prose, and a reasoning-style model spends part of the
                    // budget thinking first. At 1024 the observed reply was an order cut
                    // mid-array on LoCoMo. `parse_truncated_order` now salvages those too,
                    // so this is belt-and-braces rather than the only line of defence.
                    "max_tokens": 2048,
                    "messages": [
                        {"role": "system", "content": system},
                        {"role": "user", "content": user}
                    ]
                }),
            ),
            Provider::Gemini => (
                format!(
                    "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
                    self.model, self.key
                ),
                json!({
                    "contents": [{"role": "user", "parts": [{"text": format!("{system}\n\n{user}")}]}],
                    "generationConfig": {
                        "temperature": 0,
                        "maxOutputTokens": 1024,
                        // Mirrors eval.rs's reader: on the 3.x generation the model
                        // spends the budget thinking first, and without an explicit
                        // budget the visible answer is the fragment that survives.
                        "thinkingConfig": {"thinkingBudget": 512}
                    }
                }),
            ),
        };
        // Through the shared file-staged transport, NOT an inline `-d`: this prompt is
        // tens of thousands of characters (80 candidates x 480), and a body passed as a
        // curl *argument* dies on Windows' ~32K command-line limit (WinError 206) before
        // it leaves the machine — every assisted rerank then "failed", fell back to the
        // fused order, and the llm arm measured nothing while reporting a number.
        let headers = match self.provider {
            Provider::Kilo => vec![
                "Content-Type: application/json".to_string(),
                format!("Authorization: Bearer {}", self.key),
            ],
            Provider::Gemini => vec!["Content-Type: application/json".to_string()],
        };
        let stdout = curl_post_json(&url, &headers, &body, 45)?;
        let value: Value =
            serde_json::from_str(stdout.trim()).context("assisted rerank response was not JSON")?;
        let text = match self.provider {
            Provider::Kilo => value["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            Provider::Gemini => value["candidates"][0]["content"]["parts"]
                .as_array()
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(Value::as_str))
                        .collect::<String>()
                })
                .unwrap_or_default(),
        };
        if !text.trim().is_empty() {
            return Ok(text);
        }
        // Empty content is not necessarily an empty *answer*: this model reasons first, and
        // an 80-item ordering can land entirely in the reasoning field. The parse below
        // accepts any `order` array wherever it appears, so a ranking that exists only in
        // the scratch work is still the model's ranking. Without this fallback every
        // assisted rerank "failed", fell back to the fused order, and the `llm` arm silently
        // measured the very baseline it was supposed to improve on — a number that looks
        // like a negative result and is really a bug.
        let reasoning = match self.provider {
            Provider::Kilo => value["choices"][0]["message"]["reasoning"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            Provider::Gemini => String::new(),
        };
        if !reasoning.trim().is_empty() {
            return Ok(reasoning);
        }
        let finish = value["choices"][0]["finish_reason"]
            .as_str()
            .unwrap_or("unknown");
        anyhow::bail!("assisted rerank returned no content (finish_reason={finish})")
    }
}

/// POST a JSON body through `curl.exe`, staging the body in a temp file.
///
/// WHY NOT `-d <body>`: Windows caps a single process command line near 32K
/// characters (`CreateProcess` → `WinError 206: The filename or extension is too
/// long`). The assisted rerank prompt is 80 candidates × 480 characters, so an
/// inline body failed *before leaving the machine* — every call then fell back to
/// the fused order and the `llm` arm measured nothing while still printing a
/// number. `--data-binary @file` has no such limit, and the file is unique per
/// call (pid + counter) so concurrent workers cannot overwrite each other's body.
///
/// Self-contained rather than shared with `eval.rs`: that module is `#[cfg(test)]`,
/// and this is production code — the MCP server's assisted rerank must not depend
/// on a test-only module.
fn curl_post_json(url: &str, headers: &[String], body: &Value, timeout_s: u32) -> Result<String> {
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "memnoth-llm-body-{}-{}.json",
        std::process::id(),
        SEQ.fetch_add(1, AtomicOrdering::SeqCst)
    ));
    std::fs::write(&path, body.to_string())
        .context("failed to stage the assisted rerank body for curl")?;
    let mut cmd = std::process::Command::new("curl.exe");
    cmd.arg("-s").arg("-m").arg(timeout_s.to_string());
    for h in headers {
        cmd.arg("-H").arg(h);
    }
    cmd.arg("--data-binary")
        .arg(format!("@{}", path.display()))
        .arg(url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let out = cmd.output();
    // Staging, not a record: removed whether the call succeeded or failed.
    let _ = std::fs::remove_file(&path);
    let out = out.context("running curl.exe for the assisted rerank")?;
    if !out.status.success() {
        anyhow::bail!("assisted rerank transport failed: {}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_truncated_order_array_is_still_a_ranking() {
        // The exact shape observed on LoCoMo: the reply hit the token cap mid-array, so no
        // JSON parse can succeed, and the call used to report "no usable order" while the
        // model had ranked 22 candidates correctly.
        let cut = "{\"order\": [17, 19, 8, 35, 5, 18, 16, 68, 67, 3, 14, 15, 9, 4, 26, 1, 34, 46, 6, 12, 2, 7, 10";
        let order = parse_order(cut).expect("a cut array must still yield an order");
        assert_eq!(
            order,
            vec![
                17, 19, 8, 35, 5, 18, 16, 68, 67, 3, 14, 15, 9, 4, 26, 1, 34, 46, 6, 12, 2, 7, 10
            ]
        );
    }

    #[test]
    fn a_well_formed_order_is_unaffected_by_the_salvage_path() {
        let full = "{\"order\": [3, 1, 2]}";
        assert_eq!(parse_order(full), Some(vec![3, 1, 2]));
    }

    #[test]
    fn order_parsing_accepts_every_shape_a_model_actually_returns() {
        assert_eq!(parse_order("{\"order\": [3, 1, 2]}"), Some(vec![3, 1, 2]));
        assert_eq!(
            parse_order("```json\n{\"order\": [2, 1]}\n```"),
            Some(vec![2, 1])
        );
        assert_eq!(
            parse_order("Sure!\n{\"order\": [\"2\", \"1\"]}"),
            Some(vec![2, 1])
        );
        assert_eq!(
            parse_order("{\"order\": [{\"index\": 2}, {\"index\": 1}]}"),
            Some(vec![2, 1])
        );
        assert_eq!(parse_order("no json here at all"), None);
        assert_eq!(parse_order("{\"order\": []}"), None);
    }

    /// The contract that makes an assisted pass safe to run: it can only reorder.
    /// A model that hallucinates ids, drops passages or repeats one must not be
    /// able to shrink the candidate set.
    #[test]
    fn a_bad_model_answer_can_only_reorder_never_delete() {
        let candidates = [
            (10i64, "first".to_string()),
            (20, "second".to_string()),
            (30, "third".to_string()),
        ];
        // The `rerank` path is driven by `parse_order`, so the permutation logic is
        // exercised directly here without a provider.
        let order = parse_order("{\"order\": [99, 3, 3]}").unwrap();
        let mut seen = HashSet::new();
        let mut ranked = Vec::new();
        for position in order {
            if position >= 1 && position <= candidates.len() && seen.insert(position) {
                ranked.push(candidates[position - 1].0);
            }
        }
        for (index, (id, _)) in candidates.iter().enumerate() {
            if seen.insert(index + 1) {
                ranked.push(*id);
            }
        }
        assert_eq!(
            ranked.len(),
            candidates.len(),
            "no candidate may be dropped"
        );
        assert_eq!(ranked[0], 30, "position 3 is honoured");
        assert_eq!(
            ranked[1], 10,
            "the omitted candidates follow in fused order"
        );
        assert_eq!(ranked[2], 20);
    }

    /// `off` must win over any key, so a deployment can disable the assisted path
    /// without rotating credentials — and "unset" must NOT mean disabled, or an
    /// operator who never heard of the variable would silently lose the feature.
    #[test]
    fn the_assisted_path_is_disabled_by_exactly_three_values() {
        for off in ["off", "0", "false"] {
            assert!(
                disabled(Some(off)),
                "`{off}` must disable the assisted path"
            );
        }
        for on in ["on", "1", "true", "", "OFF "] {
            assert!(!disabled(Some(on)), "`{on}` must not disable it");
        }
        // Unset is not disabled: `client()` still requires a key, so the decision to
        // run assisted remains "a key is present and nobody said off".
        assert!(!disabled(None));
    }
}
