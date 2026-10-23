//! Tier-1 provider-wrap composition
//! (`semantic-cache → resilience → budget`).
//!
//! The boot composes the three wrappers onto the default provider in
//! inner-to-outer order. These tests pin the composition shape:
//!
//! - all-off ⇒ the wrapper is a transparent pass-through (same
//!   `name()`, no extra allocation on the hot path).
//! - cache on ⇒ the second identical call is served from the cache
//!   (`cached == true`, inner call count stays at 1).
//! - budget on ⇒ the first call exceeds the cap and the wrapper
//!   reports the trip.
//!
//! The helpers live in `boot.rs`; we exercise them through their
//! `pub(crate)` re-export.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use synthia::provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    TextContent,
};

/// In-process fake that counts `complete` calls.
struct CountingProvider {
    calls: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl ModelProvider for CountingProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), synthia::core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "counting"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "counting".into(),
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
    ) -> Result<CompletionResponse, synthia::core::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(CompletionResponse {
            id: "r".into(),
            model: "counting".into(),
            content: Content::Single(ContentPart::Text(TextContent {
                text: "ok".into(),
                cache_control: None,
            })),
            usage: synthia::provider::TokenUsage::default(),
            cached: false,
            stop_reason: None,
            replay_state: None,
        })
    }
}

fn request() -> CompletionRequest {
    use synthia::provider::{Message, Role};
    CompletionRequest {
        model: "counting".into(),
        messages: Arc::new(vec![Message {
            role: Role::User,
            content: Content::Single(ContentPart::Text(TextContent {
                text: "hello".into(),
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

/// All wrappers off ⇒ transparent pass-through: same `name()`,
/// every call goes through.
#[tokio::test]
async fn tier1_all_off_is_passthrough() {
    let calls = Arc::new(AtomicU64::new(0));
    let inner: Arc<dyn ModelProvider> = Arc::new(CountingProvider {
        calls: calls.clone(),
    });
    let cfg = crate::config::ServerConfig::default();
    let clock = synthia::core::SharedClock::system();
    let wrapped =
        crate::state::app_state::wrap_tier1_providers(inner, Some(&cfg), clock);
    assert_eq!(wrapped.name(), "counting");
    let _ = wrapped.complete(request()).await.unwrap();
    let _ = wrapped.complete(request()).await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "no wrapper installed ⇒ both calls reach the inner provider"
    );
}

/// Cache on ⇒ second identical call is served from the cache and the
/// inner provider is only called once.
#[tokio::test]
async fn tier1_cache_serves_second_identical_call() {
    let calls = Arc::new(AtomicU64::new(0));
    let inner: Arc<dyn ModelProvider> = Arc::new(CountingProvider {
        calls: calls.clone(),
    });
    let mut cfg = crate::config::ServerConfig::default();
    cfg.cache.enabled = true;
    cfg.cache.threshold = 0.99;
    let clock = synthia::core::SharedClock::system();
    let wrapped =
        crate::state::app_state::wrap_tier1_providers(inner, Some(&cfg), clock);
    let first = wrapped.complete(request()).await.unwrap();
    let second = wrapped.complete(request()).await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "cache must short-circuit the second identical request"
    );
    assert!(!first.cached, "first response is a fresh inner call");
    assert!(second.cached, "second response is cache-served");
}

/// Budget on ⇒ the first call trips the cap.
#[tokio::test]
async fn tier1_budget_trips() {
    let calls = Arc::new(AtomicU64::new(0));
    let inner: Arc<dyn ModelProvider> = Arc::new(CountingProvider {
        calls: calls.clone(),
    });
    let mut cfg = crate::config::ServerConfig::default();
    cfg.budget.enabled = true;
    cfg.budget.tokens_limit = 0; // exceeded from the start
    let clock = synthia::core::SharedClock::system();
    let wrapped =
        crate::state::app_state::wrap_tier1_providers(inner, Some(&cfg), clock);
    let r = wrapped.complete(request()).await;
    assert!(r.is_err(), "budget must reject the call");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "budget rejects before consulting the inner provider"
    );
}
