//! HNSW vector index from [`synthia_search`] — the ANN backend
//! behind the `hnsw` feature.
//!
//! The `search` feature ships the default `FlatVectorStore`
//! (O(n·dim) scan). This module is the opt-in upgrade:
//! [`HnswStore`] implements the same [`VectorStore`] trait with a
//! graph-based ANN index (`O(log n)` search at the cost of a
//! bigger build footprint), so `SearchEngine::with_vector_store`
//! swaps it in without touching the engine.
//!
//! ```rust,ignore
//! use synthia::hnsw::{HnswParams, HnswStore};
//! use synthia::search::SearchEngine;
//!
//! let store = HnswStore::new(256, HnswParams::default());
//! let engine = SearchEngine::with_vector_store(store /* … */);
//! ```
//!
//! ```bash
//! cargo run --example hnsw_bench -p synthia-search --features hnsw
//! ```
//!
//! [`VectorStore`]: crate::search::VectorStore

pub use synthia_search::vector::{HnswParams, HnswStore};
