// ============================================================================
use super::*;

// Verbatim spans — the anti-lossy-extraction spine
// ============================================================================

/// A raw utterance, kept exactly as it appeared.
///
/// This type is the response to three independent findings that converged on one
/// mechanism. MemPalace measured 96.6% LongMemEval R@5 storing raw text with **no
/// extraction at all** and argued the field over-engineers extraction because it is
/// lossy. HaluMem found hallucinations are *created at the extraction stage* and
/// propagate to QA. Our own `single-session-assistant` class scores 35.7% and every
/// miss is an abstention — the reader could not find what the assistant actually
/// said. Extracted facts are a derived artifact; they must never be the only copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub id: String,
    pub text: String,
    pub speaker: Option<String>,
    /// Unix seconds when the utterance happened.
    pub at: Option<i64>,
    pub session: Option<String>,
    pub turn: Option<usize>,
}

impl Span {
    pub fn new(id: &str, text: &str) -> Self {
        Self {
            id: id.to_string(),
            text: text.to_string(),
            speaker: None,
            at: None,
            session: None,
            turn: None,
        }
    }

    pub fn speaker(mut self, s: &str) -> Self {
        self.speaker = Some(s.to_string());
        self
    }

    pub fn at(mut self, at: i64) -> Self {
        self.at = Some(at);
        self
    }

    pub fn session(mut self, s: &str) -> Self {
        self.session = Some(s.to_string());
        self
    }

    /// `[2023-05-01 · Caroline · s3#4] the actual words`
    ///
    /// The header carries provenance so a reader can attribute a statement to a
    /// speaker and a moment, which is what the `single-session-assistant` class
    /// needs and what a bare fact string cannot supply.
    pub fn render(&self) -> String {
        let mut header: Vec<String> = Vec::new();
        if let Some(at) = self.at
            && let Some(dt) = chrono::DateTime::from_timestamp(at, 0)
        {
            header.push(dt.format("%Y-%m-%d").to_string());
        }
        if let Some(s) = &self.speaker {
            header.push(s.clone());
        }
        if let Some(s) = &self.session {
            match self.turn {
                Some(t) => header.push(format!("{s}#{t}")),
                None => header.push(s.clone()),
            }
        }
        if header.is_empty() {
            self.text.clone()
        } else {
            format!("[{}] {}", header.join(" · "), self.text)
        }
    }
}

/// Where an extracted fact came from.
///
/// A fact with no spans is a fact we cannot defend: it cannot be checked against the
/// original, it cannot be dated, and it cannot be attributed to a speaker. Making the
/// link explicit lets the harness *measure* extraction loss instead of assuming it.
#[derive(Clone, Debug, Default)]
pub struct FactProvenance {
    pub fact_id: String,
    pub span_ids: Vec<String>,
}

/// How much of a fact's own wording is actually present in its source spans.
///
/// This is the instrument for the lossy-extraction question. A value near 1.0 means
/// the fact is a faithful quote; a low value means the extractor paraphrased, and
/// paraphrases are where HaluMem found hallucinations entering the system. Reported
/// as a corpus-level distribution rather than a pass/fail, because the honest answer
/// is "this much of what we store is not literally in the source".
pub fn span_coverage(fact_text: &str, spans: &[Span]) -> f64 {
    let fact_tokens: Vec<String> = fact_text
        .to_ascii_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(str::to_string)
        .collect();
    if fact_tokens.is_empty() {
        return 1.0;
    }
    let haystack: String = spans
        .iter()
        .map(|s| s.text.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let present = fact_tokens
        .iter()
        .filter(|t| haystack.contains(t.as_str()))
        .count();
    present as f64 / fact_tokens.len() as f64
}

/// The corpus-level distribution of [`span_coverage`] over a HaluMem user set.
///
/// This is the shape HaluMem's extraction task actually asks for, and it is deliberately a
/// **distribution and not a rate**. A single mean hides the tail, and the tail is where the
/// hallucinations are: a mean of 0.9 with a 10th percentile of 0.2 means a tenth of what we
/// store cannot be checked against the source at all, which is precisely the condition that
/// lets an extraction-stage hallucination reach QA unnoticed.
#[derive(Clone, Debug, Default)]
pub struct ExtractionCoverage {
    /// Non-distractor points measured.
    pub n: usize,
    pub mean: f64,
    pub p10: f64,
    pub p50: f64,
    /// The lowest single value — the worst extraction in the corpus.
    pub min: f64,
    /// Points below 0.6, i.e. in the paraphrase zone. Counted rather than averaged because
    /// "how many" is the actionable form: each one is a stored fact that cannot be defended
    /// from the source, and each is a candidate hallucination.
    pub paraphrased: usize,
    /// The same figures over `Interference` points only. If these run **lower** than the
    /// primary ones, the extractor is at least not hoovering distractors verbatim; if they run
    /// higher, it is quoting content the benchmark injected specifically to be rejected.
    pub distractor_n: usize,
    pub distractor_mean: f64,
}

/// Measure how much of each gold memory point is literally present in its own session.
///
/// Points are measured against the session they were extracted from, so this reads the corpus's
/// own extraction loss rather than ours — which is the honest quantity: it is the ceiling any
/// system querying that corpus inherits, and quoting it as our own accuracy would be a category
/// error. Distractors are kept in a separate bucket rather than filtered out, because their
/// coverage is itself a finding (see `distractor_mean`).
pub fn extraction_coverage(users: &[HaluUser]) -> ExtractionCoverage {
    let mut real: Vec<f64> = Vec::new();
    let mut distract: Vec<f64> = Vec::new();
    for u in users {
        for s in &u.sessions {
            for p in &s.memory_points {
                let c = span_coverage(&p.content, &s.turns);
                if p.source.is_distractor() {
                    distract.push(c);
                } else {
                    real.push(c);
                }
            }
        }
    }
    let mean_of = |v: &[f64]| -> f64 {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    // Percentiles come off one *sorted* copy, so `min` and `p10` cannot disagree about which
    // point is the worst one. `total_cmp` rather than `partial_cmp().unwrap()`: coverage is a
    // ratio of counts and can only be NaN if a token was empty, but a NaN here would silently
    // reorder the whole distribution, and a total order costs nothing.
    let mut sorted = real.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let at = |q: f64| -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let idx = (((sorted.len() - 1) as f64) * q).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    };
    ExtractionCoverage {
        n: real.len(),
        mean: mean_of(&real),
        p10: at(0.10),
        p50: at(0.50),
        min: sorted.first().copied().unwrap_or(0.0),
        paraphrased: sorted.iter().filter(|c| **c < 0.6).count(),
        distractor_n: distract.len(),
        distractor_mean: mean_of(&distract),
    }
}

/// Assemble evidence from spans under a character budget, **verbatim**, newest first.
///
/// Two deliberate properties, both paid for already:
///
/// 1. **No truncation mid-span.** The previous assembly cut items at a per-item
///    character cap and we measured the consequence: `mean_context_tokens = 6474`
///    against an 80 000 budget, abstentions spiked, and the score fell to 70%.
///    A span either fits whole or is not included.
/// 2. **Newest first.** Established by direct A/B on LongMemEval: 74% vs 66% on the
///    identical 50 groups.
pub fn assemble_verbatim(spans: &[Span], budget_chars: usize) -> (String, usize) {
    let mut ordered: Vec<&Span> = spans.iter().collect();
    ordered.sort_by_key(|span| std::cmp::Reverse(span.at.unwrap_or(0)));
    let mut out = String::new();
    let mut used = 0usize;
    for s in ordered {
        let rendered = s.render();
        // +1 for the newline that joins it.
        if used + rendered.chars().count() + 1 > budget_chars {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            used += 1;
        }
        used += rendered.chars().count();
        out.push_str(&rendered);
    }
    (out, used)
}
