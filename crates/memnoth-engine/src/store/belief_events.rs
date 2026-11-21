//! Append-only, hash-chained belief-events ledger (VLT-05).
//!
//! The vault journals supersedes and revokes inside `entities` / `entity_versions`,
//! and the optional audit destination writes a hash-chained JSONL sidecar. Neither
//! answers "which decision put this belief here, who approved it, what did it
//! replace, and what changed" *inside the same commit as the write*. This module
//! is that in-database ledger.
//!
//! Two properties make it worth keeping:
//!
//! 1. **Commit atomicity.** [`append_belief_event`] takes a `&Transaction`, so the
//!    event and the belief change it describes commit or roll back together. A
//!    decision that reached the vault always has a row here; a rolled-back write
//!    leaves none.
//! 2. **Tamper evidence.** Events are chained with the exact same canonical
//!    encoding and SHA-256 seal as `audit.rs`: [`crate::audit::seal_hash`] is
//!    called directly, so there is no second preimage format to keep in sync.
//!    [`verify_belief_chain`] reports the first sequence number whose hash,
//!    sealed binding fields, or chain link fails.
//!
//! # What a row binds
//!
//! belief (entity / observation / proposal ids and content hash) → author →
//! time (`at`) → approval (`approved_by` / `approved_at`) → supersession
//! (`supersedes_id` / `successor_id`) → impact (structured JSON detail and
//! reason). Every one of those values except the four preimage fields is also
//! copied into the sealed `detail_json`; [`BeliefEvent::fields_agree`] checks the
//! columns against that sealed copy, so editing a column is detectable even if
//! a hash is recomputed.
//!
//! # What this cannot prove
//!
//! See [`NOT_MODELLED`]. In particular the ledger attests that a decision was
//! *recorded*; it cannot prove a past `DELETE`, that an author string is a real
//! identity, that a caller-supplied clock is correct, or that the head of the
//! chain was not truncated. Those stay NotModelled rather than being counted as
//! compliant.
//!
//! # Open-core boundary
//!
//! This is a public crate. Nothing in this file names a private path, a private
//! crate, a tenant, an entitlement, a seat, a plan, or a billing concept.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// DDL for the ledger. Paste this block into `store/schema.rs`'s `SCHEMA_SQL`
/// (see the wiring diff in the design report). Every statement is `IF NOT
/// EXISTS`, so opening an older vault adds the table without a version bump.
///
/// There are deliberately no `FOREIGN KEY` constraints on the id columns: the
/// ledger must remain readable even if a referenced row is gone, because "the
/// decision was recorded" is the fact this table owns, not "the row still
/// exists".
pub const BELIEF_EVENTS_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS belief_events (
    seq            INTEGER PRIMARY KEY,
    at             INTEGER NOT NULL,
    action         TEXT    NOT NULL,
    author         TEXT    NOT NULL,
    target         TEXT    NOT NULL,
    outcome        TEXT    NOT NULL,
    entity_id      INTEGER,
    observation_id INTEGER,
    proposal_id    INTEGER,
    content_hash   TEXT,
    approved_by    TEXT,
    approved_at    INTEGER,
    supersedes_id  INTEGER,
    successor_id   INTEGER,
    reason         TEXT,
    detail_json    TEXT    NOT NULL,
    prev           TEXT    NOT NULL,
    hash           TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_belief_events_entity     ON belief_events(entity_id, seq);
CREATE INDEX IF NOT EXISTS idx_belief_events_author     ON belief_events(author, seq);
CREATE INDEX IF NOT EXISTS idx_belief_events_action     ON belief_events(action, seq);
CREATE INDEX IF NOT EXISTS idx_belief_events_observation ON belief_events(observation_id);
"#;

/// Claims the chain records but cannot prove. Keep these as NotModelled in any
/// gate that reads the ledger; do not turn them into passes.
pub const NOT_MODELLED: &[&str] = &[
    "A past DELETE cannot be proven: the ledger attests a decision was recorded, not that the referenced row still exists or that no row was removed afterward.",
    "Author and approver strings are caller-supplied claims, not cryptographic identities; the chain proves they were written down, not that they are real people.",
    "Wall-clock `at` is supplied by the caller; the chain proves ordering as recorded, not clock correctness.",
    "Truncation of the whole head of the chain is undetectable from the database alone; an external checkpoint would be needed.",
];

/// Column list shared by every read so the row mapper cannot drift between
/// verification and query helpers.
const SELECT_COLUMNS: &str = "seq, at, action, author, target, outcome, entity_id, \
     observation_id, proposal_id, content_hash, approved_by, approved_at, \
     supersedes_id, successor_id, reason, detail_json, prev, hash";

/// One sealed decision in the chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeliefEvent {
    /// 1-based position in the chain.
    pub seq: u64,
    /// Wall-clock milliseconds supplied by the caller (already normalized).
    pub at: i64,
    /// What happened, e.g. `belief.approved`, `belief.superseded`, `belief.revoked`.
    pub action: String,
    /// Who authored or proposed the belief.
    pub author: String,
    /// Entity name or subject the decision was about (a preimage field).
    pub target: String,
    /// `applied`, `degraded_add`, or another outcome label.
    pub outcome: String,
    /// Entity the decision touched, when there is one.
    pub entity_id: Option<i64>,
    /// Observation the decision created, when there is one.
    pub observation_id: Option<i64>,
    /// Proposal the decision resolved, when there is one.
    pub proposal_id: Option<i64>,
    /// Content hash of the belief, when one is known.
    pub content_hash: Option<String>,
    /// Who approved the belief, when approval happened.
    pub approved_by: Option<String>,
    /// When approval happened, when it happened.
    pub approved_at: Option<i64>,
    /// Entity this decision superseded, when there is one.
    pub supersedes_id: Option<i64>,
    /// Entity that superseded this one, when there is one.
    pub successor_id: Option<i64>,
    /// Human-readable reason, redacted before sealing.
    pub reason: Option<String>,
    /// Exact JSON string sealed into `hash`; also binds the columns above.
    pub detail_json: String,
    /// Previous event's hash (`""` for the first event).
    pub prev: String,
    /// This event's SHA-256 hash.
    pub hash: String,
}

impl BeliefEvent {
    /// Recompute the hash from the seven preimage fields, exactly as
    /// `audit.rs` does for `AuditEvent`.
    pub fn hash_matches(&self) -> bool {
        crate::audit::seal_hash(
            self.seq,
            self.at,
            &self.action,
            &self.target,
            &self.outcome,
            &self.detail_json,
            &self.prev,
        ) == self.hash
    }

    /// Check the binding columns against the sealed `detail_json`.
    ///
    /// The four fields passed separately to `seal_hash` (`seq`, `at`, `action`,
    /// `target`, `outcome`, `prev`) are already covered by `hash_matches`; this
    /// covers the rest, so a recomputed hash over edited columns still fails.
    pub fn fields_agree(&self) -> bool {
        let Ok(detail) = serde_json::from_str::<Value>(&self.detail_json) else {
            return false;
        };
        let text = |key: &str| detail.get(key).and_then(Value::as_str);
        let int = |key: &str| detail.get(key).and_then(Value::as_i64);
        text("author") == Some(self.author.as_str())
            && int("entity_id") == self.entity_id
            && int("observation_id") == self.observation_id
            && int("proposal_id") == self.proposal_id
            && text("content_hash") == self.content_hash.as_deref()
            && text("approved_by") == self.approved_by.as_deref()
            && int("approved_at") == self.approved_at
            && int("supersedes_id") == self.supersedes_id
            && int("successor_id") == self.successor_id
            && text("reason") == self.reason.as_deref()
    }

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            seq: row.get::<_, i64>(0)? as u64,
            at: row.get(1)?,
            action: row.get(2)?,
            author: row.get(3)?,
            target: row.get(4)?,
            outcome: row.get(5)?,
            entity_id: row.get(6)?,
            observation_id: row.get(7)?,
            proposal_id: row.get(8)?,
            content_hash: row.get(9)?,
            approved_by: row.get(10)?,
            approved_at: row.get(11)?,
            supersedes_id: row.get(12)?,
            successor_id: row.get(13)?,
            reason: row.get(14)?,
            detail_json: row.get(15)?,
            prev: row.get(16)?,
            hash: row.get(17)?,
        })
    }
}

/// Inputs for one ledger append. Use [`BeliefEventInput::new`] then the builder
/// setters; every caller-supplied string is redacted with `audit::redact`
/// before it is sealed or stored.
#[derive(Debug, Clone)]
pub struct BeliefEventInput<'a> {
    action: &'a str,
    author: &'a str,
    target: &'a str,
    outcome: &'a str,
    at: i64,
    entity_id: Option<i64>,
    observation_id: Option<i64>,
    proposal_id: Option<i64>,
    content_hash: Option<&'a str>,
    approved_by: Option<&'a str>,
    approved_at: Option<i64>,
    supersedes_id: Option<i64>,
    successor_id: Option<i64>,
    reason: Option<&'a str>,
    impact: Value,
}

impl<'a> BeliefEventInput<'a> {
    /// A minimal event: what happened, who did it, what it touched, when, and
    /// the structured impact. All other bindings are optional.
    pub fn new(
        action: &'a str,
        author: &'a str,
        target: &'a str,
        outcome: &'a str,
        at: i64,
        impact: Value,
    ) -> Self {
        Self {
            action,
            author,
            target,
            outcome,
            at,
            entity_id: None,
            observation_id: None,
            proposal_id: None,
            content_hash: None,
            approved_by: None,
            approved_at: None,
            supersedes_id: None,
            successor_id: None,
            reason: None,
            impact,
        }
    }

    /// Entity the decision touched.
    pub fn entity_id(mut self, id: i64) -> Self {
        self.entity_id = Some(id);
        self
    }

    /// Observation the decision created.
    pub fn observation_id(mut self, id: i64) -> Self {
        self.observation_id = Some(id);
        self
    }

    /// Proposal the decision resolved.
    pub fn proposal_id(mut self, id: i64) -> Self {
        self.proposal_id = Some(id);
        self
    }

    /// Content hash of the belief.
    pub fn content_hash(mut self, hash: &'a str) -> Self {
        self.content_hash = Some(hash);
        self
    }

    /// Who approved the belief.
    pub fn approved_by(mut self, who: &'a str) -> Self {
        self.approved_by = Some(who);
        self
    }

    /// When the belief was approved.
    pub fn approved_at(mut self, at: i64) -> Self {
        self.approved_at = Some(at);
        self
    }

    /// Entity this decision replaced.
    pub fn supersedes_id(mut self, id: i64) -> Self {
        self.supersedes_id = Some(id);
        self
    }

    /// Entity that replaced this one.
    pub fn successor_id(mut self, id: i64) -> Self {
        self.successor_id = Some(id);
        self
    }

    /// Reason, redacted before sealing.
    pub fn reason(mut self, reason: &'a str) -> Self {
        self.reason = Some(reason);
        self
    }
}

/// Chain head recovered from an existing ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainHead {
    /// Number of events; 0 for an empty ledger.
    pub seq: u64,
    /// Hash of the last event; empty for an empty ledger.
    pub hash: String,
}

/// Read the chain head. A function on `&Connection` so it works both on the
/// engine's connection and on a caller's open transaction (which derefs to a
/// connection and therefore sees uncommitted rows).
pub fn chain_head(conn: &Connection) -> Result<ChainHead> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT seq, hash FROM belief_events ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        Some((seq, hash)) => ChainHead {
            seq: seq as u64,
            hash,
        },
        None => ChainHead {
            seq: 0,
            hash: String::new(),
        },
    })
}

/// Seal one event and insert it **inside the caller's transaction**.
///
/// The chain position and previous hash are read from the same transaction, so
/// the event cannot be attached to a head that another writer has moved. Pass
/// the transaction already wrapping `accept_proposal` / `supersede_entity` /
/// `revoke_entity`; committing it commits the belief and its proof together.
pub fn append_belief_event(
    tx: &Transaction<'_>,
    input: &BeliefEventInput<'_>,
) -> Result<BeliefEvent> {
    let head = chain_head(tx)?;
    let seq = head.seq + 1;

    let action = crate::audit::redact(input.action);
    let author = crate::audit::redact(input.author);
    let target = crate::audit::redact(input.target);
    let outcome = crate::audit::redact(input.outcome);
    let content_hash = input.content_hash.map(crate::audit::redact);
    let approved_by = input.approved_by.map(crate::audit::redact);
    let reason = input.reason.map(crate::audit::redact);

    let detail = crate::audit::redact_value(&json!({
        "author": author,
        "entity_id": input.entity_id,
        "observation_id": input.observation_id,
        "proposal_id": input.proposal_id,
        "content_hash": content_hash,
        "approved_by": approved_by,
        "approved_at": input.approved_at,
        "supersedes_id": input.supersedes_id,
        "successor_id": input.successor_id,
        "reason": reason,
        "impact": input.impact,
    }));
    let detail_json = serde_json::to_string(&detail)?;
    let hash = crate::audit::seal_hash(
        seq,
        input.at,
        &action,
        &target,
        &outcome,
        &detail_json,
        &head.hash,
    );

    tx.execute(
        "INSERT INTO belief_events
            (seq, at, action, author, target, outcome, entity_id, observation_id,
             proposal_id, content_hash, approved_by, approved_at, supersedes_id,
             successor_id, reason, detail_json, prev, hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18)",
        params![
            seq as i64,
            input.at,
            action,
            author,
            target,
            outcome,
            input.entity_id,
            input.observation_id,
            input.proposal_id,
            content_hash,
            approved_by,
            input.approved_at,
            input.supersedes_id,
            input.successor_id,
            reason,
            detail_json,
            head.hash,
            hash,
        ],
    )
    .with_context(|| format!("append belief event seq {seq}"))?;

    Ok(BeliefEvent {
        seq,
        at: input.at,
        action,
        author,
        target,
        outcome,
        entity_id: input.entity_id,
        observation_id: input.observation_id,
        proposal_id: input.proposal_id,
        content_hash,
        approved_by,
        approved_at: input.approved_at,
        supersedes_id: input.supersedes_id,
        successor_id: input.successor_id,
        reason,
        detail_json,
        prev: head.hash,
        hash,
    })
}

/// Verdict of [`verify_belief_chain`]. Mirrors `audit::AuditVerification` so a
/// caller can render both with the same shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeliefChainVerification {
    /// Events read.
    pub events: u64,
    /// `true` when every hash, binding and link is intact.
    pub ok: bool,
    /// First sequence number that fails, when `ok == false`.
    pub first_bad_seq: Option<u64>,
    /// Why it failed.
    pub reason: Option<String>,
}

/// Recompute the whole chain and report the first fault. Read-only; it never
/// repairs or rewrites a row, so a verification run cannot launder a tampered
/// ledger.
pub fn verify_belief_chain(conn: &Connection) -> Result<BeliefChainVerification> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM belief_events ORDER BY seq ASC"
    ))?;
    let rows = stmt.query_map([], BeliefEvent::from_row)?;
    let mut expected_prev = String::new();
    let mut events = 0u64;

    for row in rows {
        // The expected sequence is a function of how many rows were already
        // verified, not an independent counter that could drift from `events`.
        let expected_seq = events + 1;
        let event = match row {
            Ok(event) => event,
            Err(error) => {
                return Ok(BeliefChainVerification {
                    events,
                    ok: false,
                    first_bad_seq: Some(expected_seq),
                    reason: Some(format!("unreadable row: {error}")),
                });
            }
        };
        if event.seq != expected_seq {
            return Ok(BeliefChainVerification {
                events,
                ok: false,
                first_bad_seq: Some(expected_seq),
                reason: Some(format!("expected seq {expected_seq}, found {}", event.seq)),
            });
        }
        if !event.hash_matches() {
            return Ok(BeliefChainVerification {
                events,
                ok: false,
                first_bad_seq: Some(event.seq),
                reason: Some("contents do not match hash".to_string()),
            });
        }
        if !event.fields_agree() {
            return Ok(BeliefChainVerification {
                events,
                ok: false,
                first_bad_seq: Some(event.seq),
                reason: Some("a binding column disagrees with the sealed detail".to_string()),
            });
        }
        if event.prev != expected_prev {
            return Ok(BeliefChainVerification {
                events,
                ok: false,
                first_bad_seq: Some(event.seq),
                reason: Some("chain link broken".to_string()),
            });
        }
        expected_prev = event.hash;
        events += 1;
    }

    Ok(BeliefChainVerification {
        events,
        ok: true,
        first_bad_seq: None,
        reason: None,
    })
}

/// Read events newest first, optionally restricted by a single indexed column.
/// `limit == 0` means no limit. The `where` clause is one of three literals in
/// this file, never caller text.
pub fn belief_events_where(
    conn: &Connection,
    column: BeliefEventColumn,
    value: i64,
    limit: usize,
) -> Result<Vec<BeliefEvent>> {
    let sql = match column {
        BeliefEventColumn::Entity => "entity_id = ?1",
        BeliefEventColumn::Observation => "observation_id = ?1",
        BeliefEventColumn::Proposal => "proposal_id = ?1",
    };
    let query = if limit == 0 {
        format!("SELECT {SELECT_COLUMNS} FROM belief_events WHERE {sql} ORDER BY seq DESC")
    } else {
        format!("SELECT {SELECT_COLUMNS} FROM belief_events WHERE {sql} ORDER BY seq DESC LIMIT ?2")
    };
    let mut stmt = conn.prepare(&query)?;
    let params: Vec<rusqlite::types::Value> = if limit == 0 {
        vec![value.into()]
    } else {
        vec![value.into(), (limit as i64).into()]
    };
    let rows = stmt.query_map(rusqlite::params_from_iter(params), BeliefEvent::from_row)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Indexed columns the query helper accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeliefEventColumn {
    /// Filter by entity id.
    Entity,
    /// Filter by observation id.
    Observation,
    /// Filter by proposal id.
    Proposal,
}

/// Read up to `limit` events (all when `limit == 0`), newest first.
pub fn all_belief_events(conn: &Connection, limit: usize) -> Result<Vec<BeliefEvent>> {
    let query = if limit == 0 {
        format!("SELECT {SELECT_COLUMNS} FROM belief_events ORDER BY seq DESC")
    } else {
        format!("SELECT {SELECT_COLUMNS} FROM belief_events ORDER BY seq DESC LIMIT ?1")
    };
    let mut stmt = conn.prepare(&query)?;
    let rows = if limit == 0 {
        stmt.query_map([], BeliefEvent::from_row)?
    } else {
        stmt.query_map(params![limit as i64], BeliefEvent::from_row)?
    };
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Read events by author, newest first.
pub fn belief_events_by_author(
    conn: &Connection,
    author: &str,
    limit: usize,
) -> Result<Vec<BeliefEvent>> {
    let query = if limit == 0 {
        format!("SELECT {SELECT_COLUMNS} FROM belief_events WHERE author = ?1 ORDER BY seq DESC")
    } else {
        format!(
            "SELECT {SELECT_COLUMNS} FROM belief_events WHERE author = ?1 ORDER BY seq DESC LIMIT ?2"
        )
    };
    let mut stmt = conn.prepare(&query)?;
    let rows = if limit == 0 {
        stmt.query_map(params![author], BeliefEvent::from_row)?
    } else {
        stmt.query_map(params![author, limit as i64], BeliefEvent::from_row)?
    };
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().expect("in-memory vault");
        c.execute_batch(BELIEF_EVENTS_SCHEMA_SQL).expect("schema");
        c
    }

    fn approved(c: &Connection, author: &str, approved_by: &str, at: i64) -> BeliefEvent {
        let tx = c.unchecked_transaction().expect("tx");
        let event = append_belief_event(
            &tx,
            &BeliefEventInput::new(
                "belief.approved",
                author,
                "acme",
                "applied",
                at,
                json!({"observation_created": true}),
            )
            .entity_id(7)
            .observation_id(11)
            .proposal_id(3)
            .content_hash("abc123")
            .approved_by(approved_by)
            .approved_at(at)
            .reason("teammate report"),
        )
        .expect("append");
        tx.commit().expect("commit");
        event
    }

    #[test]
    fn an_empty_ledger_verifies_and_has_an_empty_head() {
        let c = conn();
        assert_eq!(
            chain_head(&c).unwrap(),
            ChainHead {
                seq: 0,
                hash: String::new()
            }
        );
        let verdict = verify_belief_chain(&c).unwrap();
        assert!(verdict.ok, "{verdict:?}");
        assert_eq!(verdict.events, 0);
        assert!(all_belief_events(&c, 0).unwrap().is_empty());
    }

    #[test]
    fn a_chain_verifies_and_its_bindings_are_queryable() {
        let c = conn();
        let a = approved(&c, "agent-1", "priya", 1000);
        let b = {
            let tx = c.unchecked_transaction().unwrap();
            let e = append_belief_event(
                &tx,
                &BeliefEventInput::new(
                    "belief.superseded",
                    "priya",
                    "acme",
                    "applied",
                    1001,
                    json!({"facts_copied": 2}),
                )
                .entity_id(8)
                .supersedes_id(7)
                .successor_id(8)
                .reason("rename"),
            )
            .unwrap();
            tx.commit().unwrap();
            e
        };
        let verdict = verify_belief_chain(&c).unwrap();
        assert!(verdict.ok, "{verdict:?}");
        assert_eq!(verdict.events, 2);
        assert_eq!(b.prev, a.hash);

        assert_eq!(a.approved_by.as_deref(), Some("priya"));
        assert_eq!(a.approved_at, Some(1000));
        assert_eq!(a.content_hash.as_deref(), Some("abc123"));
        assert!(a.fields_agree());
        assert!(a.hash_matches());
        let by_entity = belief_events_where(&c, BeliefEventColumn::Entity, 7, 0).unwrap();
        assert_eq!(by_entity.len(), 1);
        assert_eq!(by_entity[0].seq, 1);
        assert_eq!(belief_events_by_author(&c, "priya", 0).unwrap()[0].seq, 2);
        assert_eq!(all_belief_events(&c, 1).unwrap()[0].seq, 2);
    }

    #[test]
    fn editing_a_sealed_binding_column_is_detected() {
        let c = conn();
        approved(&c, "agent-1", "priya", 1000);
        c.execute(
            "UPDATE belief_events SET author = 'mallory' WHERE seq = 1",
            [],
        )
        .unwrap();
        let verdict = verify_belief_chain(&c).unwrap();
        assert!(!verdict.ok);
        assert_eq!(verdict.first_bad_seq, Some(1));
        assert!(verdict.reason.unwrap().contains("binding column"));
    }

    #[test]
    fn editing_the_detail_json_is_detected_by_the_hash() {
        let c = conn();
        approved(&c, "agent-1", "priya", 1000);
        c.execute(
            "UPDATE belief_events SET detail_json = REPLACE(detail_json, 'agent-1', 'mallory') WHERE seq = 1",
            [],
        )
        .unwrap();
        let verdict = verify_belief_chain(&c).unwrap();
        assert!(!verdict.ok);
        assert_eq!(verdict.first_bad_seq, Some(1));
        assert!(verdict.reason.unwrap().contains("hash"));
    }

    #[test]
    fn removing_a_middle_event_leaves_a_visible_gap() {
        let c = conn();
        approved(&c, "agent-1", "priya", 1000);
        approved(&c, "agent-2", "priya", 1001);
        approved(&c, "agent-3", "priya", 1002);
        // Production code never deletes; the test does, to prove a gap is seen.
        c.execute("DELETE FROM belief_events WHERE seq = 2", [])
            .unwrap();
        let verdict = verify_belief_chain(&c).unwrap();
        assert!(!verdict.ok);
        assert_eq!(verdict.first_bad_seq, Some(2));
    }

    #[test]
    fn a_spliced_event_breaks_the_previous_link() {
        let c = conn();
        let a = approved(&c, "agent-1", "priya", 1000);
        let b = approved(&c, "agent-2", "priya", 1001);
        c.execute(
            "UPDATE belief_events SET prev = ?1 WHERE seq = 2",
            params![b.prev.clone() + "deadbeef"],
        )
        .unwrap();
        let verdict = verify_belief_chain(&c).unwrap();
        assert!(!verdict.ok);
        assert_eq!(verdict.first_bad_seq, Some(2));
        assert_eq!(a.seq, 1);
    }

    #[test]
    fn identical_inputs_seal_to_the_identical_hash() {
        let c = conn();
        let a = approved(&c, "agent-1", "priya", 1000);
        let d = conn();
        let b = approved(&d, "agent-1", "priya", 1000);
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.hash.len(), 64);
    }

    #[test]
    fn event_and_belief_commit_or_roll_back_together() {
        let c = conn();
        {
            let tx = c.unchecked_transaction().unwrap();
            append_belief_event(
                &tx,
                &BeliefEventInput::new(
                    "belief.approved",
                    "agent-1",
                    "acme",
                    "applied",
                    1,
                    json!({}),
                ),
            )
            .unwrap();
            tx.rollback().unwrap();
        }
        assert_eq!(chain_head(&c).unwrap().seq, 0);
        assert!(verify_belief_chain(&c).unwrap().ok);
        approved(&c, "agent-1", "priya", 2);
        assert_eq!(chain_head(&c).unwrap().seq, 1);
    }

    #[test]
    fn secrets_are_redacted_before_sealing() {
        let c = conn();
        let tx = c.unchecked_transaction().unwrap();
        let event = append_belief_event(
            &tx,
            &BeliefEventInput::new(
                "belief.approved",
                "agent-1",
                "acme",
                "applied",
                3,
                json!({"note": "API_KEY=sk-live-123"}),
            )
            .reason("token=sk-live-123"),
        )
        .unwrap();
        tx.commit().unwrap();
        assert!(!event.detail_json.contains("sk-live-123"));
        assert!(event.detail_json.contains("***"));
        assert!(event.hash_matches());
        assert!(verify_belief_chain(&c).unwrap().ok);
    }

    #[test]
    fn the_seal_is_the_same_function_audit_uses() {
        let c = conn();
        let e = approved(&c, "agent-1", "priya", 1000);
        assert_eq!(
            e.hash,
            crate::audit::seal_hash(
                e.seq,
                e.at,
                &e.action,
                &e.target,
                &e.outcome,
                &e.detail_json,
                &e.prev,
            )
        );
    }

    #[test]
    fn not_modelled_claims_are_declared_not_counted() {
        assert!(NOT_MODELLED.iter().any(|s| s.contains("DELETE")));
        assert!(NOT_MODELLED.iter().any(|s| s.contains("identities")));
        assert!(NOT_MODELLED.iter().any(|s| s.contains("clock")));
    }
}
