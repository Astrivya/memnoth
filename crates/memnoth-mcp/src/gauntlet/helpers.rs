// ============================================================================
use super::*;

// small helpers
// ============================================================================

/// Render any JSON scalar as a string.
///
/// This one function exists because of a real, costly bug: 32 of LongMemEval's 500
/// gold answers are JSON **integers**, and `Value::as_str` returned `None` for each,
/// so the loader skipped them. They were mostly `temporal-reasoning` — the hardest
/// class — and the effect was to *raise* our published score while shrinking the set
/// from 500 to 91 per 100 groups. Any loader in this module uses this, never
/// `as_str`, so the class of bug cannot recur.
pub fn scalar_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// `Value`, or a `Value` parsed from a JSON-encoded string (HF stores dicts as text).
pub(crate) fn coerce_json(v: Value) -> Value {
    match &v {
        Value::String(s) => serde_json::from_str(s).unwrap_or(v),
        _ => v,
    }
}

/// Split one CSV line, honouring double quotes.
pub(crate) fn split_csv(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                fields.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    fields.push(cur);
    fields
}

/// PersonaMem joins its options either with `|` or with newlines inside a quoted field.
pub(crate) fn split_pipe(s: &str) -> Vec<String> {
    if s.contains('|') {
        s.split('|').map(str::to_string).collect()
    } else {
        s.split('\n').map(str::to_string).collect()
    }
}
