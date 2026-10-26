use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::{Blocking, ThreadBlocking};

#[test]
fn thread_blocking_runs_the_closure() {
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    futures::executor::block_on(ThreadBlocking.run(Box::new(move || {
        flag.store(true, Ordering::SeqCst);
    })));
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn the_closure_may_carry_its_own_result_channel() {
    let (tx, rx) = futures::channel::oneshot::channel::<u32>();
    futures::executor::block_on(ThreadBlocking.run(Box::new(move || {
        let _ = tx.send(42);
    })));
    assert_eq!(futures::executor::block_on(rx), Ok(42));
}

#[test]
fn blocking_is_usable_as_a_trait_object() {
    let blocking: Arc<dyn Blocking> = Arc::new(ThreadBlocking);
    futures::executor::block_on(blocking.run(Box::new(|| {})));
}

#[test]
fn the_closure_starts_on_the_first_poll() {
    // The work must begin on the first poll, not when `run` is
    // called — the laziness `tokio::time::sleep` has. The tell is
    // the flag: an eager implementation has already run the closure
    // by the time `run` returns.
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let future = ThreadBlocking.run(Box::new(move || {
        flag.store(true, Ordering::SeqCst);
    }));
    assert!(
        !ran.load(Ordering::SeqCst),
        "the closure must not run before the future is polled",
    );

    futures::executor::block_on(future);
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn a_dropped_future_never_runs_the_closure() {
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let future = ThreadBlocking.run(Box::new(move || {
        flag.store(true, Ordering::SeqCst);
    }));
    drop(future);
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(
        !ran.load(Ordering::SeqCst),
        "a future dropped before its first poll must run nothing",
    );
}
