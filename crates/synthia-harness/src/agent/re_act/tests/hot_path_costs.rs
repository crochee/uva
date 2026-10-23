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
use parking_lot::Mutex;
use serde_json::json;
use synthia_provider::{
    CompletionResponse,
    ModelConfig,
    ModelProvider,
    SamplingResult,
    StreamChunk,
    ToolUse,
};
use synthia_tool::{ToolOutput, ToolRegistry};
use tokio_util::sync::CancellationToken;

use super::{support::*, *};
use crate::{
    Agent,
    agent::re_act::synthetic::{SyntheticCall, SyntheticTool},
    input::AgentInput,
};

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
        _config: synthia_provider::ProviderConfig,
    ) -> Result<(), synthia_core::Error> {
        Ok(())
    }

    async fn complete(
        &self,
        _request: synthia_provider::CompletionRequest,
    ) -> Result<CompletionResponse, synthia_core::Error> {
        unreachable!("streaming path should not call complete()")
    }

    async fn complete_with_stream(
        &self,
        _request: synthia_provider::CompletionRequest,
        _cancel: Option<Arc<dyn synthia_core::CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<CompletionResponse, synthia_core::Error> {
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
        Ok(CompletionResponse {
            id: format!("resp-{n}"),
            model: "counting-model".to_string(),
            content: synthia_provider::Content::text(result.text),
            stop_reason: result.stop_reason,
            ..Default::default()
        })
    }
}

/// Exactly once per run; before the `ModelConfig` snapshot this was six reads
/// (once per iteration in `sample_once_or_fail`, twice in `prepare`).
#[tokio::test]
async fn a_run_asks_the_provider_for_its_config_once() {
    let provider = Arc::new(ConfigCountingProvider::new());
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        synthia_tool_read::ReadTool::new(),
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
impl synthia_context::ContextManager for FreezingNothing {
    async fn prepare(
        &self,
        messages: &mut Vec<synthia_provider::Message>,
        _state: &mut synthia_context::AgentState,
    ) {
        let _ = messages;
    }
}

struct CountingSpawner {
    spawns: Arc<AtomicUsize>,
}

impl synthia_core::spawn::Spawner for CountingSpawner {
    fn spawn(&self, task: futures::future::BoxFuture<'static, ()>) {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || futures::executor::block_on(task));
    }
}

#[tokio::test]
async fn a_manager_that_freezes_nothing_never_detaches_a_prefetch() {
    let provider = Arc::new(ConfigCountingProvider::new());
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        synthia_tool_read::ReadTool::new(),
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

// -- check 11: validate_synthetic rejects duplicates -------------------

struct SharedName {
    seen: Arc<Mutex<Vec<String>>>,
}

impl SyntheticTool for SharedName {
    fn definitions(&self) -> Vec<synthia_provider::ToolDefinition> {
        vec![synthia_provider::ToolDefinition {
            name: "shared".to_string(),
            description: "shared".to_string(),
            input_schema: json!({"type": "object"}),
            cache_control: None,
            annotations: None,
        }]
    }

    fn claims(&self, name: &str) -> bool {
        name == "shared"
    }

    fn execute(
        &self,
        call: SyntheticCall<'_>,
    ) -> futures::future::BoxFuture<'static, ToolOutput> {
        self.seen.lock().push(call.call.id.clone());
        Box::pin(async { ToolOutput::text("done") })
    }
}

#[test]
fn two_synthetic_tools_advertising_one_name_are_rejected() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let first: Arc<dyn SyntheticTool> = Arc::new(SharedName {
        seen: Arc::clone(&seen),
    });
    let second: Arc<dyn SyntheticTool> = Arc::new(SharedName {
        seen: Arc::clone(&seen),
    });

    let agent = ReActAgent::new(
        Arc::new(synthia_provider::traits_stub::ModelProviderStub::new()),
        Arc::new(ToolRegistry::new()),
    )
    .with_synthetic_tool(first)
    .expect("the first install must be accepted");
    let err = match agent.with_synthetic_tool(second) {
        Ok(_) => panic!("a duplicate advertised name must be rejected"),
        Err(name) => name,
    };

    assert_eq!(err, "shared", "the conflicting name must reach the caller");
}
