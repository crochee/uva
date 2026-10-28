use std::time::{Duration, Instant};

use super::{FixedSleeper, Sleeper, ThreadSleeper};

#[test]
fn fixed_sleeper_resolves_immediately() {
    let start = Instant::now();
    futures::executor::block_on(FixedSleeper.sleep(Duration::from_secs(3600)));
    assert!(start.elapsed() < Duration::from_millis(50));
}

#[test]
fn thread_sleeper_waits_at_least_the_requested_duration() {
    let start = Instant::now();
    futures::executor::block_on(ThreadSleeper.sleep(Duration::from_millis(30)));
    assert!(start.elapsed() >= Duration::from_millis(25));
}

#[test]
fn sleeper_is_usable_as_a_trait_object() {
    let sleeper: std::sync::Arc<dyn Sleeper> =
        std::sync::Arc::new(ThreadSleeper);
    futures::executor::block_on(sleeper.sleep(Duration::from_millis(1)));
}

#[test]
fn thread_sleeper_starts_the_wait_on_first_poll() {
    // The wait must begin on the first poll, not when `sleep` is
    // called — the laziness `tokio::time::sleep` has. The tell is
    // the time still outstanding at the first poll: an eager
    // implementation has already burned the delay by then, so a
    // pre-poll pause longer than the requested duration would
    // leave nothing to wait for.
    let requested = Duration::from_millis(200);
    let future = ThreadSleeper.sleep(requested);
    // Longer than `requested`, so an eager wait would have elapsed
    // in full before the future is ever polled.
    std::thread::sleep(Duration::from_millis(300));

    let start = Instant::now();
    futures::executor::block_on(future);
    let waited = start.elapsed();

    // A lazy wait still owes the whole `requested` duration at the
    // first poll. The margin is one-sided: the waiting thread sleeps
    // the full duration, so the measured wait cannot come up short.
    assert!(
        waited >= Duration::from_millis(150),
        "the wait must start on the first poll, but only \
         {waited:?} of the {requested:?} was still outstanding",
    );
}
