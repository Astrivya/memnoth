//! Split out of `store.rs` — an `impl MemoryEngine` block in a child module,
//! which keeps access to the type's private fields while cutting the file's size.

use super::*;

impl MemoryEngine {
    /// Supersede an entity: close the current row's validity at `at`, append its
    /// snapshot to history, and insert a successor carrying the new values.
    /// Returns the new entity id. Never mutates the old row (append-only).
    ///
    /// W1 atom: single-`at` shared-instant close+open with half-open intervals
    /// `[valid_from, valid_to)` — as-of `at` returns the successor only, never
    /// both. Date-only `at` is already T00:00:00Z midnight; seconds-granularity
    /// input is promoted via [`normalize_instant`] so whole-day overlap cannot
    /// arise. Inverted instants (`at` < `valid_from`) are rejected. When no open
    /// predecessor exists as-of `at`, degrades to an ADD (new live row with
    /// lineage link) instead of failing the write — zero omissions.
    #[allow(clippy::too_many_arguments)]
    pub fn supersede_entity(
        &self,
        entity_id: i64,
        kind: Option<EntityKind>,
        name: Option<&str>,
        summary: Option<&str>,
        confidence: Option<f64>,
        reason: &str,
        at: i64,
    ) -> Result<i64> {
        let at = normalize_instant(at);
        // Inverted guard first: `at` preceding birth is rejected even before
        // degrade-to-add is considered (else a pre-birth instant would silently ADD).
        if let Some(vf) = self
            .conn
            .query_row(
                "SELECT valid_from FROM entities WHERE id = ?1",
                params![entity_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            && at < vf
        {
            anyhow::bail!("inverted interval: supersede instant {at} precedes valid_from {vf}");
        }
        let old_opt = self.entity_at(entity_id, at)?;

        // Degrade to ADD when no open predecessor exists as-of `at`.
        let Some(old) = old_opt else {
            // Need enough to open a row: name or fail loudly (no invented names).
            let fallback_name = match name {
                Some(n) => n.to_string(),
                None => {
                    // Try closed-row values as defaults before giving up.
                    let closed: Option<(String, String, Option<String>, f64)> = self
                        .conn
                        .query_row(
                            "SELECT kind, name, summary, confidence FROM entities WHERE id = ?1",
                            params![entity_id],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                        )
                        .optional()?;
                    match closed {
                        Some((_, n, _, _)) => n,
                        None => anyhow::bail!(
                            "entity {entity_id} not found as-of {at}; cannot degrade to add without a name"
                        ),
                    }
                }
            };
            let fallback_kind = kind.unwrap_or_else(|| {
                self.conn
                    .query_row(
                        "SELECT kind FROM entities WHERE id = ?1",
                        params![entity_id],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
                    .map(|s| EntityKind::from_label(&s))
                    .unwrap_or(EntityKind::Concept)
            });
            let (fallback_summary, fallback_conf): (Option<String>, f64) = self
                .conn
                .query_row(
                    "SELECT summary, confidence FROM entities WHERE id = ?1",
                    params![entity_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .map(|(s, c): (Option<String>, f64)| {
                    (summary.map(str::to_string).or(s), confidence.unwrap_or(c))
                })
                .unwrap_or((summary.map(str::to_string), confidence.unwrap_or(1.0)));
            let hash =
                content_hash_entity(&fallback_kind, &fallback_name, fallback_summary.as_deref());
            self.conn.execute(
                "INSERT INTO entities (kind, name, canonical_name, summary, confidence, valid_from, metadata, created_at, content_hash, generation)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
                params![
                    fallback_kind.as_str(),
                    fallback_name,
                    crate::consolidation::canonical_name(&fallback_name),
                    fallback_summary,
                    fallback_conf,
                    at,
                    serde_json::json!({"supersedes": entity_id, "degraded_add": true}).to_string(),
                    now_ms(),
                    hash
                ],
            )?;
            let new_id = self.conn.last_insert_rowid();
            let _ = self.refresh_checksum(new_id);
            let _ = self.add_memory_link(
                new_id,
                entity_id,
                "supersedes",
                &format!("degraded add: {reason}"),
                at,
            );
            self.emit_audit(
                "entity.supersede",
                &fallback_name,
                "degraded_add",
                serde_json::json!({"superseded_id": entity_id, "successor_id": new_id, "reason": reason}),
                at,
            );
            return Ok(new_id);
        };
        if old.valid_to.is_some() {
            anyhow::bail!("entity {entity_id} is already closed; cannot supersede");
        }
        if at < old.valid_from {
            anyhow::bail!(
                "inverted interval: supersede instant {at} precedes valid_from {}",
                old.valid_from
            );
        }

        let new_kind = kind.unwrap_or(old.kind);
        let new_name = name.unwrap_or(&old.name).to_string();
        let new_summary = summary.map(str::to_string).or_else(|| old.summary.clone());
        let new_confidence = confidence.unwrap_or(old.confidence);
        // W2 lineage counters: successor inherits proof_count, generation +1.
        let (old_proof, old_gen): (i64, i64) = self
            .conn
            .query_row(
                "SELECT COALESCE(proof_count,0), COALESCE(generation,0) FROM entities WHERE id = ?1",
                params![entity_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, 0));

        let tx = self.conn.unchecked_transaction()?;
        // close old + journal it (shared instant `at`)
        tx.execute(
            "UPDATE entities SET valid_to = ?1 WHERE id = ?2 AND valid_to IS NULL",
            params![at, entity_id],
        )?;
        self.journal_entity(&old, Some(at), reason)?;

        // open successor — carrying lineage links in `metadata` so history
        // resolves across a rename in BOTH directions. Links live in the JSON
        // metadata column rather than as new columns: no migration, and no
        // `SELECT *` row mapper changes. (Graphiti-equivalent capability; our
        // journal keeps the reason on top.)
        // `with_meta_link` is the helper this file already had for exactly this:
        // it normalizes a missing or non-object column to `{}` and merges one key,
        // so the merge cannot clobber the rest. (The inline version this replaced
        // re-implemented it and then *asserted* the normalization it had just
        // performed — an `expect` whose whole job was to describe the three lines
        // above it.)
        let meta_json = with_meta_link(&old.metadata, "supersedes", serde_json::json!(entity_id));
        let new_hash = content_hash_entity(&new_kind, &new_name, new_summary.as_deref());
        tx.execute(
            "INSERT INTO entities (kind, name, canonical_name, summary, confidence, valid_from, metadata, created_at, content_hash, proof_count, generation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                new_kind.as_str(),
                new_name,
                // Recomputed from the successor's own name. Carrying the old row's
                // canonical form meant a rename to `alice-smith` kept the
                // comparison form `alice`, so asking for the new name found the
                // old entity and asking for an unrelated name could too.
                crate::consolidation::canonical_name(&new_name),
                new_summary,
                new_confidence,
                at,
                meta_json,
                now_ms(),
                new_hash,
                old_proof,
                old_gen + 1
            ],
        )?;
        let new_id = tx.last_insert_rowid();

        // Back-link on the old row. The journal snapshot for the old row was
        // already written above from the pre-supersede state, so it stays an
        // honest record; only the live row gains the pointer.
        let old_meta_json =
            with_meta_link(&old.metadata, "superseded_by", serde_json::json!(new_id));
        tx.execute(
            "UPDATE entities SET metadata = ?1 WHERE id = ?2",
            params![old_meta_json, entity_id],
        )?;
        // W1 ADD-only link row (append, never overwrite).
        tx.execute(
            "INSERT INTO memory_links (source_id, target_id, kind, reason, created_at)
             VALUES (?1, ?2, 'supersedes', ?3, ?4)",
            params![new_id, entity_id, reason, at],
        )?;
        tx.commit()?;
        // W2: fresh checksums on both rows (successor generation+1).
        let _ = self.refresh_checksum(new_id);
        let _ = self.refresh_checksum(entity_id);
        self.emit_audit(
            "entity.supersede",
            &new_name,
            "applied",
            serde_json::json!({
                "superseded_id": entity_id,
                "successor_id": new_id,
                "reason": reason
            }),
            at,
        );
        Ok(new_id)
    }

    /// W2 checked supersede: fence first (`409` on stale base/generation),
    /// then the ordinary [`supersede_entity`] atom. Concurrent agents pass the
    /// checksum + generation they read; the loser of a race gets 409, not a
    /// silent overwrite.
    #[allow(clippy::too_many_arguments)]
    pub fn supersede_entity_checked(
        &self,
        entity_id: i64,
        kind: Option<EntityKind>,
        name: Option<&str>,
        summary: Option<&str>,
        confidence: Option<f64>,
        reason: &str,
        at: i64,
        expected_checksum: Option<&str>,
        expected_generation: Option<i64>,
    ) -> Result<i64> {
        self.check_write_fence(entity_id, expected_checksum, expected_generation)?;
        self.supersede_entity(entity_id, kind, name, summary, confidence, reason, at)
    }

    /// Explicitly contradict a fact: mark revoked at `at` and journal.
    pub fn revoke_entity(&self, entity_id: i64, reason: &str, at: i64) -> Result<()> {
        let at = normalize_instant(at);
        let old = self
            .entity_at(entity_id, at)?
            .with_context(|| format!("entity {entity_id} not found"))?;
        self.conn.execute(
            "UPDATE entities SET valid_to = ?1 WHERE id = ?2",
            params![at, entity_id],
        )?;
        self.journal_entity(&old, Some(at), &format!("revoked: {reason}"))?;
        self.emit_audit(
            "entity.revoke",
            &old.name,
            "applied",
            serde_json::json!({"entity_id": entity_id, "reason": reason}),
            at,
        );
        Ok(())
    }

    /// Re-open a closed entity (revoked, superseded, or forgotten) from its own
    /// row, as a new belief starting at `at`.
    ///
    /// This is the inverse of invalidation and the reason forgetting is safe:
    /// nothing was deleted, so a closed belief can be made current again with
    /// all of its content intact. The new row carries `supersedes: <old>` and
    /// the old row gains `superseded_by: <new>`, so both directions of history
    /// resolve — the same lineage contract `supersede_entity` maintains.
    ///
    /// Fails when the entity is already current: re-opening a live belief would
    /// create two current copies of it.
    pub fn reopen_entity(&self, entity_id: i64, at: i64) -> Result<i64> {
        let at = normalize_instant(at);
        let old = self
            .conn
            .query_row(
                "SELECT id, kind, name, canonical_name, summary, confidence, valid_from, valid_to, metadata
                 FROM entities WHERE id = ?1",
                params![entity_id],
                Self::row_to_entity,
            )
            .optional()?
            .with_context(|| format!("entity {entity_id} not found"))?;
        if old.valid_to.is_none() {
            // Expiry-staged but still open counts as current for reopen guard
            // unless already expired as-of `at`.
            let inv: Option<i64> = self
                .conn
                .query_row(
                    "SELECT invalid_at FROM entities WHERE id = ?1",
                    params![entity_id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            if inv.is_none_or(|t| t > at) {
                anyhow::bail!("entity {entity_id} is already current; nothing to reinstate");
            }
        }

        let new_meta = with_meta_link(&old.metadata, "supersedes", serde_json::json!(entity_id));
        let new_hash = content_hash_entity(&old.kind, &old.name, old.summary.as_deref());
        let (old_proof, old_gen): (i64, i64) = self
            .conn
            .query_row(
                "SELECT COALESCE(proof_count,0), COALESCE(generation,0) FROM entities WHERE id = ?1",
                params![entity_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, 0));
        self.conn.execute(
            "INSERT INTO entities (kind, name, canonical_name, summary, confidence, valid_from, metadata, created_at, content_hash, proof_count, generation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                old.kind.as_str(),
                old.name,
                crate::consolidation::canonical_name(&old.name),
                old.summary,
                old.confidence,
                at,
                new_meta,
                now_ms(),
                new_hash,
                old_proof,
                old_gen + 1
            ],
        )?;
        let new_id = self.conn.last_insert_rowid();
        let _ = self.refresh_checksum(new_id);

        let old_meta = with_meta_link(&old.metadata, "superseded_by", serde_json::json!(new_id));
        self.conn.execute(
            "UPDATE entities SET metadata = ?1 WHERE id = ?2",
            params![old_meta, entity_id],
        )?;
        let _ = self.add_memory_link(new_id, entity_id, "reopen", "reinstate", at);
        self.emit_audit(
            "entity.reopen",
            &old.name,
            "applied",
            serde_json::json!({"reopened_entity_id": entity_id, "successor_id": new_id}),
            at,
        );
        Ok(new_id)
    }

    /// The entity row as it stands at instant `as_of` (None when not yet born
    /// or already closed/revoked/expired by then).
    /// W1 half-open: [valid_from, valid_to) and [valid_from, invalid_at).
    pub fn entity_at(&self, entity_id: i64, as_of: i64) -> Result<Option<Entity>> {
        let as_of = normalize_instant(as_of);
        let row: Option<(Entity, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT id, kind, name, canonical_name, summary, confidence, valid_from, valid_to, metadata, invalid_at
                 FROM entities WHERE id = ?1",
                params![entity_id],
                |r| Ok((Self::row_to_entity(r)?, r.get::<_, Option<i64>>(9)?)),
            )
            .optional()?
            .filter(|(e, invalid_at)| {
                e.valid_at(as_of) && invalid_at.is_none_or(|t| t > as_of)
            });
        Ok(row.map(|(e, _)| e))
    }

    /// All entities believed at `as_of`. Half-open + expiry-staged.
    pub fn entities_as_of(&self, as_of: i64) -> Result<Vec<Entity>> {
        let as_of = normalize_instant(as_of);
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, canonical_name, summary, confidence, valid_from, valid_to, metadata
             FROM entities WHERE valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
               AND (invalid_at IS NULL OR invalid_at > ?1)",
        )?;
        let rows = stmt
            .query_map(params![as_of], Self::row_to_entity)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Current entities (`valid_to IS NULL` and not expiry-staged past now),
    /// i.e. the live graph.
    pub fn entities_current(&self) -> Result<Vec<Entity>> {
        let now = now_ms();
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, canonical_name, summary, confidence, valid_from, valid_to, metadata
             FROM entities WHERE valid_to IS NULL AND (invalid_at IS NULL OR invalid_at > ?1) ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![now], Self::row_to_entity)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Full-graph read: every current entity + every live edge. The analogue
    /// of server-memory's `read_graph` — one call, whole belief state.
    pub fn read_graph(&self) -> Result<(Vec<Entity>, Vec<Relation>)> {
        let entities = self.entities_current()?;
        let now = now_ms();
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, target_id, kind, confidence, valid_from, valid_to, revoked_at, metadata
             FROM relations WHERE valid_to IS NULL AND revoked_at IS NULL
               AND (invalid_at IS NULL OR invalid_at > ?1) ORDER BY id",
        )?;
        let relations = stmt
            .query_map(params![now], Self::row_to_relation)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok((entities, relations))
    }

    /// Find current entities by exact name.
    /// Live entities whose name, canonical name or recorded alias matches.
    ///
    /// Canonical and alias matching are what make a merged entity reachable
    /// under every name it was ever known by, instead of only the survivor's.
    pub fn find_entities_by_name(&self, name: &str, as_of: i64) -> Result<Vec<Entity>> {
        let as_of = normalize_instant(as_of);
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, canonical_name, summary, confidence, valid_from, valid_to, metadata
             FROM entities e
             WHERE (e.name = ?1
                OR e.canonical_name = ?3
                OR e.id IN (SELECT entity_id FROM entity_aliases WHERE alias = ?3))
               AND e.valid_from <= ?2 AND (e.valid_to IS NULL OR e.valid_to > ?2)
               AND (e.invalid_at IS NULL OR e.invalid_at > ?2)
             ORDER BY e.id",
        )?;
        let canonical = crate::consolidation::canonical_name(name);
        let rows = stmt
            .query_map(params![name, as_of, canonical], Self::row_to_entity)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ------------------------------------------------------------ relations
}
