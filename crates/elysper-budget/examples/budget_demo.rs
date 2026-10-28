//! Budget demo.
//!
//! A `CountingProvider` reports 1500 input tokens and 1500 output
//! tokens per call (a real Anthropic Sonnet-equivalent pricing
//! budget). The wrapper enforces a 1000-token cap and the demo
//! prints the budget trip.
//!
//! Run with:
//! `cargo run --example budget_demo -p elysper-budget`

use std::sync::Arc;

use elysper_budget::{
    Budget,
    BudgetProvider,
    PricingTable,
    StaticPricingTable,
    TokenBudget,
    pricing::Direction,
};
use elysper_provider::{
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

struct CountingProvider;

#[async_trait::async_trait]
impl ModelProvider for CountingProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), elysper_core::Error> {
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
    ) -> Result<CompletionResponse, elysper_core::Error> {
        Ok(CompletionResponse {
            id: "r".into(),
            model: "claude-3-haiku".into(),
            content: Content::Single(ContentPart::Text(TextContent {
                text: "hi".into(),
                cache_control: None,
            })),
            usage: TokenUsage {
                prompt_tokens: 1500,
                completion_tokens: 1500,
                ..TokenUsage::default()
            },
            cached: false,
            stop_reason: None,
            replay_state: None,
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let inner: Arc<dyn ModelProvider> = Arc::new(CountingProvider);
    let _pricing: Arc<dyn PricingTable> = Arc::new(
        StaticPricingTable::new()
            .with_rate("claude-3-haiku", Direction::Input, 25)
            .with_rate("claude-3-haiku", Direction::Output, 125),
    );
    let budget = Budget::builder().tokens(TokenBudget::new(2_000)).build();
    let wrapper = BudgetProvider::new(inner, budget);

    let req = CompletionRequest::default();
    // First call uses 3000 tokens, exceeds the 2000 cap.
    let r = wrapper.complete(req.clone()).await;
    println!("first call result: {:?}", r.as_ref().map(|r| r.id.clone()));
    println!("trip_count={}", wrapper.trip_count());
    assert!(r.is_err(), "budget should trip on the first call");
    assert_eq!(
        wrapper.trip_count(),
        1,
        "trip count is bumped exactly once per exceeded call"
    );
    println!("OK: budget trip respected");
    Ok(())
}
