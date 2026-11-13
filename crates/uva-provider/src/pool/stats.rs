//! [`PoolStats`] — observable counters for the pool.
//!
//! Kept `Copy` so [`PooledHttpClient::stats`] returns by
//! value without allocation; protected by a
//! `parking_lot::RwLock` so reads from tracing spans never
//! block the hot path.

/// Live counters for the pool.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PoolStats {
    /// Number of sockets currently held idle in the pool
    /// (across all hosts). Snapshot — may move between
    /// successive calls.
    pub idle: usize,
    /// Number of requests currently in flight.
    pub in_flight: usize,
    /// Number of requests that reused an idle socket.
    pub reused: u64,
    /// Number of requests that established a fresh
    /// socket.
    pub established: u64,
}

impl PoolStats {
    /// `reused + established` — total requests the pool
    /// has handled. Useful as the denominator for the
    /// reuse ratio.
    pub fn total(&self) -> u64 {
        self.reused.saturating_add(self.established)
    }

    /// Reuse ratio over the run — `reused / total` (0.0
    /// when no requests have been handled yet). The number
    /// is the headline metric the pool's whole reason for
    /// existing: "are we paying a TCP/TLS handshake per
    /// request, or are we reusing?".
    #[allow(clippy::cast_precision_loss)]
    pub fn reuse_ratio(&self) -> f32 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        (self.reused as f32) / (total as f32)
    }
}

/// Marker returned by [`super::PooledHttpClient::stats`]
/// only when the snapshotter had to retry — never produced
/// today, but keeps the surface honest for future
/// expansions.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// A read lock could not be acquired.
    #[error("pool stats snapshot failed: {0}")]
    Lock(String),
}
