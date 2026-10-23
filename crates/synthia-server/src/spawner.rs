//! The server's [`Spawner`]: detached agent-loop work on the ambient
//! tokio runtime.
//!
//! The harness default is `synthia_core::spawn::InlineSpawner` (std
//! thread + `block_on`), which is what keeps the library
//! runtime-neutral — but a loop detached that way has **no tokio
//! reactor**, so any in-tree tool that awaits `tokio::fs` (the `read`
//! and `write` bricks) panics. The server is a tokio application and
//! every assembly site here installs [`ServerSpawner`] instead, the
//! same adapter `synthia-harness-tokio` ships for external consumers
//! (R121 keeps the server on the facade only, so the ~10-line adapter
//! is local rather than a facade feature).

use std::sync::Arc;

use synthia::core::spawn::{BoxFuture, Spawner};

/// Detached work on the ambient tokio runtime.
#[derive(Debug, Default)]
pub struct ServerSpawner;

impl Spawner for ServerSpawner {
    fn spawn(&self, task: BoxFuture<()>) {
        // The `JoinHandle` is dropped on purpose: the caller has no
        // way to observe it, and dropping it does not cancel the task
        // (a dropped SSE connection must not abort a turn mid-write).
        drop(tokio::spawn(task));
    }

    fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) {
        drop(tokio::task::spawn_blocking(f));
    }
}

/// The shared handle every assembly site installs.
pub fn server_spawner() -> Arc<ServerSpawner> {
    Arc::new(ServerSpawner)
}
