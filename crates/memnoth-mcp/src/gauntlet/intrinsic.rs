// ============================================================================
use super::*;

// Intrinsic scoring — the benchmark's own metric, computed rather than inferred
// ============================================================================
//
// WHY THIS SECTION EXISTS. It closes the largest correctness gap in this lane, and the gap
// is not a missing capability — it is an **uncalled** one. Every instrument in this module
// was written before the benchmarks were run, deliberately (see the module header), and then
// none of them was wired to the harness: every family, BEAM included, was scored by one
// generic LLM "does this match?" verdict.
//
// The consequences are specific rather than aesthetic:
//
//   * `event_ordering` is a **ranking** question. A reader that gets all five events in the
//     right order is graded by comparing its prose against a literal `A -> B -> C -> D -> E`,
//     so it fails unless it happens to reproduce the arrow format. The ability is unreachable
//     however good the memory is — and it is one of BEAM's four "state over time" abilities,
//     i.e. one of the four this vault is supposed to win on.
//   * BEAM's rubric is a **per-nugget** three-point scale whose continuous score is then
//     clamped onto {0, 0.5, 1.0}. One holistic verdict discards both the per-nugget structure
//     and the clamp, so our column would carry the benchmark's name while measuring a
//     different task.
//   * LoCoMo's own judge grants credit for a date within ±14 days and a duration within 50%.
//     Neither tolerance was reachable, because nothing in the crate could *produce* an epoch
//     value to compare — `date_within_tolerance` had no caller and no feeder. That is what
//     `parse_date` below is for.
//
// So scoring becomes two-stage: try the benchmark's own deterministic metric first, and fall
// back to a judge only where the metric genuinely needs a model. Every row records which
// instrument produced it.

/// Which instrument scored a row.
///
/// Recorded per row rather than inferred from the family, because within one family the
/// instrument differs by question: BEAM's ordering questions are arithmetic, its rubric
/// questions need a model. A published table that cannot say which metric produced each row
/// is not reproducible, and this lane has already paid once for publishing a figure nobody
/// could re-derive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoredBy {
    /// The benchmark's own deterministic metric — an order correlation or a tolerance
    /// comparison. No model involved, so the result is reproducible bit-for-bit.
    Intrinsic,
    /// Per-nugget rubric coverage, clamped by BEAM's thresholds. A model supplies the grades;
    /// the benchmark supplies the scale they are clamped onto.
    Rubric,
    /// The generic LLM match verdict, for families whose metric is genuinely a judgement call
    /// (LoCoMo, LongMemEval).
    Judge,
}

impl ScoredBy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Intrinsic => "intrinsic",
            Self::Rubric => "rubric",
            Self::Judge => "judge",
        }
    }
}

/// Earliest offset at which `needle` occurs in `haystack`, by a distinctive-substring probe.
///
/// Exact matching alone is not usable: BEAM's event strings are full sentences and a reader
/// will rephrase around the identifying nouns, so a literal search would report "never
/// mentioned" for an event the reader plainly did mention — and a missing event is scored as
/// an ordering error, so that misreading would under-count our own answers.
///
/// Rather than a fuzzy threshold (which nothing can defend), the probe is the longest run of
/// **distinctive words** (four characters or more) that appears verbatim, longest run first and
/// capped at six words: the identifying phrasing of an event is short, and an unbounded search
/// is quadratic in the length of every rubric string.
fn locate(haystack: &str, needle: &str) -> Option<usize> {
    let needle = needle.to_ascii_lowercase();
    let trimmed = needle.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(at) = haystack.find(trimmed) {
        return Some(at);
    }
    let words: Vec<&str> = trimmed
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
        .collect();
    if words.len() < 2 {
        return None;
    }
    for span in (2..=words.len().min(6)).rev() {
        for start in 0..=words.len() - span {
            let probe = words[start..start + span].join(" ");
            if probe.len() < 10 {
                continue;
            }
            if let Some(at) = haystack.find(&probe) {
                return Some(at);
            }
        }
    }
    None
}

/// The ordering of `gold_order`'s items **as induced by the answer's text**.
///
/// The reader answers an ordering question in prose ("first she booked the flight, and later
/// she moved"), so what is compared is not two literal sequences but two *orderings*. Each gold
/// item is located in the answer and the items are ranked by where they appear.
///
/// An item the answer never mentions is placed **after every item it does mention**, in gold
/// order. That is the deliberate part: `kendall_tau_b` compares only items present in both
/// lists, so merely omitting an unmentioned event would drop it from the comparison and
/// *reward* an incomplete answer with a correlation computed over the easy remainder. Keeping
/// it in the list preserves the penalty, and τ-b's tie handling does the rest.
///
/// The number of gold items actually mentioned is returned separately, so a run can report
/// how often an ordering question was answered with only part of the sequence — a diagnosis a
/// single correlation cannot give, because "wrong order" and "half the events missing" are
/// different failures.
pub fn order_from_text(answer: &str, gold_order: &[String]) -> (Vec<String>, usize) {
    let haystack = answer.to_ascii_lowercase();
    let mut found: Vec<(usize, usize, String)> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for (gold_index, item) in gold_order.iter().enumerate() {
        match locate(&haystack, item) {
            // Ties broken by gold position, so two events that share a phrasing the reader
            // used only once do not get an order invented by the sort algorithm.
            Some(at) => found.push((at, gold_index, item.clone())),
            None => missing.push(item.clone()),
        }
    }
    found.sort_by_key(|(at, gold_index, _)| (*at, *gold_index));
    let mentioned = found.len();
    let mut out: Vec<String> = found.into_iter().map(|(_, _, item)| item).collect();
    out.extend(missing);
    (out, mentioned)
}

/// Normalised Kendall τ-b for an ordering question: τ ∈ [-1, 1] → [0, 1].
///
/// Normalised because a benchmark column is a credit, not a correlation, and a negative τ
/// averaged alongside positives would silently become a bonus for other questions.
///
/// `None` when the sample cannot support a correlation (fewer than two shared items, or a
/// degenerate denominator), because a τ over one event is not a measurement and must not be
/// averaged in as a zero.
pub fn ordering_score(predicted: &[String], gold: &[String]) -> Option<f64> {
    kendall_tau_b(predicted, gold).map(|tau| (tau + 1.0) / 2.0)
}

/// BEAM's rubric score: the mean of the per-nugget grades **after each is clamped** onto the
/// benchmark's three-point scale.
///
/// Clamping per nugget rather than once over the mean is what the benchmark specifies, and the
/// two differ in a way that matters: grades of [0.4, 0.4] are 0.0 + 0.0 → **0.0** clamped per
/// nugget, against a mean of 0.4 → **0.5** clamped once. Clamping once would hand out half
/// credit for two nuggets the judge said were each mostly absent.
///
/// `None` on an empty rubric rather than 0.0, so a question whose rubric failed to load is
/// visible as unmeasured instead of as a miss.
pub fn rubric_score(grades: &[f64]) -> Option<f64> {
    let clamped: Vec<f64> = grades.iter().copied().map(nugget_score).collect();
    mean(&clamped)
}

/// Days from 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's civil-days
/// algorithm), so date comparisons are integer arithmetic in one place rather than a
/// calendar library the crate does not carry.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A month name to its number, accepting the three-letter form the datasets use.
fn month_number(token: &str) -> Option<i64> {
    Some(match token.to_ascii_lowercase().trim_end_matches('.') {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    })
}

/// Read the **first** civil date out of free text, as epoch seconds.
///
/// Supported because these are the forms the datasets and their gold answers actually use:
/// `2024-03-15`, `2024/03/15`, `March 15, 2024`, `15 March 2024`, `Mar 15 2024`. Anything else
/// returns `None`, and every caller counts those separately — an unparsed date is not a wrong
/// date, it is a question this scorer declines to grade, and the judge must still see it.
///
/// Taking the *first* date in the text is a deliberate simplification with a known cost: an
/// answer that mentions a date before the answer's own date ("she started in January, so by
/// March 15 …") will read the earlier one. It is preferred over "the last date" because gold
/// answers lead with the answer, and over "all dates" because a single tolerance comparison is
/// explainable where a set-to-set match is not.
pub fn parse_date(text: &str) -> Option<i64> {
    let tokens: Vec<String> = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect();
    for (i, tok) in tokens.iter().enumerate() {
        // Numeric forms first: a leading year is unambiguous.
        let numeric = tok.trim_matches(|c: char| !c.is_ascii_digit() && c != '-' && c != '/');
        let parts: Vec<i64> = numeric
            .split(['-', '/'])
            .filter_map(|p| p.parse::<i64>().ok())
            .collect();
        if parts.len() == 3 && parts[0] >= 1000 && parts[0] <= 9999 {
            let (y, m, d) = (parts[0], parts[1], parts[2]);
            if (1..=12).contains(&m) && (1..=31).contains(&d) {
                return Some(days_from_civil(y, m, d) * 86_400);
            }
        }
        // `March 15, 2024` — a month name, then a day, then a year within the next two tokens.
        if let Some(m) = month_number(tok) {
            let day = tokens.get(i + 1).and_then(|t| digit_run(t));
            let year = tokens
                .get(i + 2)
                .and_then(|t| digit_run(t))
                .or_else(|| tokens.get(i + 3).and_then(|t| digit_run(t)));
            if let (Some(d), Some(y)) = (day, year)
                && (1..=31).contains(&d)
                && (1000..=9999).contains(&y)
            {
                return Some(days_from_civil(y, m, d) * 86_400);
            }
        }
        // `15 March 2024` — a day, then a month name, then a year within the next two tokens.
        if let Some(d) = digit_run(tok)
            && (1..=31).contains(&d)
            && let Some(m) = tokens.get(i + 1).and_then(|t| month_number(t))
            && let Some(y) = tokens
                .get(i + 2)
                .and_then(|t| digit_run(t))
                .filter(|y| (1000..=9999).contains(y))
        {
            return Some(days_from_civil(y, m, d) * 86_400);
        }
    }
    None
}

/// The leading integer of a token, ignoring surrounding punctuation and suffixes (`15th`).
fn digit_run(token: &str) -> Option<i64> {
    let digits: String = token
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<i64>().ok()
    }
}

/// Do two texts name the same date within the benchmark's tolerance?
///
/// `None` unless **both** sides parse, so an unparseable answer is handed to the judge rather
/// than scored as wrong — the same rule every intrinsic scorer here follows.
pub fn date_agreement(gold: &str, hypothesis: &str) -> Option<bool> {
    let g = parse_date(gold)?;
    let h = parse_date(hypothesis)?;
    Some(date_within_tolerance(h, g))
}

/// Do two texts state durations within the benchmark's tolerance?
///
/// The 50%-with-a-floor rule is LoCoMo's, and it is the reason small intervals must not be
/// compared exactly: "2 days" and "3 days" are granted credit, and a strict comparison would
/// call a correct answer wrong.
pub fn duration_agreement(gold: &str, hypothesis: &str) -> Option<bool> {
    let g = parse_duration(gold)?;
    let h = parse_duration(hypothesis)?;
    Some(duration_within_tolerance(h, g))
}

// --- the rubric judge, whose output the clamp above is applied to ------------------

/// The contract for grading an answer against a BEAM rubric **nugget by nugget**.
///
/// Per-nugget rather than holistic because the benchmark's scale is per nugget: it asks, for
/// each nugget independently, whether the answer covers it, then clamps that one grade. A
/// holistic verdict cannot be clamped without inventing a mapping the benchmark does not have.
///
/// The scale is stated as a continuous 0-1 per nugget, which is what the clamp expects: the
/// model reports how well each nugget is covered and `rubric_score` applies the benchmark's
/// thresholds. Asking the model to emit 0/0.5/1 directly would move the benchmark's own
/// decision into the model.
pub const RUBRIC_JUDGE_SYSTEM: &str = "You grade an answer against a list of rubric \
    nuggets from a benchmark. For EACH nugget, independently, report how well the answer \
    covers it as a number from 0 to 1: 1 means the nugget's fact is stated, 0 means it is \
    absent, and a middle value means the substance is present but incomplete or imprecise. \
    Judge only the nugget's own content; ignore style, length and ordering. Reply with JSON \
    only, of the form {\"grades\": [0.0, 1.0, ...]} with exactly one number per nugget, in \
    the order the nuggets were given.";

/// The user turn for [`RUBRIC_JUDGE_SYSTEM`]: the question, the answer, and the numbered rubric.
pub fn rubric_judge_user(question: &str, answer: &str, nuggets: &[String]) -> String {
    let numbered: String = nuggets
        .iter()
        .enumerate()
        .map(|(i, n)| format!("{}. {n}\n", i + 1))
        .collect();
    format!("Question:\n{question}\n\nAnswer under test:\n{answer}\n\nRubric nuggets:\n{numbered}")
}
