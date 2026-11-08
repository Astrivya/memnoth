//! Proposed beliefs (D18, Wave 5.10, T3.3).
//!
//! The unattended half of a team vault writes here, never into `observations`.
//! A proposal is *visible* — `mn vault proposals` lists it, expiry marks it —
//! but it is not a belief: `recall`, `search`, `asof` and every consumer that
//! reads observations cannot see it, because there is no row for it there.
//! That is the whole enforcement mechanism, and it is structural rather than a
//! policy flag a second caller can forget to check.
//!
//! Two rules the code exists to keep:
//!
//! * a proposal **never** auto-accepts. At fourteen days it stops being queued
//!   (`status = 'expired'`); accepting afterwards is refused.
//! * the same belief proposed twice — five agents fanning out, two teammates
//!   reporting the same fact — is **one** card. Dedupe is content hash plus the
//!   entity's canonical name while the first proposal is still open.
//!
//! Acceptance is the only path from a proposal to an observation, and it names
//! the person who accepted it in the observation's provenance beside the person
//! who proposed it.

use super::*;
use crate::model::EntityKind;

/// How long a proposal waits for a decision before it expires: fourteen days,
/// the number Wave 5's gate G11 pins.
pub const PROPOSAL_TTL_MS: i64 = 14 * 86_400_000;

/// One belief proposed by an agent or teammate that no person has accepted yet.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Proposal {
    /// Row id; the handle `mn vault approve|reject` takes.
    pub id: i64,
    /// Entity name as proposed.
    pub entity_name: String,
    /// Entity kind used if acceptance has to create the entity.
    pub entity_kind: String,
    /// The belief itself.
    pub content: String,
    /// Observation kind (`fact`, `decision`, `lesson`, `gotcha`, `correction`).
    pub kind: String,
    /// Confidence the proposer claimed, 0.0–1.0.
    pub confidence: f64,
    /// Provenance JSON recorded by the proposer.
    pub provenance: String,
    /// When it was proposed (ms since epoch).
    pub proposed_at: i64,
    /// When it stops waiting (ms since epoch).
    pub expires_at: i64,
    /// `proposed`, `accepted`, `rejected` or `expired`.
    pub status: String,
    /// When a person decided it, if one did.
    pub decided_at: Option<i64>,
    /// Who decided it.
    pub decided_by: Option<String>,
    /// Why it was rejected.
    pub decision_reason: Option<String>,
    /// The observation acceptance created, once accepted.
    pub observation_id: Option<i64>,
}

const SELECT_COLUMNS: &str = "id, entity_name, entity_kind, content, kind, confidence, \
     provenance, proposed_at, expires_at, status, decided_at, decided_by, decision_reason, \
     observation_id";

fn row_to_proposal(row: &rusqlite::Row<'_>) -> rusqlite::Result<Proposal> {
    Ok(Proposal {
        id: row.get(0)?,
        entity_name: row.get(1)?,
        entity_kind: row.get(2)?,
        content: row.get(3)?,
        kind: row.get(4)?,
        confidence: row.get(5)?,
        provenance: row.get(6)?,
        proposed_at: row.get(7)?,
        expires_at: row.get(8)?,
        status: row.get(9)?,
        decided_at: row.get(10)?,
        decided_by: row.get(11)?,
        decision_reason: row.get(12)?,
        observation_id: row.get(13)?,
    })
}

impl MemoryEngine {
    /// Propose a belief. Returns `(id, created)`; `created == false` means an
    /// identical proposal from someone else is already waiting, and the caller
    /// just joined it instead of producing a second approval card.
    #[allow(clippy::too_many_arguments)]
    pub fn propose_belief(
        &self,
        entity_name: &str,
        entity_kind: EntityKind,
        content: &str,
        kind: &str,
        confidence: f64,
        provenance: &str,
        at: i64,
    ) -> Result<(i64, bool)> {
        let at = normalize_instant(at);
        let name = entity_name.trim();
        if name.is_empty() {
            anyhow::bail!("a proposal needs the entity it is about");
        }
        let content = content.trim();
        if content.is_empty() {
            anyhow::bail!("a proposal needs a belief");
        }
        let hash = content_hash_observation(content, kind);
        let canonical = crate::consolidation::canonical_name(name);

        if let Some(id) = self
            .conn
            .query_row(
                "SELECT id FROM proposals
                  WHERE status = 'proposed' AND content_hash = ?1 AND entity_canonical = ?2
                  LIMIT 1",
                params![hash, canonical],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok((id, false));
        }

        // Expire the queue before comparing against it, so a proposal that is
        // already past its window cannot absorb a fresh one through dedupe.
        self.expire_proposals(at)?;

        self.conn.execute(
            "INSERT INTO proposals
                (entity_name, entity_canonical, entity_kind, content, kind, confidence,
                 provenance, content_hash, proposed_at, expires_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'proposed')",
            params![
                name,
                canonical,
                entity_kind.as_str(),
                content,
                kind,
                confidence,
                provenance,
                hash,
                at,
                at.saturating_add(PROPOSAL_TTL_MS)
            ],
        )?;
        Ok((self.conn.last_insert_rowid(), true))
    }

    /// Mark every proposal past its window as `expired`. Never accepts one.
    /// Returns how many moved, which is what a scheduled run reports.
    pub fn expire_proposals(&self, now: i64) -> Result<usize> {
        let now = normalize_instant(now);
        let moved = self.conn.execute(
            "UPDATE proposals SET status = 'expired', decided_at = ?1
              WHERE status = 'proposed' AND expires_at <= ?1",
            params![now],
        )?;
        Ok(moved)
    }

    /// The queue, oldest first, expired rows included so a reader can see what
    /// was missed rather than what quietly vanished.
    pub fn list_proposals(&self, status: Option<&str>, now: i64) -> Result<Vec<Proposal>> {
        self.expire_proposals(now)?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM proposals
              WHERE (?1 IS NULL OR status = ?1)
              ORDER BY proposed_at ASC, id ASC"
        ))?;
        let rows = stmt
            .query_map(params![status], row_to_proposal)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// One proposal by id, whatever its status.
    pub fn proposal(&self, id: i64) -> Result<Option<Proposal>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {SELECT_COLUMNS} FROM proposals WHERE id = ?1"),
                params![id],
                row_to_proposal,
            )
            .optional()?)
    }

    /// Accept a proposal: it becomes an observation, attributed to whoever
    /// proposed it and naming whoever accepted it. Refuses anything that is not
    /// still `proposed` — an expired proposal is not a late-acceptable one.
    pub fn accept_proposal(&self, id: i64, actor: &str, now: i64) -> Result<Proposal> {
        let now = normalize_instant(now);
        self.expire_proposals(now)?;
        let proposal = self
            .proposal(id)?
            .with_context(|| format!("proposal {id} not found"))?;
        if proposal.status != "proposed" {
            anyhow::bail!(
                "proposal {id} is {}, not proposed; there is nothing to accept",
                proposal.status
            );
        }

        let entity_kind = EntityKind::from_label(&proposal.entity_kind);
        let entity_id = match self
            .find_entities_by_name(&proposal.entity_name, now)?
            .first()
        {
            Some(entity) => entity.id,
            None => {
                self.add_entity(entity_kind, &proposal.entity_name, proposal.confidence, now)?
            }
        };

        // The proposer keeps their attribution; acceptance is added beside it,
        // because "who believed this" and "who let it in" are two questions.
        let mut provenance: Value =
            serde_json::from_str(&proposal.provenance).unwrap_or_else(|_| serde_json::json!({}));
        if !provenance.is_object() {
            provenance = serde_json::json!({});
        }
        if let Some(object) = provenance.as_object_mut() {
            object.insert("accepted_by".to_string(), serde_json::json!(actor));
        }

        let observation_id = self.ingest_observation_attributed(
            entity_id,
            &proposal.content,
            &proposal.kind,
            proposal.confidence,
            now,
            &provenance.to_string(),
            Some(now),
        )?;
        self.conn.execute(
            "UPDATE proposals
                SET status = 'accepted', decided_at = ?1, decided_by = ?2, observation_id = ?3
              WHERE id = ?4",
            params![now, actor, observation_id, id],
        )?;
        self.proposal(id)?
            .with_context(|| format!("proposal {id} vanished after acceptance"))
    }

    /// Reject a proposal, keeping the reason. Nothing is written to the vault.
    pub fn reject_proposal(
        &self,
        id: i64,
        actor: &str,
        reason: &str,
        now: i64,
    ) -> Result<Proposal> {
        let now = normalize_instant(now);
        self.expire_proposals(now)?;
        let proposal = self
            .proposal(id)?
            .with_context(|| format!("proposal {id} not found"))?;
        if proposal.status != "proposed" {
            anyhow::bail!(
                "proposal {id} is {}, not proposed; it has already been decided",
                proposal.status
            );
        }
        let reason = reason.trim();
        if reason.is_empty() {
            anyhow::bail!("a rejection needs a reason");
        }
        self.conn.execute(
            "UPDATE proposals
                SET status = 'rejected', decided_at = ?1, decided_by = ?2, decision_reason = ?3
              WHERE id = ?4",
            params![now, actor, reason, id],
        )?;
        self.proposal(id)?
            .with_context(|| format!("proposal {id} vanished after rejection"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EntityKind;

    const T0: i64 = 1_760_000_000_000;
    const DAY: i64 = 86_400_000;

    fn propose(eng: &MemoryEngine, content: &str, at: i64) -> i64 {
        eng.propose_belief(
            "acme",
            EntityKind::Concept,
            content,
            "fact",
            0.8,
            r#"{"user":"agent-1","agent":"cli"}"#,
            at,
        )
        .unwrap()
        .0
    }

    #[test]
    fn a_proposal_is_not_a_belief_until_a_person_accepts_it() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        let id = propose(&eng, "the deploy runs on fridays", T0);

        // Recall and observation search cannot see it: there is no observation.
        let hits = eng.search_observations("fridays", 10, T0).unwrap();
        assert!(
            hits.is_empty(),
            "an unapproved proposal must not surface: {hits:?}"
        );
        let observations: i64 = eng
            .connection()
            .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(observations, 0);
        assert_eq!(eng.list_proposals(Some("proposed"), T0).unwrap().len(), 1);

        let accepted = eng.accept_proposal(id, "priya", T0 + DAY).unwrap();
        assert_eq!(accepted.status, "accepted");
        let observation_id = accepted
            .observation_id
            .expect("acceptance writes an observation");

        // Now it is a belief, it carries both people, and recall can find it.
        let (provenance, content): (String, String) = eng
            .connection()
            .query_row(
                "SELECT provenance, content FROM observations WHERE id = ?1",
                params![observation_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(content, "the deploy runs on fridays");
        let provenance: Value = serde_json::from_str(&provenance).unwrap();
        assert_eq!(
            provenance["user"], "agent-1",
            "the proposer keeps authorship"
        );
        assert_eq!(
            provenance["accepted_by"], "priya",
            "acceptance is attributed too"
        );
        assert_eq!(
            eng.search_observations("fridays", 10, T0 + DAY)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn five_proposers_of_one_belief_produce_one_card() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        let first = propose(&eng, "the queue is redis", T0);
        let (second, created) = eng
            .propose_belief(
                "acme",
                EntityKind::Concept,
                "the queue is redis",
                "fact",
                0.7,
                r#"{"user":"agent-2","agent":"cli"}"#,
                T0 + 1000,
            )
            .unwrap();
        assert_eq!(first, second, "the same belief is one proposal");
        assert!(!created);
        assert_eq!(eng.list_proposals(None, T0 + 1000).unwrap().len(), 1);
        // The first proposer's attribution wins; joining does not re-author it.
        assert!(
            eng.proposal(first)
                .unwrap()
                .unwrap()
                .provenance
                .contains("agent-1")
        );
    }

    #[test]
    fn a_proposal_expires_at_fourteen_days_and_never_auto_accepts() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        let id = propose(&eng, "the retry budget is two", T0);

        let queue = eng.list_proposals(None, T0 + PROPOSAL_TTL_MS).unwrap();
        assert_eq!(queue[0].status, "expired", "the window closed");

        let late = eng.accept_proposal(id, "priya", T0 + PROPOSAL_TTL_MS);
        assert!(
            late.is_err(),
            "an expired proposal must not be accepted late"
        );
        let observations: i64 = eng
            .connection()
            .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(observations, 0, "expiry is not acceptance");
    }

    #[test]
    fn a_rejection_keeps_the_reason_and_writes_nothing() {
        let eng = MemoryEngine::open_in_memory().unwrap();
        let id = propose(&eng, "we ship on fridays", T0);
        let rejected = eng
            .reject_proposal(id, "priya", "we ship on thursdays", T0 + DAY)
            .unwrap();
        assert_eq!(rejected.status, "rejected");
        assert_eq!(rejected.decided_by.as_deref(), Some("priya"));
        assert_eq!(
            rejected.decision_reason.as_deref(),
            Some("we ship on thursdays")
        );
        assert!(eng.accept_proposal(id, "priya", T0 + 2 * DAY).is_err());
        let observations: i64 = eng
            .connection()
            .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(observations, 0);
    }

    #[test]
    fn an_open_vault_written_before_proposals_gains_the_table() {
        let dir = std::env::temp_dir().join(format!("memnoth-proposals-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vault.db");
        {
            let eng = MemoryEngine::open(&path).unwrap();
            eng.add_entity(EntityKind::Concept, "acme", 1.0, T0)
                .unwrap();
        }
        // A second open runs the migration pass again; the table must be there.
        let eng = MemoryEngine::open(&path).unwrap();
        assert_eq!(eng.list_proposals(None, T0).unwrap().len(), 0);
        let id = propose(&eng, "a belief from a teammate", T0);
        assert_eq!(eng.proposal(id).unwrap().unwrap().entity_name, "acme");
        drop(eng);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
