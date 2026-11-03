//! S1 fixed-cost retrieval — per-arm recall budgets, rerank cap, token-budget
//! packing, exact per-stage accounting (`domination-plan/scale.md` Phase S1).
//!
//! The contract, in one place so every recall entry point enforces the same
//! numbers:
//!
//! * Recall arms run under a tier budget: low 100 / mid 300 / high 1000.
//!   [`clamp_limit`] is the absolute ceiling (high): no caller, however
//!   insistent, gets more than 1000 candidates out of a recall arm.
//! * No rerank pool exceeds [`RERANK_CAP`] (300). The cross-encoder pool used
//!   to run to 400; the cap is enforced at the pool computation in `lib.rs`,
//!   not here, so this module owns the number and the call site owns one call.
//! * Output packing is token-budgeted: [`pack_top_down`] takes ranked
//!   candidates and a token budget (S1: 7000 for the single-pass top-200),
//!   keeps greedily from the top, skips oversize items, and is never empty —
//!   if nothing fits, the first item is kept alone and `over_budget` is set,
//!   so a budget breach is visible instead of silent.
//! * Token estimates use the measured ratios from `eval.rs` (JSON schema text
//!   4.009 chars/token, conversational prose 4.239, calibrated against
//!   `tiktoken` `cl100k_base` on this machine). They are estimates, not exact
//!   counts: the swap-in point for exact accounting is [`exact_tokens`] —
//!   when `tiktoken-rs` lands in `Cargo.toml`, that function grows a real
//!   encoder behind it and every caller below is already routed through it.
//! * `BudgetLedger` counts tokens per pipeline stage (ingest / extract /
//!   embed / retrieve / answer) in exact `u64` arithmetic, so a published
//!   tokens/query figure is a sum, never a float.
//!
//! Budget breach = fail regardless of accuracy: the packing report and the
//! ledger exist so the harness can fail a run on cost, not just on quality.

/// Low-tier recall budget: cheap lookups stay under 100 candidates.
///
/// Test-only today: per-query tier selection is S2, so nothing in the shipping
/// recall path picks a tier yet. Gated rather than `allow(dead_code)` so the
/// warning budget stays honest if this is ever left unwired by accident.
#[cfg(test)]
pub const RECALL_BUDGET_LOW: usize = 100;
/// Mid-tier recall budget: the default working depth.
#[cfg(test)]
pub const RECALL_BUDGET_MID: usize = 300;
/// High-tier recall budget, and the absolute ceiling no arm may exceed.
pub const RECALL_BUDGET_HIGH: usize = 1000;
/// No rerank pool — MMR, cross-encoder, or assisted — may exceed 300
/// candidates. A reranker only reorders what fusion found; past 300 the pass
/// costs more than it can return.
pub const RERANK_CAP: usize = 300;
/// S1 output budget: single-pass top-200 packed into at most 7K tokens/query.
pub const TOP200_PACK_TOKEN_BUDGET: u64 = 7_000;

/// Measured chars/token ratios (`eval.rs`: calibrated against `tiktoken`
/// `cl100k_base`). Two distributions, two ratios — schema text tokenizes
/// denser than conversational prose, and a single ratio flattered one of them.
#[cfg(test)]
pub const CHARS_PER_TOKEN_JSON: f64 = 4.009;
pub const CHARS_PER_TOKEN_PROSE: f64 = 4.239;

/// Recall depth tier. S1 wires the ceiling; per-query tier selection (cheap
/// lookup vs deep synthesis with an explicit token budget each) is S2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub enum RecallTier {
    Low,
    Mid,
    High,
}

#[cfg(test)]
impl RecallTier {
    /// Candidate budget for this tier.
    pub fn budget(self) -> usize {
        match self {
            RecallTier::Low => RECALL_BUDGET_LOW,
            RecallTier::Mid => RECALL_BUDGET_MID,
            RecallTier::High => RECALL_BUDGET_HIGH,
        }
    }
}

/// Absolute ceiling for any recall-arm `limit`: no arm serves more than
/// [`RECALL_BUDGET_HIGH`] candidates, whatever the caller asked for.
pub fn clamp_limit(requested: usize) -> usize {
    requested.min(RECALL_BUDGET_HIGH)
}

/// Ceiling for a tier-budgeted arm: the arm serves at most the tier budget.
#[cfg(test)]
pub fn clamp_to_tier(requested: usize, tier: RecallTier) -> usize {
    requested.min(tier.budget())
}

/// No rerank pool exceeds [`RERANK_CAP`]. Apply to the computed pool, not the
/// tool limit: `clamp_rerank_pool((limit * factor).clamp(limit, MAX))`.
pub fn clamp_rerank_pool(pool: usize) -> usize {
    pool.min(RERANK_CAP)
}

/// Estimated tokens for JSON schema / tool-payload text (4.009 chars/token).
/// Rounds up: under-counting a budget is the direction an error must not point.
#[cfg(test)]
pub fn estimate_tokens_json(chars: usize) -> u64 {
    ((chars as f64) / CHARS_PER_TOKEN_JSON).ceil() as u64
}

/// Estimated tokens for conversational prose / retrieved content (4.239
/// chars/token). Rounds up, for the same reason as `estimate_tokens_json`.
pub fn estimate_tokens_prose(chars: usize) -> u64 {
    ((chars as f64) / CHARS_PER_TOKEN_PROSE).ceil() as u64
}

/// Exact token count for `text`. Today this is the calibrated estimate (the
/// `prose` ratio: packed output is retrieved content, not schema); when
/// `tiktoken-rs` (`o200k_base`) lands in `Cargo.toml`, the body becomes one
/// encoder call and every packing decision below turns exact with no call-site
/// change. The packing contract (top-down, skip-oversize, never-empty) does
/// not depend on which body is behind this function.
pub fn exact_tokens(text: &str) -> u64 {
    estimate_tokens_prose(text.chars().count())
}

/// Pipeline stages with independent token budgets. Every stage that spends
/// tokens records them here; tokens/query is then a sum over stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg(test)]
pub enum BudgetStage {
    Ingest,
    Extract,
    Embed,
    Retrieve,
    Answer,
}

/// Exact per-stage token accounting. `u64` throughout: token counts add, and
/// a published tokens/query figure must be reproducible to the token.
#[derive(Debug, Default, Clone)]
#[cfg(test)]
pub struct BudgetLedger {
    ingest: u64,
    extract: u64,
    embed: u64,
    retrieve: u64,
    answer: u64,
}

#[cfg(test)]
impl BudgetLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `tokens` spent in `stage`. Saturating: accounting must never
    /// wrap, even under adversarial input.
    pub fn add(&mut self, stage: BudgetStage, tokens: u64) {
        let slot = match stage {
            BudgetStage::Ingest => &mut self.ingest,
            BudgetStage::Extract => &mut self.extract,
            BudgetStage::Embed => &mut self.embed,
            BudgetStage::Retrieve => &mut self.retrieve,
            BudgetStage::Answer => &mut self.answer,
        };
        *slot = slot.saturating_add(tokens);
    }

    /// Tokens spent in one stage.
    pub fn stage(&self, stage: BudgetStage) -> u64 {
        match stage {
            BudgetStage::Ingest => self.ingest,
            BudgetStage::Extract => self.extract,
            BudgetStage::Embed => self.embed,
            BudgetStage::Retrieve => self.retrieve,
            BudgetStage::Answer => self.answer,
        }
    }

    /// Total tokens across all stages: the tokens/query figure.
    pub fn total(&self) -> u64 {
        self.ingest
            .saturating_add(self.extract)
            .saturating_add(self.embed)
            .saturating_add(self.retrieve)
            .saturating_add(self.answer)
    }
}

/// One packed candidate: kept Rank order in, kept order out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedItem<T> {
    pub item: T,
    pub tokens: u64,
}

/// What a packing run decided. `over_budget` is true only in the never-empty
/// fallback (the first item alone exceeds the budget): the breach is reported,
/// never silently absorbed.
#[derive(Debug, Clone)]
pub struct PackReport<T> {
    pub kept: Vec<PackedItem<T>>,
    pub skipped_oversize: usize,
    pub tokens_used: u64,
    pub token_budget: u64,
    pub over_budget: bool,
}

impl<T> PackReport<T> {
    /// Tokens/query contribution of this packed set.
    pub fn tokens(&self) -> u64 {
        self.tokens_used
    }
}

/// Greedy top-down packing under a token budget. Items arrive in rank order
/// with a precomputed token cost each; items that fit are kept, items that
/// would overflow are skipped (later, smaller items can still fit), and the
/// result is never empty: if nothing fits, the first item is kept alone with
/// `over_budget` set. Rank order is preserved — packing never reorders.
pub fn pack_top_down<T>(
    items: impl IntoIterator<Item = (T, u64)>,
    token_budget: u64,
) -> PackReport<T> {
    let mut kept: Vec<PackedItem<T>> = Vec::new();
    let mut skipped_oversize = 0usize;
    let mut tokens_used = 0u64;
    // First item that overflowed while nothing was kept yet: the never-empty
    // fallback. Only the head qualifies — a later item that overflows while
    // earlier items already fit is just skipped.
    let mut head: Option<(T, u64)> = None;

    for (item, tokens) in items {
        if tokens_used.saturating_add(tokens) <= token_budget {
            tokens_used = tokens_used.saturating_add(tokens);
            kept.push(PackedItem { item, tokens });
        } else {
            if kept.is_empty() && head.is_none() {
                head = Some((item, tokens));
            }
            skipped_oversize += 1;
        }
    }

    let mut over_budget = false;
    // Nothing fit, so the budget is smaller than every item: keep the head alone
    // and report the breach instead of returning empty (an empty answer to a
    // recall query is a failure the budget caused, and failures must be
    // visible). The head is *matched* rather than asserted: `kept.is_empty()` is
    // part of the condition and not implied by it — a head stashed early stays
    // stashed while a later, smaller item fits, and in that case the report is
    // not empty and the breach is not this one.
    if kept.is_empty()
        && let Some((item, tokens)) = head
    {
        tokens_used = tokens;
        kept.push(PackedItem { item, tokens });
        over_budget = true;
    }

    PackReport {
        kept,
        skipped_oversize,
        tokens_used,
        token_budget,
        over_budget,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_budgets_are_100_300_1000() {
        assert_eq!(RecallTier::Low.budget(), 100);
        assert_eq!(RecallTier::Mid.budget(), 300);
        assert_eq!(RecallTier::High.budget(), 1000);
        assert_eq!(
            (RECALL_BUDGET_LOW, RECALL_BUDGET_MID, RECALL_BUDGET_HIGH),
            (100, 300, 1000)
        );
    }

    #[test]
    fn arms_never_exceed_caps() {
        // Absolute ceiling.
        assert_eq!(clamp_limit(0), 0);
        assert_eq!(clamp_limit(20), 20);
        assert_eq!(clamp_limit(1000), 1000);
        assert_eq!(clamp_limit(10_000), 1000);
        assert_eq!(clamp_limit(usize::MAX), 1000);
        // Tier ceilings.
        assert_eq!(clamp_to_tier(50, RecallTier::Low), 50);
        assert_eq!(clamp_to_tier(500, RecallTier::Low), 100);
        assert_eq!(clamp_to_tier(500, RecallTier::Mid), 300);
        assert_eq!(clamp_to_tier(500, RecallTier::High), 500);
        assert_eq!(clamp_to_tier(5000, RecallTier::High), 1000);
        // Rerank pools never exceed 300 — the 400-deep cross-encoder pool is
        // the breach this closes.
        assert_eq!(clamp_rerank_pool(80), 80);
        assert_eq!(clamp_rerank_pool(300), 300);
        assert_eq!(clamp_rerank_pool(400), 300);
        assert_eq!(clamp_rerank_pool(usize::MAX), 300);
        assert_eq!(RERANK_CAP, 300);
    }

    #[test]
    fn token_counter_exact_on_fixture() {
        // ceil(chars / ratio): 4009 chars of schema text = exactly 1000
        // tokens; 4239 chars of prose = exactly 1000 tokens. One char more
        // tips over — rounding is up, never down.
        assert_eq!(estimate_tokens_json(4009), 1000);
        assert_eq!(estimate_tokens_json(4010), 1001);
        assert_eq!(estimate_tokens_prose(4239), 1000);
        assert_eq!(estimate_tokens_prose(4240), 1001);
        assert_eq!(estimate_tokens_json(0), 0);
        // The swap-in point agrees with the prose ratio today.
        assert_eq!(exact_tokens(&"x".repeat(4239)), 1000);
    }

    #[test]
    fn packing_top_down_skips_oversize_never_empty() {
        // Rank order in: 4000t, 5000t (skipped), 2000t (fits after skip).
        let r = pack_top_down(vec![("a", 4000), ("b", 5000), ("c", 2000)], 7000);
        assert_eq!(
            r.kept.iter().map(|p| p.item).collect::<Vec<_>>(),
            vec!["a", "c"]
        );
        assert_eq!(r.skipped_oversize, 1);
        assert_eq!(r.tokens_used, 6000);
        assert!(!r.over_budget);
    }

    #[test]
    fn packing_never_empty_reports_breach() {
        // Nothing fits a 100-token budget: the head is kept alone and the
        // breach is reported, not silently absorbed.
        let r = pack_top_down(vec![("a", 5000), ("b", 6000)], 100);
        assert_eq!(r.kept.len(), 1);
        assert_eq!(r.kept[0].item, "a");
        assert!(r.over_budget);
        assert_eq!(r.tokens_used, 5000);
    }

    #[test]
    fn packing_empty_in_empty_out() {
        let r: PackReport<&str> = pack_top_down(vec![], 7000);
        assert!(r.kept.is_empty());
        assert_eq!(r.tokens_used, 0);
        assert!(!r.over_budget);
    }

    #[test]
    fn packing_full_take_when_everything_fits() {
        let r = pack_top_down(vec![("a", 1000), ("b", 2000), ("c", 3000)], 7000);
        assert_eq!(r.kept.len(), 3);
        assert_eq!(r.skipped_oversize, 0);
        assert_eq!(r.tokens_used, 6000);
        assert!(!r.over_budget);
        assert_eq!(r.token_budget, 7000);
        assert_eq!(TOP200_PACK_TOKEN_BUDGET, 7_000);
    }

    #[test]
    fn ledger_sums_exactly_per_stage() {
        let mut l = BudgetLedger::new();
        l.add(BudgetStage::Ingest, 1000);
        l.add(BudgetStage::Extract, 500);
        l.add(BudgetStage::Embed, 250);
        l.add(BudgetStage::Retrieve, 7000);
        l.add(BudgetStage::Answer, 1200);
        assert_eq!(l.stage(BudgetStage::Retrieve), 7000);
        assert_eq!(l.total(), 1000 + 500 + 250 + 7000 + 1200);
        // Repeated adds accumulate, never replace.
        l.add(BudgetStage::Retrieve, 8);
        assert_eq!(l.stage(BudgetStage::Retrieve), 7008);
        // Saturates instead of wrapping.
        l.add(BudgetStage::Ingest, u64::MAX);
        assert_eq!(l.stage(BudgetStage::Ingest), u64::MAX);
        assert_eq!(l.total(), u64::MAX);
    }
}

/// Tier name as published (`low`/`mid`/`high`).
#[cfg(test)]
fn tier_name(tier: RecallTier) -> &'static str {
    match tier {
        RecallTier::Low => "low",
        RecallTier::Mid => "mid",
        RecallTier::High => "high",
    }
}

/// S3: the retrieval half of the context tax, as canonical rows.
///
/// The tool-schema cost spec pins `tools/list` bytes per profile; this is what a
/// *query* costs at each recall tier. Every value is a constant or fixed
/// arithmetic on constants, so two dumps are byte-identical — determinism is the
/// S3 guard: a cost row that drifts is a regression, not a measurement.
/// Publish beside `toolSchemaCost` in `server.json`.
///
/// `packTokens`/`spineMinChars` come from S1 (and R3's allocation floor); the
/// per-query figure is deliberately absent: a token count per query is a
/// *measurement* (tiktoken-exact, from the ledger), and this table is the spec it
/// must be checked against.
#[cfg(test)]
pub fn tier_cost_rows() -> Vec<serde_json::Value> {
    [RecallTier::Low, RecallTier::Mid, RecallTier::High]
        .into_iter()
        .map(|tier| {
            serde_json::json!({
                "tier": tier_name(tier),
                "recallBudget": tier.budget(),
                "rerankCap": RERANK_CAP,
                "packTokens": TOP200_PACK_TOKEN_BUDGET,
                "spineMinChars": 700,
                "charsPerTokenJson": CHARS_PER_TOKEN_JSON,
                "charsPerTokenProse": CHARS_PER_TOKEN_PROSE,
            })
        })
        .collect()
}

#[cfg(test)]
mod cost_tests {
    use super::*;

    #[test]
    fn tier_cost_rows_are_deterministic() {
        assert_eq!(
            tier_cost_rows(),
            tier_cost_rows(),
            "cost dump must not drift"
        );
    }

    #[test]
    fn tiers_are_ordered_and_capped_by_the_ceiling() {
        let rows = tier_cost_rows();
        assert_eq!(rows.len(), 3);
        let budgets: Vec<u64> = rows
            .iter()
            .map(|r| r["recallBudget"].as_u64().unwrap())
            .collect();
        assert!(
            budgets[0] < budgets[1] && budgets[1] < budgets[2],
            "{budgets:?}"
        );
        assert_eq!(budgets[2], RECALL_BUDGET_HIGH as u64);
        assert_eq!(rows[0]["tier"], "low");
        assert_eq!(rows[2]["tier"], "high");
        assert_eq!(rows[0]["rerankCap"].as_u64().unwrap(), RERANK_CAP as u64);
    }
}
