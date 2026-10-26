//! Runtime-neutral blocking-work seam.
//!
//! A consumer that has to do *blocking* work — a file write, an
//! `fsync`, a blocking read — should not have to name a runtime to
//! do it. `tokio::task::spawn_blocking` compiles only against one
//! runtime, so a crate that moves its fs work there is
//! that-runtime-only by construction. This module states the
//! offload once, with no runtime in the signature:
//!
//! - [`Blocking`] — the trait (`run` a closure off the caller's
//!   thread, get a future that resolves when the work is done).
//! - [`ThreadBlocking`] — the runtime-free implementation: a std
//!   thread runs the closure, a oneshot channel carries the
//!   completion back.
//!
//! The tokio implementation lives in the sibling crate
//! `synthia-harness-tokio` as `TokioBlocking` — next to its
//! `TokioSpawner` and `TokioSleeper` — so a consumer on tokio
//! installs it without the framework naming a runtime.
//!
//! ## Why a trait, not a function pointer
//!
//! The same reason [`Clock`](crate::clock::Clock) and
//! [`Spawner`](crate::spawn::Spawner) are traits: the call site
//! takes the seam as a parameter, so a test injects an inline
//! implementation and a production caller injects the real one. The
//! trait is object-safe on purpose — the crates that need it hold an
//! `Arc<dyn Blocking>` and swap implementations without
//! recompiling the framework.
//!
//! ## How this differs from `Spawner::spawn_blocking`
//!
//! [`Spawner::spawn_blocking`](crate::spawn::Spawner::spawn_blocking)
//! is fire-and-forget: the closure's return value is erased, so the
//! caller learns nothing about the work. This trait's future
//! resolves when the closure is done, and a closure that has to
//! report a `Result` carries it out through a channel of its own —
//! the shape a caller that writes a file and needs to know whether
//! the write landed wants.

use futures::{channel::oneshot, future::BoxFuture};

/// A runtime-neutral offload for blocking work.
///
/// Implementations own the thread the work runs on; the caller only
/// learns when it is over. Object-safe, so the crates that need it
/// hold an `Arc<dyn Blocking>` and a consumer can swap the
/// implementation (a tokio blocking pool, a dedicated thread pool,
/// an inline fake) without recompiling the framework.
pub trait Blocking: Send + Sync + 'static {
    /// A future that resolves once `f` has completed.
    ///
    /// The work starts when the returned future is first polled —
    /// the same laziness `tokio::time::sleep` has — so a future
    /// that is constructed and then dropped runs nothing at all.
    ///
    /// The returned future must not block the caller while it waits:
    /// the closure runs off the polling thread, and the future is
    /// woken when the closure is done.
    ///
    /// The closure's own return value is carried by the caller (a
    /// oneshot channel), which keeps the trait object-safe.
    fn run(
        &self,
        f: Box<dyn FnOnce() + Send + 'static>,
    ) -> BoxFuture<'static, ()>;
}

/// The runtime-free implementation: a std thread runs the closure, a
/// oneshot channel carries the completion back.
///
/// [`run`](Self::run) spawns one OS thread per call, runs the
/// closure on it, and signals a oneshot channel; the returned future
/// awaits that channel. The caller's thread is never occupied by the
/// work itself — the future is pending until the other thread
/// signals it — so a consumer can await several offloads
/// concurrently.
///
/// The closure starts on the future's first poll, not when
/// [`run`](Self::run) is called: a future dropped before its first
/// poll runs no closure at all. A future dropped *during* the work
/// cannot cancel the thread — it runs the closure out and exits —
/// so an abandoned offload costs the work once. A closure that
/// panics ends the wait too: the channel's sender goes with the
/// thread, so the future resolves instead of hanging.
///
/// The cost is one OS thread per call, which is why this is the
/// fallback rather than the default: a consumer on a real runtime
/// installs that runtime's blocking pool instead.
#[derive(Debug, Clone, Default)]
pub struct ThreadBlocking;

impl Blocking for ThreadBlocking {
    fn run(
        &self,
        f: Box<dyn FnOnce() + Send + 'static>,
    ) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let (tx, rx) = oneshot::channel::<()>();
            std::thread::spawn(move || {
                f();
                let _ = tx.send(());
            });
            let _ = rx.await;
        })
    }
}

#[cfg(test)]
mod tests;
