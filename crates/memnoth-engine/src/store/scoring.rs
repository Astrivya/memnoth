//! Split out of `store.rs` — an `impl MemoryEngine` block in a child module,
//! which keeps access to the type's private fields while cutting the file's size.

use super::*;

impl MemoryEngine {
    /// Boost factor with a kill switch: `micro_boost` when enabled, neutral
    /// 1.0 when the ablation mask turns boosts off.
    pub(crate) fn boost_factor(&self, entity: &Entity, as_of: i64, enabled: bool) -> Result<f64> {
        if enabled {
            self.micro_boost(entity, as_of)
        } else {
            Ok(1.0)
        }
    }

    /// Rerank hook for the cross-encoder second stage. The engine deliberately
    /// does NOT depend on `memnoth-embedding` (no new dep, no `Cargo.toml`
    /// change per the lane seam): the caller runs
    /// `memnoth_embedding::CrossEncoder::rerank` externally over
    /// `RERANK_BUDGET` candidates and passes the resulting `{id → logit}`
    /// map in. Scores are min-max normalized to `CE_norm ∈ [0,1]`, then
    /// `final = CE_norm × recency × temporal × proof` (Hindsight shape).
    /// An empty score map returns the input order untouched.
    ///
    /// Call-site wiring (outside this crate):
    /// ```ignore
    /// let cands: Vec<(String, String)> = hits.iter()
    ///     .take(MemoryEngine::RERANK_BUDGET)
    ///     .map(|(e, _)| (e.id.to_string(), format!("{} {}", e.name,
    ///         e.summary.clone().unwrap_or_default()))).collect();
    /// let ce = ce_model.rerank(query, &cands)?; // Vec<(String, f32)>
    /// let map: HashMap<i64, f32> = ce.into_iter()
    ///     .filter_map(|(id, s)| id.parse().ok().map(|n| (n, s))).collect();
    /// let hits = engine.rerank_with_ce(hits, &map, as_of)?;
    /// ```
    pub fn rerank_with_ce(
        &self,
        hits: Vec<(Entity, f64)>,
        ce_scores: &HashMap<i64, f32>,
        as_of: i64,
    ) -> Result<Vec<(Entity, f64)>> {
        if ce_scores.is_empty() {
            return Ok(hits);
        }
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for (e, _) in &hits {
            if let Some(s) = ce_scores.get(&e.id) {
                lo = lo.min(*s);
                hi = hi.max(*s);
            }
        }
        // `abs()` because the span is a difference of floats: reading it bare looks
        // like an equality test, and the span is non-negative by construction here.
        if !lo.is_finite() || !hi.is_finite() || (hi - lo).abs() < f32::EPSILON {
            return Ok(hits);
        }
        let mut out = Vec::with_capacity(hits.len());
        for (e, _) in hits {
            if let Some(s) = ce_scores.get(&e.id) {
                let ce_norm = ((s - lo) / (hi - lo)) as f64;
                out.push((e.clone(), ce_norm * self.micro_boost(&e, as_of)?));
            } else {
                // threshold-gate (Mem0 pattern): candidates the CE never
                // scored keep no score — they can't promote irrelevants.
                continue;
            }
        }
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(out)
    }

    /// Rough token estimate for budget packing: ~4 chars/token.
    pub fn estimate_tokens(text: &str) -> usize {
        text.len().div_ceil(4)
    }

    /// Token-budget packing: walk the ranked list top-down, take what fits,
    /// skip oversize items, never return empty when input is non-empty (the
    /// top hit is kept even over budget). `text_of` renders each hit for
    /// sizing (e.g. `name + summary`).
    pub fn pack_with_budget(
        hits: Vec<(Entity, f64)>,
        budget_tokens: usize,
        text_of: impl Fn(&Entity) -> String,
    ) -> Vec<(Entity, f64)> {
        if hits.is_empty() {
            return hits;
        }
        let mut out = Vec::new();
        let mut used = 0usize;
        let mut head: Option<(Entity, f64)> = None;
        for (e, s) in hits {
            let cost = Self::estimate_tokens(&text_of(&e));
            if used + cost <= budget_tokens {
                used += cost;
                out.push((e, s));
            } else if head.is_none() {
                // The first hit that did not fit — see the fallback below.
                head = Some((e, s));
            }
            // else: skip oversize, keep packing smaller hits below
        }
        if out.is_empty() {
            // never-empty: keep the top hit even over budget. The item is *held*
            // here rather than searched for afterwards: `head` is the first hit
            // that did not fit, which is the top hit exactly when nothing fitted,
            // so the rule has no branch that could fail to find what it promised.
            if let Some(top) = head {
                out.push(top);
            }
        }
        out
    }

    // ------------------------------------------------------- R2 arms ----
    //
    // Phase R2 (domination-plan/retrieve.md): the graph arm grows additive
    // link scoring and a temporal-window arm joins the fusion. Both are
    // query-time (SEARCH-path) only: temporal *duality columns* and
    // *write-time* date grounding are schema/write-path work owned by
    // lane-write — nothing here adds a column or touches ingest.

    /// `(semantic, causal)` weights for one relation kind (R2 graph arm).
    ///
    /// Causal aliases mirror the `memnoth-mcp` gauntlet `CausalKind`
    /// vocabulary (that crate owns it; this is a read-only mirror so the
    /// engine needs no dependency edge in the wrong direction): causes-family
    /// 1.0, enables/supports-family 0.7, prevents/blocks-family 0.6, else 0.
    /// Semantic weight always lands in `[0.7, 1.0]`: identity pins
    /// (implements/owns/authored/part-of/member-of) 1.0, lineage links
    /// (supersedes/merged-from/reopen) 0.85, causal edges 0.85, and every
    /// other (generic or unknown) kind a neutral 0.7 — an edge that exists
    /// is weak evidence, never zero.
    pub fn link_kind_scores(kind: &str) -> (f64, f64) {
        let k = kind.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        let causal = match k.as_str() {
            "causes" | "cause" | "caused" | "causality" | "leads_to" => 1.0,
            "enables" | "enable" | "enabled" | "facilitates" | "supports" => 0.7,
            "prevents" | "prevent" | "prevented" | "blocks" | "inhibits" => 0.6,
            _ => 0.0,
        };
        let semantic = if causal > 0.0 {
            0.85
        } else {
            match k.as_str() {
                "implements" | "owns" | "authored" | "authored_by" | "part_of" | "member_of" => 1.0,
                "supersedes" | "merged_from" | "reopen" | "reopened" => 0.85,
                _ => 0.7,
            }
        };
        (semantic, causal)
    }

    /// Seed-limited 1-hop link expansion with additive scoring (R2 graph
    /// arm): per target, `tanh(n·0.5) + semantic + causal`, where `n` is the
    /// number of converging expansion edges (multi-seed agreement saturates
    /// via tanh). Returns `(entity_id, link_score)` sorted score-desc,
    /// id-asc. Seeds themselves are excluded; other direct-arm members are
    /// filtered by the caller (`hybrid_search_masked`) so the arm stays pure
    /// expansion. Cap seeds at `GRAPH_SEEDS` (caller contract).
    pub fn link_expansion_scores(&self, seeds: &[i64], as_of: i64) -> Result<Vec<(i64, f64)>> {
        let edges = self.expand_frontier(seeds, as_of)?;
        let seed_set: std::collections::HashSet<i64> = seeds.iter().copied().collect();
        // target → (converging edge count, max semantic, max causal)
        let mut acc: HashMap<i64, (u32, f64, f64)> = HashMap::new();
        for edge in &edges {
            if edge.target_id == edge.source_id || seed_set.contains(&edge.target_id) {
                continue;
            }
            let (sem, cau) = Self::link_kind_scores(&edge.kind);
            let e = acc.entry(edge.target_id).or_insert((0, 0.0, 0.0));
            e.0 += 1;
            e.1 = e.1.max(sem);
            e.2 = e.2.max(cau);
        }
        let mut out: Vec<(i64, f64)> = acc
            .into_iter()
            .map(|(id, (n, s, c))| {
                let support = ((n as f64) * 0.5).tanh();
                (id, support + s + c)
            })
            .collect();
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        Ok(out)
    }

    /// ms in one day (rolling temporal windows).
    pub const DAY_MS: i64 = 86_400_000;

    pub(crate) fn month_index(name: &str) -> Option<u32> {
        Some(match name {
            "january" | "jan" => 1,
            "february" | "feb" => 2,
            "march" | "mar" => 3,
            "april" | "apr" => 4,
            "may" => 5,
            "june" | "jun" => 6,
            "july" | "jul" => 7,
            "august" | "aug" => 8,
            "september" | "sep" | "sept" => 9,
            "october" | "oct" => 10,
            "november" | "nov" => 11,
            "december" | "dec" => 12,
            _ => return None,
        })
    }

    pub(crate) fn month_bounds_ms(year: i32, month: u32) -> Option<(i64, i64)> {
        use chrono::{NaiveDate, TimeZone, Utc};
        let s = NaiveDate::from_ymd_opt(year, month, 1)?.and_hms_opt(0, 0, 0)?;
        let (ny, nm) = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month + 1)
        };
        let e = NaiveDate::from_ymd_opt(ny, nm, 1)?.and_hms_opt(0, 0, 0)?;
        Some((
            Utc.from_utc_datetime(&s).timestamp_millis(),
            Utc.from_utc_datetime(&e).timestamp_millis(),
        ))
    }

    pub(crate) fn year_bounds_ms(year: i32) -> Option<(i64, i64)> {
        use chrono::{NaiveDate, TimeZone, Utc};
        let s = NaiveDate::from_ymd_opt(year, 1, 1)?.and_hms_opt(0, 0, 0)?;
        let e = NaiveDate::from_ymd_opt(year + 1, 1, 1)?.and_hms_opt(0, 0, 0)?;
        Some((
            Utc.from_utc_datetime(&s).timestamp_millis(),
            Utc.from_utc_datetime(&e).timestamp_millis(),
        ))
    }

    /// `(start_ms, end_ms)` for one meteorological season occurrence: `year`
    /// is the year holding the season's start (spring/summer/fall), except
    /// winter, which spans Dec of `year-1` → Mar of `year`.
    pub(crate) fn season_bounds_ms(season: &str, year: i32) -> Option<(i64, i64)> {
        use chrono::{NaiveDate, TimeZone, Utc};
        let at = |y: i32, m: u32| -> Option<i64> {
            let d = NaiveDate::from_ymd_opt(y, m, 1)?.and_hms_opt(0, 0, 0)?;
            Some(Utc.from_utc_datetime(&d).timestamp_millis())
        };
        Some(match season {
            "spring" => (at(year, 3)?, at(year, 6)?),
            "summer" => (at(year, 6)?, at(year, 9)?),
            "fall" | "autumn" => (at(year, 9)?, at(year, 12)?),
            "winter" => (at(year - 1, 12)?, at(year, 3)?),
            _ => return None,
        })
    }

    /// Parse a temporal window from query text → `(start_ms, end_ms)`
    /// (R2 temporal arm). Grammar (case-insensitive):
    /// `in YYYY`, `<month> YYYY` / `in <month> YYYY`, `last week|month|year`
    /// (rolling back from `as_of`), `last spring|summer|fall|autumn|winter`
    /// (most recent *completed* occurrence). A bare 4-digit year with no
    /// `in`/month is rejected (avoids `error 2024` false positives).
    /// `None` when the query carries no temporal expression. Rolling
    /// `last <unit>` windows anchor at `as_of`; write-time grounding of
    /// relative dates (`last week` → absolute at ingest) is lane-write's.
    pub fn parse_temporal_window(query: &str, as_of: i64) -> Option<(i64, i64)> {
        let toks: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(|t| t.to_ascii_lowercase())
            .collect();
        // "last <unit|season>"
        for w in toks.windows(2) {
            if w[0] == "last" {
                match w[1].as_str() {
                    "week" => return Some((as_of - 7 * Self::DAY_MS, as_of)),
                    "month" => return Some((as_of - 30 * Self::DAY_MS, as_of)),
                    "year" => return Some((as_of - 365 * Self::DAY_MS, as_of)),
                    s if matches!(s, "spring" | "summer" | "fall" | "autumn" | "winter") => {
                        use chrono::{Datelike, TimeZone, Utc};
                        let y = Utc.timestamp_millis_opt(as_of).single()?.year();
                        let (s0, e0) = Self::season_bounds_ms(s, y)?;
                        if e0 <= as_of {
                            return Some((s0, e0));
                        }
                        return Self::season_bounds_ms(s, y - 1);
                    }
                    _ => {}
                }
            }
        }
        // "<month> <year>" / "in <month> <year>" / "in <year>"
        for (ix, t) in toks.iter().enumerate() {
            if t.len() == 4
                && let Ok(y) = t.parse::<i32>()
                && (1900..2100).contains(&y)
            {
                let prev = if ix > 0 { toks[ix - 1].as_str() } else { "" };
                if let Some(m) = Self::month_index(prev) {
                    return Self::month_bounds_ms(y, m);
                }
                if prev == "in" {
                    return Self::year_bounds_ms(y);
                }
            }
        }
        None
    }

    /// Remove the temporal expression spans from a query so FTS arms don't
    /// AND-match on `in`/`2023`/`last` (R2: parse → window, then search the
    /// topic remainder). Returns the topic remainder; empty when the query
    /// was pure-temporal (the semantic gate then applies).
    pub fn strip_temporal(query: &str) -> String {
        let toks: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(|t| t.to_ascii_lowercase())
            .collect();
        let mut drop = vec![false; toks.len()];
        let mut ix = 0;
        while ix < toks.len() {
            if toks[ix] == "last"
                && ix + 1 < toks.len()
                && matches!(
                    toks[ix + 1].as_str(),
                    "week" | "month" | "year" | "spring" | "summer" | "fall" | "autumn" | "winter"
                )
            {
                drop[ix] = true;
                drop[ix + 1] = true;
                ix += 2;
                continue;
            }
            if toks[ix] == "in" && ix + 1 < toks.len() {
                // "in <month> <year>"
                if ix + 2 < toks.len()
                    && Self::month_index(&toks[ix + 1]).is_some()
                    && toks[ix + 2].len() == 4
                    && toks[ix + 2]
                        .parse::<i32>()
                        .is_ok_and(|y| (1900..2100).contains(&y))
                {
                    drop[ix] = true;
                    drop[ix + 1] = true;
                    drop[ix + 2] = true;
                    ix += 3;
                    continue;
                }
                // "in <year>"
                if toks[ix + 1].len() == 4
                    && toks[ix + 1]
                        .parse::<i32>()
                        .is_ok_and(|y| (1900..2100).contains(&y))
                {
                    drop[ix] = true;
                    drop[ix + 1] = true;
                    ix += 2;
                    continue;
                }
            }
            // "<month> <year>"
            if ix + 1 < toks.len()
                && Self::month_index(&toks[ix]).is_some()
                && toks[ix + 1].len() == 4
                && toks[ix + 1]
                    .parse::<i32>()
                    .is_ok_and(|y| (1900..2100).contains(&y))
            {
                drop[ix] = true;
                drop[ix + 1] = true;
                ix += 2;
                continue;
            }
            ix += 1;
        }
        toks.iter()
            .enumerate()
            .filter(|(i, _)| !drop[*i])
            .map(|(_, t)| t.clone())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Temporal-window arm (R2): entities born (`valid_from`) inside
    /// `window`, relevance-first (index order: confidence desc) with bucket
    /// spreading (round-robin across weekly buckets so one burst can't crowd
    /// the arm) and center boost (near window-center first within a bucket
    /// after confidence). Returns ranked ids, capped. As-of filtered.
    /// Coarse-date end-of-period scoring needs the duality columns
    /// (lane-write schema) and is NOT approximated here.
    pub fn temporal_arm(&self, window: (i64, i64), as_of: i64, cap: usize) -> Result<Vec<i64>> {
        let (start, end) = window;
        if end <= start {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, confidence, valid_from FROM entities
             WHERE valid_from >= ?1 AND valid_from < ?2
               AND valid_from <= ?3 AND (valid_to IS NULL OR valid_to > ?3)
             ORDER BY confidence DESC LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(
                params![start, end, as_of, (cap.saturating_mul(4).max(60)) as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        // bucket spreading: weekly buckets in window order, round-robin take.
        let week = 7 * Self::DAY_MS;
        let center = start + (end - start) / 2;
        let half = ((end - start) / 2).max(1);
        let center_score = |vf: i64| -> f64 {
            (1.0 - ((vf - center).abs() as f64) / (half as f64)).clamp(0.0, 1.0)
        };
        let mut buckets: std::collections::HashMap<i64, Vec<(i64, f64, f64)>> =
            std::collections::HashMap::new();
        let mut order: Vec<i64> = Vec::new();
        for (id, conf, vf) in rows {
            let b = (vf - start) / week;
            if !buckets.contains_key(&b) {
                order.push(b);
            }
            buckets
                .entry(b)
                .or_default()
                .push((id, conf, center_score(vf)));
        }
        order.sort_unstable();
        for bucket in buckets.values_mut() {
            // relevance-first, then center boost (rows arrive conf-desc;
            // the sort keeps it, center breaks confidence ties).
            bucket.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
            });
        }
        let mut out = Vec::new();
        let mut progress = true;
        while out.len() < cap && progress {
            progress = false;
            for b in &order {
                if out.len() >= cap {
                    break;
                }
                if let Some(bucket) = buckets.get_mut(b)
                    && !bucket.is_empty()
                {
                    out.push(bucket.remove(0).0);
                    progress = true;
                }
            }
        }
        Ok(out)
    }

    /// Hybrid retrieval: FTS ranking fused with 1-hop graph expansion via
    /// N-arm Reciprocal Rank Fusion (K=60, equal weights). Neighbors of strong
    /// text matches surface even when they don't themselves contain the query
    /// terms.
    ///
    /// Direct matches fuse two text sources — the entity index (name/summary)
    /// and the observation index (an entity's stored facts). Observations are
    /// where a document's body actually lives, so a query that only appears in
    /// observation text still surfaces the owning entity instead of reporting
    /// "nothing here".
    ///
    /// R1 shape: entity-FTS arm (over-fetch 50, capped) + observation-FTS arm
    /// (over-fetch 50, capped) + 1-hop graph arm (10 seeds), fused with
    /// [`MemoryEngine::rrf_fuse_arms`], then micro-boosts. R2 adds the
    /// temporal-window arm (arm 3, fires only on temporal queries) and scores
    /// the graph arm additively (`link_expansion_scores`). External
    /// (vector/semantic/BM25) arms fuse via
    /// [`MemoryEngine::hybrid_search_masked`] — pass ranked id lists
    /// over-fetched to `max(limit*4, 60)`; no schema change needed.
    pub fn hybrid_search(
        &self,
        query: &str,
        limit: usize,
        as_of: i64,
    ) -> Result<Vec<(Entity, f64)>> {
        self.hybrid_search_with_arms(query, limit, as_of, &[])
    }

    /// [`MemoryEngine::hybrid_search`] plus external ranked-id arms (vector /
    /// semantic / BM25 from outside the engine). Semantic gate (Mem0
    /// pattern): arms fuse only when at least one text arm fired — a
    /// vector-only signal never promotes entities for a query with zero text
    /// anchor unless the caller passes its ids as the *query* arm explicitly
    /// via [`MemoryEngine::fused_search`]. Temporal expressions are parsed
    /// out of the query first (`parse_temporal_window` + `strip_temporal`)
    /// so FTS arms search the topic remainder while the temporal arm scores
    /// the window.
    pub fn hybrid_search_with_arms(
        &self,
        query: &str,
        limit: usize,
        as_of: i64,
        extra_arms: &[Vec<i64>],
    ) -> Result<Vec<(Entity, f64)>> {
        self.hybrid_search_masked(query, limit, as_of, ArmMask::full(), extra_arms)
    }

    /// [`MemoryEngine::hybrid_search_with_arms`] with per-arm control
    /// (7-run matrix ablation contract). Fixed arm slots — entity 0,
    /// observation 1, graph 2, temporal 3, extras 4+ — so the earliest-arm
    /// tiebreak keeps its meaning whatever the mask. A masked-off arm fuses
    /// as empty (no score, no tiebreak claim).
    pub fn hybrid_search_masked(
        &self,
        query: &str,
        limit: usize,
        as_of: i64,
        mask: ArmMask,
        extra_arms: &[Vec<i64>],
    ) -> Result<Vec<(Entity, f64)>> {
        let topic = Self::strip_temporal(query);
        // arm 0: entity FTS (name/summary hits are the precise signal)
        let arm_entity: Vec<i64> = if mask.entity_fts && !topic.is_empty() {
            Self::cap_arm(
                self.search_entities(&topic, Self::FTS_OVERFETCH, as_of)?
                    .iter()
                    .map(|(e, _)| e.id)
                    .collect(),
                Self::ARM_CAP,
            )
        } else {
            Vec::new()
        };
        // arm 1: observation FTS mapped to owning entities (dedup, order kept)
        let arm_obs: Vec<i64> = if mask.obs_fts && !topic.is_empty() {
            let mut seen_obs: std::collections::HashSet<i64> = std::collections::HashSet::new();
            let mut ids: Vec<i64> = Vec::new();
            for (o, _) in self
                .search_observations(&topic, Self::FTS_OVERFETCH, as_of)?
                .iter()
            {
                if seen_obs.insert(o.entity_id) {
                    ids.push(o.entity_id);
                }
            }
            Self::cap_arm(ids, Self::ARM_CAP)
        } else {
            Vec::new()
        };
        if arm_entity.is_empty() && arm_obs.is_empty() {
            // threshold-gate: no text anchor → don't let graph/vector junk
            // promote irrelevants (empty-never-errors: Ok(empty), not Err).
            // A pure-temporal query ("in 2023" with no topic token) stops
            // here by design; the sprint fixture carries ≥1 topic token per
            // temporal query (see the 7-run matrix ASK).
            return Ok(Vec::new());
        }

        // arm 2: link-expansion graph arm (R2 additive scoring), pure
        // expansion — direct-arm members filtered so text matches can't
        // double-dip through their own edges.
        let arm_graph: Vec<i64> = if mask.graph {
            let seeds: Vec<i64> = arm_entity
                .iter()
                .chain(arm_obs.iter())
                .take(Self::GRAPH_SEEDS)
                .copied()
                .collect();
            let direct: std::collections::HashSet<i64> =
                arm_entity.iter().chain(arm_obs.iter()).copied().collect();
            // S2: `MEMNOTH_GRAPH=ppr` swaps the 1-hop arm for multi-hop PPR,
            // seeded on the text arms with reciprocal-rank weights (the same
            // 1/(1+rank) shape the RRF merge uses). Default stays 1-hop.
            let expanded: Vec<(i64, f64)> = if Self::ppr_from_env() {
                let weighted: Vec<(i64, f64)> = seeds
                    .iter()
                    .enumerate()
                    .map(|(ix, id)| (*id, 1.0 / (ix as f64 + 1.0)))
                    .collect();
                self.ppr_rank(&weighted, as_of, 0.5, 20)?
            } else {
                self.link_expansion_scores(&seeds, as_of)?
            };
            Self::cap_arm(
                expanded
                    .into_iter()
                    .filter(|(id, _)| !direct.contains(id))
                    .map(|(id, _)| id)
                    .collect(),
                Self::ARM_CAP,
            )
        } else {
            Vec::new()
        };

        // arm 3: temporal window (R2; only fires on temporal queries).
        // NOT deduped against text arms — in-window + text match converging
        // is the signal (multi-arm fusion rewards it).
        let arm_temporal: Vec<i64> = if mask.temporal {
            match Self::parse_temporal_window(query, as_of) {
                Some(window) => Self::cap_arm(
                    self.temporal_arm(window, as_of, Self::ARM_CAP)?,
                    Self::ARM_CAP,
                ),
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };

        let mut arms: Vec<Vec<i64>> = vec![arm_entity, arm_obs, arm_graph, arm_temporal];
        for extra in extra_arms {
            arms.push(Self::cap_arm(extra.clone(), Self::ARM_CAP));
        }
        self.fused_search_masked(&arms, limit, as_of, mask.boosts)
    }

    /// Record that an entity was read at `at` (memary entity-knowledge-store
    /// pattern: frequency + recency both shape future ranking). Idempotent
    /// upsert; never errors on a stale/revoked id.
    pub fn record_access(&self, entity_id: i64, at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO access_stats(entity_id, access_count, last_access)
             VALUES(?1, 1, ?2)
             ON CONFLICT(entity_id) DO UPDATE SET
               access_count = access_count + 1,
               last_access = excluded.last_access",
            params![entity_id, at],
        )?;
        Ok(())
    }

    /// Top entities by recency+frequency (memary depth ranking): frequency
    /// primary, last-access recency tiebreak. `limit` max 100. Expiry-staged.
    pub fn top_entities(&self, as_of: i64, limit: usize) -> Result<Vec<(Entity, i64)>> {
        let as_of = normalize_instant(as_of);
        let limit = limit.min(100);
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.kind, e.name, e.canonical_name, e.summary, e.confidence,
                    e.valid_from, e.valid_to, e.metadata, a.access_count
             FROM access_stats a
             JOIN entities e ON e.id = a.entity_id
             WHERE e.valid_from <= ?1 AND (e.valid_to IS NULL OR e.valid_to > ?1)
               AND (e.invalid_at IS NULL OR e.invalid_at > ?1)
             ORDER BY a.access_count DESC, a.last_access DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![as_of, limit as i64], |row| {
                Ok((Self::row_to_entity(row)?, row.get::<_, i64>(9)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ------------------------------------------------------- durability ops

    /// SQLite's own integrity verdict for this vault (`"ok"` when healthy).
    ///
    /// Cheap enough to run before a backup or after a crash; the honest answer
    /// to "is this file structurally sound?" rather than an assertion that it is.
    pub fn integrity_check(&self) -> Result<String> {
        let verdict: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(verdict)
    }

    /// Foreign-key violations currently in the vault (0 is the healthy answer).
    /// `PRAGMA foreign_keys` is on, so a non-zero count means a file written by
    /// something that bypassed the engine.
    pub fn foreign_key_violations(&self) -> Result<i64> {
        let mut stmt = self.conn.prepare("PRAGMA foreign_key_check")?;
        let rows = stmt.query_map([], |_| Ok(()))?;
        let mut n = 0i64;
        for r in rows {
            r?;
            n += 1;
        }
        Ok(n)
    }

    /// Fold the write-ahead log back into the main database file.
    ///
    /// WAL keeps readers consistent without blocking writers, but it also means
    /// a `.db` copied without its `-wal` sibling is incomplete. Call this before
    /// a backup or an export; it is safe with readers attached and is a no-op
    /// when the log is already empty.
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    /// Vault counters (current vs total). Current excludes expiry-staged past now.
    pub fn stats(&self) -> Result<Stats> {
        let now = now_ms();
        let q = |sql: &str| -> Result<i64> {
            Ok(self.conn.query_row(sql, params![now], |row| row.get(0))?)
        };
        Ok(Stats {
            entities_current: q(
                "SELECT COUNT(*) FROM entities WHERE valid_to IS NULL AND (invalid_at IS NULL OR invalid_at > ?1)",
            )?,
            entities_total: self
                .conn
                .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?,
            relations_current: q(
                "SELECT COUNT(*) FROM relations WHERE valid_to IS NULL AND revoked_at IS NULL AND (invalid_at IS NULL OR invalid_at > ?1)",
            )?,
            relations_total: self
                .conn
                .query_row("SELECT COUNT(*) FROM relations", [], |row| row.get(0))?,
            observations_total: self.conn.query_row(
                "SELECT COUNT(*) FROM observations",
                [],
                |row| row.get(0),
            )?,
        })
    }

    // ------------------------------------------------------------ internals

    pub(crate) fn row_to_entity(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entity> {
        Ok(Entity {
            id: row.get(0)?,
            kind: EntityKind::from_label(&row.get::<_, String>(1)?),
            name: row.get(2)?,
            canonical_name: row.get(3)?,
            summary: row.get(4)?,
            confidence: row.get(5)?,
            valid_from: row.get(6)?,
            valid_to: row.get(7)?,
            metadata: row.get(8)?,
        })
    }

    pub(crate) fn row_to_relation(row: &rusqlite::Row<'_>) -> rusqlite::Result<Relation> {
        Ok(Relation {
            id: row.get(0)?,
            source_id: row.get(1)?,
            target_id: row.get(2)?,
            kind: row.get(3)?,
            confidence: row.get(4)?,
            valid_from: row.get(5)?,
            valid_to: row.get(6)?,
            revoked_at: row.get(7)?,
            metadata: row.get(8)?,
        })
    }

    pub(crate) fn journal_entity(
        &self,
        e: &Entity,
        valid_to: Option<i64>,
        reason: &str,
    ) -> Result<()> {
        let snapshot = serde_json::to_string(e)?;
        self.conn.execute(
            "INSERT INTO entity_versions (entity_id, snapshot, valid_from, valid_to, change_reason)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![e.id, snapshot, e.valid_from, valid_to, reason],
        )?;
        Ok(())
    }

    pub(crate) fn journal_relation(
        &self,
        r: &Relation,
        valid_to: Option<i64>,
        reason: &str,
    ) -> Result<()> {
        let snapshot = serde_json::to_string(r)?;
        self.conn.execute(
            "INSERT INTO relation_versions (relation_id, snapshot, valid_from, valid_to, change_reason)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![r.id, snapshot, r.valid_from, valid_to, reason],
        )?;
        Ok(())
    }
}
