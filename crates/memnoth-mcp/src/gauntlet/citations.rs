// ============================================================================
// The correctness spine's attribution half — FAMA's "lucky but invalid" cell
// ============================================================================
//
// WHY THIS NEEDS A CITATION PROTOCOL. FAMA's whole contribution is the cell where an answer is
// *right* but reached through a memory no longer believed. No retrieval score and no
// answer-match score can express it: both look identical to a right answer reached properly.
// The only way to measure it is to ask the reader which memories it used and check those against
// what was believed at the question's time — which is why the reader emits citations when a run
// asks for attribution.
//
// The syntax is ours, because the benchmark does not define one. `[[memory:<name>]]` is chosen to
// be unambiguous in prose, recoverable from a model that ignores the instruction (an uncited
// answer scores as *uncited*, never as invalid, so a reader that refuses to cite cannot
// manufacture a forgetting rate), and renderable by a host as a footnote — so the protocol is
// not harness-only.

/// The marker a reader uses to cite a memory.
pub const CITATION_OPEN: &str = "[[memory:";
pub const CITATION_CLOSE: &str = "]]";

/// The memory names an answer cited, in order, de-duplicated.
///
/// A malformed marker yields nothing rather than a guess. The names returned are matched against
/// fact ids, and an invented id would be treated by `relies_on_invalid` as a citation the vault
/// does not hold — which it deliberately ignores. Under-counting citations is the safe direction:
/// it can only make the forgetting rate look *better*, so a reported problem is always real.
pub fn cited_memory_ids(answer: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = answer;
    while let Some(open) = rest.find(CITATION_OPEN) {
        let after = &rest[open + CITATION_OPEN.len()..];
        let Some(close) = after.find(CITATION_CLOSE) else {
            break;
        };
        let name = after[..close].trim();
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
        rest = &after[close + CITATION_CLOSE.len()..];
    }
    out
}

/// The attribution clause appended to a reader's system prompt when a run asks for citations.
///
/// Kept beside the parser it must agree with: a prompt and a parser that drift apart produce a
/// run reporting zero citations, which reads as a reader failure rather than as the harness bug
/// it is.
pub const CITATION_CLAUSE: &str = "After your answer, on its own line, list the memories you \
    relied on, each written exactly as [[memory:<the memory's name as it appears in the \
    evidence>]]. List only the memories you actually used. If you used none, write nothing.";
