//! # synthia-semantic-cache
//!
//! Embedding-similarity cache for LLM completions.
//!
//! A `ModelProvider` wrapper that short-circuits near-duplicate
//! completions via cosine similarity over a pluggable
//! [`synthia_search::Embedder`] and a pluggable
//! [`SemanticCacheStore`]. Streaming calls pass through unchanged;
//! cache decisions are made on [`ModelProvider::complete`](synthia_provider::ModelProvider::complete) boundaries.
//!
//! ```
//! use std::sync::Arc;
//!
//! use synthia_provider::traits_stub::ModelProviderStub;
//! use synthia_search::{CjkTokenizer, Embedder, HashingEmbedder};
//! use synthia_semantic_cache::{
//!     InMemorySemanticCache,
//!     SemanticCacheConfig,
//!     SemanticCacheProvider,
//!     SharedClock,
//! };
//!
//! // A test double so the example runs without an HTTP adapter;
//! // wire your own provider here.
//! let provider: Arc<dyn synthia_provider::ModelProvider> =
//!     Arc::new(ModelProviderStub::default());
//! let embedder: Arc<dyn Embedder> =
//!     Arc::new(HashingEmbedder::new(64, Arc::new(CjkTokenizer)));
//! let _ = SemanticCacheProvider::new(
//!     provider,
//!     Arc::new(InMemorySemanticCache::default()),
//!     embedder,
//!     SemanticCacheConfig::default(),
//!     Arc::new(SharedClock::system()),
//! );
//! ```
//!
//! ## Layering
//!
//! The cache sits between the agent loop and the upstream provider:
//!
//! ```text
//!   agent loop
//!     └─ SemanticCacheProvider  ← cosine ≥ threshold → return cached
//!          └─ inner provider    ← otherwise → call through
//! ```
//!
//! `SemanticCacheProvider` implements [`ModelProvider`](synthia_provider::ModelProvider); every method
//! forwards to `inner` except `complete`, which performs the
//! lookup-then-call dance. Streaming calls (`complete_with_stream`)
//! pass through verbatim — a cache hit is an `IsDone` carrying the
//! cached [`SamplingResult`](synthia_provider::SamplingResult), so wiring it onto streaming-only
//! callers is a no-op.
//!
//! ## Storage
//!
//! The default store, [`InMemorySemanticCache`], is an LRU map behind
//! `parking_lot::RwLock`. Implement [`SemanticCacheStore`] to swap in
//! SQLite, Redis, or anything else; the trait is two methods
//! (`lookup` / `store`) plus bookkeeping (`clear`, `len`).
//!
//! ## Embedder
//!
//! The default [`HashingEmbedder`](synthia_search::HashingEmbedder) (from `synthia-search`) is
//! deterministic, CPU-only, and zero-dependency — it pulls no
//! external service. For higher hit-rate, plug in
//! `synthia_search::ModelProviderEmbedder` (with the `provider`
//! feature on `synthia-search`) or your own [`Embedder`](synthia_search::Embedder).
//!
//! ## TTL + eviction
//!
//! TTL is wall-clock via the configured [`Clock`]. Tests inject
//! [`FixedClock`]. `max_entries` is LRU capacity; the oldest
//! untouched entry is evicted on insert.

pub mod store;
mod wrapper;

pub use store::{
    CachedCompletion,
    EmbeddingKey,
    InMemorySemanticCache,
    SemanticCacheStats,
    SemanticCacheStore,
};
// `Clock` re-exports surface the trait and the two shipped impls so
// callers do not have to remember which crate owns them. Internal
// callers reach them through `synthia_core` directly.
pub use synthia_core::{Clock, FixedClock, SharedClock, SystemClock};
use thiserror::Error;
pub use wrapper::{SemanticCacheConfig, SemanticCacheProvider};

/// The README, **compiled**.
///
/// The `rust` fences in [`README.md`](../README.md) are doctests,
/// so a wiring recipe that stops compiling fails
/// `cargo test -p synthia-semantic-cache`.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct Readme;

/// Errors surfaced by the cache wrapper.
///
/// The wrapper never returns the inner provider's error — it always
/// wraps it. A cache miss or cache hit is **never** an error: the
/// inner provider is the only source of fallible outcomes.
#[derive(Debug, Error)]
pub enum SemanticCacheError {
    /// Inner provider surfaced an error during the underlying call.
    #[error("inner provider error: {0}")]
    Inner(#[from] synthia_core::Error),
    /// `Config::threshold` is outside `[0.0, 1.0]`.
    #[error("invalid threshold {0}; expected a value in [0.0, 1.0]")]
    InvalidThreshold(f32),
}

// No public `Result` alias: the wrapper never returns a custom
// error. Internal call sites use `synthia_core::Result<T, Error>` or
// `core::result::Result<T, E>` directly.

/// Cosine similarity in `[-1.0, 1.0]`. Returns `0.0` on zero-length
/// vectors (no preference signal). Public so the test suite and any
/// custom [`SemanticCacheStore`] impls can share the exact same
/// scoring the wrapper uses.
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "embedding dim mismatch");
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let x = f64::from(*x);
        let y = f64::from(*y);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = (na * nb).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        (dot / denom) as f32
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn cosine_similarity_zero_on_orthogonal() {
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_one_on_identical() {
        assert!(
            (cosine_similarity(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]) - 1.0).abs()
                < 1e-6
        );
    }

    #[test]
    fn cosine_similarity_zero_on_empty() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }
}
