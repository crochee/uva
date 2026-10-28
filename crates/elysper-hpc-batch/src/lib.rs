//! # elysper-hpc-batch
//!
//! Batched-embedder / batched-completer
//! adapters that wrap an inner
//! [`elysper_provider::ModelProvider`] / [`elysper_search::Embedder`]
//! in a chunked, concurrency-bounded driver.
//!
//! The driver forwards every call to the inner type:
//! chunking and inflight caps live here, not in the wrapped
//! provider, so the wrapped trait keeps its per-call
//! semantics and downstream consumers can swap a
//! single-call provider for its batched twin behind one
//! `.new(...)` call.
//!
//! ## Behaviour
//!
//! Each public surface—[`BatchEmbedder`] and
//! [`BatchCompleter`]—splits the input into chunks of
//! `BatchPolicy::chunk_size` and runs up to
//! `BatchPolicy::max_inflight` chunks concurrently. The
//! inner item is dispatched through `futures::stream` +
//! `buffer_unordered`, never via a specific runtime; the
//! public API therefore carries no `tokio::*` /
//! `async_std::*` / `smol::*` type
//! ([`elysper::check-public-api-runtime`](https://github.com/crochee/elysper)
//! stays green).
//!
//! ## Stats
//!
//! Every dispatch observable in [`BatchStats`]:
//! `submitted`, `inflight`, `completed`, `failed`. The
//! counts are populated lock-free through a
//! `parking_lot::Mutex` over a `Copy` struct, so probes
//! during a long batch never block the hot path.
//!
//! [`BatchEmbedder`]: crate::BatchEmbedder
//! [`BatchCompleter`]: crate::BatchCompleter
//! [`BatchPolicy`]: crate::BatchPolicy
//! [`BatchStats`]: crate::BatchStats

pub mod completer;
pub mod embedder;
pub mod policy;
pub mod stats;

pub use completer::BatchCompleter;
pub use embedder::BatchEmbedder;
pub use policy::{BatchPolicy, DEFAULT_CHUNK_SIZE, DEFAULT_MAX_INFLIGHT};
pub use stats::BatchStats;

/// Stable error type carried across the batch path.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An item in the batch failed (the per-item error is the
    /// `String` form of the inner provider's error).
    #[error("batch item failed: {0}")]
    Item(String),
    /// The inner provider / embedder returned a result whose
    /// shape did not match the request (e.g. `embed_batch`
    /// asked for 32 embeddings but the inner driver returned
    /// a different count).
    #[error("batch length mismatch: expected {expected}, got {got}")]
    LengthMismatch { expected: usize, got: usize },
}

/// Convenience alias for `Result<T, Error>` used by every batch
/// driver.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The two default knobs match the values documented in
    /// [`BatchPolicy::default`].
    #[test]
    fn default_policy_is_documented() {
        let p = BatchPolicy::default();
        assert_eq!(p.chunk_size, DEFAULT_CHUNK_SIZE);
        assert_eq!(p.max_inflight, DEFAULT_MAX_INFLIGHT);
        assert_eq!(p.chunk_size, 32);
        assert_eq!(p.max_inflight, 8);
    }
}
