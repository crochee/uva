//! [`ResilientProvider`] — the `ModelProvider` wrapper.

use std::{sync::Arc, time::Duration};

use elysper_core::{
    Clock,
    breaker::{BreakerConfig, BreakerState, Outcome},
    sleeper::{Sleeper, ThreadSleeper},
};
use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    StreamChunk,
};
use parking_lot::Mutex;

use crate::classify::{
    ClassifyFn,
    Outcome as ClassifyOutcome,
    default_classifier,
};

/// Backoff jitter strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Jitter {
    /// Constant backoff, no randomness.
    #[default]
    None,
    /// Uniform jitter over `[0, backoff]`.
    Full,
    /// Decorrelated jitter (`next = min(cap, random(base, prev*3))`).
    Decorrelated,
}

/// Resilience policy. Hot-reloadable via `RwLock`.
#[derive(Clone)]
pub struct ResiliencePolicy {
    /// Maximum number of attempts (1 = no retries, 5 = up to 5
    /// tries).
    pub max_attempts: u32,
    /// Initial backoff. Subsequent backoffs double, capped at
    /// `max_backoff`.
    pub initial_backoff: Duration,
    /// Maximum backoff per attempt.
    pub max_backoff: Duration,
    /// Jitter strategy.
    pub jitter: Jitter,
    /// Circuit breaker config.
    pub breaker: BreakerConfig,
    /// Custom classifier. `None` ⇒ default.
    pub classify: Option<ClassifyFn>,
}

impl std::fmt::Debug for ResiliencePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResiliencePolicy")
            .field("max_attempts", &self.max_attempts)
            .field("initial_backoff", &self.initial_backoff)
            .field("max_backoff", &self.max_backoff)
            .field("jitter", &self.jitter)
            .field("breaker", &self.breaker)
            .field("classify", &"<dyn Fn>")
            .finish()
    }
}

impl Default for ResiliencePolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
            jitter: Jitter::None,
            breaker: BreakerConfig::default(),
            classify: None,
        }
    }
}

impl ResiliencePolicy {
    /// Builder-style setter.
    #[must_use]
    pub fn max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n;
        self
    }

    /// Builder-style setter.
    #[must_use]
    pub fn initial_backoff(mut self, d: Duration) -> Self {
        self.initial_backoff = d;
        self
    }

    /// Builder-style setter.
    #[must_use]
    pub fn max_backoff(mut self, d: Duration) -> Self {
        self.max_backoff = d;
        self
    }

    /// Builder-style setter.
    #[must_use]
    pub fn jitter(mut self, j: Jitter) -> Self {
        self.jitter = j;
        self
    }

    /// Builder-style setter.
    #[must_use]
    pub fn breaker(mut self, c: BreakerConfig) -> Self {
        self.breaker = c;
        self
    }

    /// Builder-style setter.
    #[must_use]
    pub fn classify(mut self, f: ClassifyFn) -> Self {
        self.classify = Some(f);
        self
    }
}

/// `ModelProvider` wrapper that retries transient errors with
/// backoff + jitter, drives a circuit breaker per target, and
/// surfaces classifier-rejected errors as-is.
///
/// `ResilientProvider::complete` is the only method that drives the
/// policy; `complete_with_stream` is pass-through.
pub struct ResilientProvider {
    inner: Arc<dyn ModelProvider>,
    policy: parking_lot::RwLock<ResiliencePolicy>,
    breaker: Mutex<BreakerState>,
    clock: Arc<dyn Clock>,
    /// The delay the retry backoff waits through. The crate names no
    /// runtime: the default waits on a std thread, and a host on a
    /// real runtime installs that runtime's sleeper.
    sleeper: Arc<dyn Sleeper>,
    /// Counters for observability.
    pub(crate) attempts: parking_lot::Mutex<u64>,
    pub(crate) retries: parking_lot::Mutex<u64>,
    pub(crate) rejections: parking_lot::Mutex<u64>,
}

impl ResilientProvider {
    pub fn new(
        inner: Arc<dyn ModelProvider>,
        policy: ResiliencePolicy,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inner,
            policy: parking_lot::RwLock::new(policy),
            breaker: Mutex::new(BreakerState::default()),
            clock,
            sleeper: Arc::new(ThreadSleeper),
            attempts: parking_lot::Mutex::new(0),
            retries: parking_lot::Mutex::new(0),
            rejections: parking_lot::Mutex::new(0),
        }
    }

    /// Install the delay the retry backoff waits through.
    ///
    /// The default, [`ThreadSleeper`], waits on a std thread — it
    /// needs no reactor, at the cost of one thread per retry. A host
    /// on a real runtime installs that runtime's sleeper instead
    /// (the tokio one lives in `elysper-harness-tokio`).
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
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

    /// Snapshot of the breaker state.
    #[must_use]
    pub fn breaker_state(&self) -> BreakerState {
        self.breaker.lock().clone()
    }

    /// Number of total attempts (1 + retries).
    #[must_use]
    pub fn attempt_count(&self) -> u64 {
        *self.attempts.lock()
    }

    /// Number of retries that hit the inner provider.
    #[must_use]
    pub fn retry_count(&self) -> u64 {
        *self.retries.lock()
    }

    /// Number of calls rejected by the breaker without consulting
    /// the inner provider.
    #[must_use]
    pub fn rejection_count(&self) -> u64 {
        *self.rejections.lock()
    }
}

#[async_trait::async_trait]
impl ModelProvider for ResilientProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> core::result::Result<(), elysper_core::Error> {
        // The wrapper holds `inner` as `Arc<dyn ModelProvider>`, so
        // a mutable call into `inner.initialize` requires
        // `Arc::get_mut` (only succeeds with a single strong ref).
        // We instead require the caller to initialise the inner
        // provider before wrapping, mirroring
        // `elysper-semantic-cache::SemanticCacheProvider::initialize`.
        Ok(())
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model_config(&self) -> ModelConfig {
        self.inner.model_config()
    }

    fn tier(&self) -> elysper_provider::ModelTier {
        self.inner.tier()
    }

    fn supports_inline_cache_hints(&self) -> bool {
        self.inner.supports_inline_cache_hints()
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> core::result::Result<CompletionResponse, elysper_core::Error> {
        let policy = self.policy();
        let max_attempts = policy.max_attempts.max(1);

        let mut last_err: Option<elysper_core::Error> = None;
        for attempt in 0..max_attempts {
            // Breaker admission check before the call.
            {
                let breaker = self.breaker.lock();
                let cfg = policy.breaker.clone();
                let now = self.clock.now();
                if !breaker.admits(&cfg, now) {
                    *self.rejections.lock() += 1;
                    return Err(elysper_core::Error::Internal {
                        message: "circuit open: call rejected by breaker"
                            .into(),
                    });
                }
                // Lock released at end of block.
            }
            *self.attempts.lock() += 1;

            match self.inner.complete(request.clone()).await {
                Ok(resp) => {
                    // Success: record on the breaker.
                    let mut breaker = self.breaker.lock();
                    let cfg = policy.breaker.clone();
                    let now = self.clock.now();
                    let obs = breaker.record(Outcome::Success, &cfg, now);
                    *breaker = obs.state;
                    return Ok(resp);
                }
                Err(err) => {
                    // Classify.
                    let classifier =
                        policy.classify.clone().unwrap_or_else(|| {
                            Arc::new(default_classifier) as ClassifyFn
                        });
                    let outcome = classifier(&err);
                    // Record on breaker.
                    {
                        let mut breaker = self.breaker.lock();
                        let cfg = policy.breaker.clone();
                        let now = self.clock.now();
                        let obs = breaker.record(
                            match outcome {
                                ClassifyOutcome::Retry => Outcome::Failure,
                                ClassifyOutcome::GiveUp => Outcome::Failure,
                            },
                            &cfg,
                            now,
                        );
                        *breaker = obs.state;
                    }
                    match outcome {
                        ClassifyOutcome::GiveUp => {
                            return Err(err);
                        }
                        ClassifyOutcome::Retry => {
                            last_err = Some(err);
                            if attempt + 1 < max_attempts {
                                let backoff = compute_backoff(&policy, attempt);
                                self.sleeper.sleep(backoff).await;
                                *self.retries.lock() += 1;
                            }
                        }
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| elysper_core::Error::Internal {
            message: "no attempts made".to_string(),
        }))
    }

    async fn complete_with_stream(
        &self,
        request: CompletionRequest,
        cancel_token: Option<Arc<dyn elysper_core::CancelToken>>,
        on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> core::result::Result<CompletionResponse, elysper_core::Error> {
        // Streaming path is policy-free; the cache / breaker
        // decisions all happen on `complete`.
        self.inner
            .complete_with_stream(request, cancel_token, on_delta)
            .await
    }

    async fn embed(
        &self,
        texts: Vec<String>,
    ) -> core::result::Result<Vec<Vec<f64>>, elysper_core::Error> {
        self.inner.embed(texts).await
    }
}

impl std::fmt::Debug for ResilientProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResilientProvider")
            .field("inner", &self.inner.name())
            .field("policy", &self.policy())
            .field("breaker", &self.breaker_state())
            .finish()
    }
}

/// Compute the backoff for a given attempt. Public so tests can
/// assert the schedule without driving the full retry loop.
#[must_use]
pub fn compute_backoff(policy: &ResiliencePolicy, attempt: u32) -> Duration {
    let base = policy
        .initial_backoff
        .saturating_mul(1u32 << attempt.min(20));
    let capped = base.min(policy.max_backoff);
    match policy.jitter {
        Jitter::None => capped,
        Jitter::Full => {
            // Uniform jitter over [0, capped].
            let nanos = capped.as_nanos() as u64;
            if nanos == 0 {
                Duration::ZERO
            } else {
                let r = rand::random::<u64>() % nanos;
                Duration::from_nanos(r)
            }
        }
        Jitter::Decorrelated => {
            // Decorr. jitter: rand in [initial, backoff*3], capped
            // at max_backoff.
            let lo = policy.initial_backoff.as_nanos() as u64;
            let hi = (capped.as_nanos() as u64).saturating_mul(3);
            let r = if hi <= lo {
                lo
            } else {
                lo + rand::random::<u64>() % (hi - lo)
            };
            Duration::from_nanos(r.min(policy.max_backoff.as_nanos() as u64))
        }
    }
}

/// Drive `compute_backoff` in a loop, returning the elapsed
/// duration. Public for tests; the production retry waits through
/// the [`Sleeper`] seam instead.
pub async fn retry_with_backoff(
    policy: &ResiliencePolicy,
    attempts: u32,
) -> Duration {
    let mut elapsed = Duration::ZERO;
    for a in 0..attempts {
        elapsed += compute_backoff(policy, a);
    }
    elapsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_without_jitter() {
        let p = ResiliencePolicy::default()
            .initial_backoff(Duration::from_millis(100))
            .max_backoff(Duration::from_secs(10))
            .jitter(Jitter::None);
        assert_eq!(compute_backoff(&p, 0), Duration::from_millis(100));
        assert_eq!(compute_backoff(&p, 1), Duration::from_millis(200));
        assert_eq!(compute_backoff(&p, 2), Duration::from_millis(400));
    }

    #[test]
    fn backoff_caps_at_max() {
        let p = ResiliencePolicy::default()
            .initial_backoff(Duration::from_millis(100))
            .max_backoff(Duration::from_millis(500))
            .jitter(Jitter::None);
        assert_eq!(compute_backoff(&p, 10), Duration::from_millis(500));
    }

    #[test]
    fn full_jitter_is_non_negative() {
        let p = ResiliencePolicy::default()
            .initial_backoff(Duration::from_millis(100))
            .max_backoff(Duration::from_secs(10))
            .jitter(Jitter::Full);
        for _ in 0..100 {
            let d = compute_backoff(&p, 2);
            // Decorrelated jitter can produce a value < initial,
            // but Full jitter is in [0, capped]; both must be
            // non-negative (Duration::ZERO is the floor).
            assert!(d <= Duration::from_secs(10));
        }
    }

    #[test]
    fn default_policy_has_sane_values() {
        let p = ResiliencePolicy::default();
        assert_eq!(p.max_attempts, 3);
        assert_eq!(p.initial_backoff, Duration::from_millis(500));
        assert_eq!(p.max_backoff, Duration::from_secs(10));
        assert_eq!(p.jitter, Jitter::None);
    }

    // (No test-only `_typecheck` helper here — the resilience types
    // are exercised through end-to-end tests below.)
}
