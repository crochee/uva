//! HTTP connection-pool + warm-up + per-host cap
//! ().
//!
//! Pure forwarding—every name here is a `pub use` of one
//! underlying item in [`elysper_provider::pool`]. The
//! facade module exists so a consumer who wants the
//! performance plug-in does not have to discover the
//! underlying crate's path: the only path the
//! `'elysper_…'` tree offers for `PooledHttpClient` /
//! `PoolConfig` / `PoolStats` is `elysper::hpc_pool::*`.
//!
//! See the module-level docs in
//! [`elysper_provider::pool`]
//! for the contract—the behaviour, the runtime-neutral
//! guarantees, and the per-host cap all live there.
//!
//! ## Feature gate
//!
//! This module is `cfg(feature = "hpc-pool")`-gated; its
//! `compile_fail` doctest is in `elysper/src/lib.rs`
//! alongside the rest.

pub use elysper_provider::pool::{
    DEFAULT_CONNECT_TIMEOUT,
    DEFAULT_IDLE_TIMEOUT,
    HttpClientBackend,
    PoolConfig,
    PoolStats,
    PooledHttpClient,
    ReqwestBackend,
    warm_up_with,
};
