//! `ChainOfThoughtStrategy` behaviour tests.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt as _;
use uva_context::{AgentState, ContextManager, TruncatingContextManager};
use uva_core::{Error as CoreError, SystemClock};
use uva_harness::{
    AgentCaps,
    AgentEvent,
    AgentInput,
    AgentRuntime as CapsAgentRuntime,
    EventSink,
    ReasoningStrategy,
    SessionEndReason,
    SystemEvent,
    agent::strategy::AgentRuntime as StrategyAgentRuntime,
};
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ModelConfig,
    TokenUsage,
    ToolChoice,
};
use uva_session::{
    LogWriter,
    SessionEvent,
    SessionSink,
    in_memory::InMemorySessionSink,
};

use super::{ChainOfThoughtStrategy, DEFAULT_COT_STEPS};

struct ScriptedProvider {
    requests: parking_lot::Mutex<Vec<CompletionRequest>>,
    answer: String,
    fail: bool,
}

impl ScriptedProvider {
    fn new(answer: &str) -> Arc<Self> {
        Arc::new(Self {
            requests: parking_lot::Mutex::new(Vec::new()),
            answer: answer.to_string(),
            fail: false,
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            requests: parking_lot::Mutex::new(Vec::new()),
            answer: String::new(),
            fail: true,
        })
    }
}

#[async_trait]
impl uva_provider::ModelProvider for ScriptedProvider {
    async fn initialize(
        &mut self,
        _config: uva_provider::ProviderConfig,
    ) -> Result<(), CoreError> {
        Ok(())
    }

    fn name(&self) -> &str {
        "scripted"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "scripted-1".into(),
            provider: "scripted".into(),
            context_window: 8_192,
            max_output_tokens: 1_024,
            supports_tools: true,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, CoreError> {
        self.requests.lock().push(request);
        if self.fail {
            return Err(CoreError::internal("scripted failure"));
        }
        Ok(CompletionResponse {
            content: Content::text(self.answer.clone()),
            usage: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                ..TokenUsage::default()
            },
            ..CompletionResponse::default()
        })
    }
}

struct SpyManager {
    calls: Arc<AtomicUsize>,
    inner: TruncatingContextManager,
}

#[async_trait]
impl ContextManager for SpyManager {
    async fn prepare(
        &self,
        messages: &mut Vec<uva_provider::Message>,
        state: &mut AgentState,
    ) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.prepare(messages, state).await;
    }
}

fn runtime(
    provider: Arc<dyn uva_provider::ModelProvider>,
    context_manager: Arc<dyn ContextManager>,
    cancel: Arc<dyn uva_core::CancelToken>,
) -> StrategyAgentRuntime {
    let mut caps = AgentCaps::for_provider(provider);
    caps.model.descriptor.name = "cot".into();
    caps.model.descriptor.kind = "cot".into();
    caps.model.descriptor.instructions = "You are terse.".into();
    caps.control.context_manager = context_manager;
    StrategyAgentRuntime {
        caps: Arc::new(caps),
        cancel,
    }
}

fn drive(
    strategy: &ChainOfThoughtStrategy,
    runtime: StrategyAgentRuntime,
) -> Vec<AgentEvent> {
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    let sink = EventSink::new(Arc::new(tx), None);
    futures::executor::block_on(strategy.run(
        runtime,
        AgentInput::text("What is 2 + 2?"),
        &sink,
    ));
    drop(sink);
    let mut events = Vec::new();
    futures::executor::block_on(async {
        while let Some(event) = rx.next().await {
            events.push(event);
        }
    });
    events
}

fn reason_of(events: &[AgentEvent]) -> Option<SessionEndReason> {
    events.iter().find_map(|e| match e {
        AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
            Some(reason.clone())
        }
        _ => None,
    })
}

/// Drive the strategy with a session writer installed and return the
/// rows the run left in the log.
fn drive_logged(
    strategy: &ChainOfThoughtStrategy,
    runtime: StrategyAgentRuntime,
) -> Vec<SessionEvent> {
    let log = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(log.clone(), Arc::new(SystemClock)));
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    let event_sink = EventSink::new(
        Arc::new(tx),
        Some(Arc::clone(&writer) as Arc<dyn uva_session::SessionLog>),
    );
    futures::executor::block_on(strategy.run(
        runtime,
        AgentInput::text("What is 2 + 2?"),
        &event_sink,
    ));
    drop(event_sink);
    futures::executor::block_on(async { while rx.next().await.is_some() {} });
    futures::executor::block_on(writer.flush()).expect("flush");
    let rows = futures::executor::block_on(log.read()).expect("read");
    rows.iter().filter_map(SessionEvent::from_value).collect()
}

/// A provider that parks inside its call: the run never finishes, so
/// no later flush could have written the queued row instead.
struct ParkProvider {
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl uva_provider::ModelProvider for ParkProvider {
    async fn initialize(
        &mut self,
        _config: uva_provider::ProviderConfig,
    ) -> Result<(), CoreError> {
        Ok(())
    }

    fn name(&self) -> &str {
        "park"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "scripted-1".into(),
            provider: "park".into(),
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
    ) -> Result<CompletionResponse, CoreError> {
        unreachable!("streaming path should not call complete()")
    }

    async fn complete_with_stream(
        &self,
        _request: CompletionRequest,
        _cancel_token: Option<Arc<dyn uva_core::CancelToken>>,
        _on_delta: Box<dyn FnMut(uva_provider::StreamChunk) + Send>,
    ) -> Result<CompletionResponse, CoreError> {
        // The model is being asked. Everything it is about to see is
        // already on disk.
        self.entered.notify_one();
        std::future::pending::<()>().await;
        unreachable!("pending never resolves")
    }
}

/// A context manager that parks inside `prepare` — the seam a
/// summarising manager calls the provider from, and it runs before the
/// strategy samples.
struct ParkManager {
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ContextManager for ParkManager {
    async fn prepare(
        &self,
        _messages: &mut Vec<uva_provider::Message>,
        _state: &mut AgentState,
    ) {
        self.entered.notify_one();
        std::future::pending::<()>().await;
    }
}

/// The prompt is on disk before the context manager's summariser runs.
/// The summariser is a provider call, and it happens while the run's
/// request is being built — before the sample — so the flush that
/// covers the sample is too late for it. The summariser parks, so no
/// later flush could have written the row instead.
#[tokio::test]
async fn a_queued_row_is_on_disk_before_the_summariser_runs() {
    let log = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(log.clone(), Arc::new(SystemClock)));
    // The host's row, queued before the agent ran and never flushed by
    // the host.
    writer.append_event_nowait(&uva_session::step_start(0, 0));

    let entered = Arc::new(tokio::sync::Notify::new());
    let run = {
        let entered = Arc::clone(&entered);
        let writer = Arc::clone(&writer);
        tokio::spawn(async move {
            let (tx, _rx) = futures::channel::mpsc::unbounded();
            let sink = EventSink::new(
                Arc::new(tx),
                Some(Arc::clone(&writer) as Arc<dyn uva_session::SessionLog>),
            );
            ChainOfThoughtStrategy::default()
                .run(
                    // The provider is never reached: the summariser
                    // parks first.
                    runtime(
                        ScriptedProvider::new("unused"),
                        Arc::new(ParkManager { entered }),
                        uva_core::AtomicCancelToken::shared(),
                    ),
                    AgentInput::text("What is 2 + 2?"),
                    &sink,
                )
                .await;
        })
    };

    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("the context manager was entered");

    let rows = log.read().await.expect("read");
    assert!(
        rows.iter().any(|row| row["type"] == "step"),
        "the queued row must be on disk while the summariser is still \
         blocked; got {rows:?}"
    );
    run.abort();
}

/// The strategy flushes the session's writer before its provider call:
/// the row the host queued before it invoked the agent is on disk by
/// the time the model is asked, so a crash between the prompt and the
/// answer keeps the prompt. The provider parks inside that call, so no
/// later flush could have written the row instead.
///
/// This crate has no JSON dependency, so the queued row is a
/// structural one; what the test pins is the boundary — a row queued
/// before the run is durable when the provider is entered — not the
/// row's type.
#[tokio::test]
async fn a_queued_row_is_on_disk_before_the_model_call() {
    let log = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(log.clone(), Arc::new(SystemClock)));
    // The host's row, queued before the agent ran — the prompt's place
    // in the log — and never flushed by the host.
    writer.append_event_nowait(&uva_session::step_start(0, 0));

    let entered = Arc::new(tokio::sync::Notify::new());
    let run = {
        let entered = Arc::clone(&entered);
        let writer = Arc::clone(&writer);
        tokio::spawn(async move {
            let (tx, _rx) = futures::channel::mpsc::unbounded();
            let sink = EventSink::new(
                Arc::new(tx),
                Some(Arc::clone(&writer) as Arc<dyn uva_session::SessionLog>),
            );
            ChainOfThoughtStrategy::default()
                .run(
                    runtime(
                        Arc::new(ParkProvider { entered }),
                        Arc::new(TruncatingContextManager),
                        uva_core::AtomicCancelToken::shared(),
                    ),
                    AgentInput::text("What is 2 + 2?"),
                    &sink,
                )
                .await;
        })
    };

    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("the provider was called");

    let rows = log.read().await.expect("read");
    assert!(
        rows.iter().any(|row| row["type"] == "step"),
        "the queued row must be on disk while the model is still \
         blocked; got {rows:?}"
    );
    run.abort();
}

/// The model's answer is the run's durable fact: one
/// `assistant_message` row, holding the answer the model gave.
#[test]
fn the_models_answer_is_one_durable_row() {
    let provider = ScriptedProvider::new("Step 1: 2 + 2 = 4\nAnswer: 4");
    let rows = drive_logged(
        &ChainOfThoughtStrategy::new(3),
        runtime(
            provider,
            Arc::new(TruncatingContextManager),
            uva_core::AtomicCancelToken::shared(),
        ),
    );

    let assistant: Vec<&SessionEvent> = rows
        .iter()
        .filter(|event| event.type_tag() == "assistant_message")
        .collect();
    assert_eq!(
        assistant.len(),
        1,
        "the run's answer is one row; got {:?}",
        rows.iter()
            .map(|event| event.type_tag())
            .collect::<Vec<_>>()
    );
    let SessionEvent::AssistantMessage { data, .. } = assistant[0] else {
        panic!("the row is the assistant turn");
    };
    let text = data.to_string();
    assert!(
        text.contains("Answer: 4"),
        "the row holds the answer; got {text}"
    );
}

#[test]
fn cot_offers_no_tools_and_asks_for_steps() {
    let provider = ScriptedProvider::new("Step 1: 2 + 2 = 4\nAnswer: 4");
    let strategy = ChainOfThoughtStrategy::new(3);
    let events = drive(
        &strategy,
        runtime(
            provider.clone(),
            Arc::new(TruncatingContextManager),
            uva_core::AtomicCancelToken::shared(),
        ),
    );

    let requests = provider.requests.lock();
    assert_eq!(requests.len(), 1, "one pass, one request");
    assert!(requests[0].tools.is_empty());
    assert!(matches!(requests[0].tool_choice, ToolChoice::None));
    let system = requests[0].messages[0].content.extract_text().unwrap();
    assert!(system.contains("at most 3 numbered"), "bound: {system}");
    assert!(system.contains("You are terse."));

    assert_eq!(reason_of(&events), Some(SessionEndReason::Completed));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ModelDone(_))));
}

#[test]
fn cot_uses_the_runtime_context_manager() {
    let provider = ScriptedProvider::new("Answer: 4");
    let calls = Arc::new(AtomicUsize::new(0));
    let manager: Arc<dyn ContextManager> = Arc::new(SpyManager {
        calls: Arc::clone(&calls),
        inner: TruncatingContextManager,
    });
    let _ = drive(
        &ChainOfThoughtStrategy::default(),
        runtime(provider, manager, uva_core::AtomicCancelToken::shared()),
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn cot_honours_a_pre_cancelled_token() {
    let provider = ScriptedProvider::new("Answer: 4");
    let cancel: Arc<dyn uva_core::CancelToken> =
        uva_core::AtomicCancelToken::shared();
    cancel.cancel();
    let events = drive(
        &ChainOfThoughtStrategy::default(),
        runtime(provider.clone(), Arc::new(TruncatingContextManager), cancel),
    );
    assert_eq!(reason_of(&events), Some(SessionEndReason::Cancelled));
    assert!(provider.requests.lock().is_empty());
}

#[test]
fn cot_reports_provider_failure_as_session_end() {
    let events = drive(
        &ChainOfThoughtStrategy::default(),
        runtime(
            ScriptedProvider::failing(),
            Arc::new(TruncatingContextManager),
            uva_core::AtomicCancelToken::shared(),
        ),
    );
    match reason_of(&events) {
        Some(SessionEndReason::Error(message)) => {
            assert!(message.contains("scripted failure"));
        }
        other => panic!("expected Error end reason, got {other:?}"),
    }
}

#[test]
fn parse_steps_extracts_labelled_lines_only() {
    let text = "\
    Step 1: gather the numbers
    **Step 2:** add them
    - Thought 3: sanity-check the result
    As I noted in step 2 above, this is fine.
    Answer: 4";
    let steps = ChainOfThoughtStrategy::parse_steps(text);
    assert_eq!(
        steps,
        vec![
            "gather the numbers".to_string(),
            "add them".to_string(),
            "sanity-check the result".to_string(),
        ]
    );
}

#[test]
fn step_bound_is_configurable_and_never_zero() {
    assert!(
        ChainOfThoughtStrategy::new(7)
            .instruction()
            .contains("at most 7")
    );
    assert_eq!(ChainOfThoughtStrategy::new(0).max_steps, 1);
    assert_eq!(
        ChainOfThoughtStrategy::default().max_steps,
        DEFAULT_COT_STEPS
    );
}

#[test]
fn strategy_names_itself() {
    assert_eq!(ChainOfThoughtStrategy::default().name(), "chain-of-thought");
}

#[allow(dead_code)]
fn _caps_in_scope(_: AgentCaps, _: CapsAgentRuntime) {}
