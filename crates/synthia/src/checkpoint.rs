//! Durable resume primitives from [`synthia_harness`] — the
//! `Checkpointer` seam.
//!
//! The `checkpoint` feature is opt-in (not in the default set). It
//! re-exports the harness's durability types under one roof so a
//! consumer writing a resumable agent does not need to import from
//! two paths:
//!
//! - [`Checkpointer`] / [`InMemoryCheckpointer`] /
//!   [`CheckpointSnapshot`] — persistence.
//!
//! A run whose `AgentInput` carries a `thread_id` loads the snapshot
//! stored under `<thread_id>:<agent_name>` at setup, and the loop
//! persists one on every non-`Completed` end (deleting it on
//! `Completed`).
//!
//! ```rust,ignore
//! use std::sync::Arc;
//!
//! use synthia::checkpoint::{Checkpointer, InMemoryCheckpointer};
//!
//! let cp: Arc<dyn Checkpointer> = Arc::new(InMemoryCheckpointer::default());
//! ```
//!
//! ```bash
//! cargo run --example durable_hitl -p synthia-harness
//! ```

pub use synthia_harness::{
    CheckpointError,
    CheckpointSnapshot,
    Checkpointer,
    InMemoryCheckpointer,
};
