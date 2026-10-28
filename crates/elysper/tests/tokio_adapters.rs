//! The facade's tokio adapters — the runtime implementations of the
//! three runtime-neutral seams, reachable through the one
//! dependency instead of a second crate.
//!
//! A tokio host installs these where a runtime-neutral crate takes
//! its delay / spawn / blocking seam (e.g. `ResilientProvider::
//! with_sleeper(Arc::new(TokioSleeper))`), so a retry backoff costs
//! a timer entry rather than an OS thread.
//!
//! The whole target is behind the `harness-tokio` facade feature
//! (`required-features` in the manifest): under the default set the
//! adapters must not exist at all — the facade's root docs carry
//! that proof as a `compile_fail` doctest.

use std::time::Duration;

use elysper::{
    core::{blocking::Blocking, sleeper::Sleeper, spawn::Spawner},
    harness_tokio::{TokioBlocking, TokioSleeper, TokioSpawner},
};

#[test]
fn the_adapters_implement_the_runtime_neutral_seams() {
    fn is_sleeper<T: Sleeper>() {}
    fn is_spawner<T: Spawner>() {}
    fn is_blocking<T: Blocking>() {}

    is_sleeper::<TokioSleeper>();
    is_spawner::<TokioSpawner>();
    is_blocking::<TokioBlocking>();
}

/// The sleeper waits on the tokio timer: under a paused clock the
/// wait resolves by advancing the *virtual* clock, so a 30 s
/// backoff costs microseconds of real time — the property that
/// makes it the sleeper a tokio host must install.
#[tokio::test(start_paused = true)]
async fn the_sleeper_waits_on_the_tokio_timer() {
    let before = tokio::time::Instant::now();
    TokioSleeper.sleep(Duration::from_secs(30)).await;
    assert!(
        before.elapsed() >= Duration::from_secs(30),
        "the wait must register on the tokio clock"
    );
}
