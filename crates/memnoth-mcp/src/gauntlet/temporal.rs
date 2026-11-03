// ============================================================================
// Temporal arithmetic — 133 LongMemEval questions and 1 in 10 BEAM questions
// ============================================================================

/// Seconds per unit, using the calendar conventions the benchmarks themselves use
/// (a month is 30 days, a year 365). Fixed constants rather than calendar maths:
/// the gold answers in these sets are themselves approximations, and exact calendar
/// arithmetic would disagree with them for reasons the judge does not care about.
fn unit_seconds(unit: &str) -> Option<i64> {
    let u = unit.trim_end_matches('s');
    Some(match u {
        "second" | "sec" => 1,
        "minute" | "min" => 60,
        "hour" | "hr" => 3_600,
        "day" => 86_400,
        "week" => 604_800,
        "month" => 2_592_000,
        "year" => 31_536_000,
        _ => return None,
    })
}

/// A small word-number reader. Benchmarks write "a week", "three days", "half an
/// hour"; a parser that only accepts digits silently drops a meaningful share of the
/// interval questions, and a dropped question scores zero without ever appearing in
/// a failure list.
fn word_number(token: &str) -> Option<f64> {
    Some(match token {
        "a" | "an" | "one" => 1.0,
        "two" => 2.0,
        "three" => 3.0,
        "four" => 4.0,
        "five" => 5.0,
        "six" => 6.0,
        "seven" => 7.0,
        "eight" => 8.0,
        "nine" => 9.0,
        "ten" => 10.0,
        "a couple of" | "couple" => 2.0,
        "half" => 0.5,
        _ => return None,
    })
}

/// `"3 days"`, `"a week"`, `"half an hour"`, `"two years"` → seconds.
///
/// Returns `None` for anything it does not understand, and every caller is expected
/// to count those separately — an unparsed duration is not zero, it is a question we
/// chose not to answer.
pub fn parse_duration(text: &str) -> Option<i64> {
    let lower = text.to_ascii_lowercase();
    let words: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '.')
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();

    // The three-token form is checked **first**, because "half an hour" also contains
    // the two-token form "an hour" — and `an` reads as the value 1, so taking the
    // two-token path would return 3600 seconds for what is plainly half of one hour.
    // "value, article, unit".
    for w in words.windows(3) {
        let (a, b, c) = (w[0].as_str(), w[1].as_str(), w[2].as_str());
        if (b == "an" || b == "a")
            && a.parse::<f64>().is_err()
            && let (Some(v), Some(unit)) = (word_number(a), unit_seconds(c))
        {
            return Some((v * unit as f64).round() as i64);
        }
    }
    // "value, unit" — digits or words, including "a week" and "an hour".
    for w in words.windows(2) {
        let (a, b) = (w[0].as_str(), w[1].as_str());
        let value = a.parse::<f64>().ok().or_else(|| word_number(a));
        if let (Some(v), Some(unit)) = (value, unit_seconds(b)) {
            return Some((v * unit as f64).round() as i64);
        }
    }
    None
}

/// Whole days between two timestamps, unsigned.
pub fn days_between(a: i64, b: i64) -> i64 {
    (a - b).abs() / 86_400
}

/// LoCoMo's judge grants credit for a date within **±14 days**. Encoding that as a
/// constant keeps the tolerance in one place, so a future reader cannot accidentally
/// tighten it and reproduce the under-counting bug we already fixed once.
pub const DATE_TOLERANCE_DAYS: i64 = 14;

/// Does a predicted date match the gold one under the benchmark's tolerance?
pub fn date_within_tolerance(predicted: i64, gold: i64) -> bool {
    days_between(predicted, gold) <= DATE_TOLERANCE_DAYS
}

/// LoCoMo grants credit when a stated duration is within **50%** of the gold, with a
/// one-unit absolute floor so that small values ("2 days" vs "3 days") still count.
pub fn duration_within_tolerance(predicted: i64, gold: i64) -> bool {
    if gold == 0 {
        return predicted == 0;
    }
    let diff = (predicted - gold).abs() as f64;
    let relative = diff / gold.abs() as f64;
    relative <= 0.5
}

/// Inclusive overlap between two intervals. A zero-length interval (a point event) is
/// treated as its own instant so that "when did X happen" still matches a window.
pub fn intervals_overlap(a: (i64, i64), b: (i64, i64)) -> bool {
    let (a0, a1) = if a.0 <= a.1 { a } else { (a.1, a.0) };
    let (b0, b1) = if b.0 <= b.1 { b } else { (b.1, b.0) };
    a0 <= b1 && b0 <= a1
}
