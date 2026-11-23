//! ALG11 — typed `evidence_for` links and a local invalidation/review queue.
//!
//! This module answers one narrow question: when the source a decision rested on
//! changes revision, which *already accepted* facts must a human look at again?
//! It never edits, supersedes or rewrites an observation. The only writes it
//! makes are ADD-only [`MemoryEngine::add_memory_link`] rows and audit events:
//!
//! * an `evidence_for` link from a decision/observation entity to a source
//!   identity entity, whose `reason` carries the full `(symbol, file, revision)`
//!   triple as JSON;
//! * a `needs_review` link when the observed source revision differs from the
//!   revision recorded on that link.
//!
//! Both "source revision changed" signals are read through the engine's public
//! API — `entity_versions` via [`entity_history`] and the audit ledger via
//! [`AuditEvent`] values supplied by the caller. There is no SQL and no network
//! or model work here.
//!
//! # Invariants
//!
//! 1. A link is append-only. Re-linking the same decision/source at a new
//!    revision adds a new row; the old link is kept.
//! 2. `review_queue` is a *review* operation, not a repair operation. The fact
//!    text a decision holds is read, never written. `needs_review` is a new
//!    link plus a new audit event, so the accepted fact is still exactly what it
//!    was when it was accepted.
//! 3. Enqueue is idempotent: one `(source, observed_revision)` pair produces at
//!    most one `needs_review` mark per decision.

use std::collections::HashSet;

use anyhow::Result;
use memnoth_engine::{AuditEvent, EntityKind, MemoryEngine, entity_history, vault_timeline};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Link kind for a typed decision → source-identity edge.
pub const EVIDENCE_FOR: &str = "evidence_for";
/// Link kind for the ADD-only "a human must re-check this" mark.
pub const NEEDS_REVIEW: &str = "needs_review";
/// The `kind` tag used in revision reasons this module writes and reads.
pub const SOURCE_REVISION_KIND: &str = "source_revision";
/// Upper bound on the bulk timeline read used to enumerate candidate entities.
const TIMELINE_LIMIT: usize = 500;

/// The identity of a source a decision rests on.
///
/// `revision` is the immutable token (commit, content digest, symbol revision)
/// the fact was accepted against. It is compared, never assumed to still hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    /// Qualified symbol the fact was derived from.
    pub symbol: String,
    /// File that contained the symbol.
    pub file: String,
    /// Revision the fact was accepted against.
    pub revision: String,
}

/// Which public API reported that a source revision changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RevisionFeed {
    /// An `entity_versions` row journaled by a supersede.
    EntityVersions,
    /// An event read from the audit ledger.
    AuditLedger,
}

impl RevisionFeed {
    /// Stable string form used in the `needs_review` link reason.
    pub fn as_str(self) -> &'static str {
        match self {
            RevisionFeed::EntityVersions => "entity_versions",
            RevisionFeed::AuditLedger => "audit_ledger",
        }
    }
}

/// One `evidence_for` link as this module reads it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceLink {
    /// The `memory_links.id` of the typed edge.
    pub link_id: i64,
    /// Entity whose accepted fact depends on the source.
    pub decision_entity_id: i64,
    /// Endpoint entity that represents the source identity.
    pub source_entity_id: i64,
    /// The `(symbol, file, revision)` triple recorded on the link.
    pub source: SourceIdentity,
}

/// One item placed in the review queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewItem {
    /// Decision whose accepted fact needs a human re-check.
    pub decision_entity_id: i64,
    /// Source identity the decision recorded.
    pub source: SourceIdentity,
    /// Revision the fact was accepted against.
    pub recorded_revision: String,
    /// Revision observed in the journal or the ledger.
    pub observed_revision: String,
    /// Which public API surfaced the change.
    pub feed: RevisionFeed,
    /// The accepted fact text, verbatim and unchanged.
    pub fact_text: String,
    /// The ADD-only `needs_review` link id.
    pub mark_link_id: i64,
}

/// Build the reason string stored on a source-revision supersede.
fn revision_reason(source: &SourceIdentity, revision: &str) -> String {
    json!({
        "kind": SOURCE_REVISION_KIND,
        "revision": revision,
        "symbol": source.symbol,
        "file": source.file,
    })
    .to_string()
}

/// Extract a revision from a reason written by [`revision_reason`].
///
/// Reasons from other supersedes (or plain text reasons) are not revisions and
/// are deliberately ignored instead of being guessed at.
fn revision_from_reason(reason: &str) -> Option<String> {
    let value: Value = serde_json::from_str(reason).ok()?;
    if value.get("kind").and_then(Value::as_str) != Some(SOURCE_REVISION_KIND) {
        return None;
    }
    value
        .get("revision")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Read a source revision from audit event detail, if it is one.
///
/// Two shapes are understood: our own `evidence.source_revision` detail, which
/// carries the revision fields directly, and the engine's `entity.supersede`
/// detail, which carries the revision reason as a string.
fn source_revision_from_detail(detail: &Value, symbol: &str) -> Option<String> {
    if detail.get("kind").and_then(Value::as_str) == Some(SOURCE_REVISION_KIND)
        && detail.get("symbol").and_then(Value::as_str) == Some(symbol)
    {
        return detail
            .get("revision")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    let reason = detail.get("reason").and_then(Value::as_str)?;
    let parsed: Value = serde_json::from_str(reason).ok()?;
    if parsed.get("kind").and_then(Value::as_str) == Some(SOURCE_REVISION_KIND)
        && parsed.get("symbol").and_then(Value::as_str) == Some(symbol)
    {
        return parsed
            .get("revision")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    None
}

/// Record a typed `evidence_for` link from one decision entity to a source.
///
/// The source identity entity is created with the symbol as its name if it does
/// not exist yet. The triple is stored as JSON in the link's `reason`; the link
/// itself is ADD-only, so re-linking never erases the earlier evidence.
pub fn link_evidence_for(
    engine: &MemoryEngine,
    decision_entity_id: i64,
    source: &SourceIdentity,
    at: i64,
) -> Result<EvidenceLink> {
    let source_entity_id = engine
        .find_entities_by_name(&source.symbol, at)?
        .first()
        .map(|entity| entity.id)
        .map(Ok)
        .unwrap_or_else(|| engine.add_entity(EntityKind::Symbol, &source.symbol, 1.0, at))?;

    let reason = serde_json::to_string(source)?;
    let link_id = engine.add_memory_link(
        decision_entity_id,
        source_entity_id,
        EVIDENCE_FOR,
        &reason,
        at,
    )?;

    engine.emit_audit(
        "evidence.link",
        &source.symbol,
        "applied",
        json!({
            "decision_entity_id": decision_entity_id,
            "source_entity_id": source_entity_id,
            "link_id": link_id,
            "source": source,
        }),
        at,
    );

    Ok(EvidenceLink {
        link_id,
        decision_entity_id,
        source_entity_id,
        source: source.clone(),
    })
}

/// Record that a source moved to a new revision.
///
/// This is a normal `mem_supersede` of the source entity, so the engine journals
/// the change into `entity_versions` **and** emits its own `entity.supersede`
/// audit event. The reason is the structured revision marker, which is what the
/// review queue reads back. Nothing about the decision's fact is touched.
pub fn record_source_revision(
    engine: &MemoryEngine,
    source: &SourceIdentity,
    new_revision: &str,
    at: i64,
) -> Result<i64> {
    let reason = revision_reason(source, new_revision);
    let successor = engine.mem_supersede(
        &source.symbol,
        Some(&source.symbol),
        Some(&source.file),
        None,
        &reason,
        at,
    )?;
    engine.emit_audit(
        "evidence.source_revision",
        &source.symbol,
        "applied",
        json!({
            "kind": SOURCE_REVISION_KIND,
            "revision": new_revision,
            "symbol": source.symbol,
            "file": source.file,
            "successor_entity_id": successor.0,
        }),
        at,
    );
    Ok(successor.0)
}

/// Read back every `evidence_for` link in the vault.
///
/// `memory_links_for` is keyed by entity, so candidate entity ids come from the
/// public bulk timeline reader. Link ids are de-duplicated because a link is
/// visible from both of its endpoints.
fn all_evidence_links(engine: &MemoryEngine) -> Result<Vec<EvidenceLink>> {
    let timeline = vault_timeline(engine, TIMELINE_LIMIT)?;
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    let facts = timeline["facts"].as_array().into_iter().flatten();
    for fact in facts {
        let Some(entity_id) = fact["id"].as_i64() else {
            continue;
        };
        for row in engine.memory_links_for(entity_id)? {
            if row.3 != EVIDENCE_FOR || !seen.insert(row.0) {
                continue;
            }
            let Some(reason) = row.4.as_deref() else {
                continue;
            };
            let Ok(source) = serde_json::from_str::<SourceIdentity>(reason) else {
                continue;
            };
            links.push(EvidenceLink {
                link_id: row.0,
                decision_entity_id: row.1,
                source_entity_id: row.2,
                source,
            });
        }
    }
    links.sort_by_key(|link| link.link_id);
    Ok(links)
}

/// The newest source revision reported by `entity_versions` for `symbol`.
fn revision_from_entity_versions(engine: &MemoryEngine, symbol: &str) -> Result<Option<String>> {
    let history = entity_history(engine, symbol)?;
    let mut newest = None;
    for version in history["versions"].as_array().into_iter().flatten() {
        let Some(reason) = version["change_reason"].as_str() else {
            continue;
        };
        if let Some(revision) = revision_from_reason(reason) {
            newest = Some(revision);
        }
    }
    Ok(newest)
}

/// The newest source revision reported by the supplied audit events for `symbol`.
fn revision_from_audit(events: &[AuditEvent], symbol: &str) -> Option<String> {
    let mut newest = None;
    for event in events {
        if event.outcome == "failed" || event.outcome == "denied" {
            continue;
        }
        if let Some(revision) = source_revision_from_detail(&event.detail, symbol) {
            newest = Some(revision);
        }
    }
    newest
}

/// The accepted fact text for a decision, read only.
fn accepted_fact_text(engine: &MemoryEngine, decision_entity_id: i64, at: i64) -> Result<String> {
    let facts = memnoth_engine::vault_history::current_facts(engine, decision_entity_id, at)?;
    Ok(facts
        .iter()
        .filter_map(|fact| fact["fact"].as_str())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Whether this `(source, observed_revision)` was already marked for review.
fn already_marked(
    engine: &MemoryEngine,
    decision_entity_id: i64,
    source: &SourceIdentity,
    observed_revision: &str,
) -> Result<bool> {
    for row in engine.memory_links_for(decision_entity_id)? {
        if row.3 != NEEDS_REVIEW {
            continue;
        }
        let Some(raw) = row.4.as_deref() else {
            continue;
        };
        let Ok(mark) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if mark["observed_revision"].as_str() == Some(observed_revision)
            && mark["source"]["symbol"].as_str() == Some(source.symbol.as_str())
            && mark["source"]["file"].as_str() == Some(source.file.as_str())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Build the local review queue.
///
/// For every `evidence_for` link, the revision recorded on the link is compared
/// with revisions observed in `entity_versions` and in the supplied audit
/// events. On a mismatch the impacted decision is marked `needs_review` with a
/// new ADD-only link and a new audit event; the accepted fact text is read but
/// never rewritten. Calling this twice does not duplicate a mark.
pub fn review_queue(
    engine: &MemoryEngine,
    audit_events: &[AuditEvent],
    at: i64,
) -> Result<Vec<ReviewItem>> {
    let mut queue = Vec::new();
    for link in all_evidence_links(engine)? {
        let mut signals = Vec::new();
        if let Some(revision) = revision_from_entity_versions(engine, &link.source.symbol)? {
            signals.push((revision, RevisionFeed::EntityVersions));
        }
        if let Some(revision) = revision_from_audit(audit_events, &link.source.symbol) {
            signals.push((revision, RevisionFeed::AuditLedger));
        }

        let Some((observed_revision, feed)) = signals
            .into_iter()
            .find(|(revision, _)| revision != &link.source.revision)
        else {
            continue;
        };
        if already_marked(
            engine,
            link.decision_entity_id,
            &link.source,
            &observed_revision,
        )? {
            continue;
        }

        let fact_text = accepted_fact_text(engine, link.decision_entity_id, at)?;
        let reason = json!({
            "source": link.source,
            "recorded_revision": link.source.revision,
            "observed_revision": observed_revision,
            "feed": feed.as_str(),
        })
        .to_string();
        let mark_link_id = engine.add_memory_link(
            link.decision_entity_id,
            link.source_entity_id,
            NEEDS_REVIEW,
            &reason,
            at,
        )?;
        engine.emit_audit(
            "evidence.needs_review",
            &link.source.symbol,
            "applied",
            json!({
                "decision_entity_id": link.decision_entity_id,
                "recorded_revision": link.source.revision,
                "observed_revision": observed_revision,
                "feed": feed.as_str(),
                "mark_link_id": mark_link_id,
            }),
            at,
        );

        let recorded_revision = link.source.revision.clone();
        queue.push(ReviewItem {
            decision_entity_id: link.decision_entity_id,
            source: link.source,
            recorded_revision,
            observed_revision,
            feed,
            fact_text,
            mark_link_id,
        });
    }
    Ok(queue)
}
