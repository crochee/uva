//! The parking loop and the surface's tick: the lifecycle advances on
//! its own, without a model call.
//!
//! The loop is driven by an injected [`Sleeper`], so the test moves
//! time exactly when the loop looks at it.

use std::{sync::Arc, time::Duration};

use uva_agent::AgentRegistry;
use uva_core::sleeper::Sleeper;

use super::{
    super::{
        PARK_TICK,
        park,
        state::{ChildStatus, PARK_TTL, RoundOutcome},
    },
    support::{StepSleeper, TestClock, running_handle},
};

/// One idle child of `parent`, on a surface whose TTL runs on a test
/// clock.
fn parked_surface() -> (Arc<super::super::ControlSurface>, TestClock) {
    let clock = TestClock::new();
    let surface = Arc::new(
        super::super::ControlSurface::new(Arc::new(AgentRegistry::new()))
            .with_clock(clock.shared()),
    );
    let (handle, _steer, _token, _inbox) = running_handle("scout");
    surface.registry().register("parent", handle);
    surface
        .registry()
        .settle("parent", "child-1", RoundOutcome::Completed);
    (surface, clock)
}

/// The loop parks an idle child on its own: no tool call, no model
/// turn — just time passing and a tick.
#[tokio::test]
async fn the_park_loop_parks_an_idle_child_on_its_own() {
    let (surface, clock) = parked_surface();
    let sleeper = Arc::new(StepSleeper::new(2, {
        let clock = clock.clone();
        move || clock.advance(PARK_TTL)
    })) as Arc<dyn Sleeper>;

    // The loop never finishes; a bounded wait is enough to see its
    // first tick land.
    let _ = tokio::time::timeout(
        Duration::from_millis(500),
        park::run(Arc::downgrade(&surface), sleeper, PARK_TICK),
    )
    .await;

    let row = &surface.registry().list("parent")[0];
    assert_eq!(row.status, ChildStatus::Parked);
    assert_eq!(row.activity, "parked");
    assert_eq!(
        surface.registry().has_handles("parent", "child-1"),
        Some(false),
        "the loop released the child's memory"
    );
}

/// The surface's own `tick` parks too, so a host without a loop still
/// ages children between tool calls.
#[test]
fn a_tick_parks_through_the_surface() {
    let (surface, clock) = parked_surface();
    assert_eq!(surface.tick(), 0, "nothing has aged yet");

    clock.advance(PARK_TTL);
    assert_eq!(surface.tick(), 1);
    assert_eq!(surface.tick(), 0, "a parked child does not park twice");
    assert_eq!(
        surface.registry().list("parent")[0].status,
        ChildStatus::Parked
    );
}
