//! # Chain-of-Thought — a one-pass, tool-free reasoning strategy
//!
//! Run it:
//!
//! ```text
//! cargo run --example chain_of_thought -p synthia-strategy-cot
//! ```
//!
//! Expected tail: `COT: OK`
//!
//! The same agent builder, the same tool registry, the same
//! `ReasoningStrategy` seam — this strategy asks the model to
//! reason through numbered steps before answering, and publishes
//! the stream live. It advertises no tools; a model that tries to
//! call one anyway gets a warning (see `WarningKind::Loop`).

use std::sync::Arc;

use futures::StreamExt;
use synthia_core::{AtomicCancelToken, CancelToken, Error};
use synthia_harness::{
    Agent,
    AgentEvent,
    AgentInput,
    ReActAgent,
    ReasoningStrategy,
};
use synthia_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    StreamChunk,
    TextContent,
};
use synthia_strategy_cot::ChainOfThoughtStrategy;

/// A scripted provider that answers a single streaming call with
/// a three-step chain plus a final answer, matching the format
/// the strategy's instruction asks for.
struct ScriptedProvider;

#[async_trait::async_trait]
impl ModelProvider for ScriptedProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "scripted"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "scripted-1".to_string(),
            provider: "scripted".to_string(),
            context_window: 8_192,
            max_output_tokens: 1_024,
            supports_tools: false,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        Ok(CompletionResponse {
            content: Content::Single(synthia_provider::ContentPart::Text(
                TextContent {
                    text: COT_ANSWER.to_string(),
                    cache_control: None,
                },
            )),
            ..Default::default()
        })
    }

    async fn complete_with_stream(
        &self,
        _request: CompletionRequest,
        _cancel: Option<Arc<dyn CancelToken>>,
        mut on_chunk: Box<dyn FnMut(StreamChunk) + Send + 'static>,
    ) -> Result<CompletionResponse, Error> {
        // One content delta with the whole answer, then the
        // completion result — the shape a real stream ends with.
        on_chunk(StreamChunk::Content(synthia_provider::ContentPart::Text(
            TextContent {
                text: COT_ANSWER.to_string(),
                cache_control: None,
            },
        )));
        let response = self.complete(CompletionRequest::default()).await?;
        on_chunk(StreamChunk::IsDone {
            result: Box::new(synthia_provider::SamplingResult {
                text: COT_ANSWER.to_string(),
                ..Default::default()
            }),
        });
        Ok(response)
    }
}

const COT_ANSWER: &str = "Step 1: Parse the question.\n\
                          Step 2: Recall the fact.\n\
                          Step 3: State it.\n\
                          Answer: 42";

#[tokio::main]
async fn main() {
    let provider = Arc::new(ScriptedProvider);
    let registry = synthia_tool::ToolRegistry::new();
    let strategy = Arc::new(ChainOfThoughtStrategy::new(3));

    let agent = ReActAgent::new(provider, Arc::new(registry))
        .with_strategy(Arc::clone(&strategy) as Arc<dyn ReasoningStrategy>);

    let cancel: Arc<dyn CancelToken> = AtomicCancelToken::shared();
    let mut stream = agent
        .run(AgentInput::text("meaning of life?"), cancel)
        .await;

    let mut answer = String::new();
    let mut ended = false;
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::Model(part) => {
                if let Some(text) = part.text() {
                    answer.push_str(text);
                }
            }
            AgentEvent::System(
                synthia_harness::SystemEvent::SessionEnded { .. },
            ) => {
                ended = true;
            }
            _ => {}
        }
    }

    assert!(ended, "session must end");
    let steps = ChainOfThoughtStrategy::parse_steps(&answer).len();
    assert_eq!(steps, 3, "answer: {answer:?}");
    assert!(answer.contains("Answer: 42"), "answer: {answer:?}");
    println!("COT: OK");
}
