//! Runtime-neutral delay seam.
//!
//! A consumer that has to *wait* — a timeout, a retry backoff, a
//! poll interval — should not have to name a runtime to do it.
//! `tokio::time::sleep` and friends compile only against one
//! runtime and panic under any other executor, so a crate that
//! arms its own timer is that-runtime-only by construction. This
//! module states the wait once, with no runtime in the signature:
//!
//! - [`Sleeper`] — the trait (`sleep` a duration, get a future).
//! - [`ThreadSleeper`] — the runtime-free implementation: a std
//!   thread sleeps, a oneshot channel carries the wakeup back.
//! - [`FixedSleeper`] — resolves immediately; the test seam.
//!
//! The tokio implementation lives in the sibling crate
//! `uva-core-tokio` as `TokioSleeper` — next to its
//! `TokioSpawner` and `TokioBlocking` — so a consumer on tokio
//! installs it without the framework naming a runtime.
//!
//! ## Why a trait, not a function pointer
//!
//! The same reason [`Clock`](crate::clock::Clock) and
//! [`Spawner`](crate::spawn::Spawner) are traits: the call site
//! takes the seam as a parameter, so a test injects a fixed delay
//! and a production caller injects the real one. The trait is
//! object-safe on purpose — the crates that need it hold an
//! `Arc<dyn Sleeper>` and swap implementations without
//! recompiling the framework.

use std::time::Duration;

use futures::{channel::oneshot, future::BoxFuture};

/// A runtime-neutral delay.
///
/// Implementations own the wait; the caller only learns when it is
/// over. Object-safe, so the crates that need it hold an
/// `Arc<dyn Sleeper>` and a consumer can swap the implementation
/// (a tokio timer, a fake, a deterministic clock) without
/// recompiling the framework.
pub trait Sleeper: Send + Sync + 'static {
    /// A future that resolves once `duration` has elapsed.
    ///
    /// The wait starts when the returned future is first polled —
    /// the same laziness `tokio::time::sleep` has — so a future
    /// that is constructed and then dropped never starts a wait.
    ///
    /// The returned future must not block the caller while it
    /// waits: the wait happens off the polling thread, and the
    /// future is woken when the duration is over.
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()>;
}

/// The runtime-free sleeper: a std thread waits, a oneshot channel
/// carries the wakeup.
///
/// [`sleep`](Self::sleep) spawns one OS thread per call, sleeps for
/// the requested duration on it, and signals a oneshot channel; the
/// returned future awaits that channel. The caller's thread is
/// never occupied by the wait itself — the future is pending until
/// the other thread signals it — so a consumer can await several
/// sleeps concurrently.
///
/// The thread starts on the future's first poll, not when
/// [`sleep`](Self::sleep) is called: a future dropped before its
/// first poll starts no thread at all. A future dropped *during*
/// the wait cannot cancel the thread — it runs the sleep out and
/// exits — so an abandoned wait costs the duration once.
///
/// The cost is one OS thread per sleep, which is why this is the
/// fallback rather than the default: a consumer on a real runtime
/// installs that runtime's sleeper instead.
#[derive(Debug, Clone, Default)]
pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let (tx, rx) = oneshot::channel::<()>();
            std::thread::spawn(move || {
                std::thread::sleep(duration);
                let _ = tx.send(());
            });
            let _ = rx.await;
        })
    }
}

/// The sleeper that never waits: the returned future is ready
/// immediately, whatever duration it was handed.
///
/// The test seam — a caller that must exercise a timeout path
/// without spending the time gets `FixedSleeper` instead of a real
/// one. It is deliberately not `Default`: a production caller that
/// reaches for `Default` should be handed [`ThreadSleeper`], whose
/// wait is the real thing.
#[derive(Debug, Clone)]
pub struct FixedSleeper;

impl Sleeper for FixedSleeper {
    fn sleep(&self, _duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(std::future::ready(()))
    }
}

#[cfg(test)]
mod tests;
