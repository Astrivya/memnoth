//! Dense (embedding) retrieval arm — not compiled in.
//!
//! This file is the `semantic`-off counterpart of [`dense`](super::dense). A
//! minimal-profile build has no ONNX runtime, so there is no embedder to load
//! and no vectors to score; the whole module reduces to one honest answer.
//!
//! Keeping the module name and the function signature identical means every
//! call site is unconditional, and "no dense arm" travels the same path it
//! already travels when no model is installed — the lexical rank list and its
//! graph tail stand on their own, and nothing pretends a fusion happened.

use anyhow::Result;
use memnoth_engine::MemoryEngine;

/// Always `None`: no embedder is compiled into this build.
///
/// Same contract as the real implementation — `None` means "the dense arm is
/// unavailable, fall back to the lexical order", which is exactly what a build
/// without the semantic stack must say.
/// `(id, score)` pairs plus whether the scores were truncated to a prefix of the corpus. Spelled
/// out here too because `dense` is not compiled in this profile.
type DenseRanking = (Vec<(i64, f32)>, bool);

pub(crate) fn dense_ranking(
    _engine: &MemoryEngine,
    _query: &str,
    _now: i64,
) -> Result<Option<DenseRanking>> {
    Ok(None)
}
