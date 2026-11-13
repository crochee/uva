//! Tests for `uva-budget`.

use std::{sync::Arc, time::Duration};

use chrono::TimeZone;
use uva_core::FixedClock;
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    TextContent,
    TokenUsage,
};
use uva_tool::{Context, Tool, ToolOutput};

use crate::budget::{
    model::{Budget, BudgetKind, CostBudget, LatencyBudget, TokenBudget},
    pricing::{Direction, PricingTable, StaticPricingTable},
    provider::BudgetProvider,
    tool::BudgetTool,
};

struct CountingProvider {
    calls: Arc<std::sync::atomic::AtomicU64>,
    usage: TokenUsage,
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
            name: "claude-3-haiku".into(),
            provider: "test".into(),
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
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(CompletionResponse {
            id: "r".into(),
            model: "claude-3-haiku".into(),
            content: Content::Single(ContentPart::Text(TextContent {
                text: "ok".into(),
                cache_control: None,
            })),
            usage: self.usage.clone(),
            cached: false,
            stop_reason: None,
            replay_state: None,
        })
    }
}

#[allow(dead_code)]
fn fixed_clock(_t0: i64) -> Arc<dyn uva_core::Clock> {
    Arc::new(FixedClock::new(chrono::Utc.timestamp_opt(0, 0).unwrap()))
}

/// TokenBudget exceeded → next call returns error.
#[tokio::test]
async fn token_budget_exceeded_returns_error() {
    let inner = Arc::new(CountingProvider {
        calls: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        usage: TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 100,
            ..TokenUsage::default()
        },
    });
    let budget = Budget::builder()
        .tokens(TokenBudget::new(50)) // 50 token limit; first call uses 200
        .build();
    let wrapper = BudgetProvider::new(inner.clone(), budget);
    let req = CompletionRequest::default();
    let r = wrapper.complete(req).await;
    assert!(r.is_err());
    assert!(r.unwrap_err().to_string().contains("budget"));
}

/// Latency budget exceeded → next call returns error (uses
/// FixedClock to advance time).
#[tokio::test]
async fn latency_budget_exceeded_returns_error() {
    let inner = Arc::new(CountingProvider {
        calls: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        usage: TokenUsage::default(),
    });
    let t0 = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let clock0: Arc<dyn uva_core::Clock> = Arc::new(FixedClock::new(t0));
    let mut latency =
        LatencyBudget::new(Duration::from_secs(60), clock0.clone());
    let budget = Budget::builder().latency(latency.clone()).build();
    let wrapper = BudgetProvider::new(inner.clone(), budget);
    // Advance the wall clock past the limit.
    let t1 = t0 + chrono::Duration::seconds(120);
    let clock1: Arc<dyn uva_core::Clock> = Arc::new(FixedClock::new(t1));
    latency.clock = clock1;
    wrapper.set_budget(Budget::builder().latency(latency).build());
    let req = CompletionRequest::default();
    let r = wrapper.complete(req).await;
    assert!(r.is_err(), "latency budget should trip");
}

/// Cost budget exceeded → next call returns error.
#[tokio::test]
async fn cost_budget_exceeded_returns_error() {
    let inner = Arc::new(CountingProvider {
        calls: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        // 1000 input tokens at $0.25/1k → 25 cents; 1000 output at
        // $1.25/1k → 125 cents. Total: 150 cents.
        usage: TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 1000,
            ..TokenUsage::default()
        },
    });
    let pricing: Arc<dyn PricingTable> = Arc::new(
        StaticPricingTable::new()
            .with_rate("claude-3-haiku", Direction::Input, 25)
            .with_rate("claude-3-haiku", Direction::Output, 125),
    );
    let budget = Budget::builder()
        .cost(CostBudget::new(100, pricing)) // 100 cents limit
        .build();
    let wrapper = BudgetProvider::new(inner.clone(), budget);
    let req = CompletionRequest::default();
    let r = wrapper.complete(req).await;
    assert!(r.is_err(), "cost budget should trip");
}

/// Outcome::HardStop returns error without invoking the inner.
#[tokio::test]
async fn hard_stop_does_not_invoke_inner() {
    let inner = Arc::new(CountingProvider {
        calls: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        usage: TokenUsage::default(),
    });
    let budget = Budget::builder()
        .tokens(TokenBudget::new(0)) // zero limit → exceeded from the start
        .on_exceeded(crate::budget::BudgetOutcome::HardStop)
        .build();
    let wrapper = BudgetProvider::new(inner.clone(), budget);
    let req = CompletionRequest::default();
    let r = wrapper.complete(req).await;
    assert!(r.is_err());
    assert_eq!(
        inner.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "hard stop must reject before consulting the inner"
    );
}

/// Hot-reload: new limit observed by next call.
#[tokio::test]
async fn budget_hot_reload_is_observed() {
    let inner = Arc::new(CountingProvider {
        calls: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        usage: TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 100,
            ..TokenUsage::default()
        },
    });
    let wrapper = BudgetProvider::new(
        inner.clone(),
        Budget::builder().tokens(TokenBudget::new(50)).build(),
    );
    // First call exceeds.
    assert!(
        wrapper
            .complete(CompletionRequest::default())
            .await
            .is_err()
    );
    // Hot-reload to a generous limit.
    wrapper.set_budget(
        Budget::builder()
            .tokens(TokenBudget::new(1_000_000))
            .build(),
    );
    let r = wrapper.complete(CompletionRequest::default()).await;
    assert!(r.is_ok(), "hot-reloaded budget should allow the next call");
}

/// BudgetTool wrapper returns an error output on trip.
#[tokio::test]
async fn budget_tool_returns_error_output_on_trip() {
    struct AlwaysOk;
    #[async_trait::async_trait]
    impl Tool for AlwaysOk {
        fn name(&self) -> &str {
            "always_ok"
        }

        fn description(&self) -> &str {
            "ok"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn call(
            &self,
            _input: serde_json::Value,
            _context: &Context,
        ) -> ToolOutput {
            ToolOutput::text("ok")
        }
    }
    let inner: Arc<dyn Tool> = Arc::new(AlwaysOk);
    let wrapper = BudgetTool::new(
        inner,
        Budget::builder().tokens(TokenBudget::new(0)).build(),
    );
    let r = wrapper
        .call(serde_json::json!({}), &Context::default())
        .await;
    assert!(r.is_error.unwrap_or(false));
}

/// Unpriced model logs model_unpriced and skips cost gate.
#[tokio::test]
async fn unpriced_model_skips_cost_gate() {
    use std::sync::atomic::AtomicU64;

    use tracing_subscriber;
    static WARN_COUNT: AtomicU64 = AtomicU64::new(0);
    let _ = tracing_subscriber::fmt::try_init();
    let inner = Arc::new(CountingProvider {
        calls: Arc::new(AtomicU64::new(0)),
        usage: TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 100,
            ..TokenUsage::default()
        },
    });
    // Pricing has no rates for "unknown-model".
    let pricing: Arc<dyn PricingTable> = Arc::new(StaticPricingTable::new());
    let budget = Budget::builder()
        .cost(CostBudget::new(1, pricing)) // 1 cent limit
        .build();
    let wrapper = BudgetProvider::new(inner.clone(), budget);
    let r = wrapper.complete(CompletionRequest::default()).await;
    assert!(r.is_ok(), "unpriced model should skip the cost gate");
    let _ = WARN_COUNT; // silence unused import warning.
}

/// BudgetExceeded Display formatting is human-readable.
#[test]
fn budget_exceeded_display_is_human_readable() {
    let e = crate::budget::BudgetExceeded {
        kind: BudgetKind::Tokens,
        used: 200,
        limit: 100,
    };
    let s = format!("{e}");
    assert!(s.contains("Tokens"));
    assert!(s.contains("200"));
    assert!(s.contains("100"));
}
