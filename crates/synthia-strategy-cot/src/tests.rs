//! `ChainOfThoughtStrategy` behaviour tests.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt as _;
use synthia_context::{AgentState, ContextManager, TruncatingContextManager};
use synthia_core::Error as CoreError;
use synthia_harness::{
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
use synthia_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ModelConfig,
    TokenUsage,
    ToolChoice,
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
impl synthia_provider::ModelProvider for ScriptedProvider {
    async fn initialize(
        &mut self,
        _config: synthia_provider::ProviderConfig,
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
        messages: &mut Vec<synthia_provider::Message>,
        state: &mut AgentState,
    ) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.prepare(messages, state).await;
    }
}

fn runtime(
    provider: Arc<dyn synthia_provider::ModelProvider>,
    context_manager: Arc<dyn ContextManager>,
    cancel: Arc<dyn synthia_core::CancelToken>,
) -> StrategyAgentRuntime {
    let mut caps = AgentCaps::for_provider(provider);
    caps.model.descriptor.name = "cot".into();
    caps.model.descriptor.kind = "cot".into();
    caps.model.descriptor.instructions = "You are terse.".into();
    caps.control.context_manager = context_manager;
    StrategyAgentRuntime {
        caps: Arc::new(caps),
        cancel,
        subagent_depth: 0,
    }
}

fn drive(
    strategy: &ChainOfThoughtStrategy,
    runtime: StrategyAgentRuntime,
) -> Vec<AgentEvent> {
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    let sink = EventSink::new(Arc::new(tx), None, None);
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

#[test]
fn cot_offers_no_tools_and_asks_for_steps() {
    let provider = ScriptedProvider::new("Step 1: 2 + 2 = 4\nAnswer: 4");
    let strategy = ChainOfThoughtStrategy::new(3);
    let events = drive(
        &strategy,
        runtime(
            provider.clone(),
            Arc::new(TruncatingContextManager),
            synthia_core::AtomicCancelToken::shared(),
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
        runtime(provider, manager, synthia_core::AtomicCancelToken::shared()),
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn cot_honours_a_pre_cancelled_token() {
    let provider = ScriptedProvider::new("Answer: 4");
    let cancel: Arc<dyn synthia_core::CancelToken> =
        synthia_core::AtomicCancelToken::shared();
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
            synthia_core::AtomicCancelToken::shared(),
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
