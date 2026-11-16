//! Process-wide usage counters for `/api/v1/chat/usage`.

use std::sync::Arc;

use serde::Serialize;

/// Process-wide usage counters. Cheap to read (lock-free
/// `std::sync::atomic` snapshot) so the `/api/v1/chat/usage`
/// endpoint can poll without contention.
#[derive(Debug, Default)]
pub struct UsageMetrics {
    pub tokens_in: std::sync::atomic::AtomicU64,
    pub tokens_out: std::sync::atomic::AtomicU64,
    /// Finished runs, counted on the terminal
    /// `SystemEvent::SessionEnded` **whatever its
    /// [`SessionEndReason`](uva::harness::SessionEndReason)** — a
    /// cancelled, errored or interrupted run still consumed a turn,
    /// so counting only `Completed` would under-report the work the
    /// process actually did.
    ///
    /// Written by the session controllers' event funnel (one
    /// increment per `SessionEnded` per run); read-only here.
    pub turns: std::sync::atomic::AtomicU64,
}

impl UsageMetrics {
    /// Atomic snapshot of the three counters. Field order in
    /// `UsageResponse` mirrors this struct's order — `tokens_in`
    /// before `tokens_out` before `turns`.
    pub fn snapshot(&self) -> UsageSnapshot {
        UsageSnapshot {
            tokens_in: self
                .tokens_in
                .load(std::sync::atomic::Ordering::Relaxed),
            tokens_out: self
                .tokens_out
                .load(std::sync::atomic::Ordering::Relaxed),
            turns: self.turns.load(std::sync::atomic::Ordering::Relaxed),
        }
    }
}

/// Plain-data snapshot of [`UsageMetrics`] for the API
/// response. Cheap to clone via `Copy` since the underlying
/// counters are already integers.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct UsageSnapshot {
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub turns: u64,
}

/// The deployment's account of what its **delegated** children consumed.
///
/// A child round runs on its own harness, so its `Usage` events and its
/// terminal event never reach a [`SessionController`](crate::session)
/// — the parent's stream never sees them. Without this seam every
/// delegated token and turn would vanish from `/api/v1/chat/usage`;
/// the `task` tool reports each round here while it drains the child.
///
/// The counters are the same atomics the controllers write: one
/// process-wide total, whoever produced the tokens.
pub struct DelegatedUsage {
    metrics: Arc<UsageMetrics>,
}

impl DelegatedUsage {
    /// Report delegated rounds into `metrics`.
    #[must_use]
    pub fn new(metrics: Arc<UsageMetrics>) -> Self {
        Self { metrics }
    }
}

impl uva::agent::SubagentUsage for DelegatedUsage {
    fn tokens(&self, _child_session_id: &str, input: usize, output: usize) {
        self.metrics
            .tokens_in
            .fetch_add(input as u64, std::sync::atomic::Ordering::Relaxed);
        self.metrics
            .tokens_out
            .fetch_add(output as u64, std::sync::atomic::Ordering::Relaxed);
    }

    fn round_ended(&self, _child_session_id: &str) {
        self.metrics
            .turns
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use uva::agent::SubagentUsage as _;

    use super::*;

    /// A delegated round lands in the same counters the controllers
    /// write: `/api/v1/chat/usage` is the deployment's total, and a
    /// child's work is part of it.
    #[test]
    fn delegated_rounds_land_in_the_deployment_counters() {
        let metrics = Arc::new(UsageMetrics::default());
        let sink = DelegatedUsage::new(Arc::clone(&metrics));

        sink.tokens("child-a", 11, 5);
        sink.tokens("child-a", 2, 3);
        sink.round_ended("child-a");

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.tokens_in, 13);
        assert_eq!(snapshot.tokens_out, 8);
        assert_eq!(snapshot.turns, 1);
    }
}
