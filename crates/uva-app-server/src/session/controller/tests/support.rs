//! Shared fixtures for the `SessionController` test suite.
//!
//! Houses the factories the test modules compose against
//! (`VecFactory`, `BlockingFactory`, `RecordingFactory`,
//! `ToolCapturingProvider`), the `make_manager_and_controller`
//! helper that builds a controller wired to a fresh in-memory
//! session registry, and the small "wait until N runs landed"
//! / "wait until the log holds this string" probes that keep
//! the async tests deterministic.

use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use futures::{Stream, StreamExt};
use parking_lot::Mutex;
use uva::{
    core::{AtomicCancelToken, CancelToken},
    harness::{AgentEvent, AgentInput, SessionEndReason, SystemEvent},
    provider::{
        Content,
        ContentPart,
        Message,
        TextContent,
        traits::ModelProvider,
    },
    session::{LogWriter, SessionEvent, manager::SessionRegistry},
    tool::registry::ToolRegistry,
};

use crate::{
    agent_run_config::{AgentRunConfig, RunOverrides},
    session::controller::{RunStreamFactory, SessionController},
};

/// Persist the model-visible facts of `events` through the
/// session's writer, the way the real harness does since the
/// writer unification: one `assistant_message` row per text
/// turn, one `tool_result` row per tool result. `Model` events
/// alone are no longer durable, so a fake factory that wants
/// its turns in the log writes the rows itself. Rows are
/// queued, not flushed — the run task's tail flush lands them,
/// exactly like the harness's step-boundary flush.
pub(super) fn persist_model_facts(
    writer: &Arc<LogWriter>,
    events: &[AgentEvent],
) {
    for event in events {
        let row = match event {
            AgentEvent::Model(ContentPart::Text(text)) => {
                SessionEvent::AssistantMessage {
                    seq: 0,
                    ts: String::new(),
                    data: serde_json::to_value(Message::assistant(
                        text.text.clone(),
                    ))
                    .expect("assistant message serializes"),
                    surface_op: None,
                }
            }
            AgentEvent::Model(ContentPart::ToolResult(result)) => {
                SessionEvent::ToolResult {
                    seq: 0,
                    ts: String::new(),
                    data: serde_json::to_value(Message::tool(
                        Content::Single(ContentPart::ToolResult(
                            result.clone(),
                        )),
                        result.tool_use_id.clone(),
                    ))
                    .expect("tool result serializes"),
                    is_error: result.is_error.unwrap_or(false),
                    surface_op: None,
                }
            }
            _ => continue,
        };
        writer.append_event_nowait(&row);
    }
}

/// Build a [`RunDependencies`] with no capabilities wired — every
/// test that needs a fresh baseline calls this. Returns the
/// struct directly (not the trait) so callers can poke
/// configuration knobs (`with_optional_tool_surface`, …) on the
/// returned value.
pub(super) fn test_deps() -> crate::session::controller::RunDependencies {
    crate::session::controller::RunDependencies::new(
        Arc::new(uva::test_support::FakeProvider::new(vec![])),
        Arc::new(ToolRegistry::new()),
        std::path::PathBuf::from("/tmp"),
        uva::harness::DEFAULT_SYSTEM_PROMPT.to_string(),
    )
}

/// The registry a controller fixture hands back.
///
/// `uva-session` is runtime-free, so the fixture wraps
/// [`SessionRegistry`] and nothing else: the queued turns the
/// controller drains are durable parks in the session's own log,
/// observable by reading the sink.
pub(super) struct FixtureRegistry {
    registry: SessionRegistry,
}

impl FixtureRegistry {
    /// A registry rooted at `root` — for a test that drives a
    /// controller over a sink of its own choosing rather than the
    /// registry's.
    pub(super) fn new(root: std::path::PathBuf) -> Self {
        Self {
            // Same seam the production boot installs.
            registry: SessionRegistry::new(root)
                .with_blocking(crate::spawner::server_blocking()),
        }
    }
}

impl std::ops::Deref for FixtureRegistry {
    type Target = SessionRegistry;

    fn deref(&self) -> &SessionRegistry {
        &self.registry
    }
}

/// Spawn a fresh controller wired to a temp-dir session
/// registry. Returns the controller, the registry (handy for
/// reading the sink / queue directly), and the temp dir (handy
/// for asserting files on disk post-`close()`).
pub(super) async fn make_manager_and_controller(
    idle_timeout: Duration,
    run_factory: Arc<dyn RunStreamFactory>,
) -> (Arc<SessionController>, FixtureRegistry, tempfile::TempDir) {
    make_manager_and_controller_with_deps(
        idle_timeout,
        run_factory,
        test_deps(),
    )
    .await
}

/// [`make_manager_and_controller`] with caller-supplied
/// [`RunDependencies`](crate::session::controller::RunDependencies):
/// the same spawn path the server uses, so a test can hand the
/// controller shared state it also holds a handle on (the
/// process-wide usage counters, a pinned clock, a tool surface).
pub(super) async fn make_manager_and_controller_with_deps(
    idle_timeout: Duration,
    run_factory: Arc<dyn RunStreamFactory>,
    deps: crate::session::controller::RunDependencies,
) -> (Arc<SessionController>, FixtureRegistry, tempfile::TempDir) {
    let temp = tempfile::TempDir::new().unwrap();
    let manager = FixtureRegistry::new(temp.path().to_path_buf());
    manager
        .create_with_user("s1".to_string(), "alice".to_string())
        .await
        .unwrap();

    let session_sink = manager.sink("alice", "s1");
    let controller = SessionController::spawn(
        "alice",
        "s1",
        session_sink,
        manager.writer("alice", "s1"),
        deps,
        idle_timeout,
        run_factory,
    );

    (controller, manager, temp)
}

/// Wait until the run factory has been invoked at least `n`
/// times.
///
/// `SessionController` sets `SessionState::Running` inside
/// `maybe_start_run` BEFORE the spawned run task is ever
/// polled, and the `Cancel` op handler sets `Cancelled`
/// synchronously — so waiting on `state()` alone proves
/// neither that a run started nor that it finished. The
/// factory invocation count is the only deterministic
/// observable; same idiom as
/// `test_events_are_persisted_and_broadcast`.
pub(super) async fn wait_for_runs(
    calls: &Arc<Mutex<Vec<RecordedCall>>>,
    n: usize,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.lock().len() < n {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("run factory was not invoked in time");
}

/// Wait until the session log holds a row containing
/// `needle`, then return the full log. The rerun tests need
/// the run task's appends (prompt row, answer row) to have
/// landed before folding.
pub(super) async fn wait_for_log_row(
    manager: &SessionRegistry,
    needle: &str,
) -> Vec<serde_json::Value> {
    let sink = manager.sink("alice", "s1");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let rows = sink.read().await.unwrap_or_default();
            if rows.iter().any(|row| row.to_string().contains(needle)) {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("row never landed in the session log")
}

/// The flat text of a message, for asserting fold output.
pub(super) fn message_text(message: &Message) -> String {
    match &message.content {
        Content::Single(ContentPart::Text(t)) => t.text.clone(),
        _ => String::new(),
    }
}

/// Find the `events.jsonl` written under `root` (the layout
/// `SessionRegistry` uses nests one directory per session).
pub(super) fn find_events_jsonl(
    root: &std::path::Path,
) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_events_jsonl(&path) {
                return Some(found);
            }
        } else if path.file_name().and_then(|n| n.to_str())
            == Some("events.jsonl")
        {
            return Some(path);
        }
    }
    None
}

/// Every `events.jsonl` under `root`, in a deterministic order.
///
/// `find_events_jsonl` returns the first one its walk meets, and the
/// walk follows the filesystem's readdir order — which differs per
/// machine. A test that has to *identify* one session's log among
/// several sorts this list first.
pub(super) fn find_all_events_jsonl(
    root: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str())
                == Some("events.jsonl")
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// A factory that emits a fixed list of events, useful for
/// persistence and broadcast tests.
pub(super) struct VecFactory {
    pub(super) events: Vec<AgentEvent>,
    pub(super) calls: Arc<Mutex<Vec<RecordedCall>>>,
}

#[allow(dead_code)]
pub(super) struct RecordedCall {
    pub(super) config: AgentRunConfig,
    pub(super) overrides: RunOverrides,
    pub(super) input: AgentInput,
}

impl VecFactory {
    pub(super) fn new(
        events: Vec<AgentEvent>,
        calls: Arc<Mutex<Vec<RecordedCall>>>,
    ) -> Self {
        Self { events, calls }
    }
}

impl RunStreamFactory for VecFactory {
    fn run_stream(
        &self,
        config: AgentRunConfig,
        overrides: RunOverrides,
        input: AgentInput,
        cancel: Arc<AtomicCancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>> {
        self.calls.lock().push(RecordedCall {
            config: config.clone(),
            overrides,
            input,
        });

        let events = self.events.clone();
        if let Some(writer) = &config.log_writer {
            persist_model_facts(writer, &events);
        }

        let stream = futures::stream::iter(events).take_while(move |_| {
            futures::future::ready(!cancel.is_cancelled())
        });
        Box::pin(stream)
    }
}

/// A factory that blocks until its cancellation token fires,
/// useful for verifying that only one run is active at a time.
pub(super) struct BlockingFactory {
    pub(super) calls: Arc<Mutex<Vec<RecordedCall>>>,
    pub(super) progress_count: Arc<AtomicUsize>,
}

impl RunStreamFactory for BlockingFactory {
    fn run_stream(
        &self,
        config: AgentRunConfig,
        overrides: RunOverrides,
        input: AgentInput,
        cancel: Arc<AtomicCancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>> {
        self.calls.lock().push(RecordedCall {
            config: config.clone(),
            overrides,
            input,
        });

        let count = Arc::clone(&self.progress_count);

        let stream = async_stream::stream! {
            while !cancel.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(5)).await;
                count.fetch_add(1, Ordering::SeqCst);
                yield AgentEvent::Model(ContentPart::Text(TextContent {
                    text: format!("working {}", count.load(Ordering::SeqCst)),
                    cache_control: None,
                }));
            }
            yield AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Cancelled,
            });
        };
        Box::pin(stream)
    }
}

/// A factory whose single run blocks until its cancellation token
/// fires, then records that it saw the token and ends — so a test
/// can prove an orphaned run was *stopped* (not merely left
/// appending beside its replacement controller).
pub(super) struct CancelObservingFactory {
    pub(super) calls: Arc<Mutex<Vec<RecordedCall>>>,
    /// Set once the run observed its cancel token.
    pub(super) cancelled: Arc<AtomicBool>,
}

impl RunStreamFactory for CancelObservingFactory {
    fn run_stream(
        &self,
        config: AgentRunConfig,
        overrides: RunOverrides,
        input: AgentInput,
        cancel: Arc<AtomicCancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>> {
        self.calls.lock().push(RecordedCall {
            config,
            overrides,
            input,
        });
        let cancelled = Arc::clone(&self.cancelled);
        let stream = async_stream::stream! {
            while !cancel.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            cancelled.store(true, Ordering::SeqCst);
            yield AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Cancelled,
            });
        };
        Box::pin(stream)
    }
}

/// Records every `(input)` the controller hands to the agent,
/// plus the events the factory streams back. Used by
/// `test_second_prompt_seeds_history_from_persisted_turns` to
/// pin the multi-turn memory contract.
pub(super) struct RecordingFactory {
    pub(super) calls: Arc<Mutex<Vec<AgentInput>>>,
    pub(super) first_run_events: Vec<AgentEvent>,
    pub(super) second_run_events: Vec<AgentEvent>,
}

impl RecordingFactory {
    pub(super) fn new(
        calls: Arc<Mutex<Vec<AgentInput>>>,
        first_run_events: Vec<AgentEvent>,
        second_run_events: Vec<AgentEvent>,
    ) -> Self {
        Self {
            calls,
            first_run_events,
            second_run_events,
        }
    }
}

impl RunStreamFactory for RecordingFactory {
    fn run_stream(
        &self,
        config: AgentRunConfig,
        _overrides: RunOverrides,
        input: AgentInput,
        cancel: Arc<AtomicCancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>> {
        self.calls.lock().push(input);
        let run_index = self.calls.lock().len() - 1;
        let events = if run_index == 0 {
            self.first_run_events.clone()
        } else {
            self.second_run_events.clone()
        };
        if let Some(writer) = &config.log_writer {
            persist_model_facts(writer, &events);
        }
        let stream = futures::stream::iter(events).take_while(move |_| {
            futures::future::ready(!cancel.is_cancelled())
        });
        Box::pin(stream)
    }
}

/// Provider that records the tool list of every completion request
/// and then finishes the turn with text.
pub(super) struct ToolCapturingProvider {
    pub(super) captured:
        parking_lot::Mutex<Vec<Arc<Vec<uva::provider::ToolDefinition>>>>,
}

#[async_trait::async_trait]
impl ModelProvider for ToolCapturingProvider {
    async fn initialize(
        &mut self,
        _config: uva::provider::ProviderConfig,
    ) -> Result<(), uva::core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "tool-capturing"
    }

    fn model_config(&self) -> uva::provider::types::ModelConfig {
        uva::provider::types::ModelConfig {
            name: "fake".to_string(),
            provider: "tool-capturing".to_string(),
            context_window: 128_000,
            max_output_tokens: 4096,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: uva::provider::CompletionRequest,
    ) -> Result<uva::provider::CompletionResponse, uva::core::Error> {
        unreachable!("the factory's agent streams")
    }

    async fn complete_with_stream(
        &self,
        request: uva::provider::CompletionRequest,
        _cancel_token: Option<Arc<dyn uva::core::CancelToken>>,
        mut on_delta: Box<dyn FnMut(uva::provider::StreamChunk) + Send>,
    ) -> Result<uva::provider::CompletionResponse, uva::core::Error> {
        self.captured.lock().push(request.tools.clone());
        on_delta(uva::provider::StreamChunk::IsDone {
            result: Box::new(uva::provider::SamplingResult {
                text: "done".to_string(),
                usage: uva::provider::TokenUsage::default(),
                ..Default::default()
            }),
        });
        Ok(uva::provider::CompletionResponse {
            id: "captured-1".to_string(),
            model: "fake".to_string(),
            content: Content::Single(ContentPart::Text(TextContent {
                text: "done".to_string(),
                cache_control: None,
            })),
            usage: uva::provider::TokenUsage::default(),
            cached: false,
            replay_state: None,
            stop_reason: Some("end_turn".to_string()),
        })
    }
}

/// Test provider that replays a scripted `StreamChunk` sequence
/// per `complete_with_stream` call, so a controller test can
/// drive the real agent through tool calls and turns.
pub(super) struct ScriptedStreamProvider {
    scripted: parking_lot::Mutex<Vec<Vec<uva::provider::StreamChunk>>>,
}

impl ScriptedStreamProvider {
    pub(super) fn new(scripted: Vec<Vec<uva::provider::StreamChunk>>) -> Self {
        Self {
            scripted: parking_lot::Mutex::new(scripted),
        }
    }

    fn take_next(&self) -> Vec<uva::provider::StreamChunk> {
        let mut guard = self.scripted.lock();
        if guard.is_empty() {
            return vec![uva::provider::StreamChunk::IsDone {
                result: Box::default(),
            }];
        }
        guard.remove(0)
    }
}

#[async_trait::async_trait]
impl ModelProvider for ScriptedStreamProvider {
    async fn initialize(
        &mut self,
        _config: uva::provider::ProviderConfig,
    ) -> Result<(), uva::core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "scripted-stream"
    }

    fn model_config(&self) -> uva::provider::types::ModelConfig {
        uva::provider::types::ModelConfig {
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
        _request: uva::provider::CompletionRequest,
    ) -> Result<uva::provider::CompletionResponse, uva::core::Error> {
        unreachable!("streaming path should not call complete()")
    }

    async fn complete_with_stream(
        &self,
        _request: uva::provider::CompletionRequest,
        _cancel_token: Option<Arc<dyn uva::core::CancelToken>>,
        mut on_delta: Box<dyn FnMut(uva::provider::StreamChunk) + Send>,
    ) -> Result<uva::provider::CompletionResponse, uva::core::Error> {
        let chunks = self.take_next();
        let mut sampling = uva::provider::SamplingResult::default();
        for chunk in chunks {
            if let uva::provider::StreamChunk::IsDone { result } = &chunk {
                sampling = (**result).clone();
            }
            on_delta(chunk);
        }
        Ok(uva::provider::CompletionResponse {
            id: "resp-1".to_string(),
            model: "fake".to_string(),
            content: Content::Single(ContentPart::Text(TextContent {
                text: sampling.text.clone(),
                cache_control: None,
            })),
            usage: sampling.usage.clone(),
            cached: false,
            replay_state: None,
            stop_reason: sampling.stop_reason.clone(),
        })
    }
}
