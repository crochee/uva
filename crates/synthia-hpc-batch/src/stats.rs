//! Live counters for a batch driver.
//!
//! Populated through a `parking_lot::Mutex<BatchStats>`,
//! never through atomics — the counters are written in three
//! phases (claim → commit / fail) and the mutability
//! patterns do not benefit from atomics here.

/// Live counters — `submitted`, `inflight`, `completed`,
/// `failed`. `Copy` so the snapshot accessor returns by
/// value without an allocation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BatchStats {
    /// Total items ever submitted (across every chunk in
    /// the run).
    pub submitted: usize,
    /// Items currently in flight (claimed but not yet
    /// committed / failed).
    pub inflight: usize,
    /// Items successfully completed.
    pub completed: usize,
    /// Items failed (the batch driver records `Error::Item`
    /// for each, surfacing the inner provider's stringified
    /// error to the caller).
    pub failed: usize,
}

impl BatchStats {
    /// `submitted - completed - failed` — items still in
    /// flight or scheduled but not yet polled.
    pub fn pending(&self) -> usize {
        self.submitted
            .saturating_sub(self.completed)
            .saturating_sub(self.failed)
    }

    /// Highest `inflight` value reached in the lifetime of
    /// the run; useful as a tunable knob for
    /// `BatchPolicy::max_inflight`.
    pub fn inflight_peak_hint(&self) -> usize {
        self.inflight
    }
}
