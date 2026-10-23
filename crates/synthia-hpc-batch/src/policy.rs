//! Bounded-concurrency policy shared by both batch drivers.
//!
//! [`BatchPolicy`] is `Copy` so `BatchEmbedder::with_policy`
//! and `BatchCompleter::with_policy` can chain a single
//! builder call without `&mut self`. The two knobs are
//! intentionally minimal — chunk_size controls how many
//! items a single dispatch handles before yielding to the
//! next, max_inflight caps how many chunks may sit in
//! flight at any moment. Consumers needing finer control
//! (per-host cap, retry budget) compose over their inner
//! provider instead of teaching this struct more fields.

/// Default `chunk_size` — the number of items in one
/// dispatch.
pub const DEFAULT_CHUNK_SIZE: usize = 32;
/// Default `max_inflight` — the in-flight chunk cap.
pub const DEFAULT_MAX_INFLIGHT: usize = 8;

/// Bounded-concurrency policy for `BatchEmbedder` /
/// `BatchCompleter`.
///
/// Defaults are [`DEFAULT_CHUNK_SIZE`] and
/// [`DEFAULT_MAX_INFLIGHT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchPolicy {
    /// Maximum number of items handled in a single chunk.
    /// Larger chunks raise throughput but flatten each
    /// dispatch's tail; smaller chunks lower latency per
    /// item but increase per-item overhead.
    pub chunk_size: usize,
    /// Maximum number of chunks running concurrently. A
    /// value of `1` collapses the driver to serial
    /// dispatch.
    pub max_inflight: usize,
}

impl Default for BatchPolicy {
    fn default() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            max_inflight: DEFAULT_MAX_INFLIGHT,
        }
    }
}
