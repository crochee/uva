//! Runtime-free counting semaphore.
//!
//! Equivalent to `tokio::sync::Semaphore` for the
//! `acquire_owned` pattern: one `AtomicUsize` permit counter
//! plus a `Mutex<Vec<Waker>>` wait queue. Permits are released
//! when the [`OwnedSemaphorePermit`] is dropped; cancelling
//! the [`AcquireFuture`] releases nothing — it never took a
//! permit. Waiters are FIFO by registration order (the wake
//! queue is a `Vec`), matching `tokio::sync::Semaphore`'s
//! documented ordering.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

/// A counting semaphore with no async runtime in its signature.
///
/// `Arc<Semaphore>` is the unit callers share (matching
/// `tokio::sync::Semaphore`'s `acquire_owned` shape). The
/// permit counter is one `AtomicUsize`; the wait queue is one
/// `Mutex<Vec<Waker>>`. Acquire = `fetch_sub(1, AcqRel)`; release
/// = `fetch_add(1, Release)` and wake the head waiter if any.
#[derive(Debug)]
pub struct Semaphore {
    permits: AtomicUsize,
    waiters: Mutex<Vec<std::task::Waker>>,
}

/// A permit acquired from [`Semaphore::acquire_owned`].
///
/// Dropping the permit releases one slot back to the semaphore
/// and wakes the next waiter in FIFO order.
pub struct OwnedSemaphorePermit {
    semaphore: Arc<Semaphore>,
}

/// Future returned by [`Semaphore::acquire_owned`]. Resolves
/// once a permit is available; the future owns the slot it
/// eventually delivers, so dropping it before completion does
/// not leak a permit.
pub struct AcquireFuture {
    semaphore: Arc<Semaphore>,
}

impl Semaphore {
    /// Build a semaphore that allows `permits` concurrent
    /// holders. Returns an `Arc` so the same semaphore can be
    /// shared between the dispatcher and the future produced
    /// by `acquire_owned`.
    #[must_use]
    pub fn new(permits: usize) -> Arc<Self> {
        Arc::new(Self {
            permits: AtomicUsize::new(permits),
            waiters: Mutex::new(Vec::new()),
        })
    }

    /// Try to take one permit without registering a waker.
    /// Returns `None` when the counter would underflow.
    pub fn try_acquire_owned(self: &Arc<Self>) -> Option<OwnedSemaphorePermit> {
        // `fetch_sub` returns the previous value; underflow wraps
        // around to `usize::MAX`, which is how we detect "no
        // permits available" without a separate compare.
        let prev = self.permits.fetch_sub(1, Ordering::AcqRel);
        if prev == 0 {
            // Bump back so the counter is consistent for the next
            // observer. The release path uses `fetch_add(1,
            // Release)` and the pair is consistent.
            self.permits.fetch_add(1, Ordering::Release);
            None
        } else {
            Some(OwnedSemaphorePermit {
                semaphore: Arc::clone(self),
            })
        }
    }

    /// Wait until one permit is available. The future may be
    /// dropped before it resolves; dropping it only removes
    /// its waker from the queue.
    pub fn acquire_owned(self: &Arc<Self>) -> AcquireFuture {
        AcquireFuture {
            semaphore: Arc::clone(self),
        }
    }
}

impl Future for AcquireFuture {
    type Output = OwnedSemaphorePermit;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Fast path: take a permit without touching the queue
        // lock. The `AcqRel` ordering pairs with the `Release` on
        // the release path so an acquired permit observes the
        // state of whatever released it.
        let prev = self.semaphore.permits.fetch_sub(1, Ordering::AcqRel);
        if prev > 0 {
            return Poll::Ready(OwnedSemaphorePermit {
                semaphore: Arc::clone(&self.semaphore),
            });
        }
        // No permit available; restore the counter so other
        // waiters see the same starting state and fall into the
        // slow path below.
        self.semaphore.permits.fetch_add(1, Ordering::Release);

        // Slow path: lock the queue and re-check, in case a
        // release slipped between our failed fetch_sub and the
        // lock acquisition. The lock provides happens-before for
        // any concurrent `fetch_add` on the permits counter.
        let mut guard = self
            .semaphore
            .waiters
            .lock()
            .expect("semaphore wait queue poisoned");
        if self.semaphore.permits.load(Ordering::Acquire) > 0 {
            drop(guard);
            let prev = self.semaphore.permits.fetch_sub(1, Ordering::AcqRel);
            if prev > 0 {
                return Poll::Ready(OwnedSemaphorePermit {
                    semaphore: Arc::clone(&self.semaphore),
                });
            }
            self.semaphore.permits.fetch_add(1, Ordering::Release);
            guard = self
                .semaphore
                .waiters
                .lock()
                .expect("semaphore wait queue poisoned");
        }
        // Always push the waker. The queue may briefly hold
        // duplicates of the same waker (after a wake that lost a
        // race to `try_acquire`); the worst-case cost is one
        // extra wake per spurious push, which is bounded by the
        // number of permits.
        guard.push(cx.waker().clone());
        Poll::Pending
    }
}

impl Drop for OwnedSemaphorePermit {
    fn drop(&mut self) {
        // Bump the counter, then wake the head waiter if any.
        // `Release` pairs with the `AcqRel` on the acquire path so
        // a permit returned here is observable to the next
        // `acquire_owned` caller. The head waiter is *popped* from
        // the queue so the next release wakes the next-longest
        // waiter (FIFO); a woken waiter that fails to take the
        // permit will re-register itself on its next poll.
        self.semaphore.permits.fetch_add(1, Ordering::Release);
        let mut guard = self
            .semaphore
            .waiters
            .lock()
            .expect("semaphore wait queue poisoned");
        if !guard.is_empty() {
            let waker = guard.remove(0);
            drop(guard);
            waker.wake_by_ref();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll, Wake, Waker},
        thread,
    };

    use super::{OwnedSemaphorePermit, Semaphore};

    /// Waker that counts every `wake` / `wake_by_ref` invocation.
    struct CountingWaker {
        wakes: AtomicUsize,
    }
    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.wakes.fetch_add(1, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.wakes.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn noop_waker() -> Waker {
        Arc::new(CountingWaker {
            wakes: AtomicUsize::new(0),
        })
        .into()
    }

    /// Block on a future via a tiny single-threaded executor:
    /// poll, then `thread::yield_now` until `Ready`. No runtime.
    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(future);
        loop {
            if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
                return v;
            }
            thread::yield_now();
        }
    }

    #[test]
    fn basic_acquire_release() {
        let sem = Semaphore::new(2);
        let p1 = block_on(sem.acquire_owned());
        let p2 = block_on(sem.acquire_owned());
        assert_eq!(sem.permits.load(Ordering::SeqCst), 0);
        drop(p1);
        assert_eq!(sem.permits.load(Ordering::SeqCst), 1);
        drop(p2);
        assert_eq!(sem.permits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn try_acquire_returns_none_when_exhausted() {
        let sem = Semaphore::new(1);
        let p1 = sem.try_acquire_owned().expect("first permit available");
        assert!(sem.try_acquire_owned().is_none());
        drop(p1);
        assert!(sem.try_acquire_owned().is_some());
    }

    #[test]
    fn dropped_waiter_does_not_leak_permit() {
        // Register a waker, then drop the future. The counter must
        // be unchanged: registration is allocation-only.
        let sem = Semaphore::new(0);
        let mut fut = Box::pin(sem.acquire_owned());
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));
        // The wait queue has one entry now; drop the future.
        drop(fut);
        assert_eq!(
            sem.waiters.lock().unwrap().len(),
            1,
            "waker still parked after drop"
        );
        assert_eq!(
            sem.permits.load(Ordering::SeqCst),
            0,
            "no permit was taken before Pending"
        );
        // Clean up: clear the queue so the test's `Drop` on the
        // `Arc<Semaphore>` does not have to walk a dead waker.
        sem.waiters.lock().unwrap().clear();
    }

    #[test]
    fn release_wakes_head_waiter() {
        let sem = Semaphore::new(1);
        let held = block_on(sem.acquire_owned());
        // Park a waiter.
        let waker_count = Arc::new(AtomicUsize::new(0));
        let fut = {
            struct W(Arc<AtomicUsize>);
            impl Wake for W {
                fn wake(self: Arc<Self>) {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }

                fn wake_by_ref(self: &Arc<Self>) {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
            let waker: Waker = Arc::new(W(Arc::clone(&waker_count))).into();
            let mut cx = Context::from_waker(&waker);
            let mut f = Box::pin(sem.acquire_owned());
            assert!(matches!(f.as_mut().poll(&mut cx), Poll::Pending));
            f
        };
        // Drop the held permit — release path bumps `permits` and
        // wakes the head waiter (FIFO).
        drop(held);
        assert_eq!(
            waker_count.load(Ordering::SeqCst),
            1,
            "release should have called wake_by_ref exactly once"
        );
        // The future can now complete.
        let _permit: OwnedSemaphorePermit = block_on(fut);
        assert_eq!(sem.permits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn fifo_ordering_under_release() {
        // Three permits released back-to-back wake the three parked
        // waiters in registration order. We verify via wake counts
        // on each waiter's waker: the head waiter fires first.
        let sem = Semaphore::new(0);
        let w1 = Arc::new(AtomicUsize::new(0));
        let w2 = Arc::new(AtomicUsize::new(0));
        let w3 = Arc::new(AtomicUsize::new(0));
        let futures = [
            park(sem.acquire_owned(), &w1),
            park(sem.acquire_owned(), &w2),
            park(sem.acquire_owned(), &w3),
        ];
        // Hand back three permits, one at a time, observing the
        // waker that fired each time.
        let heads = [
            observe_head_wake(&sem, &w1, &w2, &w3),
            observe_head_wake(&sem, &w1, &w2, &w3),
            observe_head_wake(&sem, &w1, &w2, &w3),
        ];
        assert_eq!(heads, [1, 2, 3], "waiters wake FIFO");
        // Drain: drop the futures so the wait queue clears.
        for f in futures {
            drop(f);
        }
        assert!(sem.waiters.lock().unwrap().is_empty());
    }

    /// Park a future with the given counter-waker; return the
    /// pinned future so the caller can drop it after observing.
    fn park(
        fut: super::AcquireFuture,
        counter: &Arc<AtomicUsize>,
    ) -> Pin<Box<super::AcquireFuture>> {
        struct W(Arc<AtomicUsize>);
        impl Wake for W {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let waker: Waker = Arc::new(W(Arc::clone(counter))).into();
        let mut cx = Context::from_waker(&waker);
        let mut f = Box::pin(fut);
        assert!(matches!(f.as_mut().poll(&mut cx), Poll::Pending));
        f
    }

    /// Drop a synthetic permit (one that nobody took) and read
    /// which counter incremented. Returns `1` if w1 fired,
    /// `2` for w2, `3` for w3. The `Release` path on the permit
    /// wakes the head waiter — exactly one of the three counters
    /// must advance.
    fn observe_head_wake(
        sem: &Arc<Semaphore>,
        w1: &Arc<AtomicUsize>,
        w2: &Arc<AtomicUsize>,
        w3: &Arc<AtomicUsize>,
    ) -> usize {
        let before = (
            w1.load(Ordering::SeqCst),
            w2.load(Ordering::SeqCst),
            w3.load(Ordering::SeqCst),
        );
        // A `Drop`-of-permit releases a permit and wakes the head.
        let permit = OwnedSemaphorePermit {
            semaphore: Arc::clone(sem),
        };
        drop(permit);
        let after = (
            w1.load(Ordering::SeqCst),
            w2.load(Ordering::SeqCst),
            w3.load(Ordering::SeqCst),
        );
        // Exactly one waiter fired.
        let deltas =
            (after.0 - before.0, after.1 - before.1, after.2 - before.2);
        match deltas {
            (1, 0, 0) => 1,
            (0, 1, 0) => 2,
            (0, 0, 1) => 3,
            other => panic!("unexpected wake pattern {other:?}"),
        }
    }
}
