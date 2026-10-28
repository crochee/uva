//! Tokio-backed adapters for the runtime-neutral seams of
//! `elysper-harness`. [`TokioSpawner`] is opt-in via
//! `ReActAgent::with_spawner(Arc::new(TokioSpawner))`; the default
//! build uses [`elysper_core::spawn::InlineSpawner`].
//!
//! The crate also hosts the tokio implementations of the two new
//! runtime-neutral seams from `elysper-core`: [`TokioSleeper`] for
//! [`Sleeper`] and [`TokioBlocking`] for [`Blocking`]. A consumer
//! that installs [`TokioSpawner`] is already on tokio, so it takes
//! all three adapters from here.
//!
//! `elysper-harness` is runtime-neutral by design: its `[dependencies]`
//! does not name tokio, and its public API surfaces only the
//! `Spawner` trait from `elysper-core`. Consumers on a tokio runtime
//! depend on this crate and install the [`TokioSpawner`] adapter;
//! consumers on `smol` / `async-std` / a bare `futures` executor do
//! not, and supply their own `Spawner` implementation instead.
//!
//! `ReActAgent::run` detaches one task per run so the loop keeps
//! running to completion even if the caller stops polling the event
//! stream (a dropped SSE connection must not cancel a turn
//! mid-write). A consumer on another runtime installs its own
//! spawner and never enters this crate.

use std::time::Duration;

use elysper_core::{
    blocking::Blocking,
    sleeper::Sleeper,
    spawn::{BoxFuture, Spawner},
};

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

/// The ambient tokio timer, as a [`Sleeper`].
///
/// `tokio::time::sleep` computes its deadline when it is called and
/// only defers driver registration, so the call lives inside an
/// `async` block: the deadline is anchored on the first poll of the
/// returned future, not when [`sleep`](Sleeper::sleep) is called. A
/// future held unpolled therefore still owes the whole duration, and
/// one dropped before its first poll starts no wait at all — the
/// same observable behaviour as
/// [`ThreadSleeper`](elysper_core::sleeper::ThreadSleeper), and the
/// laziness the trait requires. Same zero-sized, `Arc`-friendly shape
/// as [`TokioSpawner`].
#[derive(Debug, Default)]
pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    fn sleep(&self, duration: Duration) -> BoxFuture<()> {
        Box::pin(async move { tokio::time::sleep(duration).await })
    }
}

/// The ambient tokio blocking pool, as a [`Blocking`].
///
/// `tokio::task::spawn_blocking` queues the closure the moment it is
/// called, so the call lives inside an `async` block: the closure is
/// queued on the first poll of the returned future, and a future
/// dropped before then runs nothing — the laziness the trait
/// requires. The `JoinHandle` is dropped for the same reason as in
/// [`TokioSpawner`]: the caller has no way to observe it, and
/// dropping it does not cancel the work. A closure that panics
/// surfaces as a `JoinError`, which is discarded with the handle:
/// the future resolves instead of hanging, the same outcome
/// [`ThreadBlocking`](elysper_core::blocking::ThreadBlocking)
/// documents.
#[derive(Debug, Default)]
pub struct TokioBlocking;

impl Blocking for TokioBlocking {
    fn run(&self, f: Box<dyn FnOnce() + Send + 'static>) -> BoxFuture<()> {
        Box::pin(async move {
            drop(tokio::task::spawn_blocking(f).await);
        })
    }
}
