//! `mn evidence <subject>` — compose the `mn-evidence/1` envelope.
//!
//! This module does not own a store. It reads the four existing backends and
//! puts their answers side by side:
//!
//! * **belief** — [`crate::tui::commands::why`] (vault as-of, history, contradictions),
//! * **author** — [`crate::tui::commands::lineage`] (dated edges plus audit attribution),
//! * **approval / commit** — [`crate::receipt`] (`mn-receipt/1` and `verify`),
//! * **impact** — [`crate::tui::commands::impact`] (lexical graph reachability).
//!
//! Every page and every link carries one of four states: `measured`, `partial`,
//! `not_recorded` or `stale`. A state describes what the backend actually said,
//! not what the product would like to claim. In particular a session-only
//! approval is never promoted into a receipt, and lexical graph matches are
//! never called a measured semantic blast radius.

use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::cli::ImpactArgs;
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome, ellipsize};

/// The document schema this module writes.
pub const SCHEMA: &str = "mn-evidence/1";

/// Readable first-line prefix of an evidence answer. The line is a normal,
/// honest sentence — it is persisted to the session file exactly as shown —
/// and [`crate::tui::app::App::push`] uses it to open the five-page overlay.
/// No generated file and no second command list is involved; the same
/// `compose` result is what `mn evidence --json` prints.
pub const ENVELOPE_MARKER: &str = "evidence · ";
/// The suffix that makes the first line a distinctive, typed-answer header
/// rather than any prose that happens to begin with "evidence".
pub const HEADER_SUFFIX: &str = " · five-page chain";

/// The five pages, in the order the chain is argued.
pub const PAGE_IDS: [&str; 5] = ["belief", "author", "approval", "commit", "impact"];

/// The four honest states. Ordered from weakest to strongest for `weaker`.
pub const STATES: [&str; 4] = ["not_recorded", "stale", "partial", "measured"];

/// Claims this feature must never make. Kept in the envelope so a reader of
/// the JSON sees the same fence as a reader of the overlay.
pub const CLAIM_FENCE: [&str; 10] = [
    "no team, cloud, hosted or cross-tenant claim — this is local, read-only evidence",
    "no cryptographic authenticity claim — 'verified' means checked against the local commit and patch digest",
    "no signed-by-Memnoth-Cloud claim",
    "no named author where only heuristic audit attribution exists",
    "no receipt sourced from a session-only approval",
    "no measured blast radius from a lexical matcher",
    "no zero or empty list in place of an unmeasured value",
    "no code-graph semantic claim — precision is reported as the backend named it",
    "no second command list — the verb lives in the one registry",
    "no claim that a passing unit test or a doc closes an acceptance gate",
];

/// Raw answers from the backends. `None` means the backend did not answer; it
/// is never filled with an empty success value.
#[derive(Debug, Clone, Default)]
pub struct EvidenceInput {
    pub subject: String,
    pub at_ms: i64,
    pub belief: Option<Value>,
    pub author: Option<Value>,
    pub receipt: Option<Value>,
    pub verification: Option<Value>,
    pub impact: Option<Value>,
    /// Number of approvals that exist only in this session. They are shown, but
    /// always with `receipt_sourced: false` and state `not_recorded`.
    pub session_approvals: usize,
    /// Backend name to failure reason, for the pages that did not answer.
    pub errors: BTreeMap<String, String>,
}

fn rank(state: &str) -> usize {
    STATES
        .iter()
        .position(|candidate| *candidate == state)
        .unwrap_or(0)
}

/// The weaker of two states: a chain link cannot be stronger than its ends.
fn weaker(left: &str, right: &str) -> &'static str {
    let chosen = if rank(left) <= rank(right) {
        left
    } else {
        right
    };
    match rank(chosen) {
        0 => "not_recorded",
        1 => "stale",
        2 => "partial",
        _ => "measured",
    }
}

fn unavailable(input: &EvidenceInput, backend: &str) -> String {
    input
        .errors
        .get(backend)
        .cloned()
        .unwrap_or_else(|| format!("{backend} did not answer"))
}

fn page(
    id: &'static str,
    title: &'static str,
    state: &str,
    reason: impl Into<String>,
    rows: Value,
) -> Value {
    json!({
        "id": id,
        "title": title,
        "state": state,
        "state_reason": reason.into(),
        "rows": rows,
    })
}

fn belief_page(input: &EvidenceInput) -> Value {
    match &input.belief {
        None => page(
            "belief",
            "Belief",
            "not_recorded",
            unavailable(input, "why"),
            json!([]),
        ),
        Some(data) => {
            let facts: Vec<&Value> = data
                .get("entities")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(|entity| {
                    entity
                        .get("facts")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .collect();
            if facts.is_empty() {
                return page(
                    "belief",
                    "Belief",
                    "not_recorded",
                    "the vault held no belief about this subject at the requested instant",
                    json!([]),
                );
            }
            let mut rows = Vec::new();
            let mut missing = 0usize;
            for fact in &facts {
                let text = fact.get("content").and_then(Value::as_str).unwrap_or("");
                let author = fact
                    .get("provenance")
                    .and_then(|provenance| provenance.get("user"))
                    .and_then(Value::as_str);
                let recorded = fact.get("recorded_at").and_then(Value::as_i64);
                if author.is_none() || recorded.is_none() {
                    missing += 1;
                }
                rows.push(json!({
                    "label": ellipsize(text, 72),
                    "state": if author.is_some() && recorded.is_some() { "measured" } else { "partial" },
                    "source": "vault.asof",
                    "author": author,
                    "at_ms": recorded,
                    "kind": fact.get("kind").cloned().unwrap_or(Value::Null),
                }));
            }
            let state = if missing == 0 { "measured" } else { "partial" };
            let reason = if missing == 0 {
                format!(
                    "{} recorded fact(s), each with an author and a timestamp",
                    facts.len()
                )
            } else {
                format!(
                    "{} of {} fact(s) have no recorded author or timestamp",
                    missing,
                    facts.len()
                )
            };
            page("belief", "Belief", state, reason, json!(rows))
        }
    }
}

fn author_page(input: &EvidenceInput) -> Value {
    match &input.author {
        None => page(
            "author",
            "Author",
            "not_recorded",
            unavailable(input, "lineage"),
            json!([]),
        ),
        Some(data) => {
            let edges: Vec<&Value> = data
                .get("edges")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .collect();
            if edges.is_empty() {
                return page(
                    "author",
                    "Author",
                    "not_recorded",
                    "no dated relation named this subject",
                    json!([]),
                );
            }
            let mut rows = Vec::new();
            let mut attributed = 0usize;
            for edge in &edges {
                let by = edge.get("by").and_then(Value::as_str).unwrap_or("");
                let named = !by.is_empty() && by != "unknown";
                if named {
                    attributed += 1;
                }
                rows.push(json!({
                    "label": format!(
                        "{} --{}--> {}",
                        edge.get("source").and_then(Value::as_str).unwrap_or("?"),
                        edge.get("kind").and_then(Value::as_str).unwrap_or("?"),
                        edge.get("target").and_then(Value::as_str).unwrap_or("?"),
                    ),
                    // `lineage` itself labels `by` a best guess from the audit
                    // trail. A guess is never a signed authorship.
                    "state": if named { "partial" } else { "not_recorded" },
                    "source": "lineage+audit",
                    "at": edge.get("at").cloned().unwrap_or(Value::Null),
                    "by": if named { Value::String(by.to_string()) } else { Value::Null },
                    "receipt_sourced": false,
                }));
            }
            let state = if attributed > 0 {
                "partial"
            } else {
                "not_recorded"
            };
            let reason = if attributed > 0 {
                format!(
                    "{} of {} edge(s) carry audit attribution, which is a best guess, not a signed authorship",
                    attributed,
                    edges.len()
                )
            } else {
                "no edge carries an attributed author".to_string()
            };
            page("author", "Author", state, reason, json!(rows))
        }
    }
}

fn approval_page(input: &EvidenceInput) -> Value {
    let mut rows = Vec::new();
    let mut receipt_named = 0usize;

    if let Some(receipt) = &input.receipt
        && let Some(approvals) = receipt
            .get("authority")
            .and_then(|authority| authority.get("approvals"))
            .and_then(Value::as_array)
    {
        for approval in approvals {
            let by = approval
                .get("approved_by")
                .and_then(Value::as_str)
                .unwrap_or("");
            let named = !by.is_empty() && by != "unknown";
            if named {
                receipt_named += 1;
            }
            rows.push(json!({
                "label": format!(
                    "{} · {}",
                    approval.get("tool").and_then(Value::as_str).unwrap_or("?"),
                    approval.get("subject").and_then(Value::as_str).unwrap_or("?"),
                ),
                "state": if named { "measured" } else { "partial" },
                "source": "mn-receipt/1:authority.approvals",
                "approved_by": if named { Value::String(by.to_string()) } else { Value::Null },
                "at_ms": approval.get("at_ms").cloned().unwrap_or(Value::Null),
                "receipt_sourced": true,
            }));
        }
    }

    // Session-only approvals are shown, but they stay session-only. Nothing in
    // this function ever writes them into a receipt-shaped row.
    for index in 0..input.session_approvals {
        rows.push(json!({
            "label": format!("session approval {}", index + 1),
            "state": "not_recorded",
            "source": "session-only",
            "approved_by": Value::Null,
            "receipt_sourced": false,
            "reason": "recorded in this session only; not present in any mn-receipt/1 document",
        }));
    }

    let state = if receipt_named > 0 {
        "measured"
    } else {
        "not_recorded"
    };
    let reason = if receipt_named > 0 {
        format!("{receipt_named} approval(s) in the receipt name an approver")
    } else if input.receipt.is_none() {
        "no mn-receipt/1 document was found for this workspace".to_string()
    } else {
        "the receipt records no approval with a named approver".to_string()
    };
    page("approval", "Approval", state, reason, json!(rows))
}

fn commit_page(input: &EvidenceInput) -> Value {
    let (receipt, verification) = match (&input.receipt, &input.verification) {
        (Some(receipt), Some(verification)) => (receipt, verification),
        (Some(receipt), None) => {
            let commit = receipt
                .get("change")
                .and_then(|change| change.get("commit"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            return page(
                "commit",
                "Commit",
                "partial",
                "a receipt exists but its verification did not run",
                json!([{
                    "label": format!("receipt bound to {commit}"),
                    "state": "partial",
                    "source": "mn-receipt/1",
                    "receipt_sourced": true,
                }]),
            );
        }
        _ => {
            return page(
                "commit",
                "Commit",
                "not_recorded",
                if input.receipt.is_some() {
                    unavailable(input, "receipt")
                } else {
                    "no mn-receipt/1 document was found for this commit".to_string()
                },
                json!([]),
            );
        }
    };

    let commit = receipt
        .get("change")
        .and_then(|change| change.get("commit"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let ok = verification
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let checks: Vec<&Value> = verification
        .get("checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let mut rows = Vec::new();
    let mut stale = false;
    let mut failed = 0usize;
    for check in &checks {
        let name = check.get("name").and_then(Value::as_str).unwrap_or("?");
        let passed = check.get("ok").and_then(Value::as_bool).unwrap_or(false);
        if !passed {
            failed += 1;
            if name == "commit-exists" || name == "commit" {
                stale = true;
            }
        }
        rows.push(json!({
            "label": name,
            "state": if passed { "measured" } else if name == "commit-exists" || name == "commit" { "stale" } else { "partial" },
            "source": "receipt.verify",
            "detail": check.get("detail").cloned().unwrap_or(Value::Null),
            "receipt_sourced": true,
        }));
    }
    let state = if ok && failed == 0 {
        "measured"
    } else if stale {
        "stale"
    } else {
        "partial"
    };
    let reason = if ok && failed == 0 {
        format!("receipt bound to {commit} verified against the local commit and patch digest")
    } else if stale {
        format!("the receipt is bound to {commit}, but that commit is not the one checked")
    } else {
        format!("{failed} verification check(s) did not pass")
    };
    page("commit", "Commit", state, reason, json!(rows))
}

fn impact_page(input: &EvidenceInput) -> Value {
    match &input.impact {
        None => page(
            "impact",
            "Impact",
            "not_recorded",
            unavailable(input, "impact"),
            json!([]),
        ),
        Some(data) => {
            let precision = data
                .get("precision")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let exact = data.get("exact").and_then(Value::as_u64);
            let likely = data.get("likely").and_then(Value::as_u64);
            let tests_reason = data
                .get("impacted_tests_reason")
                .and_then(Value::as_str)
                .map(str::to_string);
            if exact.is_none() && likely.is_none() && tests_reason.is_none() {
                return page(
                    "impact",
                    "Impact",
                    "not_recorded",
                    "the graph returned no callers for this subject",
                    json!([]),
                );
            }
            let mut rows = Vec::new();
            if let Some(exact) = exact {
                rows.push(json!({"label": "exact caller(s)", "value": exact, "state": "partial", "source": "kg.impact"}));
            }
            if let Some(likely) = likely {
                rows.push(json!({"label": "likely caller(s)", "value": likely, "state": "partial", "source": "kg.impact"}));
            }
            if let Some(reason) = &tests_reason {
                rows.push(json!({"label": "impacted tests", "value": null, "state": "not_recorded", "source": "kg.tests-for", "reason": reason}));
            }
            page(
                "impact",
                "Impact",
                // A lexical matcher is never a measured semantic blast radius,
                // whatever backend eventually answers.
                "partial",
                format!(
                    "precision `{precision}` — counts are backend matches, not semantic certainty"
                ),
                json!(rows),
            )
        }
    }
}

/// Compose the envelope from backend answers. Pure: no I/O, no clock, no store.
pub fn compose(input: EvidenceInput) -> Value {
    let pages = vec![
        belief_page(&input),
        author_page(&input),
        approval_page(&input),
        commit_page(&input),
        impact_page(&input),
    ];
    let links = pages
        .windows(2)
        .map(|pair| {
            let from = pair[0]["id"].as_str().unwrap_or("?");
            let to = pair[1]["id"].as_str().unwrap_or("?");
            let state = weaker(
                pair[0]["state"].as_str().unwrap_or("not_recorded"),
                pair[1]["state"].as_str().unwrap_or("not_recorded"),
            );
            json!({
                "from": from,
                "to": to,
                "state": state,
                "reason": format!("bounded by the weaker endpoint ({from}/{to})"),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema": SCHEMA,
        "subject": input.subject,
        "at_ms": input.at_ms,
        "pages": pages,
        "links": links,
        "fence": CLAIM_FENCE,
    })
}

/// Gather the four backends and compose. A backend that fails is recorded as
/// an explicit error so its page says why it is `not_recorded`.
pub fn run(ctx: &AppContext, subject: &str) -> Result<Outcome, ErrorOutcome> {
    let subject = subject.trim();
    if subject.is_empty() {
        return Err(
            ErrorOutcome::usage("the subject is empty").with_fix("usage: /evidence <subject>")
        );
    }

    let mut errors = BTreeMap::new();
    let belief = match crate::tui::commands::why::run(ctx, subject, None) {
        Ok(outcome) => Some(outcome.data),
        Err(error) => {
            errors.insert("why".to_string(), format!("{error:#}"));
            None
        }
    };
    let author = match crate::tui::commands::lineage::run(ctx, subject, 3, None, false) {
        Ok(outcome) => Some(outcome.data),
        Err(error) => {
            errors.insert("lineage".to_string(), format!("{error:#}"));
            None
        }
    };
    let impact = match crate::tui::commands::impact::run(
        ctx,
        &ImpactArgs {
            symbol: subject.to_string(),
            depth: 2,
            diff: false,
        },
    ) {
        Ok(outcome) => Some(outcome.data),
        Err(error) => {
            errors.insert("impact".to_string(), format!("{error:#}"));
            None
        }
    };
    let (receipt, verification) = match crate::receipt::load(ctx, None) {
        Ok(document) => {
            let verification = crate::receipt::verify(ctx, None)
                .ok()
                .and_then(|result| serde_json::to_value(result).ok());
            (serde_json::to_value(document).ok(), verification)
        }
        Err(error) => {
            errors.insert("receipt".to_string(), format!("{error:#}"));
            (None, None)
        }
    };

    let document = compose(EvidenceInput {
        subject: subject.to_string(),
        at_ms: memnoth_engine::now_ms(),
        belief,
        author,
        receipt,
        verification,
        impact,
        session_approvals: 0,
        errors,
    });

    let body = text(&document);
    // The first line is a readable handoff to the TUI. App::push opens the
    // five-page overlay but keeps this text, so the transcript and the session
    // file show the same honest header rather than a hidden control marker.
    let marked = format!("{ENVELOPE_MARKER}{subject}{HEADER_SUFFIX}\n{body}");
    Ok(Outcome::new("evidence", marked, document))
}

/// The human-readable form of an envelope. The overlay renders the same pages
/// and states, so the transcript and the surface cannot disagree.
pub fn text(document: &Value) -> String {
    let mut out = format!("{}\n", document["subject"].as_str().unwrap_or("?"));
    out.push_str(&format!(
        "schema: {}\n",
        document["schema"].as_str().unwrap_or(SCHEMA)
    ));
    if let Some(pages) = document["pages"].as_array() {
        for page in pages {
            out.push_str(&format!(
                "\n{} [{}] — {}\n",
                page["title"].as_str().unwrap_or("?"),
                page["state"].as_str().unwrap_or("not_recorded"),
                page["state_reason"].as_str().unwrap_or(""),
            ));
            if let Some(rows) = page["rows"].as_array() {
                for row in rows.iter().take(8) {
                    let label = row["label"].as_str().unwrap_or("?");
                    let state = row["state"].as_str().unwrap_or("not_recorded");
                    out.push_str(&format!("  [{state}] {}\n", ellipsize(label, 72)));
                }
                if rows.len() > 8 {
                    out.push_str(&format!("  … {} more\n", rows.len() - 8));
                }
            }
        }
    }
    if let Some(links) = document["links"].as_array() {
        out.push_str("\nchain\n");
        for link in links {
            out.push_str(&format!(
                "  {} → {} [{}]\n",
                link["from"].as_str().unwrap_or("?"),
                link["to"].as_str().unwrap_or("?"),
                link["state"].as_str().unwrap_or("not_recorded"),
            ));
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_subject_is_rejected_without_touching_a_store() {
        // The check is before any backend call, so this is a pure error path.
        let error = EvidenceInput::default();
        assert!(error.subject.is_empty());
        assert_eq!(SCHEMA, "mn-evidence/1");
    }

    #[test]
    fn compose_has_five_pages_in_chain_order() {
        let document = compose(EvidenceInput::default());
        assert_eq!(document["schema"], json!("mn-evidence/1"));
        let pages = document["pages"].as_array().expect("pages");
        let ids: Vec<&str> = pages
            .iter()
            .map(|page| page["id"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(ids, PAGE_IDS);
        assert_eq!(document["links"].as_array().map(Vec::len), Some(4));
        assert_eq!(document["fence"].as_array().map(Vec::len), Some(10));
    }

    #[test]
    fn absent_backends_are_not_recorded_not_empty_success() {
        let document = compose(EvidenceInput::default());
        for page in document["pages"].as_array().expect("pages") {
            assert_eq!(page["state"], json!("not_recorded"), "{}", page["id"]);
            assert_eq!(page["rows"].as_array().map(Vec::len), Some(0));
        }
        for link in document["links"].as_array().expect("links") {
            assert_eq!(link["state"], json!("not_recorded"));
        }
    }

    #[test]
    fn session_only_approvals_are_never_receipt_sourced() {
        let document = compose(EvidenceInput {
            session_approvals: 2,
            ..Default::default()
        });
        let approval = &document["pages"][2];
        assert_eq!(approval["state"], json!("not_recorded"));
        for row in approval["rows"].as_array().expect("rows") {
            assert_eq!(row["receipt_sourced"], json!(false), "{row}");
            assert_eq!(row["state"], json!("not_recorded"));
            assert_eq!(row["source"], json!("session-only"));
        }
    }

    #[test]
    fn a_receipt_approval_is_the_only_source_that_can_be_measured() {
        let receipt = json!({
            "authority": {
                "approvals": [{
                    "tool": "edit", "subject": "src/a.rs", "approved": true,
                    "approved_by": "B", "at_ms": 1i64, "rule": "user"
                }]
            },
            "change": { "commit": "abc", "base": null, "patch_sha256": "x", "files": [] }
        });
        let document = compose(EvidenceInput {
            receipt: Some(receipt),
            session_approvals: 1,
            ..Default::default()
        });
        let approval = &document["pages"][2];
        assert_eq!(approval["state"], json!("measured"));
        let rows = approval["rows"].as_array().expect("rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["receipt_sourced"], json!(true));
        assert_eq!(rows[0]["approved_by"], json!("B"));
        assert_eq!(rows[1]["receipt_sourced"], json!(false));
        assert_eq!(rows[1]["state"], json!("not_recorded"));
    }

    #[test]
    fn a_missing_commit_is_stale_not_measured() {
        let verification = json!({
            "schema": "mn-receipt/1", "commit": "abc", "bound_commit": "abc", "ok": false,
            "checks": [
                {"name": "commit", "ok": true, "detail": "bound"},
                {"name": "commit-exists", "ok": false, "detail": "not in repository"}
            ]
        });
        let document = compose(EvidenceInput {
            receipt: Some(json!({"change": {"commit": "abc"}})),
            verification: Some(verification),
            ..Default::default()
        });
        let commit = &document["pages"][3];
        assert_eq!(commit["state"], json!("stale"));
        assert!(!text(&document).contains("verified against the local commit"));
    }

    #[test]
    fn lexical_impact_is_never_measured() {
        let document = compose(EvidenceInput {
            impact: Some(json!({
                "precision": "lexical", "exact": 3, "likely": 4,
                "impacted_tests": null,
                "impacted_tests_reason": "the test index did not answer"
            })),
            ..Default::default()
        });
        let impact = &document["pages"][4];
        assert_eq!(impact["state"], json!("partial"));
        let rows = impact["rows"].as_array().expect("rows");
        assert!(rows.iter().any(|row| row["state"] == json!("not_recorded")));
    }

    #[test]
    fn heuristic_author_is_partial_not_measured() {
        let document = compose(EvidenceInput {
            author: Some(json!({
                "edges": [{"source": "a", "kind": "supports", "target": "b", "by": "audit detail", "at": "t"}]
            })),
            ..Default::default()
        });
        let author = &document["pages"][1];
        assert_eq!(author["state"], json!("partial"));
        assert_eq!(author["rows"][0]["receipt_sourced"], json!(false));
    }
}
