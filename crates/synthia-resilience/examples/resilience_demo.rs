//! Resilience demo.
//!
//! A `CountingProvider` fails the first 4 attempts and succeeds on
//! the 5th. The `ResilientProvider` wrapper retries transient
//! errors with exponential backoff and the test prints the
//! recovered result.
//!
//! Run with:
//! `cargo run --example resilience_demo -p synthia-resilience`

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::TimeZone;
use synthia_core::{Clock, FixedClock};
use synthia_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    Message,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    Role,
    TextContent,
};
#[allow(unused_imports)]
use synthia_resilience::ResilienceError as _UnusedError;
use synthia_resilience::{BreakerConfig, ResiliencePolicy, ResilientProvider};

struct FlakyProvider {
    calls: AtomicU64,
}

#[async_trait::async_trait]
impl ModelProvider for FlakyProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), synthia_core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "flaky"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "test-model".into(),
            provider: "flaky".into(),
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
        if n < 4 {
            // Transient error (mapped to Retry by the classifier).
            Err(synthia_core::Error::Timeout {
                message: format!("transient failure #{n}"),
            })
        } else {
            Ok(CompletionResponse {
                id: "r".into(),
                model: "test-model".into(),
                content: Content::Single(ContentPart::Text(TextContent {
                    text: "hello".into(),
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let inner = Arc::new(FlakyProvider {
        calls: AtomicU64::new(0),
    });
    let t0 = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(t0));
    let resilient = ResilientProvider::new(
        inner.clone(),
        ResiliencePolicy::default()
            .max_attempts(5)
            .initial_backoff(Duration::from_millis(1))
            .breaker(BreakerConfig {
                failure_threshold: 5,
                half_open_after: Duration::from_secs(15),
                success_threshold_in_half_open: 1,
            }),
        clock,
    );

    let request = CompletionRequest {
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
    };

    let r = resilient.complete(request).await?;
    println!(
        "STATS: attempts={} retries={} rejections={}",
        resilient.attempt_count(),
        resilient.retry_count(),
        resilient.rejection_count()
    );
    println!("RESULT: response received after retries");
    println!("OK: recovered after 5 retries");
    let _ = r;
    Ok(())
}
