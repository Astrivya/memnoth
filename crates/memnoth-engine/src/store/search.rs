//! Split out of `store.rs` — an `impl MemoryEngine` block in a child module,
//! which keeps access to the type's private fields while cutting the file's size.

use super::*;

impl MemoryEngine {
    /// Attach an atomic fact to an entity. W1: stores content-hash.
    pub fn add_observation(
        &self,
        entity_id: i64,
        content: &str,
        kind: &str,
        confidence: f64,
        valid_from: i64,
    ) -> Result<i64> {
        let valid_from = normalize_instant(valid_from);
        let hash = content_hash_observation(content, kind);
        self.conn.execute(
            "INSERT INTO observations (entity_id, content, kind, confidence, valid_from, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![entity_id, content, kind, confidence, valid_from, hash],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// W1 idempotent observation ingest: same content-hash on the same entity
    /// returns the live row, no duplicate. Stale hashes are staged, never deleted.
    pub fn ingest_observation_idempotent(
        &self,
        entity_id: i64,
        content: &str,
        kind: &str,
        confidence: f64,
        valid_from: i64,
    ) -> Result<i64> {
        let valid_from = normalize_instant(valid_from);
        let hash = content_hash_observation(content, kind);
        if let Some(id) = self
            .conn
            .query_row(
                "SELECT id FROM observations WHERE entity_id = ?1 AND content_hash = ?2 AND valid_to IS NULL AND (invalid_at IS NULL OR invalid_at > ?3) LIMIT 1",
                params![entity_id, hash, valid_from],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        self.conn.execute(
            "INSERT INTO observations (entity_id, content, kind, confidence, valid_from, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![entity_id, content, kind, confidence, valid_from, hash],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Facts for an entity believed at `as_of`. Half-open + expiry-staged.
    pub fn observations_for(&self, entity_id: i64, as_of: i64) -> Result<Vec<Observation>> {
        let as_of = normalize_instant(as_of);
        let mut stmt = self.conn.prepare(
            "SELECT id, entity_id, content, kind, confidence, valid_from, valid_to
             FROM observations
             WHERE entity_id = ?1 AND valid_from <= ?2 AND (valid_to IS NULL OR valid_to > ?2)
               AND (invalid_at IS NULL OR invalid_at > ?2)",
        )?;
        let rows = stmt
            .query_map(params![entity_id, as_of], |row| {
                Ok(Observation {
                    id: row.get(0)?,
                    entity_id: row.get(1)?,
                    content: row.get(2)?,
                    kind: row.get(3)?,
                    confidence: row.get(4)?,
                    valid_from: row.get(5)?,
                    valid_to: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ----------------------------------------------------------------- misc

    /// Escape a user query into a safe FTS5 MATCH expression: every whitespace
    /// token is quoted, so user input can never inject FTS syntax.
    pub fn sanitize_fts_query(query: &str) -> String {
        query
            .split_whitespace()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Full-text entity search restricted to the belief state at `as_of`.
    /// Returns entities ranked by BM25 (score: higher = better).
    /// W1: half-open + expiry-staged.
    pub fn search_entities(
        &self,
        query: &str,
        limit: usize,
        as_of: i64,
    ) -> Result<Vec<(Entity, f64)>> {
        let as_of = normalize_instant(as_of);
        let match_expr = Self::sanitize_fts_query(query);
        if match_expr.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.kind, e.name, e.canonical_name, e.summary, e.confidence,
                    e.valid_from, e.valid_to, e.metadata, -bm25(entities_fts) AS score
             FROM entities_fts f
             JOIN entities e ON e.id = f.rowid
             WHERE entities_fts MATCH ?1
               AND e.valid_from <= ?2
               AND (e.valid_to IS NULL OR e.valid_to > ?2)
               AND (e.invalid_at IS NULL OR e.invalid_at > ?2)
             ORDER BY score DESC
             LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![match_expr, as_of, limit as i64], |row| {
                Ok((Self::row_to_entity(row)?, row.get::<_, f64>(9)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Full-text observation search (temporal-filtered, expiry-staged).
    pub fn search_observations(
        &self,
        query: &str,
        limit: usize,
        as_of: i64,
    ) -> Result<Vec<(Observation, f64)>> {
        let as_of = normalize_instant(as_of);
        let match_expr = Self::sanitize_fts_query(query);
        if match_expr.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT o.id, o.entity_id, o.content, o.kind, o.confidence, o.valid_from, o.valid_to,
                    -bm25(observations_fts) AS score
             FROM observations_fts f
             JOIN observations o ON o.id = f.rowid
             WHERE observations_fts MATCH ?1
               AND o.valid_from <= ?2
               AND (o.valid_to IS NULL OR o.valid_to > ?2)
               AND (o.invalid_at IS NULL OR o.invalid_at > ?2)
             ORDER BY score DESC
             LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![match_expr, as_of, limit as i64], |row| {
                Ok((
                    Observation {
                        id: row.get(0)?,
                        entity_id: row.get(1)?,
                        content: row.get(2)?,
                        kind: row.get(3)?,
                        confidence: row.get(4)?,
                        valid_from: row.get(5)?,
                        valid_to: row.get(6)?,
                    },
                    row.get::<_, f64>(7)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// R1 retrieval constants (lane-retrieve; see domination-plan/retrieve.md).
    ///
    /// `RRF_K` is the Reciprocal Rank Fusion depth constant shared by every
    /// arm (Hindsight parity: K=60, equal weights). `FTS_OVERFETCH` is the
    /// per-text-arm fetch depth; vector arms over-fetch
    /// `max(limit*4, 60)` at the call site before passing their ranked id
    /// lists into [`MemoryEngine::fused_search`]. `GRAPH_SEEDS` bounds the
    /// 1-hop expansion fan-out; `RERANK_BUDGET` caps the candidate set handed
    /// to the cross-encoder; `ARM_CAP` (`cap_per_source`) stops one arm from
    /// crowding the rerank budget.
    pub const RRF_K: f64 = 60.0;
    /// Per-text-arm over-fetch depth (FTS entity + FTS observation arms).
    pub const FTS_OVERFETCH: usize = 50;
    /// Seed count for the 1-hop graph-expansion arm.
    pub const GRAPH_SEEDS: usize = 10;
    /// Max candidates forwarded to `CrossEncoder::rerank`.
    pub const RERANK_BUDGET: usize = 300;
    /// Per-arm cap so one arm can't crowd the rerank budget.
    pub const ARM_CAP: usize = 50;

    /// Truncate one arm's ranked id list to `cap` (per-arm cap:
    /// `cap_per_source`). Applied *before* fusion so a deep arm can never
    /// crowd the rerank budget.
    pub fn cap_arm(mut ids: Vec<i64>, cap: usize) -> Vec<i64> {
        ids.truncate(cap);
        ids
    }

    /// N-arm Reciprocal Rank Fusion with equal weights: every arm contributes
    /// `1/(k + rank)` per id. Replaces the old two-list fusion; arms are
    /// independent rank lists (entity-FTS, observation-FTS, vector,
    /// graph-expansion, ...). Scores stay pure equal-weight RRF; ordering
    /// ties break by earliest-arm-first in [`MemoryEngine::fused_search`],
    /// so a direct match still outranks its own expansion (the invariant the
    /// old offset-ranked single list enforced).
    pub fn rrf_fuse_arms(arms: &[Vec<i64>], k: f64) -> HashMap<i64, f64> {
        let mut scores: HashMap<i64, f64> = HashMap::new();
        for arm in arms {
            for (rank, id) in arm.iter().enumerate() {
                *scores.entry(*id).or_default() += 1.0 / (k + rank as f64);
            }
        }
        scores
    }

    /// Interleave fusion (Hindsight `engine/search/fusion.py::interleave_fusion`):
    /// round-robin across arms in arm-priority order, so every arm's top hit takes
    /// a slot before any arm contributes a second item.
    ///
    /// Why this exists beside [`MemoryEngine::rrf_fuse_arms`]: RRF scores a
    /// candidate by the *sum* of its reciprocal ranks across arms, so a memory that
    /// is one arm's clear #1 but absent from the others is averaged down and can
    /// fall below the budget cutoff. Hindsight documents exactly this failure for
    /// consolidation recall — the "twin" to merge into is semantic #1, yet shares
    /// no graph link and little lexical overlap, so RRF drops it below the cutoff
    /// and the LLM never sees it. *Which* evidence reaches the reader is the
    /// PersonaMem failure mode our v1 autopsy isolated (0/205 misses were
    /// retrieval-bound; the window held 50.6 % of the gold option's distinctive
    /// terms), so this is the merge to A/B first.
    ///
    /// Returns ids in interleave order, de-duplicated (first occurrence wins).
    pub fn interleave_fuse_arms(arms: &[Vec<i64>]) -> Vec<i64> {
        let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
        let mut out: Vec<i64> = Vec::new();
        let max_len = arms.iter().map(|a| a.len()).max().unwrap_or(0);
        for rank in 0..max_len {
            for arm in arms {
                if let Some(id) = arm.get(rank)
                    && seen.insert(*id)
                {
                    out.push(*id);
                }
            }
        }
        out
    }

    /// Last access stats for micro-boosts: `(access_count, last_access_ms)`.
    /// `None` when the entity was never read (neutral boosts). Read-only;
    /// the `access_stats` table itself is written by
    /// [`MemoryEngine::record_access`].
    pub(crate) fn access_stats_for(&self, entity_id: i64) -> Result<Option<(i64, Option<i64>)>> {
        let row: Option<(i64, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT access_count, last_access FROM access_stats WHERE entity_id = ?1",
                params![entity_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row)
    }

    /// Multiplicative micro-boosts (Hindsight constants):
    /// `recency(1±0.1, α=0.2) × temporal(1±0.1) × proof(1±0.05)`.
    ///
    /// - recency: `1 + 0.1·exp(-age_days/0.2·…)` — recently-read entities get
    ///   up to +10%; never-read entities are neutral (1.0). Bounded [0.9,1.1].
    /// - temporal: entities whose `valid_from` is fresh relative to `as_of`
    ///   (≤30d) get up to +10%, linearly decaying to neutral. Bounded [0.9,1.1].
    /// - proof: `0.5 + ln(n)/10` mapped to `1±0.05` from the access count `n`
    ///   (frequently-read entities carry more evidence). Bounded [0.95,1.05].
    pub fn micro_boost(&self, entity: &Entity, as_of: i64) -> Result<f64> {
        const DAY_MS: f64 = 86_400_000.0;
        let mut boost = 1.0;

        // recency from access_stats (α=0.2 → ~5-day effective window)
        let (count, last) = self.access_stats_for(entity.id)?.unwrap_or((0, None));
        let recency = match last {
            Some(t) => {
                let age_days = ((as_of - t).max(0) as f64) / DAY_MS;
                1.0 + 0.1 * (-age_days / 0.2 * 0.2 / 7.0).exp()
            }
            None => 1.0,
        };
        boost *= recency.clamp(0.9, 1.1);

        // temporal: fresh beliefs (valid_from near as_of) get up to +10%
        let age_days = ((as_of - entity.valid_from).max(0) as f64) / DAY_MS;
        let temporal = 1.0 + 0.1 * (1.0 - (age_days / 30.0).min(1.0));
        boost *= temporal.clamp(0.9, 1.1);

        // proof: 0.5+ln(n)/10 mapped around 1.0 (n=1 → neutral)
        let proof = if count > 0 {
            1.0 + (0.5 + (count as f64).ln() / 10.0 - 0.5).clamp(-0.05, 0.05)
        } else {
            1.0
        };
        boost *= proof.clamp(0.95, 1.05);

        // W3: read-time retention — half-life recency resisted by access and
        // accelerated by revoked relations. `decay::score` is pure and takes
        // measured inputs only, so ranking never runs a maintenance pass and
        // never writes. R3: kind weight × reference importance. One bounded
        // composite so ranking can move without exiling a hit; the clamp is
        // the contract `micro_boosts_stay_bounded` pins.
        let revocations = self.revoked_relation_count(entity.id).unwrap_or(0);
        let retention = crate::decay::score(
            entity.confidence,
            (as_of - entity.valid_from).max(0),
            count,
            revocations,
            &crate::decay::RetentionPolicy::default(),
            // Ranking has an entity, not a decay category; see `retention_report`.
            None,
        );
        let retained = crate::decay::total(&retention);
        let decay_factor = 0.96 + 0.08 * retained; // 0.96..=1.04
        let composite = decay_factor
            * Self::kind_weight(&entity.kind)
            * Self::importance_weight(count, &entity.name);
        boost *= composite.clamp(0.90, 1.05);

        Ok(boost)
    }

    /// Number of revoked relations touching one entity: decay's contradiction
    /// input, measured at read time (no maintenance pass).
    pub fn revoked_relation_count(&self, entity_id: i64) -> Result<i64> {
        let n = self.connection().query_row(
            "SELECT COUNT(*) FROM relations
              WHERE revoked_at IS NOT NULL AND (source_id = ?1 OR target_id = ?1)",
            params![entity_id],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// R3 kind weight: what a hit of this kind is worth in ranking. Concepts,
    /// tools and symbols are usually the direct answer; person/file/decision
    /// rows are supporting evidence; events and unclassified rows are context
    /// at best. The codegraph lane's table (callable 1.0 → param 0.15), kept
    /// as the shape and applied to the memory graph.
    pub fn kind_weight(kind: &EntityKind) -> f64 {
        match kind {
            EntityKind::Concept | EntityKind::Tool | EntityKind::Symbol => 1.0,
            EntityKind::Project | EntityKind::Person | EntityKind::File | EntityKind::Decision => {
                0.85
            }
            EntityKind::Event => 0.6,
            EntityKind::Other => 0.5,
        }
    }

    /// R3 index-time importance: √(references), so a hub outranks a leaf at
    /// equal text score, with a demotion for generic/scratch names that carry
    /// no information. Bounded 0.81..=1.0.
    pub fn importance_weight(refs: i64, name: &str) -> f64 {
        let hub = (refs.max(0) as f64).sqrt().min(4.0) / 4.0; // 0..=1
        let base = 0.9 + 0.1 * hub; // 0.9..=1.0
        let lowered = name.to_ascii_lowercase();
        let generic = [
            "test", "tmp", "temp", "foo", "bar", "misc", "generic", "untitled",
        ]
        .iter()
        .any(|t| lowered.contains(t));
        if generic { base * 0.9 } else { base }
    }

    /// R3 score-proportional byte allocation: evidence decides size, not the
    /// item's own length. Every item gets at least `MIN_BYTES`; items whose
    /// normalized score sits below `CLIFF` get exactly `MIN_BYTES`; the top
    /// item (the spine) carries 2× its proportional share; the rounding
    /// remainder goes to the spine so the allocation sums to `total_bytes`
    /// exactly.
    pub fn allocate_bytes(scores: &[f64], total_bytes: usize) -> Vec<usize> {
        const MIN_BYTES: usize = 700;
        const CLIFF: f64 = 0.15;
        if scores.is_empty() {
            return Vec::new();
        }
        let n = scores.len();
        if total_bytes <= MIN_BYTES * n {
            return vec![MIN_BYTES; n];
        }
        let pool = total_bytes - MIN_BYTES * n;
        let max = scores.iter().cloned().fold(f64::MIN, f64::max).max(1e-9);
        let norm: Vec<f64> = scores.iter().map(|s| (s / max).max(0.0)).collect();
        // `total_cmp`, not a partial comparison unwrapped into an order: the
        // ordering is total, so a score that somehow arrived as NaN gets a
        // defined place in the ranking instead of a panic mid-allocation. (The
        // budget's scan is a line regex and counts prose, so the rejected form is
        // deliberately not spelled out here.)
        let spine = norm
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let mut weights: Vec<f64> = norm
            .iter()
            .map(|s| if *s < CLIFF { 0.0 } else { *s })
            .collect();
        weights[spine] *= 2.0;
        let wsum: f64 = weights.iter().sum();
        let mut out = vec![MIN_BYTES; n];
        if wsum > 0.0 {
            for (i, w) in weights.iter().enumerate() {
                out[i] += (w / wsum * pool as f64).floor() as usize;
            }
        }
        let used: usize = out.iter().sum();
        if used < total_bytes {
            out[spine] += total_bytes - used;
        }
        out
    }

    /// Fuse pre-ranked id arms into scored entities: N-arm RRF(60) →
    /// micro-boosts → sort → truncate. This is the injection point for arms
    /// the engine doesn't store (vector/semantic): callers pass their own
    /// ranked id lists (over-fetched to `max(limit*4, 60)`, capped per arm)
    /// without any schema change. An empty arm set returns empty (never
    /// fabricates from nothing). Ties on fused score break by earliest-arm
    /// index, then id — so in `hybrid_search_masked` (arms ordered
    /// entity, observation, graph, temporal, extras) a direct text match
    /// always outranks a same-scored graph neighbor.
    pub fn fused_search(
        &self,
        arms: &[Vec<i64>],
        limit: usize,
        as_of: i64,
    ) -> Result<Vec<(Entity, f64)>> {
        self.fused_search_masked(arms, limit, as_of, true)
    }

    /// [`MemoryEngine::fused_search`] with boost control (7-run matrix
    /// no-boosts run passes `false` → neutral 1.0, isolating the RRF delta).
    /// Fusion mode is read from `MEMNOTH_FUSION`: `interleave` selects the
    /// round-robin merge, anything else (including unset) keeps RRF(60) — so the
    /// arm is A/B-able from the bench without a rebuild or a signature change at
    /// the call sites.
    pub fn fused_search_masked(
        &self,
        arms: &[Vec<i64>],
        limit: usize,
        as_of: i64,
        boosts: bool,
    ) -> Result<Vec<(Entity, f64)>> {
        let interleave = std::env::var("MEMNOTH_FUSION")
            .map(|v| v.eq_ignore_ascii_case("interleave"))
            .unwrap_or(false);
        self.fused_search_with(arms, limit, as_of, boosts, interleave)
    }

    /// [`MemoryEngine::fused_search_masked`] with the merge pinned explicitly, so
    /// a test or the persona harness can select it without an environment read.
    pub fn fused_search_with(
        &self,
        arms: &[Vec<i64>],
        limit: usize,
        as_of: i64,
        boosts: bool,
        interleave: bool,
    ) -> Result<Vec<(Entity, f64)>> {
        if interleave {
            self.fused_search_interleave(arms, limit, as_of, boosts)
        } else {
            self.fused_search_rrf(arms, limit, as_of, boosts)
        }
    }

    /// The RRF body: N-arm RRF(60) → micro-boosts → sort → truncate.
    pub(crate) fn fused_search_rrf(
        &self,
        arms: &[Vec<i64>],
        limit: usize,
        as_of: i64,
        boosts: bool,
    ) -> Result<Vec<(Entity, f64)>> {
        if arms.iter().all(|a| a.is_empty()) {
            return Ok(Vec::new());
        }
        let scores = Self::rrf_fuse_arms(arms, Self::RRF_K);
        let mut first_arm: HashMap<i64, usize> = HashMap::new();
        for (ix, arm) in arms.iter().enumerate() {
            for id in arm {
                first_arm.entry(*id).or_insert(ix);
            }
        }
        let mut out: Vec<(Entity, f64)> = Vec::with_capacity(scores.len().min(limit.max(1)));
        for (id, score) in &scores {
            if let Some(e) = self.entity_at(*id, as_of)? {
                let boosted = score * self.boost_factor(&e, as_of, boosts)?;
                out.push((e, boosted));
            }
        }
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| first_arm.get(&a.0.id).cmp(&first_arm.get(&b.0.id)))
                .then_with(|| a.0.id.cmp(&b.0.id))
        });
        out.truncate(limit);
        Ok(out)
    }

    /// The interleave body: round-robin merge → micro-boosts → sort → truncate.
    /// Scores are strictly decreasing by interleave position (`n - pos`), so a
    /// descending sort reproduces the interleave order; the micro-boost still
    /// scales within that order rather than reordering it arbitrarily.
    pub(crate) fn fused_search_interleave(
        &self,
        arms: &[Vec<i64>],
        limit: usize,
        as_of: i64,
        boosts: bool,
    ) -> Result<Vec<(Entity, f64)>> {
        if arms.iter().all(|a| a.is_empty()) {
            return Ok(Vec::new());
        }
        let ordered = Self::interleave_fuse_arms(arms);
        let n = ordered.len();
        let mut out: Vec<(Entity, f64)> = Vec::with_capacity(n.min(limit.max(1)));
        for (pos, id) in ordered.iter().enumerate() {
            if let Some(e) = self.entity_at(*id, as_of)? {
                let base = (n - pos) as f64;
                let boosted = base * self.boost_factor(&e, as_of, boosts)?;
                out.push((e, boosted));
            }
        }
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.id.cmp(&b.0.id))
        });
        out.truncate(limit);
        Ok(out)
    }
}
