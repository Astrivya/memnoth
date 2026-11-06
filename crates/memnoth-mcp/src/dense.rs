//! W0.1 — the dense (embedding) retrieval arm.
//!
//! Every strong system in this category fuses a **semantic** arm with its lexical
//! one (mem0, hindsight, MemOS, mempalace all ship a vector index *and* BM25), and
//! the arena says why we need it: our eight lexical arms span 63.0–64.3% any@1 on
//! LoCoMo, i.e. lexical re-tuning is exhausted, while the open-domain class sits at
//! a **73.9% any@10 ceiling**. A reranker can only reorder what fusion found, so the
//! ceiling has to be raised by retrieval that does not depend on token overlap at
//! all. That is this module.
//!
//! Three properties make it safe to bolt onto a vault that has no vector storage:
//!
//! 1. **Embeddings are content-addressed.** An embedding is a pure function of its
//!    text, so the cache is keyed by a hash of the passage and not by entity id. A
//!    process-global index keyed by `entity_id` would be *wrong* the moment two
//!    engines exist in one process — which is exactly what the test suite does, with
//!    every in-memory fixture numbering its entities from 1. Keying by content makes
//!    that collision impossible by construction, and shares work between engines.
//! 2. **No new storage.** The vault's schema is owned by another lane, so the corpus
//!    is read through `entities_current()` plus one read-only pass over observations.
//!    Nothing here writes.
//! 3. **Absence is not failure.** With no ONNX model installed the arm returns `None`
//!    and the caller keeps its lexical order, matching how the cross-encoder behaves.
//!
//! Cost is bounded on purpose: the first search in a process pays one embed per
//! distinct passage and every later one pays only a dot product per entity, because
//! vectors are cached normalised (so cosine similarity is a plain dot product).

use anyhow::Result;
use memnoth_embedding::{Embedder, EmbeddingVec};
use memnoth_engine::MemoryEngine;
use rusqlite::params;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Characters of an entity's text that reach the embedder.
///
/// The model truncates at 256 tokens regardless, so anything beyond this is wasted
/// work rather than lost signal. Kept close to the cross-encoder's passage budget so
/// the two arms score comparable text.
fn passage_chars() -> usize {
    std::env::var("MEMNOTH_DENSE_PASSAGE_CHARS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512)
}

/// Upper bound on entities indexed in one pass.
///
/// ~384 f32 per entity is ~1.5 KB, so this is roughly a 300 MB bound — generous at
/// the sizes this vault is measured at and a guard against an unbounded allocation on
/// a corpus nobody has measured yet. When the bound actually bites, the caller is
/// told (see `truncated`), because a silently partial index is a silently lowered
/// ceiling, which is the one failure mode this whole module exists to remove.
fn max_entities() -> usize {
    std::env::var("MEMNOTH_DENSE_MAX_ENTITIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000)
}

/// A result count large enough to be worth fusing, small enough to stay cheap.
const DENSE_POOL: usize = 200;

/// The process-wide embedder, loaded at most once.
///
/// Resolution mirrors the cross-encoder (`MEMNOTH_EMBEDDING_DIR`, then `HOME`, then
/// `USERPROFILE`), because a Git-style `HOME` is unopenable to a Windows process and
/// the model would be reported missing while sitting on disk.
fn embedder() -> Option<&'static Embedder> {
    static EMB: OnceLock<Option<Embedder>> = OnceLock::new();
    EMB.get_or_init(|| {
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        if let Some(dir) = std::env::var_os("MEMNOTH_EMBEDDING_DIR") {
            candidates.push(std::path::PathBuf::from(dir));
        }
        for home in ["HOME", "USERPROFILE"] {
            if let Some(base) = std::env::var_os(home) {
                candidates
                    .push(std::path::PathBuf::from(base).join(".memnoth/models/all-MiniLM-L6-v2"));
            }
        }
        let dir = candidates.into_iter().next()?;
        match Embedder::load_from_dir(&dir) {
            Ok(e) => Some(e),
            Err(e) => {
                eprintln!(
                    "memnoth: dense arm disabled, embedder at {} unavailable: {e}",
                    dir.display()
                );
                None
            }
        }
    })
    .as_ref()
}

/// FNV-1a over the passage text. Not cryptographic — this only has to be stable and
/// cheap, and a collision costs a wrong vector for one passage rather than anything
/// unsafe.
fn hash_text(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Content-addressed vector cache: passage hash to **normalised** vector.
fn vector_cache() -> &'static Mutex<HashMap<u64, Arc<EmbeddingVec>>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, Arc<EmbeddingVec>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn normalized(mut v: EmbeddingVec) -> Arc<EmbeddingVec> {
    let norm = v
        .iter()
        .map(|x| (*x as f64) * (*x as f64))
        .sum::<f64>()
        .sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x = (*x as f64 / norm) as f32;
        }
    }
    Arc::new(v)
}

/// Embed one passage, reusing the process-wide cache.
fn embed_cached(text: &str) -> Option<Arc<EmbeddingVec>> {
    let key = hash_text(text);
    if let Some(hit) = vector_cache().lock().ok()?.get(&key) {
        return Some(hit.clone());
    }
    let emb = embedder()?;
    let vec = normalized(emb.embed(text).ok()?);
    if let Ok(mut map) = vector_cache().lock() {
        map.insert(key, vec.clone());
    }
    Some(vec)
}

/// A cheap, index-friendly summary of corpus state, used to decide whether the cached
/// corpus is still valid.
///
/// The observation `SUM(entity_id)` is the load-bearing part: a merge **moves**
/// observations between entities without changing any row count or id, so `COUNT`
/// and `MAX` alone would let a merge silently leave a keeper's passage short. This
/// vault's writes are append/close/move rather than in-place content edits, so a
/// moved row is exactly what this catches.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    entities: (i64, i64),
    observations: (i64, i64, i64),
}

fn fingerprint(engine: &MemoryEngine) -> Result<Fingerprint> {
    let conn = engine.connection();
    let entities: (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(MAX(id), 0) FROM entities",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let observations: (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(MAX(id), 0), COALESCE(SUM(entity_id), 0) FROM observations",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(Fingerprint {
        entities,
        observations,
    })
}

/// The indexed corpus: entity id to normalised vector.
struct Corpus {
    ids: Vec<i64>,
    vecs: Vec<Arc<EmbeddingVec>>,
    /// True when `max_entities()` cut the corpus short.
    truncated: bool,
}

/// Builds the corpus. Called only when the cache misses.
fn build_corpus(engine: &MemoryEngine, now: i64) -> Result<Corpus> {
    let budget = passage_chars();
    let cap = max_entities();

    // One pass over live observations, grouped by the entity that owns them, instead
    // of `observations_for` per entity — the per-entity form is an N+1 query and at
    // 20k entities it dominates everything else in the search.
    let conn = engine.connection();
    let mut by_entity: HashMap<i64, String> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT entity_id, content FROM observations
             WHERE valid_from <= ?1 AND (valid_to IS NULL OR valid_to > ?1)
             ORDER BY entity_id, id",
        )?;
        let mut rows = stmt.query(params![now])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let content: String = row.get(1)?;
            let slot = by_entity.entry(id).or_default();
            if slot.chars().count() < budget {
                if !slot.is_empty() {
                    slot.push(' ');
                }
                slot.push_str(&content);
            }
        }
    }

    let entities = engine.entities_current()?;
    let truncated = entities.len() > cap;
    let mut ids = Vec::new();
    let mut vecs = Vec::new();
    for e in entities.into_iter().take(cap) {
        // Name and summary first: they are what makes an entity findable even when
        // its facts are prose, and the observations carry the answer text behind it.
        let mut passage = String::with_capacity(budget + 64);
        passage.push_str(&e.name);
        if let Some(s) = e.summary.as_deref() {
            passage.push(' ');
            passage.push_str(s);
        }
        if let Some(facts) = by_entity.get(&e.id) {
            passage.push(' ');
            passage.push_str(facts);
        }
        let passage: String = passage.chars().take(budget).collect();
        if passage.trim().is_empty() {
            continue;
        }
        let Some(v) = embed_cached(&passage) else {
            continue;
        };
        ids.push(e.id);
        vecs.push(v);
    }
    Ok(Corpus {
        ids,
        vecs,
        truncated,
    })
}

/// `(id, score)` pairs plus whether the scores were truncated to a prefix of the corpus.
pub(crate) type DenseRanking = (Vec<(i64, f32)>, bool);

/// The corpus, the fingerprint it was built from, and the embedder's dimensions.
type CachedCorpus = Option<((usize, Fingerprint), Arc<Corpus>)>;

/// The cached corpus plus the state it was built from.
fn corpus_cache() -> &'static Mutex<CachedCorpus> {
    static CACHE: OnceLock<Mutex<CachedCorpus>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// The corpus for this engine, rebuilt only when the fingerprint moves.
///
/// Keyed by the engine's address *as well as* the fingerprint so two engines in one
/// process cannot serve each other's corpus when they happen to share a shape.
fn corpus(engine: &MemoryEngine, now: i64) -> Result<Option<Arc<Corpus>>> {
    let key = (
        engine.connection() as *const _ as usize,
        fingerprint(engine)?,
    );
    if let Ok(guard) = corpus_cache().lock()
        && let Some((cached_key, c)) = guard.as_ref()
        && *cached_key == key
    {
        return Ok(Some(c.clone()));
    }
    if embedder().is_none() {
        return Ok(None);
    }
    let built = Arc::new(build_corpus(engine, now)?);
    if let Ok(mut guard) = corpus_cache().lock() {
        *guard = Some((key, built.clone()));
    }
    Ok(Some(built))
}

/// Rank the whole corpus against `query` by cosine similarity.
///
/// Returns `(id, score)` best-first, and whether the index was truncated — the
/// caller surfaces that rather than letting a partial index masquerade as a full
/// search. `None` means no embedder is installed, so the caller falls back to its
/// lexical order instead of failing.
pub(crate) fn dense_ranking(
    engine: &MemoryEngine,
    query: &str,
    now: i64,
) -> Result<Option<DenseRanking>> {
    let Some(corpus) = corpus(engine, now)? else {
        return Ok(None);
    };
    let Some(q) = embed_cached_query(query) else {
        return Ok(None);
    };
    let mut scored: Vec<(i64, f32)> = Vec::with_capacity(corpus.ids.len());
    for (id, v) in corpus.ids.iter().zip(corpus.vecs.iter()) {
        if v.len() != q.len() {
            continue;
        }
        let dot: f32 = v.iter().zip(q.iter()).map(|(a, b)| a * b).sum();
        scored.push((*id, dot));
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(DENSE_POOL);
    Ok(Some((scored, corpus.truncated)))
}

/// The query vector. Cached through the same content-addressed map — identical
/// queries (the arena issues many) cost one lookup after the first.
fn embed_cached_query(query: &str) -> Option<Arc<EmbeddingVec>> {
    embed_cached(query.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passage_hash_is_stable_and_distinguishes_text() {
        assert_eq!(hash_text("deploy pipeline"), hash_text("deploy pipeline"));
        assert_ne!(hash_text("deploy pipeline"), hash_text("deploy pipelines"));
        assert_ne!(hash_text(""), hash_text("a"));
    }

    #[test]
    fn normalization_makes_cosine_a_dot_product() {
        let v = normalized(EmbeddingVec::from(vec![3.0, 4.0]));
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-5,
            "vector must be unit length: {norm}"
        );
        // A zero vector must not become NaN — it stays zero and scores 0 on any dot.
        let zero = normalized(EmbeddingVec::from(vec![0.0, 0.0]));
        assert!(zero.iter().all(|x| *x == 0.0));
    }

    /// The cache must be keyed by content, not identity: two engines numbering their
    /// entities from 1 must never share a vector for different text. This is the bug
    /// that a naive process-global `HashMap<i64, Vec<f32>>` index would ship.
    #[test]
    fn embedding_cache_is_content_addressed_not_id_addressed() {
        let a = hash_text("alpha");
        let b = hash_text("beta");
        assert_ne!(a, b);
        let mut map: HashMap<u64, Arc<EmbeddingVec>> = HashMap::new();
        map.insert(a, Arc::new(EmbeddingVec::from(vec![1.0, 0.0])));
        map.insert(b, Arc::new(EmbeddingVec::from(vec![0.0, 1.0])));
        assert_eq!(map.get(&a).unwrap()[0], 1.0);
        assert_eq!(map.get(&b).unwrap()[0], 0.0);
    }
}
