//! [`VectorStore`] — pluggable dense-vector index.
//!
//! [`FlatVectorStore`] is the default. It is a `Vec<Option<Vec<f32>>>`
//! where `None` marks a tombstoned slot. `search` is a full scan
//! over alive slots with cosine similarity (assumes L2-normalised
//! vectors — both [`crate::HashingEmbedder`] and any
//! `ModelProviderEmbedder` are expected to L2-normalise before
//! insertion).
//!
//! `HnswStore` (feature `hnsw`) is the ANN alternative: an HNSW
//! graph with `O(log n)` search at the cost of a bigger build
//! footprint. Implementors pass either store to
//! `SearchEngine::with_vector_store`.

#[cfg(feature = "hnsw")]
mod hnsw;
#[cfg(feature = "hnsw")]
pub use hnsw::{HnswParams, HnswStore};

#[cfg(feature = "simd")]
pub mod dot;
#[cfg(feature = "simd")]
pub use dot::{DotRoute, dot as simd_dot, scalar_dot};

use crate::error::{Result, SearchError};

pub trait VectorStore: Send + Sync + 'static {
    fn add(&mut self, v: Vec<f32>) -> Result<usize>;
    fn mark_deleted(&mut self, idx: usize);
    fn is_alive(&self, idx: usize) -> bool;
    fn n_alive(&self) -> usize;
    fn dim(&self) -> usize;
    fn search(&self, q: &[f32], top_k: usize) -> Vec<(usize, f32)>;
    /// 快照用：取槽位向量；tombstone 或越界槽返回 `None`。
    fn vector_at(&self, idx: usize) -> Option<Vec<f32>>;
}

pub struct FlatVectorStore {
    vectors: Vec<Option<Vec<f32>>>,
    alive: usize,
    dim: usize,
}

impl FlatVectorStore {
    pub fn new(dim: usize) -> Self {
        Self {
            vectors: Vec::new(),
            alive: 0,
            dim,
        }
    }
}

impl VectorStore for FlatVectorStore {
    fn add(&mut self, mut v: Vec<f32>) -> Result<usize> {
        if v.len() != self.dim {
            return Err(SearchError::DimMismatch {
                expected: self.dim,
                got: v.len(),
            });
        }
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 1e-12 {
            for x in v.iter_mut() {
                *x /= n;
            }
        }
        let idx = self.vectors.len();
        self.vectors.push(Some(v));
        self.alive += 1;
        Ok(idx)
    }

    fn mark_deleted(&mut self, idx: usize) {
        if let Some(slot) = self.vectors.get_mut(idx)
            && slot.is_some()
        {
            *slot = None;
            self.alive = self.alive.saturating_sub(1);
        }
    }

    fn is_alive(&self, idx: usize) -> bool {
        self.vectors.get(idx).and_then(|o| o.as_ref()).is_some()
    }

    fn n_alive(&self) -> usize {
        self.alive
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn vector_at(&self, idx: usize) -> Option<Vec<f32>> {
        self.vectors.get(idx).cloned().flatten()
    }

    fn search(&self, q: &[f32], top_k: usize) -> Vec<(usize, f32)> {
        if q.len() != self.dim {
            return Vec::new();
        }
        let mut scores: Vec<(usize, f32)> = self
            .vectors
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.as_ref().map(|v| (i, dot(q, v).max(0.0))))
            .collect();
        scores.sort_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        });
        scores.truncate(top_k);
        scores
    }
}

/// Scalar fallback (matches the original
/// `FlatVectorStore::search` dot). Exposed publicly so
/// `SimdVectorStore::search` can fall through on
/// non-x86 / non-aarch64 builds, and so the bench
/// can compare SIMD vs scalar on the same data.
///
/// When the `simd` feature is on, the canonical name is
/// `synthia_search::vector::dot::scalar_dot`; this
/// public re-export keeps the default-only build
/// ergonomic.
#[cfg(not(feature = "simd"))]
pub fn scalar_dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(feature = "simd")]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    simd_dot(a, b)
}

#[cfg(not(feature = "simd"))]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    scalar_dot(a, b)
}

/// SIMD-backed vector store — a `FlatVectorStore` whose
/// `search` runs through the arch-specific dot
/// ([`simd_dot`]). Construction + tombstone / vector_at
/// semantics are otherwise identical to the parent.
///
/// Only available with the `simd` feature; default
/// builds compile to nothing.
#[cfg(feature = "simd")]
pub struct SimdVectorStore {
    inner: FlatVectorStore,
}

#[cfg(feature = "simd")]
impl SimdVectorStore {
    pub fn new(dim: usize) -> Self {
        Self {
            inner: FlatVectorStore::new(dim),
        }
    }

    /// Drop-down to the underlying scalar store when the
    /// caller needs the rest of the `FlatVectorStore`
    /// surface (clone, persistence, …).
    pub fn into_inner(self) -> FlatVectorStore {
        self.inner
    }
}

#[cfg(feature = "simd")]
impl Default for SimdVectorStore {
    fn default() -> Self {
        Self::new(0)
    }
}

#[cfg(feature = "simd")]
impl VectorStore for SimdVectorStore {
    fn add(&mut self, v: Vec<f32>) -> Result<usize> {
        self.inner.add(v)
    }

    fn mark_deleted(&mut self, idx: usize) {
        self.inner.mark_deleted(idx)
    }

    fn is_alive(&self, idx: usize) -> bool {
        self.inner.is_alive(idx)
    }

    fn n_alive(&self) -> usize {
        self.inner.n_alive()
    }

    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn search(&self, q: &[f32], top_k: usize) -> Vec<(usize, f32)> {
        if q.len() != self.inner.dim {
            return Vec::new();
        }
        let mut scores: Vec<(usize, f32)> = self
            .inner
            .vectors
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                v.as_ref().map(|v| (i, simd_dot(q, v).max(0.0)))
            })
            .collect();
        scores.sort_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        });
        scores.truncate(top_k);
        scores
    }

    fn vector_at(&self, idx: usize) -> Option<Vec<f32>> {
        self.inner.vector_at(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_rejects_dim_mismatch() {
        let mut s = FlatVectorStore::new(4);
        let err = s.add(vec![1.0, 2.0, 3.0]).unwrap_err();
        match err {
            SearchError::DimMismatch { expected, got } => {
                assert_eq!(expected, 4);
                assert_eq!(got, 3);
            }
            other => panic!("expected DimMismatch, got {other:?}"),
        }
    }

    #[test]
    fn search_filters_tombstoned_slots() {
        let mut s = FlatVectorStore::new(3);
        let v0 = vec![1.0, 0.0, 0.0];
        let v1 = vec![0.0, 1.0, 0.0];
        let i0 = s.add(v0).unwrap();
        let i1 = s.add(v1).unwrap();
        s.mark_deleted(i0);
        let q = vec![0.0, 1.0, 0.0];
        let hits = s.search(&q, 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, i1);
    }

    #[test]
    fn search_returns_top_k_descending() {
        let mut s = FlatVectorStore::new(2);
        let a = s.add(vec![1.0, 0.0]).unwrap();
        let b = s.add(vec![0.9, 0.1]).unwrap();
        let c = s.add(vec![0.0, 1.0]).unwrap();
        let q = vec![1.0, 0.0];
        let hits = s.search(&q, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, a);
        assert_eq!(hits[1].0, b);
        assert!(!hits.iter().any(|(i, _)| *i == c));
    }

    #[test]
    fn search_returns_empty_on_dim_mismatch() {
        let s = FlatVectorStore::new(2);
        let hits = s.search(&[1.0, 0.0, 0.0], 5);
        assert!(hits.is_empty());
    }

    #[test]
    fn n_alive_tracks_tombstones() {
        let mut s = FlatVectorStore::new(2);
        let a = s.add(vec![1.0, 0.0]).unwrap();
        s.add(vec![0.0, 1.0]).unwrap();
        assert_eq!(s.n_alive(), 2);
        s.mark_deleted(a);
        assert_eq!(s.n_alive(), 1);
    }

    #[test]
    fn vector_at_returns_alive_vector_and_none_for_tombstone() {
        let mut s = super::FlatVectorStore::new(2);
        let a = s.add(vec![1.0, 0.0]).unwrap();
        let b = s.add(vec![0.0, 1.0]).unwrap();
        assert_eq!(s.vector_at(a), Some(vec![1.0, 0.0]));
        s.mark_deleted(b);
        assert_eq!(s.vector_at(b), None);
        assert_eq!(s.vector_at(999), None);
    }
}
