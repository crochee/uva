//! The process shutdown budget.
//!
//! Every deadline the graceful-shutdown path uses lives here, together
//! with the arithmetic that keeps a stop inside a container's usual
//! grace period. `main` owns the *order* of the stop; this module owns
//! the *size* of it.
//!
//! The shape of a stop:
//!
//! ```text
//! signal ─┬─► controller drain  (bounded by DRAIN_DEADLINE)
//!         └─► HTTP drain        (bounded by HTTP_DRAIN_DEADLINE)
//!                    ↓ (both done)
//!              telemetry flush  (bounded by TRACE_FLUSH_DEADLINE)
//! ```
//!
//! The two drains start *at the signal* and run *concurrently*, so the
//! stop costs `max(DRAIN_DEADLINE, HTTP_DRAIN_DEADLINE)` — not their
//! sum — and the telemetry flush runs strictly after both. The
//! deadlines are sized so the whole stop still fits inside the grace
//! an orchestrator gives a container before it SIGKILLs the process.
//!
//! Nothing here bounds the server's *uptime*: the HTTP deadline starts
//! when the signal arrives, not when the process boots.

use std::{sync::Arc, time::Duration};

use tracing::{error, info};

/// The stop grace a container is normally given before the
/// orchestrator SIGKILLs the process (SIGKILL after that is the
/// orchestrator's call, not ours). The budgets below are sized to
/// fit inside it.
pub const STOP_GRACE: Duration = Duration::from_secs(10);

/// How long the whole controller drain may take: every close runs
/// concurrently, so this is one controller's worst case, sized to fit
/// inside a container's usual 10 s stop grace period (SIGKILL after
/// that is the orchestrator's call, not ours).
///
/// A controller's own close is bounded by `CLOSE_WAIT_TIMEOUT` (6 s in
/// `session::controller`), so this deadline stays above it: the
/// controller's "did not stop in time" warning is reachable before
/// the drain abandons the rest.
pub const DRAIN_DEADLINE: Duration = Duration::from_secs(8);

/// How long the HTTP drain may linger **after the signal**: axum's
/// graceful shutdown waits for every connection, and a streaming body
/// only ends when its controller does — so this runs concurrently with
/// [`DRAIN_DEADLINE`], and both are sized to fit inside a container's
/// usual 10 s stop grace period.
///
/// The deadline bounds the *drain*, not the server's uptime: the
/// graceful-shutdown future only resolves when the cancel token fires,
/// so the budget may only start once the signal has arrived.
pub const HTTP_DRAIN_DEADLINE: Duration = Duration::from_secs(8);

/// Budget left for the OTLP trace flush. The arithmetic: the signal
/// starts `DRAIN_DEADLINE` and `HTTP_DRAIN_DEADLINE` concurrently
/// (max 8 s), and the flush runs strictly after both, so 8 s + 1 s
/// leaves 1 s of slack inside a 10 s container stop grace — losing
/// the telemetry tail beats exiting 137 after every lease was
/// already released.
pub const TRACE_FLUSH_DEADLINE: Duration = Duration::from_secs(1);

/// Close every live session controller, concurrently, under one
/// deadline. `close()` is idempotent and always `Ok(())`, so a
/// controller that already exited (idle timeout) is a no-op; one that
/// hangs past the deadline is abandoned — the lease TTL covers it.
pub async fn drain_session_controllers(state: &Arc<crate::state::AppState>) {
    let controllers: Vec<Arc<crate::session::controller::SessionController>> =
        state
            .active_sessions
            .iter()
            .map(|entry| entry.clone())
            .collect();
    if controllers.is_empty() {
        return;
    }
    info!(
        sessions = controllers.len(),
        "shutdown: draining session controllers"
    );
    let closes =
        futures::future::join_all(controllers.iter().map(|c| c.close()));
    if tokio::time::timeout(DRAIN_DEADLINE, closes).await.is_err() {
        error!(
            "shutdown: controller drain exceeded {DRAIN_DEADLINE:?}; \
             abandoning the rest"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DRAIN_DEADLINE,
        HTTP_DRAIN_DEADLINE,
        STOP_GRACE,
        TRACE_FLUSH_DEADLINE,
    };

    /// The post-signal budget must fit inside the container's stop
    /// grace: the two drains run concurrently (so the stop costs their
    /// *maximum*, not their sum) and the telemetry flush runs after
    /// both.
    #[test]
    fn the_post_signal_budget_fits_inside_the_stop_grace() {
        let drains = DRAIN_DEADLINE.max(HTTP_DRAIN_DEADLINE);
        assert!(
            drains + TRACE_FLUSH_DEADLINE <= STOP_GRACE,
            "the stop must fit in the grace: {} + {} > {}",
            drains.as_secs(),
            TRACE_FLUSH_DEADLINE.as_secs(),
            STOP_GRACE.as_secs()
        );
    }

    /// The controller's own close (6 s) must stay under the drain's
    /// deadline (8 s), or the drain would abandon a controller whose
    /// close was still making progress.
    #[test]
    fn the_controller_close_budget_stays_under_the_drain_deadline() {
        assert!(
            crate::session::controller::CLOSE_WAIT_TIMEOUT < DRAIN_DEADLINE,
            "a controller close must fit inside the drain deadline"
        );
    }
}
