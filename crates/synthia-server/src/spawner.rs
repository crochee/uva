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
//!
//! The same reasoning covers the two seams `synthia-session` takes as
//! `Arc<dyn …>`: [`ServerBlocking`] (fs work off the worker thread)
//! and [`ServerSleeper`] (the summary deadline). Their defaults are
//! runtime-free and correct but pay one OS thread per call, so every
//! assembly site in this application installs the tokio-backed
//! adapter instead.

use std::sync::Arc;

use synthia::core::{
    blocking::Blocking,
    sleeper::Sleeper,
    spawn::{BoxFuture, Spawner},
};

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

/// Blocking work on the ambient tokio blocking pool.
///
/// `tokio::task::spawn_blocking` queues the closure when it is
/// called, so the call lives inside an `async` block: the closure is
/// queued on the returned future's first poll, which is the laziness
/// [`Blocking`] requires (same anchoring as
/// `synthia_harness_tokio::TokioBlocking`). The `JoinHandle` is
/// dropped for the same reason [`ServerSpawner`] drops its own:
/// nobody observes it, and dropping it does not cancel the work.
#[derive(Debug, Default)]
pub struct ServerBlocking;

impl Blocking for ServerBlocking {
    fn run(&self, f: Box<dyn FnOnce() + Send + 'static>) -> BoxFuture<()> {
        Box::pin(async move {
            drop(tokio::task::spawn_blocking(f).await);
        })
    }
}

/// The shared blocking seam for every [`synthia::session`] sink and
/// index this application builds.
pub fn server_blocking() -> Arc<dyn Blocking> {
    Arc::new(ServerBlocking)
}

/// Delays on the ambient tokio timer.
///
/// `tokio::time::sleep` computes its deadline when it is called and
/// only defers driver registration, so the call lives inside an
/// `async` block: the deadline is anchored on the returned future's
/// first poll, not when [`sleep`](Sleeper::sleep) is called — the
/// laziness [`Sleeper`] requires (same anchoring as
/// `synthia_harness_tokio::TokioSleeper`).
#[derive(Debug, Default)]
pub struct ServerSleeper;

impl Sleeper for ServerSleeper {
    fn sleep(&self, duration: std::time::Duration) -> BoxFuture<()> {
        Box::pin(async move { tokio::time::sleep(duration).await })
    }
}

/// The shared sleeper for every deadline this application arms
/// through a seam.
pub fn server_sleeper() -> Arc<dyn Sleeper> {
    Arc::new(ServerSleeper)
}
