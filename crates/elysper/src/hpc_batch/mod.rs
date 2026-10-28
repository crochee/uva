//! : batched embedder / batched
//! completer plug-in.
//!
//! Pure forwarding—every name here is a `pub use` of
//! one underlying item in `elysper-hpc-batch`. The facade
//! module exists so a consumer who wants the plug-in
//! does not have to discover the underlying crate's
//! path: the only path the `'elysper_…'` tree offers for
//! `BatchEmbedder` / `BatchCompleter` / `BatchPolicy` /
//! `BatchStats` is `elysper::hpc_batch::*`.
//!
//! See the module-level docs in
//! [`elysper_hpc_batch`](elysper_hpc_batch) for the
//! contract—the order guarantee, the bounded-concurrency
//! cap, and the runtime-neutral `Send + Sync` trait
//! surface all live there.

pub use elysper_hpc_batch::{
    BatchCompleter,
    BatchEmbedder,
    BatchPolicy,
    BatchStats,
    DEFAULT_CHUNK_SIZE,
    DEFAULT_MAX_INFLIGHT,
    Error as BatchError,
    Result as BatchResult,
};
