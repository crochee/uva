//! `BestOfNStrategy` behaviour tests.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt as _;
use parking_lot::Mutex;
use synthia_core::Error as CoreError;
use synthia_harness::{
    AgentCaps,
    AgentEvent,
    AgentInput,
    ReasoningStrategy,
    SessionEndReason,
    SystemEvent,
    agent::strategy::{
        AgentRuntime,
        AgentRuntime as _StrategyAgentRuntimeCheck,
    },
};
use synthia_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ModelConfig,
    ProviderConfig,
};

use super::{BestOfNStrategy, DEFAULT_CANDIDATES, LongestAnswer};

struct ScriptedProvider {
    answers: Mutex<Vec<String>>,
    calls: AtomicUsize,
    fail_every: Option<usize>,
}

impl ScriptedProvider {
    fn new(answers: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(
                answers.iter().map(|a| (*a).to_string()).collect(),
            ),
            calls: AtomicUsize::new(0),
            fail_every: None,
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            fail_every: Some(1),
        })
    }
}

#[async_trait]
impl synthia_provider::ModelProvider for ScriptedProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), CoreError> {
        Ok(())
    }

    fn name(&self) -> &str {
        "scripted-bon"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "bon-test".into(),
            provider: "scripted-bon".into(),
            context_window: 8_192,
            max_output_tokens: 512,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, CoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self
            .fail_every
            .is_some_and(|every| (call + 1).is_multiple_of(every))
        {
            return Err(CoreError::internal("candidate failure"));
        }
        let text = {
            let mut answers = self.answers.lock();
            if answers.is_empty() {
                format!("answer {call}")
            } else {
                answers.remove(0)
            }
        };
        Ok(CompletionResponse {
            content: Content::text(text),
            ..CompletionResponse::default()
        })
    }
}

struct InlineSpawner {
    spawns: Arc<AtomicUsize>,
}

impl synthia_core::spawn::Spawner for InlineSpawner {
    fn spawn(&self, task: synthia_core::spawn::BoxFuture<()>) {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        let mut task = task;
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let _ = std::future::Future::poll(
            std::pin::Pin::as_mut(&mut task),
            &mut cx,
        );
    }

    fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        f();
    }
}

fn runtime(
    provider: Arc<dyn synthia_provider::ModelProvider>,
    spawner: Arc<dyn synthia_core::spawn::Spawner>,
    cancel: Arc<dyn synthia_core::CancelToken>,
) -> AgentRuntime {
    let mut caps = AgentCaps::for_provider(provider);
    caps.model.descriptor.name = "bon".into();
    caps.model.descriptor.kind = "bon".into();
    caps.model.descriptor.instructions = "Answer.".into();
    caps.env.spawner = spawner;
    AgentRuntime {
        caps: Arc::new(caps),
        cancel,
        subagent_depth: 0,
    }
}

fn drive(strategy: &BestOfNStrategy, runtime: AgentRuntime) -> Vec<AgentEvent> {
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    let sink = synthia_harness::EventSink::new(Arc::new(tx), None, None);
    futures::executor::block_on(strategy.run(
        runtime,
        AgentInput::text("answer me"),
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

fn answer_of(events: &[AgentEvent]) -> Option<String> {
    events.iter().rev().find_map(|event| match event {
        AgentEvent::ModelDone(sampling) => Some(sampling.text.clone()),
        _ => None,
    })
}

fn reason_of(events: &[AgentEvent]) -> Option<SessionEndReason> {
    events.iter().find_map(|event| match event {
        AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
            Some(reason.clone())
        }
        _ => None,
    })
}

fn progress_count(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event, AgentEvent::System(SystemEvent::Progress { .. }))
        })
        .count()
}

#[test]
fn samples_through_the_runtime_spawner_and_publishes_the_winner() {
    let provider = ScriptedProvider::new(&["short", "the longest answer"]);
    let spawns = Arc::new(AtomicUsize::new(0));
    let events = drive(
        &BestOfNStrategy::new(2, Arc::new(LongestAnswer)),
        runtime(
            provider.clone(),
            Arc::new(InlineSpawner {
                spawns: Arc::clone(&spawns),
            }),
            synthia_core::AtomicCancelToken::shared(),
        ),
    );

    assert_eq!(spawns.load(Ordering::SeqCst), 2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(answer_of(&events).as_deref(), Some("the longest answer"));
    assert_eq!(reason_of(&events), Some(SessionEndReason::Completed));
    assert_eq!(progress_count(&events), 3);
}

#[test]
fn the_scorer_decides_which_candidate_wins() {
    struct Shortest;
    #[async_trait]
    impl super::CandidateScorer for Shortest {
        async fn score(&self, candidate: &str) -> f64 {
            -(candidate.chars().count() as f64)
        }

        fn name(&self) -> &str {
            "shortest"
        }
    }

    let provider = ScriptedProvider::new(&["short", "a much longer answer"]);
    let events = drive(
        &BestOfNStrategy::new(2, Arc::new(Shortest)),
        runtime(
            provider,
            Arc::new(InlineSpawner {
                spawns: Arc::new(AtomicUsize::new(0)),
            }),
            synthia_core::AtomicCancelToken::shared(),
        ),
    );
    assert_eq!(answer_of(&events).as_deref(), Some("short"));
}

#[test]
fn a_cancelled_run_samples_nothing() {
    let provider = ScriptedProvider::new(&["a", "b"]);
    let cancel: Arc<dyn synthia_core::CancelToken> =
        synthia_core::AtomicCancelToken::shared();
    cancel.cancel();
    let events = drive(
        &BestOfNStrategy::default(),
        runtime(
            provider.clone(),
            Arc::new(InlineSpawner {
                spawns: Arc::new(AtomicUsize::new(0)),
            }),
            cancel,
        ),
    );
    assert_eq!(reason_of(&events), Some(SessionEndReason::Cancelled));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn all_candidates_failing_ends_with_an_error() {
    let events = drive(
        &BestOfNStrategy::new(3, Arc::new(LongestAnswer)),
        runtime(
            ScriptedProvider::failing(),
            Arc::new(InlineSpawner {
                spawns: Arc::new(AtomicUsize::new(0)),
            }),
            synthia_core::AtomicCancelToken::shared(),
        ),
    );
    match reason_of(&events) {
        Some(SessionEndReason::Error(message)) => assert!(
            message.contains("candidate failure"),
            "the provider's reason must survive: {message}"
        ),
        other => panic!("expected Error end reason, got {other:?}"),
    }
    assert!(answer_of(&events).is_none());
}

#[test]
fn ties_go_to_the_lowest_index() {
    let provider = ScriptedProvider::new(&["same", "same"]);
    let events = drive(
        &BestOfNStrategy::new(2, Arc::new(LongestAnswer)),
        runtime(
            provider,
            Arc::new(InlineSpawner {
                spawns: Arc::new(AtomicUsize::new(0)),
            }),
            synthia_core::AtomicCancelToken::shared(),
        ),
    );
    let selected = events.iter().find_map(|event| match event {
        AgentEvent::System(SystemEvent::Progress { message, .. })
            if message.contains("selected candidate") =>
        {
            Some(message.clone())
        }
        _ => None,
    });
    let selected = selected.unwrap_or_default();
    assert!(
        selected.contains("candidate 0"),
        "the first candidate must win a tie: {selected:?}"
    );
}

#[test]
fn names_itself_and_its_scorer() {
    let strategy = BestOfNStrategy::default();
    assert_eq!(strategy.name(), "best-of-n");
    assert_eq!(strategy.scorer.name(), "longest-answer");
    assert_eq!(strategy.candidates, DEFAULT_CANDIDATES);
    assert_eq!(
        BestOfNStrategy::new(0, Arc::new(LongestAnswer)).candidates,
        1
    );
    assert!(format!("{strategy:?}").contains("longest-answer"));
}

#[test]
fn longest_answer_counts_characters_not_bytes() {
    futures::executor::block_on(async {
        use super::CandidateScorer;
        let scorer = Arc::new(LongestAnswer);
        assert_eq!(scorer.score("abc").await, 3.0);
        assert_eq!(scorer.score("文档库").await, 3.0);
        assert!(
            scorer.score("abcdefgh").await > scorer.score("文档库").await,
            "nine bytes must not outrank eight characters"
        );
    });
}

struct CountingContextManager {
    inner: synthia_context::TruncatingContextManager,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl synthia_context::ContextManager for CountingContextManager {
    async fn prepare(
        &self,
        messages: &mut Vec<synthia_provider::Message>,
        state: &mut synthia_context::AgentState,
    ) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.prepare(messages, state).await;
    }

    async fn prepare_arc(
        &self,
        messages: Arc<Vec<synthia_provider::Message>>,
        state: &mut synthia_context::AgentState,
    ) -> Arc<Vec<synthia_provider::Message>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.prepare_arc(messages, state).await
    }
}

#[test]
fn best_of_n_prepares_context_once() {
    let provider = ScriptedProvider::new(&["a", "bb", "ccc"]);
    let prepares = Arc::new(AtomicUsize::new(0));
    let manager: Arc<dyn synthia_context::ContextManager> =
        Arc::new(CountingContextManager {
            inner: synthia_context::TruncatingContextManager,
            calls: Arc::clone(&prepares),
        });
    let mut caps = AgentCaps::for_provider(provider.clone());
    caps.model.descriptor.name = "bon".into();
    caps.model.descriptor.kind = "bon".into();
    caps.model.descriptor.instructions = "Answer.".into();
    caps.control.context_manager = manager;
    caps.env.spawner = Arc::new(InlineSpawner {
        spawns: Arc::new(AtomicUsize::new(0)),
    });
    let rt = AgentRuntime {
        caps: Arc::new(caps),
        cancel: synthia_core::AtomicCancelToken::shared(),
        subagent_depth: 0,
    };

    let events = drive(&BestOfNStrategy::new(3, Arc::new(LongestAnswer)), rt);
    assert_eq!(prepares.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert_eq!(reason_of(&events), Some(SessionEndReason::Completed));
}

#[allow(dead_code)]
fn _mark_agent_runtime(_: AgentRuntime, _: _StrategyAgentRuntimeCheck) {}
