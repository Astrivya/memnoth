//! Split out of `store.rs` — an `impl MemoryEngine` block in a child module,
//! which keeps access to the type's private fields while cutting the file's size.

use super::*;

impl MemoryEngine {
    /// Create a directed edge starting to be valid at `valid_from`.
    pub fn add_relation(
        &self,
        source_id: i64,
        target_id: i64,
        kind: &str,
        confidence: f64,
        valid_from: i64,
    ) -> Result<i64> {
        let valid_from = normalize_instant(valid_from);
        self.conn.execute(
            "INSERT INTO relations (source_id, target_id, kind, confidence, valid_from)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![source_id, target_id, kind, confidence, valid_from],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Temporal invalidation (graphiti mechanism): close the conflicting edge
    /// and open its replacement. Journals both transitions.
    ///
    /// W1 atom: single-`at` shared-instant close+open, half-open
    /// `[valid_from, valid_to)`, inverted instants rejected. ADD-only link row
    /// records the retarget; recency + retrieval arbitrate afterwards.
    pub fn supersede_relation(
        &self,
        relation_id: i64,
        target_id: Option<i64>,
        kind: Option<&str>,
        confidence: Option<f64>,
        reason: &str,
        at: i64,
    ) -> Result<i64> {
        let at = normalize_instant(at);
        // Inverted guard before as-of lookup: pre-birth instant must report
        // inverted, not merely not-found.
        if let Some(vf) = self
            .conn
            .query_row(
                "SELECT valid_from FROM relations WHERE id = ?1",
                params![relation_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            && at < vf
        {
            anyhow::bail!("inverted interval: supersede instant {at} precedes valid_from {vf}");
        }
        let old = self
            .relation_at(relation_id, at)?
            .with_context(|| format!("relation {relation_id} not found as-of {at}"))?;
        if old.valid_to.is_some() {
            anyhow::bail!("relation {relation_id} already closed");
        }

        let new_target = target_id.unwrap_or(old.target_id);
        let new_kind = kind.unwrap_or(&old.kind).to_string();
        let new_confidence = confidence.unwrap_or(old.confidence);

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE relations SET valid_to = ?1 WHERE id = ?2 AND valid_to IS NULL",
            params![at, relation_id],
        )?;
        self.journal_relation(&old, Some(at), reason)?;

        tx.execute(
            "INSERT INTO relations (source_id, target_id, kind, confidence, valid_from)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![old.source_id, new_target, new_kind, new_confidence, at],
        )?;
        let new_id = tx.last_insert_rowid();
        tx.commit()?;
        // ADD-only entity link for the retarget (best-effort: entities may have
        // been closed concurrently; the relation journal is the source of truth).
        let _ = self.add_memory_link(old.source_id, new_target, "relation_retarget", reason, at);
        let _ = new_id;
        // Re-read to return the successor id (last_insert from tx is committed).
        let successor: i64 = self.conn.query_row(
            "SELECT id FROM relations WHERE source_id = ?1 AND target_id = ?2 AND kind = ?3 AND valid_from = ?4 ORDER BY id DESC LIMIT 1",
            params![old.source_id, new_target, new_kind, at],
            |r| r.get(0),
        )?;
        Ok(successor)
    }

    /// Mark an edge contradicted: excluded from "now" forever, still visible
    /// historically via as-of queries before `at`.
    pub fn revoke_relation(&self, relation_id: i64, reason: &str, at: i64) -> Result<()> {
        let old = self
            .relation_at(relation_id, at)?
            .with_context(|| format!("relation {relation_id} not found"))?;
        // Both writes share one transaction: a revoke whose journal entry failed
        // would leave the graph claiming something the audit trail never saw, in
        // a product whose promise is that nothing is lost silently.
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE relations SET revoked_at = ?1 WHERE id = ?2",
            params![at, relation_id],
        )?;
        self.journal_relation(&old, None, &format!("revoked: {reason}"))?;
        tx.commit()?;
        Ok(())
    }

    /// Edge state at instant `as_of`. Half-open + expiry-staged.
    pub fn relation_at(&self, relation_id: i64, as_of: i64) -> Result<Option<Relation>> {
        let as_of = normalize_instant(as_of);
        let row: Option<(Relation, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT id, source_id, target_id, kind, confidence, valid_from, valid_to, revoked_at, metadata, invalid_at
                 FROM relations WHERE id = ?1",
                params![relation_id],
                |r| Ok((Self::row_to_relation(r)?, r.get::<_, Option<i64>>(9)?)),
            )
            .optional()?
            .filter(|(rel, invalid_at)| {
                rel.valid_at(as_of) && invalid_at.is_none_or(|t| t > as_of)
            });
        Ok(row.map(|(r, _)| r))
    }

    /// Outgoing edges of `entity_id` believed at `as_of`. Half-open + expiry.
    pub fn outgoing_relations(&self, entity_id: i64, as_of: i64) -> Result<Vec<Relation>> {
        let as_of = normalize_instant(as_of);
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, target_id, kind, confidence, valid_from, valid_to, revoked_at, metadata
             FROM relations
             WHERE source_id = ?1
               AND valid_from <= ?2
               AND (valid_to IS NULL OR valid_to > ?2)
               AND (revoked_at IS NULL OR revoked_at > ?2)
               AND (invalid_at IS NULL OR invalid_at > ?2)",
        )?;
        let rows = stmt
            .query_map(params![entity_id, as_of], Self::row_to_relation)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// One-hop neighborhood expansion used by retrieval's graph stage:
    /// edges out of `ids` believed at `as_of`.
    pub fn expand_frontier(&self, ids: &[i64], as_of: i64) -> Result<Vec<Relation>> {
        let mut out = Vec::new();
        for id in ids {
            out.extend(self.outgoing_relations(*id, as_of)?);
        }
        Ok(out)
    }

    /// S5 — link-type weight for spreading (Hindsight
    /// `retrieve_temporal_combined_sql`): causal edges carry more than
    /// associative ones, so a walk prefers `causes`/`enables` over generic
    /// hops. Hindsight's constants: causes ×2.0, enables/prevents ×1.5.
    pub fn link_kind_boost(kind: &str) -> f64 {
        let k = kind.to_ascii_lowercase();
        match k.as_str() {
            "causes" | "caused_by" | "cause" | "because" => 2.0,
            "enables" | "prevents" | "enable" | "prevent" => 1.5,
            _ => 1.0,
        }
    }

    /// Whether the graph arm uses PPR instead of 1-hop expansion.
    /// `MEMNOTH_GRAPH=ppr` selects it; anything else (including unset) keeps the
    /// shipping 1-hop `link_expansion_scores` arm.
    pub(crate) fn ppr_from_env() -> bool {
        std::env::var("MEMNOTH_GRAPH")
            .map(|v| v.eq_ignore_ascii_case("ppr"))
            .unwrap_or(false)
    }

    /// Personalized PageRank over the memory graph (HippoRAG `run_ppr`,
    /// damping 0.5). Query-anchored mass spreads across the link graph and ranks
    /// every reachable entity — the **multi-hop** counterpart to the shipping
    /// 1-hop [`MemoryEngine::link_expansion_scores`] arm.
    ///
    /// `seeds` are `(entity_id, weight)` reset points (the fused text arm);
    /// `damping` is the PPR damping factor; `iterations` bounds the power loop.
    /// Edge weight is `relation.confidence × link_kind_boost(kind)` (S5), so
    /// causal edges dominate the walk. Dangling mass (a node with no out-edges)
    /// is returned to the reset distribution, so probability is conserved and
    /// the vector always sums to 1.
    ///
    /// The reachable subgraph is discovered once by BFS and capped at
    /// `PPR_NODE_CAP`, so cost is bounded on a dense graph with no schema change.
    pub fn ppr_rank(
        &self,
        seeds: &[(i64, f64)],
        as_of: i64,
        damping: f64,
        iterations: usize,
    ) -> Result<Vec<(i64, f64)>> {
        const PPR_NODE_CAP: usize = 2000;
        let mut reset: HashMap<i64, f64> = HashMap::new();
        for (id, w) in seeds {
            if *w > 0.0 {
                *reset.entry(*id).or_default() += *w;
            }
        }
        if reset.is_empty() {
            return Ok(Vec::new());
        }
        let seed_sum: f64 = reset.values().sum();
        for v in reset.values_mut() {
            *v /= seed_sum;
        }

        // Discover the reachable subgraph once (out-edges only), capped.
        let mut edges: Vec<(i64, i64, f64)> = Vec::new();
        let mut nodes: std::collections::HashSet<i64> = reset.keys().copied().collect();
        let mut stack: Vec<i64> = reset.keys().copied().collect();
        while let Some(id) = stack.pop() {
            if nodes.len() >= PPR_NODE_CAP {
                break;
            }
            for r in self.outgoing_relations(id, as_of)? {
                let w = r.confidence.max(0.0) * Self::link_kind_boost(&r.kind);
                if w <= 0.0 {
                    continue;
                }
                edges.push((id, r.target_id, w));
                if nodes.insert(r.target_id) {
                    stack.push(r.target_id);
                }
            }
        }

        // Out-strength per node, for row-normalised propagation.
        let mut out_deg: HashMap<i64, f64> = HashMap::new();
        for (f, _, w) in &edges {
            *out_deg.entry(*f).or_default() += *w;
        }

        let mut p: HashMap<i64, f64> = reset.clone();
        for _ in 0..iterations {
            let mut next: HashMap<i64, f64> = HashMap::new();
            let mut dangling = 0.0;
            for (id, val) in &p {
                if out_deg.get(id).copied().unwrap_or(0.0) <= 0.0 {
                    dangling += *val;
                }
            }
            for (id, r) in &reset {
                // teleport + conserved dangling mass
                *next.entry(*id).or_default() += (1.0 - damping) * r + damping * dangling * r;
            }
            for (f, t, w) in &edges {
                let d = out_deg.get(f).copied().unwrap_or(0.0);
                if d <= 0.0 {
                    continue;
                }
                let contribution = damping * p.get(f).copied().unwrap_or(0.0) * w / d;
                *next.entry(*t).or_default() += contribution;
            }
            p = next;
        }

        let mut out: Vec<(i64, f64)> = p.into_iter().collect();
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        Ok(out)
    }

    // ---------------------------------------------------------- observations
}
