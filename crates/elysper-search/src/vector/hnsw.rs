//! [`HnswStore`] — HNSW-backed [`VectorStore`].
//!
//! Compiled only when the `hnsw` feature is on (the default build
//! stays zero-dep; `make check-mvp-deps` asserts this feature also
//! stays offline — `hnsw_rs` pulls in only `serde` + `rand` +
//! `rayon` + `anndists`, none of `reqwest` / `hyper` / `h2` /
//! `tower`).
//!
//! ## Design
//!
//! The store wraps `hnsw_rs::hnsw::Hnsw<'static, f32, DistCosine>`
//! in a `parking_lot::RwLock`. Insertion appends the vector to a
//! side-buffer (`Vec<Vec<f32>>`) so `vector_at(idx)` can be served
//! from a plain slice read (HNSW's own storage does not expose a
//! `get(idx)` API). Search uses the HNSW graph with the
//! `ef_search` from [`HnswParams`].
//!
//! ## Tombstones
//!
//! `mark_deleted(idx)` sets a bit in a `HashSet<usize>`; searches
//! filter tombstoned ids out of the result list (so recall is
//! unaffected). `n_alive` reports the pre-tombstone count minus
//! the tombstone count.
//!
//! ## Why `DistCosine`
//!
//! The engine's contract (see `FlatVectorStore::search`) assumes
//! L2-normalised vectors; cosine similarity on normalised vectors
//! is monotonic with dot product, so the ranking is equivalent.

use std::{collections::HashSet, sync::Arc};

use hnsw_rs::{anndists::dist::DistCosine, hnsw::Hnsw};
use parking_lot::{Mutex, RwLock};

use crate::{
    error::{Result, SearchError},
    vector::VectorStore,
};

/// Tuning parameters for [`HnswStore`]. The defaults (`m = 16`,
/// `ef_construction = 200`, `ef_search = 64`,
/// `max_elements = 1_000_000`) are the canonical starting points
/// from the HNSW paper (Malkov & Yashunin, 2016) and match the
/// values used by `hnsw_rs`'s own benchmarks.
#[derive(Debug, Clone, Copy)]
pub struct HnswParams {
    /// Number of bi-directional links created for every new
    /// element. Larger `m` ⇒ better recall, more memory.
    pub m: usize,
    /// Size of the dynamic candidate list during construction.
    /// Larger ⇒ better graph quality, slower build.
    pub ef_construction: usize,
    /// Size of the dynamic candidate list during search. Larger ⇒
    /// better recall, slower query.
    pub ef_search: usize,
    /// Upper bound on the number of elements the index will hold.
    /// The library does not grow past this.
    pub max_elements: usize,
    /// Maximum number of layers in the graph. `hnsw_rs` internally
    /// clamps to `NB_LAYER_MAX`; 16 is the canonical default.
    pub max_layer: usize,
}

impl Default for HnswParams {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 200,
            ef_search: 64,
            max_elements: 1_000_000,
            max_layer: 16,
        }
    }
}

/// HNSW-backed [`VectorStore`] (feature `hnsw`).
///
/// Build via `HnswStore::new(dim, HnswParams::default())`, then
/// insert with `add`. The index is grown incrementally (the HNSW
/// algorithm is designed for that), so a long-running engine can
/// `add` without rebuilding.
#[derive(Clone)]
pub struct HnswStore {
    /// The HNSW index. `Arc` so the `Clone` impl shares the graph
    /// rather than deep-copying it (the inner `parking_lot::RwLock`
    /// is the concurrency handle).
    inner: Arc<RwLock<Hnsw<'static, f32, DistCosine>>>,
    /// Side-buffer mirroring the vectors, so `vector_at(idx)`
    /// serves a plain slice read.
    vectors: Arc<RwLock<Vec<Vec<f32>>>>,
    /// Tombstoned slot indices.
    tombstones: Arc<Mutex<HashSet<usize>>>,
    /// The dimensionality every vector must have.
    dim: usize,
    /// Search-time `ef` (from `HnswParams`; overridable at runtime
    /// via [`Self::set_ef_search`]).
    ef_search: Arc<Mutex<usize>>,
    /// Number of live (non-tombstoned) inserts.
    alive: Arc<Mutex<usize>>,
}

impl HnswStore {
    /// New empty HNSW store for `dim`-dimensional vectors.
    ///
    /// The `dim` is used only for the insert-time length check —
    /// `hnsw_rs::Hnsw::new` does not take it directly (the graph
    /// adapts to whatever the first insert brings).
    #[must_use]
    pub fn new(dim: usize, params: HnswParams) -> Self {
        // Signature: (max_nb_connection, max_elements, max_layer,
        // ef_construction, distance). `m` maps to
        // `max_nb_connection`; `dim` is validated at insert.
        let hnsw = Hnsw::new(
            params.m,
            params.max_elements,
            params.max_layer,
            params.ef_construction,
            DistCosine {},
        );
        Self {
            inner: Arc::new(RwLock::new(hnsw)),
            vectors: Arc::new(RwLock::new(Vec::new())),
            tombstones: Arc::new(Mutex::new(HashSet::new())),
            dim,
            ef_search: Arc::new(Mutex::new(params.ef_search)),
            alive: Arc::new(Mutex::new(0)),
        }
    }

    /// The `ef_search` the store was configured with (the HNSW
    /// quality / latency knob at query time).
    #[must_use]
    pub fn ef_search(&self) -> usize {
        *self.ef_search.lock()
    }

    /// Override the `ef_search` at runtime (higher ⇒ better
    /// recall, slower query). Useful when a deployment tunes
    /// latency against its own recall target after the index is
    /// built.
    pub fn set_ef_search(&self, ef: usize) {
        *self.ef_search.lock() = ef.max(1);
    }

    /// Number of tombstoned slots.
    #[must_use]
    pub fn tombstone_count(&self) -> usize {
        self.tombstones.lock().len()
    }
}

impl VectorStore for HnswStore {
    fn add(&mut self, v: Vec<f32>) -> Result<usize> {
        if v.len() != self.dim {
            return Err(SearchError::DimMismatch {
                expected: self.dim,
                got: v.len(),
            });
        }
        let idx = self.vectors.read().len();
        // `insert` takes `(&[T], usize)` — a slice view + the
        // caller's id. The graph stores the id in `Neighbour.d_id`
        // on search.
        self.inner.read().insert((&v, idx));
        self.vectors.write().push(v);
        *self.alive.lock() += 1;
        Ok(idx)
    }

    fn mark_deleted(&mut self, idx: usize) {
        if self.tombstones.lock().insert(idx) {
            // Compute the decrement under one lock guard — taking
            // the same non-reentrant Mutex twice in one expression
            // (read + write) would deadlock.
            let mut alive = self.alive.lock();
            *alive = (*alive).saturating_sub(1);
        }
    }

    fn is_alive(&self, idx: usize) -> bool {
        !self.tombstones.lock().contains(&idx)
            && idx < self.vectors.read().len()
    }

    fn n_alive(&self) -> usize {
        *self.alive.lock()
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn vector_at(&self, idx: usize) -> Option<Vec<f32>> {
        self.vectors.read().get(idx).cloned()
    }

    fn search(&self, q: &[f32], top_k: usize) -> Vec<(usize, f32)> {
        if q.len() != self.dim || self.n_alive() == 0 {
            return Vec::new();
        }
        let ef = *self.ef_search.lock();
        let results = self.inner.read().search(q, top_k, ef);
        let tombstones = self.tombstones.lock();
        results
            .into_iter()
            .filter(|n| !tombstones.contains(&n.d_id))
            .map(|n| (n.d_id, n.distance))
            .collect()
    }
}

impl std::fmt::Debug for HnswStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HnswStore")
            .field("dim", &self.dim)
            .field("n_alive", &self.n_alive())
            .field("tombstones", &self.tombstone_count())
            .field("ef_search", &self.ef_search())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two orthogonal unit vectors: the nearest neighbour of each
    /// is itself. The HNSW search should return it as top-1.
    #[test]
    fn orthogonal_unit_vectors_find_themselves() {
        let mut store = HnswStore::new(2, HnswParams::default());
        let v0 = vec![1.0, 0.0];
        let v1 = vec![0.0, 1.0];
        let i0 = store.add(v0.clone()).unwrap();
        let i1 = store.add(v1.clone()).unwrap();
        assert_eq!(i0, 0);
        assert_eq!(i1, 1);
        let hits = store.search(&v0, 1);
        assert!(!hits.is_empty(), "expected a hit, got {hits:?}");
        assert_eq!(hits[0].0, 0);
        // Cosine distance for a self-match is ~0.
        assert!(
            hits[0].1.abs() < 0.01,
            "self-distance should be ~0, got {}",
            hits[0].1
        );
    }

    /// Tombstoned slots do not appear in the result list.
    #[test]
    fn tombstones_are_filtered_from_search() {
        let mut store = HnswStore::new(2, HnswParams::default());
        store.add(vec![1.0, 0.0]).unwrap();
        store.add(vec![0.9, 0.1]).unwrap();
        store.add(vec![0.0, 1.0]).unwrap();
        store.mark_deleted(0);
        assert!(!store.is_alive(0));
        assert!(store.is_alive(1));
        assert_eq!(store.n_alive(), 2);
        let hits = store.search(&[1.0, 0.0], 3);
        assert!(
            hits.iter().all(|(idx, _)| *idx != 0),
            "tombstoned slot leaked into results: {hits:?}"
        );
    }

    /// Dim-mismatch on `add` errors instead of panicking.
    #[test]
    fn add_rejects_wrong_dim() {
        let mut store = HnswStore::new(3, HnswParams::default());
        let err = store.add(vec![1.0, 0.0]).unwrap_err();
        assert!(
            matches!(err, SearchError::DimMismatch { .. }),
            "got: {err:?}"
        );
    }

    /// `vector_at` mirrors the side-buffer; a fresh slot returns
    /// the exact vector inserted.
    #[test]
    fn vector_at_round_trips() {
        let mut store = HnswStore::new(2, HnswParams::default());
        let v = vec![0.5, 0.5];
        let idx = store.add(v.clone()).unwrap();
        assert_eq!(store.vector_at(idx), Some(v));
        assert_eq!(store.vector_at(999), None);
    }

    /// Empty store + any query yields no hits.
    #[test]
    fn empty_store_returns_no_hits() {
        let store = HnswStore::new(4, HnswParams::default());
        assert!(store.search(&[1.0, 0.0, 0.0, 0.0], 5).is_empty());
        assert_eq!(store.n_alive(), 0);
    }

    /// `set_ef_search` is observable through `ef_search()` and
    /// clamped to ≥ 1.
    #[test]
    fn set_ef_search_updates_the_knob() {
        let store = HnswStore::new(
            2,
            HnswParams {
                ef_search: 32,
                ..Default::default()
            },
        );
        assert_eq!(store.ef_search(), 32);
        store.set_ef_search(128);
        assert_eq!(store.ef_search(), 128);
        store.set_ef_search(0);
        assert_eq!(store.ef_search(), 1, "0 must clamp to 1");
    }
}
