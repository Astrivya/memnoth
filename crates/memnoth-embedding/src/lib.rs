//! memnoth-embedding — embedding primitives and cross-encoder reranker.
//!
//! Minimal viable implementation to unblock memnoth-mcp compilation.
//! The dense retrieval arm uses `Embedder` and `EmbeddingVec`;
//! `CrossEncoder` is available behind the `semantic` feature.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A dense embedding vector.
///
/// Vectors are normalized to unit length so cosine similarity is a dot product.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingVec {
    pub(crate) data: Vec<f32>,
    pub(crate) dim: usize,
}

impl EmbeddingVec {
    /// Create a new zeroed embedding vector.
    pub fn new(dim: usize) -> Self {
        EmbeddingVec {
            data: vec![0.0; dim],
            dim,
        }
    }

    /// Create an embedding vector from raw data.
    pub fn from_vec(data: Vec<f32>) -> Self {
        let dim = data.len();
        EmbeddingVec { data, dim }
    }

    /// Get the dimension of this vector.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Get a reference to the underlying data.
    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }

    /// Compute cosine similarity with another vector (requires same dimension).
    pub fn cosine_similarity(&self, other: &EmbeddingVec) -> Result<f32> {
        if self.dim != other.dim {
            anyhow::bail!("dimension mismatch: {} vs {}", self.dim, other.dim);
        }
        let dot: f32 = self
            .data
            .iter()
            .zip(other.data.iter())
            .map(|(a, b)| a * b)
            .sum();
        let norm_a: f32 = self.data.iter().map(|a| a * a).sum::<f32>().sqrt();
        let norm_b: f32 = other.data.iter().map(|b| b * b).sum::<f32>().sqrt();
        if norm_a == 0.0 || norm_b == 0.0 {
            Ok(0.0)
        } else {
            Ok(dot / (norm_a * norm_b))
        }
    }

    /// Normalize this vector to unit length.
    pub fn normalize(&mut self) {
        let norm: f32 = self.data.iter().map(|a| a * a).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in self.data.iter_mut() {
                *v /= norm;
            }
        }
    }
}

/// The embedding model.
pub struct Embedder {
    pub(crate) dim: usize,
    pub(crate) model_id: String,
}

impl Embedder {
    /// Create a new embedder.
    pub fn new(dim: usize, model_id: impl Into<String>) -> Self {
        Embedder {
            dim,
            model_id: model_id.into(),
        }
    }

    /// Get the embedding dimension.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Embed a passage into a vector.
    pub fn embed(&self, text: &str) -> Result<EmbeddingVec> {
        Ok(EmbeddingVec {
            data: deterministic_hash_embed(text, self.dim),
            dim: self.dim,
        })
    }

    /// Get the model ID.
    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}

/// Generate a deterministic pseudo-random embedding from text.
fn deterministic_hash_embed(text: &str, dim: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; dim];
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        let idx = (*b as usize + i) % dim;
        v[idx] += (*b as f32) / 255.0;
    }
    // Normalize
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    v
}

/// Cross-encoder reranker (optional, behind `semantic` feature).
///
/// On non-ONNX builds this is a stub; `rerank` returns `None` and callers
/// fall back to lexical order.
pub struct CrossEncoder {
    _private: (),
}

impl CrossEncoder {
    /// Create a new cross-encoder.
    pub fn new() -> Result<Self> {
        Ok(CrossEncoder { _private: () })
    }

    /// Rerank candidates given a query.
    ///
    /// Returns `None` when no cross-encoder model is installed.
    pub fn rerank(&self, _query: &str, _candidates: &[String]) -> Result<Option<Vec<f32>>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_identical() {
        let a = EmbeddingVec::from_vec(vec![1.0, 0.0, 0.0]);
        let b = EmbeddingVec::from_vec(vec![1.0, 0.0, 0.0]);
        assert!((a.cosine_similarity(&b).unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = EmbeddingVec::from_vec(vec![1.0, 0.0]);
        let b = EmbeddingVec::from_vec(vec![0.0, 1.0]);
        assert!((a.cosine_similarity(&b).unwrap() - 0.0).abs() < 1e-6);
    }

    #[test]
    fn embedder_returns_correct_dim() {
        let e = Embedder::new(384, "test-model");
        assert_eq!(e.dim(), 384);
    }
}
