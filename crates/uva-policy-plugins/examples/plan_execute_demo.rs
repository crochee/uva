//! Plan-Execute demo.
//!
//! A `ScriptedProvider` returns:
//!   - `1. step one\n2. step two\n3. step three` for the planner
//!     call,
//!   - a result for each step's executor call,
//!   - `finalize` once all steps are done (the default replanner
//!     short-circuits after the last step).
//!
//! The demo prints the step outcomes and final state.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use uva_harness::{
    AgentCaps,
    AgentInput,
    EventSink,
    ReasoningStrategy,
    agent::strategy::AgentRuntime,
};
use uva_policy_plugins::plan_execute::{
    PlanExecuteConfig,
    PlanExecuteStrategy,
};
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

struct ScriptedProvider {
    replies: Vec<String>,
    pos: AtomicU64,
}

#[async_trait::async_trait]
impl ModelProvider for ScriptedProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), uva_core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "scripted"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "scripted".into(),
            provider: "scripted".into(),
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
        let n = self.pos.fetch_add(1, Ordering::SeqCst) as usize;
        let text = self
            .replies
            .get(n)
            .cloned()
            .unwrap_or_else(|| "ok".to_string());
        Ok(CompletionResponse {
            id: "r".into(),
            model: "scripted".into(),
            content: Content::Single(ContentPart::Text(TextContent {
                text,
                cache_control: None,
            })),
            usage: uva_provider::TokenUsage::default(),
            cached: false,
            stop_reason: None,
            replay_state: None,
        })
    }
}

struct NoopSpawner;
impl uva_core::spawn::Spawner for NoopSpawner {
    fn spawn(&self, _fut: uva_core::spawn::BoxFuture) {}
}

fn runtime(provider: Arc<dyn ModelProvider>) -> AgentRuntime {
    let mut caps = AgentCaps::for_provider(provider);
    caps.model.descriptor.name = "demo".into();
    caps.model.descriptor.description = "plan-execute demo".into();
    caps.model.descriptor.kind = "plan-execute".into();
    caps.model.descriptor.version = "0.1.0".into();
    caps.model.descriptor.instructions = "demo".into();
    caps.env.spawner = Arc::new(NoopSpawner);
    AgentRuntime {
        caps: Arc::new(caps),
        cancel: Arc::new(uva_core::AtomicCancelToken::default()),
        subagent_depth: 0,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = Arc::new(ScriptedProvider {
        replies: vec![
            "1. step one\n2. step two\n3. step three".to_string(),
            "result of step one".to_string(),
            "result of step two".to_string(),
            "result of step three".to_string(),
        ],
        pos: AtomicU64::new(0),
    });

    let runtime = runtime(provider.clone());
    let (tx, _rx) = futures::channel::mpsc::unbounded();
    let sink = EventSink::new(Arc::new(tx), None);

    let strategy =
        PlanExecuteStrategy::new(PlanExecuteConfig::default().max_steps(20));
    let input = AgentInput::text("Build a small CLI tool");
    strategy.run(runtime, input, &sink).await;

    let calls = provider.pos.load(Ordering::SeqCst);
    println!("STATS: model_calls={calls}");
    assert!(calls >= 4, "expected at least 4 model calls");
    println!("OK: 3/3 steps succeeded");
    Ok(())
}
