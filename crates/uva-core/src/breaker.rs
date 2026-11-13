//! [`BreakerState`] — a small circuit-breaker state machine.
//!
//! Lives in `uva-core` (runtime-neutral, no HTTP, no tokio)
//! so the resilience layer can wrap any provider or tool without
//! adding a runtime dependency. The state machine itself is small
//! enough to fit in a single file; tests pin every transition.
//!
//! State diagram:
//!
//! ```text
//!   ┌────────┐  failure_threshold consecutive failures   ┌────────┐
//!   │ Closed │ ─────────────────────────────────────────▶│  Open  │
//!   └────────┘                                            └────────┘
//!        ▲                                                      │
//!        │ success_threshold successes                          │ half_open_after
//!        │ in half-open                                         │ elapses
//!        │                                                      ▼
//!        │  probe success                              ┌────────────┐
//!        └────────────────────────────────────────────│  HalfOpen   │
//!                                                     └────────────┘
//!                                                            │
//!                                                            │ probe failure
//!                                                            ▼
//!                                                     (back to Open)
//! ```
//!
//! ## Threading
//!
//! `BreakerState` is intentionally a plain (non-`Mutex`) state —
//! callers wrap it in their preferred primitive
//! (`parking_lot::Mutex` is the workspace default). The state
//! transitions are pure functions of `(state, outcome, now)`; a
//! caller driving multiple calls under one lock gets the right
//! sequencing for free.
//!
//! ## Determinism
//!
//! Every transition takes [`Clock::now`] for the wall-clock half-open
//! timer. Tests inject [`FixedClock`] to advance time deterministically.
//!
//! [`Clock::now`]: crate::clock::Clock::now
//! [`FixedClock`]: crate::clock::FixedClock

use core::time::Duration;

/// Outcome a wrapped call observes. `uva-policy-plugins` provides
/// the classifier that maps a `ProviderError` (or a `ToolOutput`
/// error flag) to one of these variants; the state machine is
/// generic over the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// Call succeeded.
    Success,
    /// Call failed in a way the classifier marked as retryable
    /// (transient transport error, 5xx, rate limit).
    Failure,
}

/// Closed / Open / HalfOpen: the three canonical states. `Open`
/// rejects calls immediately; `HalfOpen` admits a single probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BreakerPhase {
    Closed,
    Open,
    HalfOpen,
}

/// Configuration for the breaker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakerConfig {
    /// Consecutive failures (in `Closed`) that flip the breaker to
    /// `Open`.
    pub failure_threshold: u32,
    /// Time the breaker stays `Open` before flipping to `HalfOpen`
    /// and admitting a single probe.
    pub half_open_after: Duration,
    /// Consecutive successes (in `HalfOpen`) that close the breaker.
    pub success_threshold_in_half_open: u32,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            half_open_after: Duration::from_secs(15),
            success_threshold_in_half_open: 1,
        }
    }
}

/// Internal counters the state machine keeps. Public for tests;
/// callers drive the state machine through [`BreakerState::record`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakerState {
    /// Current phase.
    pub phase: BreakerPhase,
    /// Consecutive failures since the last successful reset. Reset
    /// to 0 on every `Success`.
    pub consecutive_failures: u32,
    /// Consecutive successes observed while `HalfOpen`. Reset to 0
    /// on every `Failure` (and on every transition out of `HalfOpen`).
    pub consecutive_successes_in_half_open: u32,
    /// Wall-clock instant the breaker last flipped to `Open`. Used
    /// together with `half_open_after` to flip to `HalfOpen`.
    pub opened_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for BreakerState {
    fn default() -> Self {
        Self {
            phase: BreakerPhase::Closed,
            consecutive_failures: 0,
            consecutive_successes_in_half_open: 0,
            opened_at: None,
        }
    }
}

/// Result of an [`BreakerState::record`] call: the new phase, the
/// counters after the call, and whether the breaker is currently
/// "admitting" calls (i.e. not `Open`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakerObservation {
    pub state: BreakerState,
    pub admitted: bool,
}

impl BreakerState {
    /// Apply one outcome and return the new state plus the
    /// admission verdict for the **next** call.
    ///
    /// `now` is the wall-clock instant; tests inject
    /// [`FixedClock`](crate::FixedClock) to drive the half-open
    /// timer deterministically.
    ///
    /// The half-open timer is checked **first**: if the breaker is
    /// `Open` and `now - opened_at >= half_open_after`, it flips to
    /// `HalfOpen` and the call is admitted as a probe.
    #[must_use]
    pub fn record(
        &self,
        outcome: Outcome,
        config: &BreakerConfig,
        now: chrono::DateTime<chrono::Utc>,
    ) -> BreakerObservation {
        let mut state = self.clone();

        // Half-open timer: if `Open` long enough, flip to HalfOpen
        // and admit this call as a probe.
        if state.phase == BreakerPhase::Open
            && let Some(opened_at) = state.opened_at
        {
            let elapsed = now.signed_duration_since(opened_at);
            let threshold = chrono::Duration::from_std(config.half_open_after)
                .unwrap_or_else(|_| chrono::Duration::seconds(i64::MAX));
            if elapsed >= threshold {
                state.phase = BreakerPhase::HalfOpen;
                state.consecutive_successes_in_half_open = 0;
            }
        }

        let admitted = state.phase != BreakerPhase::Open;

        match (state.phase, outcome) {
            (BreakerPhase::Closed, Outcome::Success) => {
                state.consecutive_failures = 0;
            }
            (BreakerPhase::Closed, Outcome::Failure) => {
                state.consecutive_failures =
                    state.consecutive_failures.saturating_add(1);
                if state.consecutive_failures >= config.failure_threshold {
                    state.phase = BreakerPhase::Open;
                    state.opened_at = Some(now);
                }
            }
            (BreakerPhase::HalfOpen, Outcome::Success) => {
                state.consecutive_successes_in_half_open =
                    state.consecutive_successes_in_half_open.saturating_add(1);
                if state.consecutive_successes_in_half_open
                    >= config.success_threshold_in_half_open
                {
                    state.phase = BreakerPhase::Closed;
                    state.consecutive_failures = 0;
                    state.consecutive_successes_in_half_open = 0;
                    state.opened_at = None;
                }
            }
            (BreakerPhase::HalfOpen, Outcome::Failure) => {
                // Probe failed: back to Open with a fresh timer.
                state.phase = BreakerPhase::Open;
                state.opened_at = Some(now);
                state.consecutive_successes_in_half_open = 0;
            }
            (BreakerPhase::Open, _) => {
                // Should not happen — we flipped above if the timer
                // had elapsed. If we still observe `Open` here, the
                // call was admitted against a stale state. The
                // counters are left unchanged; the caller treats
                // this call as "rejected" anyway.
            }
        }

        BreakerObservation { state, admitted }
    }

    /// True when a call may proceed right now. Equivalent to
    /// `record(Success, config, now).admitted` for callers that
    /// have not yet observed an outcome.
    #[must_use]
    pub fn admits(
        &self,
        config: &BreakerConfig,
        now: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        if self.phase != BreakerPhase::Open {
            return true;
        }
        match self.opened_at {
            Some(opened_at) => {
                let elapsed = now.signed_duration_since(opened_at);
                let threshold =
                    chrono::Duration::from_std(config.half_open_after)
                        .unwrap_or_else(|_| {
                            chrono::Duration::seconds(i64::MAX)
                        });
                elapsed >= threshold
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::{Clock, FixedClock};

    fn clock(t: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.timestamp_opt(t, 0).unwrap()
    }

    #[test]
    fn closed_admits() {
        let s = BreakerState::default();
        assert!(s.admits(&BreakerConfig::default(), clock(0)));
    }

    #[test]
    fn failure_threshold_flips_to_open() {
        let cfg = BreakerConfig {
            failure_threshold: 3,
            half_open_after: Duration::from_secs(10),
            success_threshold_in_half_open: 1,
        };
        let mut s = BreakerState::default();
        for i in 0..3 {
            let obs = s.record(Outcome::Failure, &cfg, clock(i));
            s = obs.state;
        }
        assert_eq!(s.phase, BreakerPhase::Open);
        assert_eq!(s.consecutive_failures, 3);
        assert_eq!(s.opened_at, Some(clock(2)));
    }

    #[test]
    fn success_in_closed_resets_counter() {
        let cfg = BreakerConfig::default();
        let mut s = BreakerState::default();
        let obs = s.record(Outcome::Failure, &cfg, clock(1));
        s = obs.state;
        let obs = s.record(Outcome::Success, &cfg, clock(2));
        s = obs.state;
        assert_eq!(s.consecutive_failures, 0);
        assert_eq!(s.phase, BreakerPhase::Closed);
    }

    #[test]
    fn open_rejects_until_half_open_after() {
        let cfg = BreakerConfig {
            failure_threshold: 1,
            half_open_after: Duration::from_secs(10),
            success_threshold_in_half_open: 1,
        };
        let mut s = BreakerState::default();
        let obs = s.record(Outcome::Failure, &cfg, clock(0));
        s = obs.state;
        assert_eq!(s.phase, BreakerPhase::Open);
        assert!(!s.admits(&cfg, clock(5)));
        assert!(!s.admits(&cfg, clock(9)));
        assert!(s.admits(&cfg, clock(10)));
    }

    #[test]
    fn probe_success_in_half_open_closes_breaker() {
        let cfg = BreakerConfig {
            failure_threshold: 1,
            half_open_after: Duration::from_secs(10),
            success_threshold_in_half_open: 1,
        };
        let mut s = BreakerState::default();
        let obs = s.record(Outcome::Failure, &cfg, clock(0));
        s = obs.state;
        let obs = s.record(Outcome::Success, &cfg, clock(10));
        s = obs.state;
        assert_eq!(s.phase, BreakerPhase::Closed);
        assert_eq!(s.consecutive_successes_in_half_open, 0);
    }

    #[test]
    fn probe_failure_returns_to_open() {
        let cfg = BreakerConfig {
            failure_threshold: 1,
            half_open_after: Duration::from_secs(10),
            success_threshold_in_half_open: 1,
        };
        let mut s = BreakerState::default();
        let obs = s.record(Outcome::Failure, &cfg, clock(0));
        s = obs.state;
        // Timer elapses → half-open → admit probe → fail.
        let obs = s.record(Outcome::Failure, &cfg, clock(10));
        s = obs.state;
        assert_eq!(s.phase, BreakerPhase::Open);
        assert_eq!(s.opened_at, Some(clock(10)));
    }

    #[test]
    fn deterministic_clock_advances_half_open_after() {
        let cfg = BreakerConfig {
            failure_threshold: 1,
            half_open_after: Duration::from_secs(60),
            success_threshold_in_half_open: 1,
        };
        let clock = FixedClock::new(clock(0));
        let mut s = BreakerState::default();
        let obs = s.record(Outcome::Failure, &cfg, clock.now());
        s = obs.state;
        // Same instant → not admitted.
        assert!(!s.admits(&cfg, clock.now()));
    }
}
