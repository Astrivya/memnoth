//! Selection-first compaction.
//!
//! The naive compactor asks a model to summarise the conversation. That is a *generation*
//! channel, and a generation channel cannot be relied on for exact answers: it can drop a
//! decision id, paraphrase a path, or round a version, while sounding perfectly confident.
//! The result is a summary that passes a human skim and fails a `grep` — which is precisely the
//! Codex #5772 failure ("after auto-compact it doesn't reread agents.md").
//!
//! The rule here is structural:
//!
//! * [`FactKind::is_exact`] facts — decisions, paths, symbols, versions, tasks, corrections — are
//!   **selected**, verbatim, from the store. They are never summarised and never dropped.
//! * Only [`FactKind::Narrative`] may be summarised: what was tried, how it felt, the story of the
//!   exploration. Losing detail there costs comfort, not correctness.
//! * [`retention_ok`] checks the outcome instead of trusting the process: every exact fact that
//!   went in must come out with the same id and the same bytes.
//!
//! A summary is therefore a *tail* artefact. The prefix keeps the facts.

use crate::budget::estimate_tokens;

/// What kind of thing a remembered fact is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FactKind {
    /// A choice and its reason. The most expensive thing to lose.
    Decision,
    /// A file path.
    Path,
    /// A qualified symbol name.
    Symbol,
    /// A commit, version, schema or protocol number.
    Version,
    /// Open work with an owner.
    Task,
    /// A user correction. Must survive so the same mistake is not repeated.
    Correction,
    /// Story: exploration, attempts, rationale prose.
    Narrative,
}

impl FactKind {
    /// Whether this kind must survive compaction byte-for-byte.
    ///
    /// Everything except narrative does. The test is not "is it important?" — importance is a
    /// judgement the compactor is bad at — it is "does an exact query depend on these bytes?".
    pub fn is_exact(self) -> bool {
        !matches!(self, FactKind::Narrative)
    }
}

/// One durable fact considered for the compacted context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    /// Stable id (`D-17`, `path:crates/x/src/y.rs`). Identity is what retention checks.
    pub id: String,
    pub kind: FactKind,
    pub text: String,
    /// Pinned facts are kept even if their kind would otherwise allow summarising.
    pub pinned: bool,
}

impl Fact {
    /// Build a fact from an id, kind, and text. Exact kinds are pinned by default.
    pub fn new(id: impl Into<String>, kind: FactKind, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            text: text.into(),
            pinned: kind.is_exact(),
        }
    }

    /// Build a narrative fact (not pinned, may be summarised).
    pub fn narrative(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: FactKind::Narrative,
            text: text.into(),
            pinned: false,
        }
    }

    fn must_survive(&self) -> bool {
        self.pinned || self.kind.is_exact()
    }
}

/// The result of compaction: what stays verbatim, and what may be rewritten.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub kept: Vec<Fact>,
    /// Narrative the caller may summarise. Empty means nothing needs a model call.
    pub summarised: Vec<Fact>,
    /// Tokens of the kept facts. May exceed the budget when exact facts alone do.
    pub tokens: usize,
}

impl Selection {
    /// Whether the exact facts alone overflow the budget.
    ///
    /// This is not an error: it is a signal to shorten a *narrative* or raise the budget, never to
    /// drop a decision. Callers should surface it rather than "fix" it by trimming facts.
    pub fn exact_over_budget(&self, budget_tokens: usize) -> bool {
        self.kept
            .iter()
            .filter(|fact| fact.must_survive())
            .map(|fact| estimate_tokens(&fact.text))
            .sum::<usize>()
            > budget_tokens
    }
}

/// Select what survives, in input order.
///
/// Exact and pinned facts are always kept. Narrative fills the remaining budget in the order it
/// was recorded; narrative that does not fit is returned in `summarised` for the caller to
/// condense into one block. If there is no room at all, the caller gets an empty `kept` narrative
/// list and a non-empty `summarised` one — which is the honest outcome, not a silent truncation.
pub fn select(facts: &[Fact], budget_tokens: usize) -> Selection {
    let mut selection = Selection::default();

    // Exact facts first, so a later narrative cannot consume their room.
    for fact in facts.iter().filter(|fact| fact.must_survive()) {
        selection.tokens += estimate_tokens(&fact.text);
        selection.kept.push(fact.clone());
    }

    for fact in facts.iter().filter(|fact| !fact.must_survive()) {
        let cost = estimate_tokens(&fact.text);
        if selection.tokens + cost <= budget_tokens {
            selection.tokens += cost;
            selection.kept.push(fact.clone());
        } else {
            selection.summarised.push(fact.clone());
        }
    }

    // Restore the caller's order: facts read in a different order than they were recorded is a
    // subtle way to change what the model attends to.
    let order: Vec<&str> = facts.iter().map(|fact| fact.id.as_str()).collect();
    selection.kept.sort_by_key(|fact| {
        order
            .iter()
            .position(|id| *id == fact.id.as_str())
            .unwrap_or(usize::MAX)
    });
    selection
}

/// Verify that compaction did not lose an exact fact.
///
/// Compares id **and** bytes: a summary that keeps `D-17` but rewrites its text has still lost the
/// decision. Returns the ids that are missing or altered.
pub fn retention_ok(before: &[Fact], after: &Selection) -> Result<(), Vec<String>> {
    let mut failures = Vec::new();
    for fact in before.iter().filter(|fact| fact.must_survive()) {
        match after.kept.iter().find(|kept| kept.id == fact.id) {
            Some(kept) if kept.text == fact.text => {}
            Some(_) => failures.push(format!("{} (text changed)", fact.id)),
            None => failures.push(format!("{} (missing)", fact.id)),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Vec<Fact> {
        vec![
            Fact::new(
                "D-17",
                FactKind::Decision,
                "Vault writes go through the lease",
            ),
            Fact::new(
                "path:store.rs",
                FactKind::Path,
                "crates/memnoth-engine/src/store.rs",
            ),
            Fact::new("v1.4", FactKind::Version, "schema v1.4"),
            Fact::narrative("n1", "tried a summary-first approach, it lost the path"),
            Fact::narrative("n2", "then tried selection, which worked"),
        ]
    }

    #[test]
    fn exact_facts_survive_a_budget_that_cannot_hold_them() {
        let selection = select(&facts(), 0);
        let ids: Vec<&str> = selection.kept.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"D-17"));
        assert!(ids.contains(&"path:store.rs"));
        assert!(ids.contains(&"v1.4"));
        assert!(selection.exact_over_budget(0));
    }

    #[test]
    fn narrative_is_summarised_only_when_the_budget_is_gone() {
        let selection = select(&facts(), 0);
        assert_eq!(selection.summarised.len(), 2);
        assert!(selection.kept.iter().all(|f| f.kind.is_exact()));
    }

    #[test]
    fn narrative_fits_when_there_is_room() {
        let all = facts();
        let exact_cost: usize = all
            .iter()
            .filter(|f| f.kind.is_exact())
            .map(|f| estimate_tokens(&f.text))
            .sum();
        let selection = select(&all, exact_cost + 100);
        assert!(selection.summarised.is_empty());
        assert_eq!(selection.kept.len(), all.len());
    }

    #[test]
    fn retention_passes_for_a_faithful_selection() {
        let all = facts();
        let selection = select(&all, 0);
        assert_eq!(retention_ok(&all, &selection), Ok(()));
    }

    #[test]
    fn retention_fails_when_a_decision_is_paraphrased() {
        let all = facts();
        let mut selection = select(&all, 0);
        selection.kept[0].text = "Vault writes use a lease".into();
        let failures = retention_ok(&all, &selection).unwrap_err();
        assert!(failures[0].contains("D-17"));
        assert!(failures[0].contains("text changed"));
    }

    #[test]
    fn retention_fails_when_an_exact_fact_is_missing() {
        let all = facts();
        let mut selection = select(&all, 0);
        selection.kept.retain(|fact| fact.id != "v1.4");
        let failures = retention_ok(&all, &selection).unwrap_err();
        assert!(failures.iter().any(|failure| failure.contains("v1.4")));
    }

    #[test]
    fn a_pinned_narrative_also_must_survive() {
        let all = vec![Fact {
            id: "n3".into(),
            kind: FactKind::Narrative,
            text: "the reason we chose selection".into(),
            pinned: true,
        }];
        let selection = select(&all, 0);
        assert_eq!(selection.kept.len(), 1);
        assert_eq!(retention_ok(&all, &selection), Ok(()));
    }

    #[test]
    fn kept_facts_preserve_the_recorded_order() {
        let all = facts();
        let selection = select(&all, 1_000);
        let ids: Vec<&str> = selection.kept.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["D-17", "path:store.rs", "v1.4", "n1", "n2"]);
    }

    #[test]
    fn selection_is_deterministic() {
        let a = select(&facts(), 40);
        let b = select(&facts(), 40);
        assert_eq!(a, b);
    }
}
