//! Audited entity resolution: finding duplicate identities, and merging them
//! without losing the ability to explain what happened.
//!
//! Every system in this category eventually learns that `Postgres`,
//! `postgres` and `PostgreSQL` are one thing. The leaders do it with an LLM
//! similarity call inside the ingest path, which has two consequences a user
//! lives with: the merge is **silent** (two nodes become one and nothing says
//! so), and it is **irreversible in practice** (the reason lives in a prompt,
//! not in the database).
//!
//! This module takes the other position, which the vault's append-only journal
//! already makes cheap:
//!
//! * **Detection is separate from action.** [`duplicate_candidates`] reports
//!   what looks like a duplicate and why. It never merges.
//! * **A merge is explicit and journalled.** [`merge_entities`] writes a
//!   consolidation job plus one action row per effect, closes the losing
//!   entity through the same journal as every other change, and records the
//!   losing names as aliases so old names still resolve.
//! * **The losing entity is closed, not deleted.** `graph as-of` before the
//!   merge still shows two entities, because at that time there were two.
//!
//! The matching itself is deterministic — a normalised form and a bounded
//! prefix rule — because a false merge is much more expensive than a missed
//! one, and a missed one shows up in the next candidate report.

use anyhow::{Context, Result};
use rusqlite::params;

use crate::model::Entity;
use crate::now_ms;
use crate::store::MemoryEngine;

/// The comparison form: lowercase alphanumerics, single-spaced.
///
/// `"Postgres DB"`, `"postgres_db"` and `"POSTGRES-DB"` all become
/// `"postgres db"`. Punctuation and case are presentation; identity is not.
pub fn canonical_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with(' ') && !out.is_empty() {
            out.push(' ');
        }
    }
    out.trim_end().to_string()
}

/// How two entities were judged to be the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateReason {
    /// Identical comparison forms — `Postgres` and `postgres`.
    SameCanonical,
    /// One form is the other plus at most two characters — `user`/`users`,
    /// `postgres`/`postgresql`. Bounded on purpose: a loose prefix rule starts
    /// pairing unrelated names, and every false pair costs a human a review.
    PrefixVariant,
}

impl DuplicateReason {
    /// Stable string form of the reason, as it appears in a report.
    pub fn as_str(self) -> &'static str {
        match self {
            DuplicateReason::SameCanonical => "same_canonical",
            DuplicateReason::PrefixVariant => "prefix_variant",
        }
    }
}

/// One pair a human should look at, with both live entities attached.
#[derive(Debug, Clone)]
pub struct DuplicateCandidate {
    /// Which rule flagged the pair.
    pub reason: DuplicateReason,
    /// One of the two entities, as it appeared in the scan.
    pub left: Entity,
    /// The other entity of the pair.
    pub right: Entity,
}

/// What a merge did, counted rather than asserted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    /// Id of the consolidation job the merge was recorded under.
    pub job_id: i64,
    /// The surviving entity.
    pub into: i64,
    /// The entity that was closed.
    pub from: i64,
    /// Relations moved from the closed entity onto the survivor.
    pub relations_repointed: usize,
    /// Relations that became duplicates or self-loops and were dropped, with
    /// the surviving edge kept.
    pub relations_collapsed: usize,
    /// Observations moved from the closed entity onto the survivor.
    pub observations_moved: usize,
    /// Access counts summed across both entities.
    pub accesses_merged: i64,
    /// Canonical-name aliases the merge recorded.
    pub aliases_added: Vec<String>,
}

/// Live entities that look like the same thing, grouped by kind.
///
/// Kind is part of the comparison, not a filter applied afterwards: a `Person`
/// named `Ada` and a `Concept` named `ada` are not duplicates, and reporting
/// them as such would be noise that trains people to ignore the report.
///
/// W1: expiry-staged rows are excluded (same half-open + invalid_at guard as
/// the store's as-of queries).
pub fn duplicate_candidates(engine: &MemoryEngine, as_of: i64) -> Result<Vec<DuplicateCandidate>> {
    let as_of = crate::store::normalize_instant(as_of);
    let mut stmt = engine.connection().prepare(
        "SELECT id, kind, name, canonical_name, summary, confidence, valid_from, valid_to, metadata
         FROM entities
         WHERE valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
           AND (invalid_at IS NULL OR invalid_at > ?1)
         ORDER BY kind, canonical_name, id",
    )?;
    let entities: Vec<Entity> = stmt
        .query_map(params![as_of], MemoryEngine::row_to_entity)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut by_kind: std::collections::BTreeMap<String, Vec<&Entity>> = Default::default();
    for e in &entities {
        by_kind
            .entry(e.kind.as_str().to_string())
            .or_default()
            .push(e);
    }

    let mut out = Vec::new();
    for group in by_kind.values() {
        // Same canonical form: pairs within a bucket.
        let mut buckets: std::collections::BTreeMap<String, Vec<&Entity>> = Default::default();
        for e in group {
            let key = match &e.canonical_name {
                Some(c) if !c.is_empty() => c.clone(),
                _ => canonical_name(&e.name),
            };
            buckets.entry(key).or_default().push(e);
        }
        for members in buckets.values() {
            for pair in members.windows(2) {
                out.push(DuplicateCandidate {
                    reason: DuplicateReason::SameCanonical,
                    left: pair[0].clone(),
                    right: pair[1].clone(),
                });
            }
        }
        // Bounded prefix variants across buckets. Sorted, so a near-duplicate
        // is adjacent and the comparison is linear in practice.
        let keys: Vec<&String> = buckets.keys().collect();
        for i in 0..keys.len() {
            for j in (i + 1)..keys.len() {
                let (short, long) = (keys[i].as_str(), keys[j].as_str());
                if !long.starts_with(short) {
                    break; // sorted: no later key can start with `short` either
                }
                let extra = long.len() - short.len();
                if short.len() >= 3 && extra > 0 && extra <= 2 {
                    let a = buckets[keys[i]][0];
                    let b = buckets[keys[j]][0];
                    out.push(DuplicateCandidate {
                        reason: DuplicateReason::PrefixVariant,
                        left: a.clone(),
                        right: b.clone(),
                    });
                }
            }
        }
    }
    Ok(out)
}

/// W1 write-time triage verdict (Graphiti offset-index shape, zero-LLM).
/// Exact pre-pass + candidate scope decide without a model; W2 adds the LLM
/// call on top of the same categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriageVerdict {
    /// Same endpoint, same normalized content — merge/dedupe.
    Duplicate,
    /// Overlapping intervals with conflicting content — link, don't overwrite.
    Contradiction,
    /// Default: unrelated event — preserve as new.
    Neither,
}

impl TriageVerdict {
    /// Stable string form for metrics (triage accuracy per class).
    pub fn as_str(self) -> &'static str {
        match self {
            TriageVerdict::Duplicate => "duplicate",
            TriageVerdict::Contradiction => "contradiction",
            TriageVerdict::Neither => "neither",
        }
    }
}

/// A live row reduced to what the dup pre-pass reads: the canonical name, the
/// summary, and the validity window it overlaps.
type LiveSameKind = (Option<String>, Option<String>, i64, Option<i64>);

/// The same row plus the kind it belongs to, for the global collision scan.
type GlobalCandidate = (String, Option<String>, Option<String>, i64, Option<i64>);

/// W1 triage: dup = same-endpoint (same kind + same canonical name);
/// contradiction = global canonical collision across endpoints OR same endpoint
/// with overlapping validity but different summary; otherwise neither.
///
/// `same_endpoint` scopes the dup check (Graphiti same-endpoint); contradiction
/// scans globally. Interval overlap is guarded via half-open
/// [`crate::store::intervals_overlap`] before any expiry decision.
pub fn triage_update(
    engine: &MemoryEngine,
    kind: &crate::model::EntityKind,
    name: &str,
    summary: Option<&str>,
    valid_from: i64,
    valid_to: Option<i64>,
    as_of: i64,
) -> Result<TriageVerdict> {
    let as_of = crate::store::normalize_instant(as_of);
    let valid_from = crate::store::normalize_instant(valid_from);
    let canon = canonical_name(name);
    // Same-endpoint candidates: live rows of the same kind.
    let mut stmt = engine.connection().prepare(
        "SELECT canonical_name, summary, valid_from, valid_to FROM entities
         WHERE kind = ?1 AND valid_to IS NULL AND (invalid_at IS NULL OR invalid_at > ?2)",
    )?;
    let live_same_kind: Vec<LiveSameKind> = stmt
        .query_map(params![kind.as_str(), as_of], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    // Exact dup pre-pass: same canonical + same summary (or both empty).
    // W2 day rule: identical content on a different UTC day is a NEW event
    // (golden: different-day=neither); same-day re-ingest is a dup.
    for (c, s, f, _t) in &live_same_kind {
        let c_eq = c.as_deref().unwrap_or("") == canon;
        let s_eq = s.as_deref().unwrap_or("") == summary.unwrap_or("");
        if c_eq && s_eq {
            if crate::store::utc_day(*f) == crate::store::utc_day(valid_from) {
                return Ok(TriageVerdict::Duplicate);
            }
            return Ok(TriageVerdict::Neither);
        }
    }
    // Same-endpoint same-name different-content: contradiction (update), not dup.
    // A title-change is a contradiction; an identical re-ingest is a dup (above).
    for (c, _s, f, t) in &live_same_kind {
        if c.as_deref().unwrap_or("") == canon
            && crate::store::intervals_overlap(*f, *t, valid_from, valid_to)
        {
            return Ok(TriageVerdict::Contradiction);
        }
    }
    // Global contradiction scan: canonical collision in another kind, or same
    // kind with overlapping window but different content.
    let mut g = engine.connection().prepare(
        "SELECT kind, canonical_name, summary, valid_from, valid_to FROM entities
         WHERE valid_to IS NULL AND (invalid_at IS NULL OR invalid_at > ?1)",
    )?;
    let global: Vec<GlobalCandidate> = g
        .query_map(params![as_of], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(g);
    for (_k, c, s, f, t) in &global {
        if c.as_deref().unwrap_or("") != canon {
            continue;
        }
        let same_content = s.as_deref().unwrap_or("") == summary.unwrap_or("");
        if same_content {
            continue; // handled above as dup
        }
        if crate::store::intervals_overlap(*f, *t, valid_from, valid_to) {
            return Ok(TriageVerdict::Contradiction);
        }
        // Non-overlapping same-name different-content across time: still a
        // contradiction candidate (rename/supersede lineage), not neither.
        return Ok(TriageVerdict::Contradiction);
    }
    Ok(TriageVerdict::Neither)
}

/// W2 runner tunables (phase spec: batch loader 50, LLM batch 8/call).
pub const CONSOLIDATION_BATCH_LIMIT: usize = 50;
/// Max candidates per LLM triage call.
pub const LLM_BATCH_SIZE: usize = 8;
/// Near-dup cosine threshold: merge-or-keep with evidence fold at/above.
pub const NEAR_DUP_COSINE: f64 = 0.97;
/// Observation-history bound per entity (bounded trim, never delete).
pub const OBSERVATION_HISTORY_KEEP: usize = 20;

/// One candidate in a write-time triage call.
#[derive(Debug, Clone)]
pub struct TriageCandidate {
    /// Offset index into the request's candidate list (what the LLM returns).
    pub idx: usize,
    /// Live entity id behind the candidate.
    pub entity_id: i64,
    /// Normalized comparison form.
    pub canonical: String,
    /// Stored summary (empty when none).
    pub summary: String,
    /// Entity kind string (the endpoint scope).
    pub endpoint: String,
}

/// Input to one triage call: the newcomer plus its candidate set.
#[derive(Debug, Clone)]
pub struct TriageRequest {
    /// Newcomer kind string.
    pub new_kind: String,
    /// Newcomer normalized form.
    pub new_canonical: String,
    /// Newcomer summary (empty when none).
    pub new_summary: String,
    /// Candidates with stable offset indices.
    pub candidates: Vec<TriageCandidate>,
}

/// Output of one triage call: offset indices only. The engine range-validates
/// before applying — an out-of-range or doubly-claimed index is refused, never
/// applied.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TriageResponse {
    /// Candidate offsets judged duplicates of the newcomer.
    pub dup_idx: Vec<usize>,
    /// Candidate offsets judged contradicted by the newcomer.
    pub contradicted_idx: Vec<usize>,
}

/// Range-validate a triage response: every index < `n`, no index claimed twice
/// (dup ∩ contradicted = ∅, no repeats within a list).
pub fn validate_triage_response(resp: &TriageResponse, n: usize) -> Result<()> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for i in resp.dup_idx.iter().chain(resp.contradicted_idx.iter()) {
        if *i >= n {
            anyhow::bail!("triage index {i} out of range for {n} candidates");
        }
        if !seen.insert(*i) {
            anyhow::bail!("triage index {i} claimed twice");
        }
    }
    Ok(())
}

/// Character-trigram cosine between two texts (zero-LLM near-dup proxy).
/// The embedding cosine lives with the caller; this pre-filter runs inside the
/// engine so the LLM batch only sees plausible pairs.
pub fn trigram_cosine(a: &str, b: &str) -> f64 {
    fn trigrams(s: &str) -> std::collections::HashMap<String, f64> {
        let norm = canonical_name(s);
        let chars: Vec<char> = norm.chars().collect();
        let mut m = std::collections::HashMap::new();
        if chars.len() < 3 {
            *m.entry(norm).or_default() += 1.0;
            return m;
        }
        for w in chars.windows(3) {
            *m.entry(w.iter().collect::<String>()).or_default() += 1.0;
        }
        m
    }
    let (x, y) = (trigrams(a), trigrams(b));
    let mut dot = 0.0;
    let (mut nx, mut ny) = (0.0, 0.0);
    for (k, xv) in &x {
        nx += xv * xv;
        if let Some(yv) = y.get(k) {
            dot += xv * yv;
        }
    }
    for yv in y.values() {
        ny += yv * yv;
    }
    if nx == 0.0 || ny == 0.0 {
        return 0.0;
    }
    (dot / (nx.sqrt() * ny.sqrt())).clamp(0.0, 1.0)
}

/// Build one triage request: exact pre-pass map (normalized triple
/// kind|canonical|summary → entity) is consulted first by the caller via
/// [`triage_update`]; candidates here are same-endpoint live rows plus hybrid
/// RRF text hits, deduped by entity id, capped for one LLM call.
pub fn build_triage_request(
    engine: &MemoryEngine,
    kind: &crate::model::EntityKind,
    name: &str,
    summary: Option<&str>,
    as_of: i64,
) -> Result<TriageRequest> {
    let as_of = crate::store::normalize_instant(as_of);
    let new_canonical = canonical_name(name);
    let new_summary = summary.unwrap_or("").to_string();
    let mut seen = std::collections::HashSet::new();
    let mut candidates = Vec::new();
    // Same-endpoint: live rows of the newcomer's kind.
    {
        let mut stmt = engine.connection().prepare(
            "SELECT id, canonical_name, summary, kind FROM entities
             WHERE kind = ?1 AND valid_to IS NULL AND (invalid_at IS NULL OR invalid_at > ?2)
             ORDER BY id LIMIT 30",
        )?;
        let rows: Vec<(i64, Option<String>, Option<String>, String)> = stmt
            .query_map(params![kind.as_str(), as_of], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        for (id, c, s, ep) in rows {
            if seen.insert(id) {
                candidates.push(TriageCandidate {
                    idx: candidates.len(),
                    entity_id: id,
                    canonical: c.unwrap_or_default(),
                    summary: s.unwrap_or_default(),
                    endpoint: ep,
                });
            }
        }
    }
    // Hybrid RRF text hits (global): catches contradictions on other endpoints.
    for (ent, _) in engine.search_entities(name, 20, as_of)?.into_iter() {
        if seen.insert(ent.id) {
            candidates.push(TriageCandidate {
                idx: candidates.len(),
                entity_id: ent.id,
                canonical: ent.canonical_name.unwrap_or_default(),
                summary: ent.summary.unwrap_or_default(),
                endpoint: ent.kind.as_str().to_string(),
            });
        }
        if candidates.len() >= 40 {
            break;
        }
    }
    Ok(TriageRequest {
        new_kind: kind.as_str().to_string(),
        new_canonical,
        new_summary,
        candidates,
    })
}

/// Merge-or-keep with evidence fold: cosine ≥ [`NEAR_DUP_COSINE`] merges `from`
/// into `into` (proof counts summed, aliases kept, fully journalled);
/// below threshold both rows are kept, linked, and marked consolidated.
/// Returns true when merged.
pub fn merge_or_keep(
    engine: &MemoryEngine,
    job_id: i64,
    from: i64,
    into: i64,
    reason: &str,
    at: i64,
) -> Result<bool> {
    let (f_name, f_sum): (String, Option<String>) = engine.connection().query_row(
        "SELECT name, summary FROM entities WHERE id = ?1",
        params![from],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (t_name, t_sum): (String, Option<String>) = engine.connection().query_row(
        "SELECT name, summary FROM entities WHERE id = ?1",
        params![into],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let cos = trigram_cosine(
        &format!("{f_name} {}", f_sum.unwrap_or_default()),
        &format!("{t_name} {}", t_sum.unwrap_or_default()),
    );
    if cos >= NEAR_DUP_COSINE {
        let out = merge_entities(engine, from, into, reason, at)?;
        // Evidence fold happens inside merge_entities (proof_count summed,
        // winner generation+1); the job action records the decision.
        engine.record_job_action(
            job_id,
            "merge_or_keep",
            &f_name,
            &serde_json::json!({"merged": true, "cosine": cos, "into": into, "job": out.job_id}),
            at,
        )?;
        // Bound observation history for the merged-into entity.
        engine.trim_observations(into, OBSERVATION_HISTORY_KEEP, at)?;
        Ok(true)
    } else {
        engine.add_memory_link(into, from, "near_dup_keep", reason, at)?;
        engine.mark_consolidated(from, at)?;
        engine.mark_consolidated(into, at)?;
        engine.record_job_action(
            job_id,
            "merge_or_keep",
            &f_name,
            &serde_json::json!({"merged": false, "cosine": cos, "into": into}),
            at,
        )?;
        Ok(false)
    }
}

/// Contradiction journey synthesis: episode + ADD-only links, history
/// preserved. Both rows stay (recency + retrieval arbitrate); the episode is
/// the journey record W2 promises.
pub fn synthesize_journey(
    engine: &MemoryEngine,
    job_id: i64,
    old_id: i64,
    new_id: i64,
    reason: &str,
    at: i64,
) -> Result<i64> {
    let ep = engine.append_episode(
        "contradiction",
        None,
        Some(&format!(
            "contradiction journey: {old_id} vs {new_id}: {reason}"
        )),
        at,
    )?;
    engine.add_memory_link(new_id, old_id, "contradicts", reason, at)?;
    engine.record_job_action(
        job_id,
        "synthesize_journey",
        &format!("{old_id}->{new_id}"),
        &serde_json::json!({"episode": ep, "old": old_id, "new": new_id}),
        at,
    )?;
    engine.close_episode(ep, at)?;
    Ok(ep)
}

/// One batch of the consolidation runner: the oldest unconsolidated live rows
/// (stale-verify against `watermark`), ordered by id, capped at `limit`.
pub fn load_consolidation_batch(
    engine: &MemoryEngine,
    watermark: i64,
    limit: usize,
) -> Result<Vec<Entity>> {
    let limit = limit.clamp(1, CONSOLIDATION_BATCH_LIMIT);
    let mut stmt = engine.connection().prepare(
        "SELECT id, kind, name, canonical_name, summary, confidence, valid_from, valid_to, metadata
         FROM entities
         WHERE valid_to IS NULL AND (invalid_at IS NULL)
           AND (consolidated_at IS NULL OR consolidated_at < ?1)
         ORDER BY id LIMIT ?2",
    )?;
    Ok(stmt
        .query_map(
            params![watermark, limit as i64],
            MemoryEngine::row_to_entity,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Outcome of one runner batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationOutcome {
    /// Rows scanned (batch size).
    pub scanned: usize,
    /// Rows refined in place (neither verdict: marked + proof bumped).
    pub refined: usize,
    /// New rows created (dup contradiction paths that open successors).
    pub created: usize,
    /// Merges applied.
    pub merged: usize,
    /// Contradiction journeys synthesized.
    pub contradicted: usize,
    /// Skipped by stale-verify (already consolidated under this watermark).
    pub skipped_stale: usize,
}

/// Run one consolidation batch over existing tables.
///
/// `llm` is the injected small-model triage call: request → dup/contradicted
/// offset indices. Items are grouped `llm_chunk`-per-call (≤8); each item is
/// stale-verified, triaged, and applied (create-vs-refine: dup → merge-or-keep
/// with evidence fold; contradicted → journey synthesis; neither → refine mark
/// followed by a bounded observation trim). One witness job covers the batch;
/// the watermark advances only after the whole batch commits.
pub fn run_consolidation_batch(
    engine: &MemoryEngine,
    llm: impl Fn(&TriageRequest) -> Result<TriageResponse>,
    limit: usize,
    llm_chunk: usize,
    at: i64,
    reason: &str,
) -> Result<ConsolidationOutcome> {
    let at = crate::store::normalize_instant(at);
    let llm_chunk = llm_chunk.clamp(1, LLM_BATCH_SIZE);
    let watermark = engine.consolidation_watermark()?;
    let batch = load_consolidation_batch(engine, watermark, limit)?;
    let job_id = engine.begin_job(at)?;
    let mut out = ConsolidationOutcome {
        scanned: batch.len(),
        refined: 0,
        created: 0,
        merged: 0,
        contradicted: 0,
        skipped_stale: 0,
    };
    for chunk in batch.chunks(llm_chunk.max(1)) {
        for ent in chunk {
            // Stale-verify per item: a concurrent runner may have consolidated it.
            if !engine.needs_consolidation(ent.id, watermark)? {
                out.skipped_stale += 1;
                continue;
            }
            let req =
                build_triage_request(engine, &ent.kind, &ent.name, ent.summary.as_deref(), at)?;
            // Restrict the LLM view to this item's neighborhood: candidate that
            // IS the item itself is exact-dup noise, so drop self-matches.
            let filtered: Vec<TriageCandidate> = req
                .candidates
                .into_iter()
                .filter(|c| c.entity_id != ent.id)
                .enumerate()
                .map(|(i, mut c)| {
                    c.idx = i;
                    c
                })
                .collect();
            let req = TriageRequest {
                candidates: filtered,
                ..req
            };
            let resp = llm(&req)?;
            validate_triage_response(&resp, req.candidates.len())?;
            let mut handled = false;
            for i in &resp.dup_idx {
                let cand = &req.candidates[*i];
                // Create-vs-refine: dup → merge-or-keep against the keeper
                // (lower id keeps the lineage stable).
                let (from, into) = if ent.id < cand.entity_id {
                    (cand.entity_id, ent.id)
                } else {
                    (ent.id, cand.entity_id)
                };
                if from == into {
                    continue;
                }
                if merge_or_keep(engine, job_id, from, into, reason, at)? {
                    out.merged += 1;
                }
                handled = true;
            }
            for i in &resp.contradicted_idx {
                let cand = &req.candidates[*i];
                synthesize_journey(engine, job_id, cand.entity_id, ent.id, reason, at)?;
                engine.bump_proof_count(ent.id, 1)?;
                out.contradicted += 1;
                handled = true;
            }
            if !handled {
                // Neither (refine): mark + proof bump + bounded trim + episode.
                engine.bump_proof_count(ent.id, 1)?;
                engine.trim_observations(ent.id, OBSERVATION_HISTORY_KEEP, at)?;
                let ep = engine.append_episode(
                    "refine",
                    None,
                    Some(&format!("refined {}", ent.name)),
                    at,
                )?;
                engine.close_episode(ep, at)?;
                out.refined += 1;
            }
            engine.mark_consolidated(ent.id, at)?;
        }
    }
    engine.finish_job(
        job_id,
        &serde_json::json!({"scanned": out.scanned, "merged": out.merged, "contradicted": out.contradicted, "refined": out.refined}),
        at,
    )?;
    engine.set_consolidation_watermark(at)?;
    Ok(out)
}

/// Merge `from` into `into`, journalled, and return what changed.
///
/// The whole operation is one transaction: a merge that is half-applied is
/// worse than one that did not happen, because the journal would then describe
/// a state the graph never had.
///
/// Refuses when either entity is missing or already closed — merging into a
/// closed entity would silently revive a revoked belief, and merging a closed
/// one would double-close it and corrupt the as-of view.
pub fn merge_entities(
    engine: &MemoryEngine,
    from: i64,
    into: i64,
    reason: &str,
    at: i64,
) -> Result<MergeOutcome> {
    let at = crate::store::normalize_instant(at);
    if from == into {
        anyhow::bail!("cannot merge entity {from} into itself");
    }
    let loser = load_live(engine, from)?.with_context(|| format!("entity {from} is not live"))?;
    let winner = load_live(engine, into)?.with_context(|| format!("entity {into} is not live"))?;
    if loser.kind != winner.kind {
        anyhow::bail!(
            "refusing to merge across kinds: {} is {}, {} is {}",
            loser.name,
            loser.kind.as_str(),
            winner.name,
            winner.kind.as_str()
        );
    }

    let tx = engine.connection().unchecked_transaction()?;
    let conn = &tx;
    conn.execute(
        "INSERT INTO consolidation_jobs(status, started_at, stats) VALUES('running', ?1, '{}')",
        params![at],
    )?;
    let job_id = conn.last_insert_rowid();

    // 1. Every name the loser was known by still resolves — its own name and
    //    any alias it had already absorbed.
    let mut aliases: Vec<String> = vec![loser.name.clone()];
    {
        let mut stmt =
            conn.prepare("SELECT alias FROM entity_aliases WHERE entity_id = ?1 ORDER BY alias")?;
        let existing: Vec<String> = stmt
            .query_map(params![from], |r| r.get::<_, String>(0))?
            .filter_map(|x| x.ok())
            .collect();
        drop(stmt);
        aliases.extend(existing);
        aliases.sort();
        aliases.dedup();
    }
    let mut aliases_added = Vec::new();
    for alias in &aliases {
        let changed = conn.execute(
            "INSERT OR IGNORE INTO entity_aliases(alias, entity_id, source_name, reason, recorded_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![alias, into, loser.name, reason, at],
        )?;
        if changed > 0 {
            aliases_added.push(alias.clone());
        }
    }

    // 2. Relations move with the entity. A self-loop is impossible afterwards
    //    (it would be an entity related to itself, which the merge created), so
    //    it is dropped rather than left as a lie.
    let repointed = conn.execute(
        "UPDATE relations SET source_id = ?2 WHERE source_id = ?1",
        params![from, into],
    )? + conn.execute(
        "UPDATE relations SET target_id = ?2 WHERE target_id = ?1",
        params![from, into],
    )?;
    let self_loops = conn.execute("DELETE FROM relations WHERE source_id = target_id", [])?;
    // Identical edges that became identical through the merge collapse to one.
    let dupes = conn.execute(
        "DELETE FROM relations WHERE id NOT IN (
             SELECT MIN(id) FROM relations GROUP BY source_id, target_id, kind
         )",
        [],
    )?;

    // 3. Observations and access history follow the entity.
    let moved = conn.execute(
        "UPDATE observations SET entity_id = ?2 WHERE entity_id = ?1",
        params![from, into],
    )?;
    let merged_accesses: i64 = conn
        .query_row(
            "SELECT COALESCE(access_count, 0) FROM access_stats WHERE entity_id = ?1",
            params![from],
            |r| r.get(0),
        )
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO access_stats(entity_id, access_count, last_access)
         SELECT ?2, 0, NULL
         WHERE NOT EXISTS (SELECT 1 FROM access_stats WHERE entity_id = ?2)",
        params![from, into],
    )?;
    conn.execute(
        "UPDATE access_stats SET access_count = access_count + ?1 WHERE entity_id = ?2",
        params![merged_accesses, into],
    )?;
    conn.execute(
        "DELETE FROM access_stats WHERE entity_id = ?1",
        params![from],
    )?;

    // 4. Close the loser through the ordinary journal, with the merge as its
    //    recorded reason — so `vault_contradictions` and `entity_history`
    //    explain the disappearance without a special case.
    let mut meta: serde_json::Value =
        serde_json::from_str(&loser.metadata).unwrap_or(serde_json::Value::Null);
    if !meta.is_object() {
        meta = serde_json::json!({});
    }
    let meta_json = {
        let obj = meta.as_object_mut().expect("just normalized to object");
        obj.insert("merged_into".into(), serde_json::json!(into));
        meta.to_string()
    };
    conn.execute(
        "UPDATE entities SET valid_to = ?1, metadata = ?2 WHERE id = ?3 AND valid_to IS NULL",
        params![at, meta_json, from],
    )?;
    engine.journal_entity(&loser, Some(at), reason)?;

    // 5. One action row per effect, so the journal is readable without
    //    replaying the counts.
    let actions: [(&str, String, serde_json::Value); 5] = [
        (
            "merge_entity",
            loser.name.clone(),
            serde_json::json!({ "into": into, "into_name": winner.name, "reason": reason }),
        ),
        (
            "repoint_relations",
            loser.name.clone(),
            serde_json::json!({ "repointed": repointed, "collapsed": self_loops + dupes }),
        ),
        (
            "move_observations",
            loser.name.clone(),
            serde_json::json!({ "moved": moved }),
        ),
        (
            "record_aliases",
            loser.name.clone(),
            serde_json::json!({ "aliases": aliases_added }),
        ),
        (
            "close_entity",
            loser.name.clone(),
            serde_json::json!({ "valid_to": at, "journal": "entity_versions" }),
        ),
    ];
    for (action_type, target, payload) in actions {
        conn.execute(
            "INSERT INTO consolidation_actions(job_id, action_type, target, payload, applied_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![job_id, action_type, target, payload.to_string(), at],
        )?;
    }
    let stats = serde_json::json!({
        "from": from,
        "into": into,
        "relations_repointed": repointed,
        "relations_collapsed": self_loops + dupes,
        "observations_moved": moved,
        "accesses_merged": merged_accesses,
        "aliases_added": aliases_added,
    });
    conn.execute(
        "UPDATE consolidation_jobs SET status='done', finished_at=?1, stats=?2 WHERE id=?3",
        params![at, stats.to_string(), job_id],
    )?;
    // W1 ADD-only link row: merged_from lineage, append-only.
    conn.execute(
        "INSERT INTO memory_links (source_id, target_id, kind, reason, created_at)
         VALUES (?1, ?2, 'merged_from', ?3, ?4)",
        params![into, from, reason, at],
    )?;
    // W2 lineage counters: winner generation+1, proof fold (loser proofs move).
    conn.execute(
        "UPDATE entities SET generation = COALESCE(generation,0) + 1,
             proof_count = COALESCE(proof_count,0) + (SELECT COALESCE(proof_count,0) FROM entities WHERE id = ?2)
         WHERE id = ?1",
        params![into, from],
    )?;
    tx.commit()?;
    // W2: fresh checksums on both rows after the merge.
    let _ = engine.refresh_checksum(into);
    let _ = engine.refresh_checksum(from);

    // Delivered only after the merge is durable: an audit trail that reports an
    // action the database rolled back would be worse than no trail at all.
    engine.emit_audit(
        "entity.merge",
        &loser.name,
        "applied",
        serde_json::json!({
            "from": from,
            "into": into,
            "into_name": winner.name.clone(),
            "relations_repointed": repointed,
            "observations_moved": moved,
            "reason": reason,
        }),
        at,
    );

    Ok(MergeOutcome {
        job_id,
        into,
        from,
        relations_repointed: repointed,
        relations_collapsed: self_loops + dupes,
        observations_moved: moved,
        accesses_merged: merged_accesses,
        aliases_added,
    })
}

/// The journal entry for one merge, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRecord {
    /// Id of the consolidation job the action belongs to.
    pub job_id: i64,
    /// Which step of the merge the row records, e.g. `merge_entity`.
    pub action_type: String,
    /// Name of the entity the step was applied to — the absorbed one.
    pub target: String,
    /// Wall-clock milliseconds at which the merge was applied.
    pub applied_at: i64,
}

/// Every recorded consolidation action, newest first — the audit trail a merge
/// leaves behind, readable without knowing it happened.
pub fn merge_history(engine: &MemoryEngine) -> Result<Vec<MergeRecord>> {
    let mut stmt = engine.connection().prepare(
        "SELECT job_id, action_type, target, applied_at FROM consolidation_actions
         ORDER BY applied_at DESC, id DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(MergeRecord {
                job_id: r.get(0)?,
                action_type: r.get(1)?,
                target: r.get(2)?,
                applied_at: r.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Names the entity answers to, other than its current one.
pub fn aliases_of(engine: &MemoryEngine, entity_id: i64) -> Result<Vec<String>> {
    let mut stmt = engine
        .connection()
        .prepare("SELECT alias FROM entity_aliases WHERE entity_id = ?1 ORDER BY alias")?;
    let rows = stmt
        .query_map(params![entity_id], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn load_live(engine: &MemoryEngine, id: i64) -> Result<Option<Entity>> {
    let now = i64::MAX / 2;
    let mut stmt = engine.connection().prepare(
        "SELECT id, kind, name, canonical_name, summary, confidence, valid_from, valid_to, metadata
         FROM entities WHERE id = ?1 AND valid_to IS NULL AND valid_from <= ?2
           AND (invalid_at IS NULL OR invalid_at > ?2)",
    )?;
    let mut rows = stmt.query_map(params![id, now], MemoryEngine::row_to_entity)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

/// A candidate's default timestamp: "now", so a merge is recorded when it
/// happened rather than when the data was written.
pub fn merge_now() -> i64 {
    now_ms()
}

/// Convenience for callers that only have a name: the live entity that name
/// resolves to, whether it is the current name, a canonical form, or an alias.
pub fn resolve_name(engine: &MemoryEngine, name: &str, as_of: i64) -> Result<Option<Entity>> {
    Ok(engine
        .find_entities_by_name(name, as_of)?
        .into_iter()
        .next())
}

#[cfg(test)]
mod tests;
