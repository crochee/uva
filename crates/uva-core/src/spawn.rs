//! Runtime-neutral task spawning.
//!
//! The agent loop, the tool dispatch, and the session bookkeeping all
//! need to *detach* work: run one future to completion on an executor
//! the caller never polls again. Doing that with `tokio::spawn`
//! compiles only against tokio and panics at runtime ("there is no
//! reactor running") under any other executor — the coupling that
//! stops this framework from being a library for consumers who did not
//! choose tokio.
//!
//! [`Spawner`] is that one operation, with no runtime in its
//! signature: box a future in, nothing comes out. The harness's
//! default is the runtime-free [`InlineSpawner`]; a consumer on
//! `tokio` (via `uva-core-tokio`), `smol`, `async-std`, or a
//! bare `futures::executor::ThreadPool` injects their own through
//! `ReActAgent::with_spawner`. Nothing in the loop knows which one it got.
//!
//! ## What is *not* runtime-neutral
//!
//! Being honest about the boundary is part of the contract:
//!
//! - the builtin tools (`read` / `write` / `shell`) use `tokio::fs`
//!   and `tokio::process`;
//! - provider HTTP adapters use `reqwest` + a tokio reactor;
//! - the JSONL session sink uses `tokio::fs` and `spawn_blocking`.
//!
//! Those are *plugins* — a consumer on another runtime implements
//! [`Tool`](https://docs.rs/uva-tool) / `ModelProvider` /
//! `SessionSink` themselves and keeps the loop. The loop itself, the
//! event stream, the context managers, and the steering layer use
//! nothing but `futures` plus this trait.

use std::{future::Future, pin::Pin, sync::Arc};

/// A boxed, detached, `Send` future — the unit [`Spawner::spawn`]
/// accepts.
pub type BoxFuture<T = ()> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Spawn detached work without naming a runtime.
///
/// Implementations own an executor (a tokio runtime, a
/// `futures::executor::ThreadPool`, a queue drained by the caller, …)
/// and must run the future to completion on it.
///
/// Object-safe on purpose: the crates that need it hold
/// `Arc<dyn Spawner>` so a consumer can swap the executor without
/// recompiling the framework.
pub trait Spawner: Send + Sync + 'static {
    /// Run `task` to completion somewhere this spawner owns.
    ///
    /// The caller never observes the result; a panicking task must not
    /// take the process down (implementations decide how — tokio's
    /// `spawn` catches it into the `JoinHandle` it drops).
    fn spawn(&self, task: BoxFuture<()>);

    /// Run a blocking closure off the async worker threads.
    ///
    /// Used by file-mutation paths where the critical section is
    /// synchronous. The default implementation runs `f` on the async
    /// executor via [`Spawner::spawn`] — correct but impolite for a
    /// real blocking call, so an implementation with a blocking pool
    /// (tokio's, `smol`'s `unblock`, a dedicated thread) SHOULD
    /// override it.
    ///
    /// The closure is erased to `FnOnce()` (no generics) to keep the
    /// trait object-safe; a caller that needs the return value hands
    /// it out through a channel of its own.
    fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) {
        self.spawn(Box::pin(async move { f() }));
    }
}

/// Blanket impl: `Arc<T>` derefs to `T`, so an `Arc<dyn Spawner>`
/// field can hold any concrete spawner.
impl<T: Spawner + ?Sized> Spawner for Arc<T> {
    fn spawn(&self, task: BoxFuture<()>) {
        (**self).spawn(task);
    }

    fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) {
        (**self).spawn_blocking(f);
    }
}

/// A shared spawner handle — what the crates actually store.
pub type SharedSpawner = Arc<dyn Spawner>;

/// The runtime-free fallback spawner.
///
/// [`spawn`](Self::spawn) hands the future to a fresh OS thread
/// (via [`std::thread::spawn`]) and drives it to completion on
/// that thread with [`futures::executor::block_on`]. There is no
/// background worker, no queue, and no async-runtime reactor —
/// every `spawn` call pays for a `std::thread::spawn`. The
/// harness installs this as its unconditional default; a consumer
/// who wants detached work behind a real executor installs their
/// own (the tokio-backed one lives in `uva-core-tokio`)
/// through
/// [`ReActAgent::with_spawner`](https://docs.rs/uva-harness).
///
/// `spawn_blocking` runs the closure on a fresh thread directly
/// (no `block_on` involved).
///
/// Why a fresh thread instead of `block_on` on the caller: when
/// the caller is itself running inside an async runtime
/// (e.g. `#[tokio::test]`), `block_on` on the caller would
/// deadlock the runtime. Detaching via `std::thread::spawn`
/// keeps the caller unblocked and avoids the deadlock; the cost
/// is one OS thread per `spawn`.
#[derive(Debug, Default)]
pub struct InlineSpawner;

impl Spawner for InlineSpawner {
    fn spawn(&self, task: BoxFuture<()>) {
        std::thread::spawn(move || {
            futures::executor::block_on(task);
        });
    }

    fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) {
        std::thread::spawn(f);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    /// `InlineSpawner` detaches onto a fresh OS thread, so the test
    /// polls for the side effect instead of racing the scheduler.
    fn settles(payload: &Arc<AtomicUsize>, expected: usize) {
        for _ in 0..2_000 {
            if payload.load(Ordering::SeqCst) == expected {
                return;
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        assert_eq!(payload.load(Ordering::SeqCst), expected);
    }

    #[test]
    fn spawn_runs_the_future_without_a_runtime() {
        let payload = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&payload);
        InlineSpawner.spawn(Box::pin(async move {
            seen.store(7, Ordering::SeqCst);
        }));
        settles(&payload, 7);
    }

    #[test]
    fn default_spawn_blocking_runs_the_closure() {
        let cell = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&cell);
        InlineSpawner.spawn_blocking(Box::new(move || {
            seen.store(11, Ordering::SeqCst);
        }));
        settles(&cell, 11);
    }

    #[test]
    fn a_spawner_is_object_safe() {
        let spawner: Arc<dyn Spawner> = Arc::new(InlineSpawner);
        let ran = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&ran);
        spawner.spawn(Box::pin(async move {
            seen.fetch_add(1, Ordering::SeqCst);
        }));
        settles(&ran, 1);
    }
}
