use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use synthia_core::{blocking::Blocking, sleeper::Sleeper};
use synthia_harness_tokio::{TokioBlocking, TokioSleeper};

#[tokio::test]
async fn tokio_sleeper_waits() {
    let start = Instant::now();
    TokioSleeper.sleep(Duration::from_millis(30)).await;
    assert!(start.elapsed() >= Duration::from_millis(25));
}

#[tokio::test]
async fn tokio_sleeper_starts_the_wait_on_first_poll() {
    // Same tell as the core `ThreadSleeper` anchoring test: build the
    // future, leave it unpolled for longer than the requested
    // duration, then poll it. An implementation that computes the
    // deadline when `sleep` is called has already burned the delay by
    // the first poll and resolves immediately; one that anchors at
    // first poll still owes the whole duration.
    let requested = Duration::from_millis(200);
    let future = TokioSleeper.sleep(requested);
    // Longer than `requested`, so an eager deadline is long gone.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let start = Instant::now();
    future.await;
    let waited = start.elapsed();

    // The margin is one-sided: the timer expires no earlier than the
    // requested duration after the first poll, so the measured wait
    // cannot come up short.
    assert!(
        waited >= Duration::from_millis(150),
        "the wait must start on the first poll, but only {waited:?} \
         of the {requested:?} was still outstanding",
    );
}

#[tokio::test]
async fn tokio_blocking_runs_off_the_worker_thread() {
    let worker = std::thread::current().id();
    let (tx, rx) =
        tokio::sync::oneshot::channel::<(u32, std::thread::ThreadId)>();
    TokioBlocking
        .run(Box::new(move || {
            let _ = tx.send((7, std::thread::current().id()));
        }))
        .await;
    let (value, thread) = rx.await.unwrap();
    assert_eq!(value, 7);
    assert_ne!(
        thread, worker,
        "the closure ran on the polling thread, not the blocking pool",
    );
}

#[tokio::test]
async fn tokio_blocking_runs_nothing_until_polled() {
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let future = TokioBlocking.run(Box::new(move || {
        flag.store(true, Ordering::SeqCst);
    }));
    drop(future);
    // An eager `spawn_blocking` would already have queued the
    // closure; give the pool time to run it before checking.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !ran.load(Ordering::SeqCst),
        "the closure ran although the future was never polled",
    );
}
