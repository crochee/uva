//! [`ResilientTool`] — the `Tool` wrapper.

use std::{pin::Pin, sync::Arc};

use futures::Stream;
use parking_lot::Mutex;
use uva_core::{
    Clock,
    breaker::{BreakerConfig, BreakerState, Outcome},
    sleeper::{Sleeper, ThreadSleeper},
};
use uva_tool::{
    Context,
    ExecutionMode,
    StreamOutput,
    Tool,
    ToolAnnotations,
    ToolOutput,
    ToolOutputDefinition,
};

use crate::resilience::{
    classify::Outcome as ClassifyOutcome,
    provider::{ResiliencePolicy, compute_backoff},
};

/// Per-target tool breaker state. The wrapper owns one
/// `BreakerState` keyed by the tool name, so different tools do not
/// share state.
pub struct ResilientTool {
    inner: Arc<dyn Tool>,
    policy: parking_lot::RwLock<ResiliencePolicy>,
    /// Per-tool breaker state. The map is keyed by `inner.name()`
    /// so a wrapper that protects multiple tools keeps each tool's
    /// breaker independent. Tests can inspect via [`ResilientTool::breaker_state`].
    breakers: Mutex<std::collections::HashMap<String, BreakerState>>,
    clock: Arc<dyn Clock>,
    /// The delay the retry backoff waits through. The crate names no
    /// runtime: the default waits on a std thread, and a host on a
    /// real runtime installs that runtime's sleeper.
    sleeper: Arc<dyn Sleeper>,
}

impl ResilientTool {
    pub fn new(
        inner: Arc<dyn Tool>,
        policy: ResiliencePolicy,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inner,
            policy: parking_lot::RwLock::new(policy),
            breakers: Mutex::new(std::collections::HashMap::new()),
            clock,
            sleeper: Arc::new(ThreadSleeper),
        }
    }

    /// Install the delay the retry backoff waits through.
    ///
    /// The default, [`ThreadSleeper`], waits on a std thread — it
    /// needs no reactor, at the cost of one thread per retry. A host
    /// on a real runtime installs that runtime's sleeper instead
    /// (the tokio one lives in `uva-core-tokio`).
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// Snapshot of the per-tool breaker state (cloned).
    #[must_use]
    pub fn breaker_state(&self) -> BreakerState {
        self.breakers
            .lock()
            .get(self.inner.name())
            .cloned()
            .unwrap_or_default()
    }

    /// Snapshot of the live policy.
    #[must_use]
    pub fn policy(&self) -> ResiliencePolicy {
        self.policy.read().clone()
    }

    /// Hot-reload policy.
    pub fn set_policy(&self, policy: ResiliencePolicy) {
        *self.policy.write() = policy;
    }
}

#[async_trait::async_trait]
impl Tool for ResilientTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> serde_json::Value {
        self.inner.parameters()
    }

    fn mode(&self) -> ExecutionMode {
        self.inner.mode()
    }

    fn annotations(&self) -> Option<ToolAnnotations> {
        self.inner.annotations()
    }

    fn output_definition(&self) -> ToolOutputDefinition {
        self.inner.output_definition()
    }

    async fn truncate(&self, output: &mut ToolOutput, context: &Context) {
        self.inner.truncate(output, context).await;
    }

    fn stream<'a>(
        &'a self,
        input: serde_json::Value,
        context: &'a Context,
    ) -> Pin<Box<dyn Stream<Item = StreamOutput> + Send + 'a>> {
        // Streaming path is policy-free.
        self.inner.stream(input, context)
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        let policy = self.policy();
        let max_attempts = policy.max_attempts.max(1);
        let tool_name = self.inner.name().to_string();

        for attempt in 0..max_attempts {
            // Admission check.
            {
                let mut breakers = self.breakers.lock();
                let cfg: BreakerConfig = policy.breaker.clone();
                let now = self.clock.now();
                let state = breakers.entry(tool_name.clone()).or_default();
                if !state.admits(&cfg, now) {
                    return ToolOutput::error(format!(
                        "circuit open: tool `{tool_name}` rejected by breaker"
                    ));
                }
            }

            let output = self.inner.call(input.clone(), context).await;
            // Classify: error outputs are retryable per policy.
            let retryable = output.is_error.unwrap_or(false);
            let outcome = if retryable {
                ClassifyOutcome::Retry
            } else {
                ClassifyOutcome::GiveUp
            };
            // Record on breaker.
            {
                let mut breakers = self.breakers.lock();
                let cfg: BreakerConfig = policy.breaker.clone();
                let now = self.clock.now();
                let state = breakers.entry(tool_name.clone()).or_default();
                let obs = state.record(
                    if retryable {
                        Outcome::Failure
                    } else {
                        Outcome::Success
                    },
                    &cfg,
                    now,
                );
                *state = obs.state;
            }
            match outcome {
                ClassifyOutcome::GiveUp => return output,
                ClassifyOutcome::Retry => {
                    if attempt + 1 >= max_attempts {
                        return output;
                    }
                    let backoff = compute_backoff(&policy, attempt);
                    self.sleeper.sleep(backoff).await;
                }
            }
        }
        // Unreachable: the loop returns on every iteration.
        ToolOutput::error("no attempts made".to_string())
    }
}

impl std::fmt::Debug for ResilientTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResilientTool")
            .field("inner", &self.inner.name())
            .field("policy", &self.policy())
            .field("breaker", &self.breaker_state())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    use chrono::TimeZone;

    use super::*;

    struct FlakyTool {
        calls: AtomicU64,
    }
    #[async_trait::async_trait]
    impl Tool for FlakyTool {
        fn name(&self) -> &str {
            "flaky"
        }

        fn description(&self) -> &str {
            "flaky"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn call(
            &self,
            _input: serde_json::Value,
            _context: &Context,
        ) -> ToolOutput {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                ToolOutput::error("transient".to_string())
            } else {
                ToolOutput::text("ok")
            }
        }
    }

    #[tokio::test]
    async fn flaky_tool_succeeds_after_retries() {
        let inner: Arc<dyn Tool> = Arc::new(FlakyTool {
            calls: AtomicU64::new(0),
        });
        let clock: Arc<dyn Clock> = Arc::new(uva_core::FixedClock::new(
            chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        ));
        let resilient = ResilientTool::new(
            inner,
            ResiliencePolicy::default()
                .max_attempts(5)
                .initial_backoff(Duration::from_millis(1)),
            clock,
        );
        let r = resilient
            .call(serde_json::json!({}), &Context::default())
            .await;
        assert!(!r.is_error.unwrap_or(false));
        assert_eq!(r, ToolOutput::text("ok"));
    }
}
