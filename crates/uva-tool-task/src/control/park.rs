//! The parking loop: the tick that ages idle children.
//!
//! The lifecycle's `tick` is a state-machine action; something has to
//! drive it. A model does not call a tool because five minutes went
//! by, so the surface runs one loop that sleeps
//! [`PARK_TICK`](super::PARK_TICK), advances the clock's contribution
//! to every idle child, and parks the ones past the TTL.
//!
//! The loop holds a **weak** reference to the surface: unloading the
//! `tool-task` brick (or dropping a test's surface) ends the loop
//! instead of leaking it. The seams are the runtime-neutral
//! [`Spawner`] and [`Sleeper`] the crate already depends on, so this
//! module names no runtime.

use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use uva_core::{sleeper::Sleeper, spawn::SharedSpawner};

use super::ControlSurface;

/// Sleep until the surface is gone or another tick is due.
pub(super) fn spawn_loop(
    surface: &Arc<ControlSurface>,
    spawner: SharedSpawner,
    sleeper: Arc<dyn Sleeper>,
    interval: Duration,
) {
    let surface: Weak<ControlSurface> = Arc::downgrade(surface);
    spawner.spawn(Box::pin(run(surface, sleeper, interval)));
}

/// The loop's body: wait, then advance the lifecycle.
///
/// Split from [`spawn_loop`] so a test can drive it without an
/// executor of its own.
pub(super) async fn run(
    surface: Weak<ControlSurface>,
    sleeper: Arc<dyn Sleeper>,
    interval: Duration,
) {
    loop {
        sleeper.sleep(interval).await;
        let Some(surface) = surface.upgrade() else {
            // The brick unloaded: nothing left to park.
            break;
        };
        surface.tick();
    }
}
