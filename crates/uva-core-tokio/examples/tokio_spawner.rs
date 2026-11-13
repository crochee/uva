//! # Bridge the agent loop onto a tokio executor
//!
//! ```bash
//! cargo run -p uva-core-tokio --example tokio_spawner
//! ```
//!
//! Expected tail: `TOKIO-SPAWNER: OK`
//!
//! The harness crate itself is runtime-neutral: its
//! `[dependencies]` carries zero tokio, and its public API exposes
//! only the [`Spawner`](uva_core::spawn::Spawner) trait.
//! Consumers on a tokio runtime depend on this companion crate and
//! install [`TokioSpawner`] via `ReActAgent::with_spawner`. This
//! example spins a scripted agent on `#[tokio::main]` (multi-thread)
//! and proves the wiring compiles end to end.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use futures::StreamExt;
use uva_core::{AtomicCancelToken, CancelToken, Error};
use uva_core_tokio::TokioSpawner;
use uva_harness::{Agent, AgentInput, ReActAgent};
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
};
use uva_tool::ToolRegistry;

/// Scripted provider: emits a single final answer so the loop
/// exits after one iteration. The example proves the spawner
/// installs, not that the model is interesting.
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
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        Ok(CompletionResponse {
            content: Content::text("hello from a tokio-spawned turn"),
            ..CompletionResponse::default()
        })
    }
}

#[tokio::main]
async fn main() {
    let provider = Arc::new(ScriptedProvider);
    let registry: Arc<ToolRegistry> = Arc::new(ToolRegistry::new());

    // Install the tokio spawner at build time; `run` itself takes
    // only `(input, cancel)`.
    let agent = ReActAgent::new(provider, registry)
        .with_spawner(Arc::new(TokioSpawner));

    let cancel: Arc<dyn CancelToken> = Arc::new(AtomicCancelToken::new());

    // `agent.run` is async — `await` it once to obtain the event
    // stream, then poll the stream.
    let mut stream = agent.run(AgentInput::text("hello"), cancel).await;

    let events = Arc::new(AtomicUsize::new(0));
    while let Some(_event) = stream.next().await {
        events.fetch_add(1, Ordering::SeqCst);
    }

    assert!(events.load(Ordering::SeqCst) >= 1);
    println!("TOKIO-SPAWNER: OK");
}
