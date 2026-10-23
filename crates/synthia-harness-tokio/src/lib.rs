//! Tokio-backed [`Spawner`] for `synthia-harness`. Opt-in via
//! `ReActAgent::with_spawner(Arc::new(TokioSpawner))`; the default
//! build uses [`synthia_core::spawn::InlineSpawner`].
//!
//! `synthia-harness` is runtime-neutral by design: its `[dependencies]`
//! does not name tokio, and its public API surfaces only the
//! `Spawner` trait from `synthia-core`. Consumers on a tokio runtime
//! depend on this crate and install the [`TokioSpawner`] adapter;
//! consumers on `smol` / `async-std` / a bare `futures` executor do
//! not, and supply their own `Spawner` implementation instead.
//!
//! `ReActAgent::run` detaches one task per run so the loop keeps
//! running to completion even if the caller stops polling the event
//! stream (a dropped SSE connection must not cancel a turn
//! mid-write). A consumer on another runtime installs its own
//! spawner and never enters this crate.

use synthia_core::spawn::{BoxFuture, Spawner};

/// Detached work on the ambient tokio runtime.
///
/// `#[derive(Default)]` keeps the type zero-sized and `Arc`-friendly,
/// so installing it costs the same allocation as the default
/// in-process spawner. `Debug` lets test fixtures print the type
/// when something goes wrong.
#[derive(Debug, Default)]
pub struct TokioSpawner;

impl Spawner for TokioSpawner {
    fn spawn(&self, task: BoxFuture<()>) {
        // The `JoinHandle` is dropped on purpose: the caller has no
        // way to observe it, and dropping it does not cancel the task.
        drop(tokio::spawn(task));
    }

    fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) {
        drop(tokio::task::spawn_blocking(f));
    }
}
