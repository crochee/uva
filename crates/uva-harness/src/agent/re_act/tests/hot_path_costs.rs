//! Per-run and per-iteration cost regressions.
//!
//! Each test here pins an allocation or a `dyn` call the loop
//! used to pay on every run, so a later round that reintroduces
//! one fails loudly instead of silently doubling the cost of a
//! turn.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uva_provider::{
    CompletionResponse,
    ModelConfig,
    ModelProvider,
    SamplingResult,
    StreamChunk,
    ToolUse,
};
use uva_tool::ToolRegistry;

use super::{support::*, *};
use crate::{Agent, input::AgentInput};

// -- check 6: one provider-config read per run -------------------------

struct ConfigCountingProvider {
    config_reads: AtomicUsize,
    iteration: AtomicUsize,
}

impl ConfigCountingProvider {
    fn new() -> Self {
        Self {
            config_reads: AtomicUsize::new(0),
            iteration: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ModelProvider for ConfigCountingProvider {
    fn name(&self) -> &str {
        "counting"
    }

    fn model_config(&self) -> ModelConfig {
        self.config_reads.fetch_add(1, Ordering::SeqCst);
        ModelConfig {
            name: "counting-model".to_string(),
            provider: "counting".to_string(),
            context_window: 32_000,
            max_output_tokens: 1024,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn initialize(
        &mut self,
        _config: uva_provider::ProviderConfig,
    ) -> Result<(), uva_core::Error> {
        Ok(())
    }

    async fn complete(
        &self,
        _request: uva_provider::CompletionRequest,
    ) -> Result<CompletionResponse, uva_core::Error> {
        unreachable!("streaming path should not call complete()")
    }

    async fn complete_with_stream(
        &self,
        _request: uva_provider::CompletionRequest,
        _cancel: Option<Arc<dyn uva_core::CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<SamplingResult, uva_core::Error> {
        let n = self.iteration.fetch_add(1, Ordering::SeqCst);
        let result = if n < 2 {
            SamplingResult {
                tool_calls: vec![ToolUse {
                    id: format!("call-{n}"),
                    name: "read".to_string(),
                    input: json!({"path": "notes.txt"}),
                }],
                ..Default::default()
            }
        } else {
            SamplingResult {
                text: "done".to_string(),
                ..Default::default()
            }
        };
        for call in &result.tool_calls {
            on_delta(StreamChunk::ToolCallStart {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: serde_json::Value::String(
                    serde_json::to_string(&call.input).unwrap_or_default(),
                ),
            });
            on_delta(StreamChunk::ToolCallEnd {
                id: call.id.clone(),
            });
        }
        on_delta(StreamChunk::IsDone {
            result: Box::new(result.clone()),
        });
        Ok(result)
    }
}

// Exactly once per run; before the `ModelConfig` snapshot this was six reads
// (once per iteration in `sample_once_or_fail`, twice in `prepare`).
#[tokio::test]
async fn a_run_asks_the_provider_for_its_config_once() {
    let provider = Arc::new(ConfigCountingProvider::new());
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(
        uva_tool_plugins::read::ReadTool::new(),
    )));
    let events = run_and_collect(
        provider.clone(),
        registry,
        CancellationToken::new(),
        AgentInput::text("go"),
    )
    .await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Completed
            })
        )),
        "the run must complete; events were: {events:#?}"
    );
    assert_eq!(
        provider.config_reads.load(Ordering::SeqCst),
        1,
        "one run must cost exactly one provider config read"
    );
}

// -- check 7: no prefetch allocation for a manager that freezes nothing

struct FreezingNothing;

#[async_trait]
impl uva_context::ContextManager for FreezingNothing {
    async fn prepare(
        &self,
        messages: &mut Vec<uva_provider::Message>,
        _state: &mut uva_context::AgentState,
    ) {
        let _ = messages;
    }
}

struct CountingSpawner {
    spawns: Arc<AtomicUsize>,
}

impl uva_core::spawn::Spawner for CountingSpawner {
    fn spawn(&self, task: futures::future::BoxFuture<'static, ()>) {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || futures::executor::block_on(task));
    }
}

#[tokio::test]
async fn a_manager_that_freezes_nothing_never_detaches_a_prefetch() {
    let provider = Arc::new(ConfigCountingProvider::new());
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(
        uva_tool_plugins::read::ReadTool::new(),
    )));
    let spawns = Arc::new(AtomicUsize::new(0));
    let agent = ReActAgent::new(provider, registry)
        .with_context_manager(Arc::new(FreezingNothing))
        .with_spawner(Arc::new(CountingSpawner {
            spawns: Arc::clone(&spawns),
        }));

    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Completed
            })
        )),
        "the run must complete; events were: {events:#?}"
    );
    assert_eq!(
        spawns.load(Ordering::SeqCst),
        1,
        "only the run's own strategy task may be detached; an empty \
         frozen slice must return before the prefetch spawns one \
         more (got {spawns:?} spawns)",
    );
}
