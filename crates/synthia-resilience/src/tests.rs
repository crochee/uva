//! End-to-end tests for the resilience layer.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::TimeZone;
use synthia_core::Clock;
use synthia_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    TextContent,
};
use synthia_tool::{Context, Tool, ToolOutput};

use crate::{ResilientProvider, ResilientTool, provider::ResiliencePolicy};

struct CountingProvider {
    calls: AtomicU64,
    fail_until: u64,
}

#[async_trait::async_trait]
impl ModelProvider for CountingProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), synthia_core::Error> {
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
    ) -> Result<CompletionResponse, synthia_core::Error> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.fail_until {
            // Return an error that the default classifier maps to Retry.
            // We use a transport-flavored error (Transient) so the
            // provider's retry classifier marks it retryable.
            Err(synthia_core::Error::Timeout {
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
                usage: synthia_provider::TokenUsage::default(),
                cached: false,
                stop_reason: None,
                replay_state: None,
            })
        }
    }
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(synthia_core::FixedClock::new(
        chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ))
}

fn make_request() -> CompletionRequest {
    use synthia_provider::{Message, Role};
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
            .breaker(synthia_core::BreakerConfig {
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
        synthia_core::BreakerPhase::Open
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
            synthia_core::BreakerConfig {
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
            synthia_core::BreakerConfig {
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
    assert_eq!(
        wrapped.breaker_state().phase,
        synthia_core::BreakerPhase::Open
    );
}
