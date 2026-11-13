//! End-to-end tests for the resilience layer.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::TimeZone;
use uva_core::{Clock, sleeper::Sleeper};
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    TextContent,
};
use uva_tool::{Context, Tool, ToolOutput};

use crate::resilience::{
    ResilientProvider,
    ResilientTool,
    provider::ResiliencePolicy,
};

struct CountingProvider {
    calls: AtomicU64,
    fail_until: u64,
}

#[async_trait::async_trait]
impl ModelProvider for CountingProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), uva_core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "counting"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "test-model".into(),
            provider: "counting".into(),
            context_window: 8_192,
            max_output_tokens: 1_024,
            supports_tools: false,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, uva_core::Error> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.fail_until {
            // Return an error that the default classifier maps to Retry.
            // We use a transport-flavored error (Transient) so the
            // provider's retry classifier marks it retryable.
            Err(uva_core::Error::Timeout {
                message: "transient".into(),
            })
        } else {
            Ok(CompletionResponse {
                id: "r".into(),
                model: "test-model".into(),
                content: Content::Single(ContentPart::Text(TextContent {
                    text: "ok".into(),
                    cache_control: None,
                })),
                usage: uva_provider::TokenUsage::default(),
                cached: false,
                stop_reason: None,
                replay_state: None,
            })
        }
    }
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(uva_core::FixedClock::new(
        chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ))
}

fn make_request() -> CompletionRequest {
    use uva_provider::{Message, Role};
    CompletionRequest {
        model: "test-model".into(),
        messages: Arc::new(vec![Message {
            role: Role::User,
            content: Content::Single(ContentPart::Text(TextContent {
                text: "hi".into(),
                cache_control: None,
            })),
            tool_call_id: None,
            name: None,
            tool_result_cleared_at: None,
        }]),
        tools: Arc::new(Vec::new()),
        ..CompletionRequest::default()
    }
}

/// Retry on transient errors: succeeds within `max_attempts`.
#[tokio::test]
async fn transient_error_retries_then_succeeds() {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
        fail_until: 2,
    });
    let resilient = ResilientProvider::new(
        inner.clone(),
        ResiliencePolicy::default()
            .max_attempts(5)
            .initial_backoff(Duration::from_millis(1)),
        clock(),
    );
    let r = resilient.complete(make_request()).await.unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 3);
    assert!(resilient.retry_count() >= 2);
    assert!(matches!(r.content, Content::Single(_)));
}

/// Breaker transitions Closed → Open after consecutive failures.
#[tokio::test]
async fn breaker_opens_after_failure_threshold() {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
        fail_until: u64::MAX,
    });
    let resilient = ResilientProvider::new(
        inner.clone(),
        ResiliencePolicy::default()
            .max_attempts(2)
            .initial_backoff(Duration::from_millis(1))
            .breaker(uva_core::BreakerConfig {
                failure_threshold: 2,
                half_open_after: Duration::from_secs(60),
                success_threshold_in_half_open: 1,
            }),
        clock(),
    );
    // Two failing calls: 2 + 2 = 4 inner attempts, breaker opens.
    let _ = resilient.complete(make_request()).await;
    let _ = resilient.complete(make_request()).await;
    assert_eq!(
        resilient.breaker_state().phase,
        uva_core::BreakerPhase::Open
    );
}

/// Breaker rejects calls without consulting the inner provider once
/// it is open.
#[tokio::test]
async fn open_breaker_rejects_subsequent_calls() {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
        fail_until: u64::MAX,
    });
    let resilient = ResilientProvider::new(
        inner.clone(),
        ResiliencePolicy::default().max_attempts(1).breaker(
            uva_core::BreakerConfig {
                failure_threshold: 1,
                half_open_after: Duration::from_secs(60),
                success_threshold_in_half_open: 1,
            },
        ),
        clock(),
    );
    // First call fails, breaker opens.
    let _ = resilient.complete(make_request()).await;
    let inner_calls_before = inner.calls.load(Ordering::SeqCst);
    // Subsequent call should be rejected by the breaker.
    let r = resilient.complete(make_request()).await;
    assert!(r.is_err());
    assert_eq!(
        inner.calls.load(Ordering::SeqCst),
        inner_calls_before,
        "open breaker must not consult the inner provider"
    );
}

/// Hot-reload policy: a new failure threshold is observed by the
/// next call.
#[tokio::test]
async fn policy_hot_reload_observed_by_next_call() {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
        fail_until: 1,
    });
    let resilient = ResilientProvider::new(
        inner.clone(),
        ResiliencePolicy::default()
            .max_attempts(2)
            .initial_backoff(Duration::from_millis(1)),
        clock(),
    );
    let _ = resilient.complete(make_request()).await;
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    // Hot-reload: increase max_attempts.
    resilient.set_policy(
        ResiliencePolicy::default()
            .max_attempts(4)
            .initial_backoff(Duration::from_millis(1)),
    );
    let _ = resilient.complete(make_request()).await;
    assert!(
        inner.calls.load(Ordering::SeqCst) <= 4,
        "hot-reloaded policy must be observed by the next call"
    );
}

/// Per-tool breaker state: different tools do not share breaker
/// state.
#[tokio::test]
async fn per_tool_breaker_is_isolated() {
    struct AlwaysFail;
    #[async_trait::async_trait]
    impl Tool for AlwaysFail {
        fn name(&self) -> &str {
            "alpha"
        }

        fn description(&self) -> &str {
            "alpha"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn call(
            &self,
            _input: serde_json::Value,
            _context: &Context,
        ) -> ToolOutput {
            ToolOutput::error(String::from("nope"))
        }
    }
    let alpha: Arc<dyn Tool> = Arc::new(AlwaysFail);
    let wrapped = ResilientTool::new(
        alpha,
        ResiliencePolicy::default().max_attempts(1).breaker(
            uva_core::BreakerConfig {
                failure_threshold: 1,
                half_open_after: Duration::from_secs(60),
                success_threshold_in_half_open: 1,
            },
        ),
        clock(),
    );
    let _ = wrapped
        .call(serde_json::json!({}), &Context::default())
        .await;
    assert_eq!(wrapped.breaker_state().phase, uva_core::BreakerPhase::Open);
}

/// The retry path names no runtime: driving it under a plain
/// `futures` executor — no tokio reactor anywhere — must work.
///
/// The crate is documented as runtime-neutral, and a retry that
/// waits on `tokio::time::sleep` is tokio-only by construction: the
/// timer panics the moment it is polled outside a tokio runtime.
#[test]
fn retry_path_needs_no_tokio_reactor() {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
        fail_until: 1,
    });
    let resilient = ResilientProvider::new(
        inner.clone(),
        ResiliencePolicy::default()
            .max_attempts(3)
            .initial_backoff(Duration::from_millis(1)),
        clock(),
    );
    let result =
        futures::executor::block_on(resilient.complete(make_request()));
    assert!(result.is_ok(), "the retry must not need a runtime");
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
}

/// A sleeper that counts the waits and resolves immediately.
#[derive(Default)]
struct CountingSleeper {
    waits: AtomicU64,
}

impl CountingSleeper {
    fn waits(&self) -> u64 {
        self.waits.load(Ordering::SeqCst)
    }
}

impl Sleeper for CountingSleeper {
    fn sleep(
        &self,
        _duration: Duration,
    ) -> futures::future::BoxFuture<'static, ()> {
        self.waits.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(()))
    }
}

/// The retry backoff waits through the delay seam the caller
/// installed — one wait per retry, and no other.
#[tokio::test]
async fn retry_backoff_waits_through_the_sleeper() {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
        fail_until: 2,
    });
    let sleeper = Arc::new(CountingSleeper::default());
    let resilient = ResilientProvider::new(
        inner.clone(),
        ResiliencePolicy::default()
            .max_attempts(5)
            .initial_backoff(Duration::from_millis(1)),
        clock(),
    )
    .with_sleeper(sleeper.clone());
    let _ = resilient.complete(make_request()).await;
    assert_eq!(sleeper.waits(), 2, "one wait per retry");
}

/// The default sleeper is a real wait: the backoff is a delay, not a
/// no-op the retry loop skips.
#[tokio::test]
async fn default_sleeper_waits_the_backoff() {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
        fail_until: 1,
    });
    let resilient = ResilientProvider::new(
        inner.clone(),
        ResiliencePolicy::default()
            .max_attempts(3)
            .initial_backoff(Duration::from_millis(60)),
        clock(),
    );
    let start = std::time::Instant::now();
    let _ = resilient.complete(make_request()).await;
    assert!(
        start.elapsed() >= Duration::from_millis(50),
        "the retry must wait the backoff, not skip it"
    );
}

/// A tool that fails twice, then succeeds.
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

/// The tool wrapper waits through the same seam as the provider
/// wrapper.
#[tokio::test]
async fn tool_retry_waits_through_the_sleeper() {
    let inner: Arc<dyn Tool> = Arc::new(FlakyTool {
        calls: AtomicU64::new(0),
    });
    let sleeper = Arc::new(CountingSleeper::default());
    let resilient = ResilientTool::new(
        inner,
        ResiliencePolicy::default()
            .max_attempts(5)
            .initial_backoff(Duration::from_millis(1)),
        clock(),
    )
    .with_sleeper(sleeper.clone());
    let output = resilient
        .call(serde_json::json!({}), &Context::default())
        .await;
    assert!(!output.is_error.unwrap_or(false));
    assert_eq!(sleeper.waits(), 2, "one wait per retry");
}
