//! Shared fixtures for the [`super`] test suite.
//!
//! Every test submodule pulls from this file; the per-section
//! helpers (`tools_with`, `build_panel_fixture`,
//! `run_and_collect_with_restriction`, …) live with the test
//! that uses them, not here. Only the cross-section fixtures
//! live in one place so the test bodies stay byte-identical
//! to the previous monolithic `tests.rs`.
//!
//! [`super`]: crate::agent::re_act

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use elysper_context::AgentState;
use elysper_core::{CancelToken, Error};
use elysper_provider::{
    CompletionResponse,
    ContentPart,
    ProviderConfig,
    SamplingResult,
    StreamChunk,
    TokenUsage,
    ToolDefinition,
    ToolUse,
    traits::ModelProvider,
    types::ModelConfig,
};
use elysper_steering::{GuardResult, GuardSeverity};
use elysper_tool::{Context, ToolOutput, ToolRegistry};
use futures::StreamExt;
use serde_json::json;
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

use super::*;

// -- ScriptedStreamProvider -------------------------------------------

/// Test provider that emits a pre-scripted sequence of
/// `StreamChunk`s per `complete_with_stream` call.
#[derive(Debug)]
pub(super) struct ScriptedStreamProvider {
    scripted: Arc<TokioMutex<Vec<Vec<StreamChunk>>>>,
    call_count: Arc<AtomicUsize>,
}

impl ScriptedStreamProvider {
    pub(super) fn new(scripted: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            scripted: Arc::new(TokioMutex::new(scripted)),
            call_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    async fn take_next(&self) -> Vec<StreamChunk> {
        let mut guard = self.scripted.lock().await;
        if guard.is_empty() {
            return vec![StreamChunk::IsDone {
                result: Box::new(SamplingResult::default()),
            }];
        }
        guard.remove(0)
    }
}

#[async_trait]
impl ModelProvider for ScriptedStreamProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "scripted-stream"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "fake".to_string(),
            provider: "scripted".to_string(),
            context_window: 128_000,
            max_output_tokens: 4096,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
        }
    }

    async fn complete(
        &self,
        _request: elysper_provider::CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        unreachable!("streaming path should not call complete()")
    }

    async fn complete_with_stream(
        &self,
        _request: elysper_provider::CompletionRequest,
        _cancel_token: Option<Arc<dyn CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<CompletionResponse, Error> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let chunks = self.take_next().await;
        let mut final_sampling: Option<SamplingResult> = None;
        for chunk in chunks {
            if let StreamChunk::IsDone { result } = &chunk {
                final_sampling = Some((**result).clone());
            }
            on_delta(chunk);
        }
        let sampling = final_sampling.unwrap_or_default();
        Ok(CompletionResponse {
            id: format!("resp-{}", self.call_count.load(Ordering::SeqCst)),
            model: "fake".to_string(),
            content: elysper_provider::Content::Single(ContentPart::Text(
                TextContent {
                    text: sampling.text.clone(),
                    cache_control: None,
                },
            )),
            usage: sampling.usage.clone(),
            cached: false,
            replay_state: None,
            stop_reason: sampling.stop_reason.clone(),
        })
    }
}

// -- CancellingStreamProvider ----------------------------------------

/// Provider that fires the run's cancel token the first time the
/// loop asks it to sample, then returns normally. This is the
/// "cancelled *during* the run" shape: setup already completed, so
/// the loop is past [`crate::agent::EventSink::begin`] and reports
/// a [`SystemEvent::SessionInterrupted`] before the terminal event.
#[derive(Debug)]
pub(super) struct CancellingStreamProvider {
    token: CancellationToken,
    fired: Arc<AtomicBool>,
}

impl CancellingStreamProvider {
    pub(super) fn new(token: CancellationToken) -> Self {
        Self {
            token,
            fired: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait]
impl ModelProvider for CancellingStreamProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "cancelling-stream"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "fake".to_string(),
            provider: "cancelling".to_string(),
            context_window: 128_000,
            max_output_tokens: 4096,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
        }
    }

    async fn complete(
        &self,
        _request: elysper_provider::CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        unreachable!("streaming path should not call complete()")
    }

    async fn complete_with_stream(
        &self,
        _request: elysper_provider::CompletionRequest,
        _cancel_token: Option<Arc<dyn CancelToken>>,
        _on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<CompletionResponse, Error> {
        if !self.fired.swap(true, Ordering::SeqCst) {
            self.token.cancel();
        }
        Ok(CompletionResponse {
            id: "cancelled".to_string(),
            model: "fake".to_string(),
            content: elysper_provider::Content::Single(ContentPart::Text(
                TextContent {
                    text: String::new(),
                    cache_control: None,
                },
            )),
            usage: TokenUsage::default(),
            cached: false,
            replay_state: None,
            stop_reason: None,
        })
    }
}

// -- CapturingProvider -----------------------------------------------

/// Provider that records every `CompletionRequest` it receives.
/// Used to assert what the ReAct loop actually sends to the
/// LLM, not just what is stored on the agent.
pub(super) struct CapturingProvider {
    pub(super) captured:
        Arc<TokioMutex<Vec<Arc<Vec<elysper_provider::Message>>>>>,
    pub(super) captured_tools: Arc<TokioMutex<Vec<Arc<Vec<ToolDefinition>>>>>,
    // The model of every request, so a test can pin what the loop
    // asked the provider for.
    pub(super) captured_models: Arc<TokioMutex<Vec<String>>>,
    scripted: Arc<TokioMutex<Vec<Vec<StreamChunk>>>>,
    pub(super) call_count: AtomicUsize,
    // The window the provider reports, so a test can put the loop's
    // context manager into the overflow path without a second
    // provider type.
    context_window: usize,
}

impl CapturingProvider {
    pub(super) fn new(scripted: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            captured: Arc::new(TokioMutex::new(Vec::new())),
            captured_tools: Arc::new(TokioMutex::new(Vec::new())),
            captured_models: Arc::new(TokioMutex::new(Vec::new())),
            scripted: Arc::new(TokioMutex::new(scripted)),
            call_count: AtomicUsize::new(0),
            context_window: 128_000,
        }
    }

    /// Report a context window small enough to make the loop's
    /// context manager prune the transcript.
    pub(super) fn with_context_window(mut self, window: usize) -> Self {
        self.context_window = window;
        self
    }
}

#[async_trait]
impl ModelProvider for CapturingProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "capturing"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "capturing".into(),
            provider: "test".into(),
            context_window: self.context_window,
            max_output_tokens: 4096,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: elysper_provider::types::CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        unreachable!("streaming path")
    }

    async fn complete_with_stream(
        &self,
        request: elysper_provider::types::CompletionRequest,
        _cancel: Option<Arc<dyn CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<CompletionResponse, Error> {
        self.captured.lock().await.push(request.messages.clone());
        self.captured_tools.lock().await.push(request.tools.clone());
        self.captured_models
            .lock()
            .await
            .push(request.model.clone());
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let chunks = {
            let mut g = self.scripted.lock().await;
            if g.is_empty() {
                vec![StreamChunk::IsDone {
                    result: Box::new(SamplingResult::default()),
                }]
            } else {
                g.remove(0)
            }
        };
        let mut sampling: Option<SamplingResult> = None;
        for c in chunks {
            if let StreamChunk::IsDone { result } = &c {
                sampling = Some((**result).clone());
            }
            on_delta(c);
        }
        let s = sampling.unwrap_or_default();
        Ok(CompletionResponse {
            id: "cap".into(),
            model: "capturing".into(),
            content: elysper_provider::Content::Single(ContentPart::Text(
                elysper_provider::TextContent {
                    text: s.text.clone(),
                    cache_control: None,
                },
            )),
            usage: s.usage.clone(),
            cached: false,
            replay_state: None,
            stop_reason: s.stop_reason.clone(),
        })
    }
}

// -- Chunk-sequence helpers ------------------------------------------

pub(super) fn empty_response() -> Vec<StreamChunk> {
    vec![StreamChunk::IsDone {
        result: Box::new(SamplingResult {
            text: "hi there".to_string(),
            tool_calls: vec![],
            reasoning: String::new(),
            reasoning_signature: None,
            usage: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cached_prompt_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            },
            ..Default::default()
        }),
    }]
}

pub(super) fn tool_call_response(tool_calls: Vec<ToolUse>) -> Vec<StreamChunk> {
    let mut chunks = Vec::new();
    for call in &tool_calls {
        chunks.push(StreamChunk::ToolCallStart {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: serde_json::Value::String(
                serde_json::to_string(&call.input).unwrap_or_default(),
            ),
        });
        chunks.push(StreamChunk::ToolCallEnd {
            id: call.id.clone(),
        });
    }
    // Real providers re-list tool calls in `IsDone`; the loop
    // consumes them only once (via `ToolCallEnd`), so emit empty
    // here to avoid double-counting.
    chunks.push(StreamChunk::IsDone {
        result: Box::new(SamplingResult {
            text: String::new(),
            tool_calls: Vec::new(),
            reasoning: String::new(),
            reasoning_signature: None,
            usage: TokenUsage::default(),
            ..Default::default()
        }),
    });
    chunks
}

/// `StreamChunk::Usage` carrying a non-default
/// [`TokenUsage`]. Used by tests that need to assert the
/// per-iteration Usage emission pipeline.
pub(super) fn tool_call_response_with_usage(
    tool_calls: Vec<ToolUse>,
    usage: TokenUsage,
) -> Vec<StreamChunk> {
    let mut chunks = vec![StreamChunk::Usage(usage)];
    chunks.extend(tool_call_response(tool_calls));
    chunks
}

// -- Run helper ------------------------------------------------------

// -- Echo registry helper --------------------------------------------

/// Registry with one `echo` tool; returns the execution counter.
pub(super) fn echo_registry()
-> (Arc<ToolRegistry>, Arc<parking_lot::Mutex<usize>>) {
    let tool = elysper_test_support::FakeTool::new("echo", "echoed");
    let executions = Arc::clone(&tool.call_count);
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(elysper_tool::ToolEntry::new(Arc::new(tool)));
    (registry, executions)
}

/// Run one [`ReActAgent`] session and drain every [`AgentEvent`]
/// through the channel. Returns the captured event sequence.
pub(super) async fn run_and_collect(
    provider: Arc<dyn ModelProvider>,
    registry: Arc<ToolRegistry>,
    cancel: CancellationToken,
    input: AgentInput,
) -> Vec<AgentEvent> {
    let agent = ReActAgent::new(provider, registry);
    let mut stream = agent.run(input, Arc::new(cancel)).await;
    let mut out = Vec::new();
    while let Some(ev) = stream.next().await {
        out.push(ev);
    }
    out
}

// -- Peer-agent descriptor (shared by prompt integration + panel) ----

/// Build a peer [`AgentDescriptor`] for the panel fixture.
/// Only `name`, `description`, and `handoff_hint` are
/// exercised by the prompt assembler, so every other
/// field is filled with a minimal sentinel.
pub(super) fn peer_descriptor(
    name: &str,
    description: &str,
    handoff_hint: Option<&str>,
) -> AgentDescriptor {
    AgentDescriptor {
        name: name.into(),
        description: description.into(),
        kind: "panel".into(),
        version: "1.0.0".into(),
        instructions: "".into(),
        capabilities: Vec::new(),
        tools: Vec::new(),
        model_hint: None,
        handoffs: Vec::new(),
        handoff_hint: handoff_hint.map(str::to_string),
        output_schema: None,
        owner: None,
        domain: None,
        persona: None,
        display_name: None,
        max_iterations: None,
    }
}

pub(super) fn done_chunk(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: text.to_string(),
            cache_control: None,
        })),
        StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: String::new(),
                tool_calls: vec![],
                reasoning: String::new(),
                reasoning_signature: None,
                usage: TokenUsage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cached_prompt_tokens: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                },
                ..Default::default()
            }),
        },
    ]
}

// --- Host wiring helpers ----------------------------
//
// run_and_collect_with_host — helper for host wiring tests.
// DenyEchoGuard — guard fixture that always denies "echo".

pub(super) async fn run_and_collect_with_host(
    provider: Arc<dyn ModelProvider>,
    registry: Arc<ToolRegistry>,
    host: crate::agent::host::Host,
    input: AgentInput,
) -> Vec<AgentEvent> {
    let agent = ReActAgent::new(provider, registry)
        .with_host(host)
        .expect("bind the host");
    let mut stream = agent.run(input, Arc::new(CancellationToken::new())).await;
    let mut out = Vec::new();
    while let Some(ev) = stream.next().await {
        out.push(ev);
    }
    out
}

pub(super) struct DenyEchoGuard;
impl elysper_steering::Guard for DenyEchoGuard {
    fn name(&self) -> &str {
        "deny_echo"
    }

    fn check(
        &self,
        action: &elysper_steering::Action,
        _state: &AgentState,
    ) -> GuardResult {
        match action {
            elysper_steering::Action::ToolCall { name, .. }
                if name == "echo" =>
            {
                GuardResult::Deny {
                    reason: "echo is disabled for this run".to_string(),
                    severity: GuardSeverity::High,
                }
            }
            _ => GuardResult::Allow,
        }
    }
}

// --- Parallel-dispatch helpers ----------------------------

pub(super) static UNSAFE_RUNNING: AtomicUsize = AtomicUsize::new(0);
pub(super) static MAX_CONCURRENT_UNSAFE: AtomicUsize = AtomicUsize::new(0);

pub(super) struct CountingSafeTool;
pub(super) struct CountingUnsafeTool;

#[async_trait]
impl elysper_tool::Tool for CountingSafeTool {
    fn name(&self) -> &str {
        "safe"
    }

    fn description(&self) -> &str {
        "safe"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type":"object"})
    }

    fn mode(&self) -> elysper_tool::traits::ExecutionMode {
        elysper_tool::traits::ExecutionMode::Parallel
    }

    async fn call(&self, _: serde_json::Value, _: &Context) -> ToolOutput {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        ToolOutput::text("safe-done")
    }
}

#[async_trait]
impl elysper_tool::Tool for CountingUnsafeTool {
    fn name(&self) -> &str {
        "unsafe"
    }

    fn description(&self) -> &str {
        "unsafe"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type":"object"})
    }

    fn mode(&self) -> elysper_tool::traits::ExecutionMode {
        elysper_tool::traits::ExecutionMode::Sequential
    }

    async fn call(&self, _: serde_json::Value, _: &Context) -> ToolOutput {
        let n = UNSAFE_RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
        MAX_CONCURRENT_UNSAFE.fetch_max(n, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        UNSAFE_RUNNING.fetch_sub(1, Ordering::SeqCst);
        ToolOutput::text("unsafe-done")
    }
}

// --- LongTool helper -------------------------------------

pub(super) struct LongTool;
#[async_trait]
impl elysper_tool::Tool for LongTool {
    fn name(&self) -> &str {
        "long"
    }

    fn description(&self) -> &str {
        "long"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    fn mode(&self) -> elysper_tool::traits::ExecutionMode {
        elysper_tool::traits::ExecutionMode::Parallel
    }

    async fn call(&self, _: serde_json::Value, _: &Context) -> ToolOutput {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        ToolOutput::text("never")
    }
}
