//! HTTP connection-pool + warm-up + per-host cap
//! ().
//!
//! Pure forwarding—every name here is a `pub use` of one
//! underlying item in [`synthia_provider::pool`]. The
//! facade module exists so a consumer who wants the
//! performance plug-in does not have to discover the
//! underlying crate's path: the only path the
//! `'synthia_…'` tree offers for `PooledHttpClient` /
//! `PoolConfig` / `PoolStats` is `synthia::hpc_pool::*`.
//!
//! See the module-level docs in
//! [`synthia_provider::pool`](synthia_provider::pool)
//! for the contract—the behaviour, the runtime-neutral
//! guarantees, and the per-host cap all live there.
//!
//! ## Feature gate
//!
//! This module is `cfg(feature = "hpc-pool")`-gated; its
//! `compile_fail` doctest is in `synthia/src/lib.rs`
//! alongside the rest.

pub use synthia_provider::pool::{
    DEFAULT_CONNECT_TIMEOUT,
    DEFAULT_IDLE_TIMEOUT,
    HttpClientBackend,
    PoolConfig,
    PoolStats,
    PooledHttpClient,
    ReqwestBackend,
    warm_up_with,
};
