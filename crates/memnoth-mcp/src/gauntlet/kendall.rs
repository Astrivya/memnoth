// ============================================================================
use super::*;

// Kendall τ-b — BEAM scores `event_ordering` as a ranking, not a string
// ============================================================================

/// Kendall's τ-b over two orderings of the same items.
///
/// BEAM evaluates reconstructed event sequences with τ-b, which means an answer that
/// gets every event right but two adjacent ones swapped scores high, while a
/// string-match scorer would call it wrong. Implementing `event_ordering` without
/// this function would make the ability permanently unreachable.
///
/// τ-b (rather than τ-a) is used so tied ranks are handled — a reader that emits two
/// events on the same date must not be punished for a tie the data itself has.
///
/// Returns `None` when fewer than two items are shared or the denominator is zero,
/// because a τ over one item is not a measurement.
pub fn kendall_tau_b(predicted: &[String], gold: &[String]) -> Option<f64> {
    let pred_rank: HashMap<&str, usize> = predicted
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();
    let gold_rank: HashMap<&str, usize> = gold
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();

    // Only items present in both lists can be compared, in gold order.
    let shared: Vec<&String> = gold
        .iter()
        .filter(|s| pred_rank.contains_key(s.as_str()))
        .collect();
    let n = shared.len();
    if n < 2 {
        return None;
    }

    let (mut concordant, mut discordant) = (0i64, 0i64);
    let (mut tie_x, mut tie_y) = (0i64, 0i64);
    for i in 0..n {
        for j in (i + 1)..n {
            let gx = gold_rank[shared[i].as_str()] as i64;
            let gy = gold_rank[shared[j].as_str()] as i64;
            let px = pred_rank[shared[i].as_str()] as i64;
            let py = pred_rank[shared[j].as_str()] as i64;
            let dx = (gx - gy).signum();
            let dy = (px - py).signum();
            if dx == 0 && dy == 0 {
                continue;
            } else if dx == 0 {
                tie_y += 1;
            } else if dy == 0 {
                tie_x += 1;
            } else if dx == dy {
                concordant += 1;
            } else {
                discordant += 1;
            }
        }
    }

    let n0 = (n * (n - 1) / 2) as f64;
    let denom = ((n0 - tie_x as f64) * (n0 - tie_y as f64)).sqrt();
    if denom <= 0.0 {
        return None;
    }
    Some((concordant - discordant) as f64 / denom)
}
