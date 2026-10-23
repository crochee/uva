//! HTTP connection pool + warm-up + per-host cap.
//!
//! `reqwest::Client` already owns a connection pool. What
//! this module adds is the three seams a production HTTP
//! adapter wants and `reqwest` does not surface by default:
//!
//! 1. **per-host idle cap**—surfaced through
//!    [`reqwest::ClientBuilder::pool_max_idle_per_host`].
//! 2. **warm-up**—issue a `HEAD /` per host at boot so the
//!    first user request never pays the TCP + TLS handshake.
//! 3. **observable stats**—[`PoolStats`] tracks how many
//!    sockets sat idle, were in flight, were reused,
//!    and were freshly established during the run.
//!
//! The module is `pub` only when the `hpc-pool` feature is
//! on (the `lib.rs` `cfg` gates it). Default builds compile
//! to nothing and the adapters use the raw
//! `reqwest::Client::new()` exactly as before—proving the
//! "default behaviour is byte-identical" promise from the
//! plan.
//!
//! ## Runtime neutrality
//!
//! No `tokio::*` in the public API ([`HttpClientBackend::execute`]
//! takes a [`reqwest::Request`] and returns a
//! [`reqwest::Response`], both of which are already in the
//! provider's surface). The pool itself is a plain
//! `Arc<RwLock<PoolStats>>`; warm-up hands its `HEAD /` calls
//! to the same backing client so it inherits the same async
//! runtime the adapter was launched on.

mod backend;
mod config;
mod stats;

pub use backend::{
    HttpClientBackend,
    PooledHttpClient,
    ReqwestBackend,
    warm_up_with,
};
pub use config::{DEFAULT_CONNECT_TIMEOUT, DEFAULT_IDLE_TIMEOUT, PoolConfig};
pub use stats::{PoolStats, SnapshotError};

#[cfg(test)]
mod tests;
