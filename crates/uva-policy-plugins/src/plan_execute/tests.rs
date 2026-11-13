//! Tests for `uva-strategy-plan-execute`.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use uva_core::CancelToken;
use uva_harness::{
    AgentCaps,
    AgentInput,
    AgentRuntime,
    EventSink,
    ReasoningStrategy,
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

use crate::plan_execute::{
    defaults::{DefaultExecutor, DefaultPlanGenerator, DefaultRePlanner},
    strategy::{Plan, PlanExecuteStrategy, ReplanDecision, Step, StepOutcome},
};

#[derive(Clone)]
struct ScriptedProvider {
    replies: Vec<String>,
    pos: Arc<AtomicU64>,
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
            .unwrap_or_else(|| "fallback".to_string());
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

impl ScriptedProvider {
    fn pos(&self) -> u64 {
        self.pos.load(Ordering::SeqCst)
    }
}

fn minimal_runtime(provider: Arc<dyn ModelProvider>) -> AgentRuntime {
    let mut caps = AgentCaps::for_provider(provider);
    caps.model.descriptor.name = "plan".into();
    caps.model.descriptor.description = "plan-execute test agent".into();
    caps.model.descriptor.kind = "plan-execute".into();
    caps.model.descriptor.version = "0.1.0".into();
    caps.model.descriptor.instructions = "do the plan".into();
    caps.env.spawner = Arc::new(NoopSpawner);
    AgentRuntime {
        caps: Arc::new(caps),
        cancel: Arc::new(uva_core::AtomicCancelToken::default()),
        subagent_depth: 0,
    }
}

struct NoopSpawner;
impl uva_core::spawn::Spawner for NoopSpawner {
    fn spawn(&self, _fut: uva_core::spawn::BoxFuture) {}
}

fn scripted(
    replies: Vec<&str>,
) -> (Arc<dyn ModelProvider>, Arc<ScriptedProvider>) {
    let p = ScriptedProvider {
        replies: replies.into_iter().map(String::from).collect(),
        pos: Arc::new(AtomicU64::new(0)),
    };
    let dyn_p: Arc<dyn ModelProvider> = Arc::new(p.clone());
    (dyn_p, Arc::new(p))
}

fn text_input() -> AgentInput {
    use uva_provider::ContentPart as CP;
    AgentInput {
        content: vec![CP::Text(TextContent {
            text: "do the task".into(),
            cache_control: None,
        })],
        ..AgentInput::text("do the task")
    }
}

#[tokio::test]
async fn three_step_plan_executes_in_order() {
    let (provider, scripted_arc) = scripted(vec![
        "1. step one\n2. step two\n3. step three",
        "result of step one",
        "continue",
        "result of step two",
        "continue",
        "result of step three",
        "finalize",
    ]);
    let runtime = minimal_runtime(provider);
    let (tx, _rx) = futures::channel::mpsc::unbounded();
    let sink = EventSink::new(Arc::new(tx), None);
    let strategy = PlanExecuteStrategy::new(
        crate::plan_execute::strategy::PlanExecuteConfig::default()
            .max_steps(20),
    );
    strategy.run(runtime, text_input(), &sink).await;
    assert_eq!(scripted_arc.pos(), 6);
}

/// The model's output is the run's durable fact: one
/// `assistant_message` row per model call — the plan, then each
/// step's answer.
#[tokio::test]
async fn the_models_output_is_one_row_per_call() {
    use uva_session::{
        LogWriter,
        SessionEvent,
        SessionSink,
        in_memory::InMemorySessionSink,
    };

    let (provider, _scripted_arc) = scripted(vec![
        "1. step one\n2. step two",
        "result of step one",
        "continue",
        "result of step two",
        "finalize",
    ]);
    let runtime = minimal_runtime(provider);
    let log = Arc::new(InMemorySessionSink::new("s"));
    let writer =
        Arc::new(LogWriter::new(log.clone(), Arc::new(uva_core::SystemClock)));
    let (tx, _rx) = futures::channel::mpsc::unbounded();
    let sink = EventSink::new(
        Arc::new(tx),
        Some(Arc::clone(&writer) as Arc<dyn uva_session::SessionLog>),
    );
    let strategy = PlanExecuteStrategy::new(
        crate::plan_execute::strategy::PlanExecuteConfig::default()
            .max_steps(20),
    );
    strategy.run(runtime, text_input(), &sink).await;
    writer.flush().await.expect("flush");

    let rows = log.read().await.expect("read");
    let assistant: Vec<SessionEvent> = rows
        .iter()
        .filter_map(SessionEvent::from_value)
        .filter(|event| event.type_tag() == "assistant_message")
        .collect();
    assert_eq!(
        assistant.len(),
        3,
        "the plan and the two step answers are three rows; got {}",
        rows.len()
    );
    let texts: Vec<String> = assistant
        .iter()
        .map(|event| match event {
            SessionEvent::AssistantMessage { data, .. } => data.to_string(),
            other => other.type_tag().to_string(),
        })
        .collect();
    assert!(
        texts[0].contains("step one"),
        "the plan is the first row; got {}",
        texts[0]
    );
    assert!(
        texts[1].contains("result of step one"),
        "the first answer is the second row; got {}",
        texts[1]
    );
    assert!(
        texts[2].contains("result of step two"),
        "the second answer is the third row; got {}",
        texts[2]
    );
}

#[tokio::test]
async fn replanner_abort_stops_subsequent_steps() {
    use async_trait::async_trait;

    use crate::plan_execute::strategy::{Executor, Plan, RePlanner};

    struct AlwaysFailExecutor;
    #[async_trait]
    impl Executor for AlwaysFailExecutor {
        async fn execute(
            &self,
            _step: &Step,
            _runtime: &AgentRuntime,
            _cancel: Arc<dyn uva_core::CancelToken>,
            _sink: &EventSink,
        ) -> Result<StepOutcome, uva_core::Error> {
            Ok(StepOutcome::failed("nope"))
        }
    }

    struct AbortRePlanner;
    #[async_trait]
    impl RePlanner for AbortRePlanner {
        async fn replan(
            &self,
            _plan: &Plan,
            outcomes: &[StepOutcome],
            _runtime: &AgentRuntime,
        ) -> Result<ReplanDecision, uva_core::Error> {
            if outcomes
                .iter()
                .any(|o| matches!(o, StepOutcome::Failed { .. }))
            {
                Ok(ReplanDecision::Abort)
            } else {
                Ok(ReplanDecision::Finalize)
            }
        }
    }

    let (provider, _scripted_arc) = scripted(vec!["1. only step"]);
    let runtime = minimal_runtime(provider);
    let (_tx, _rx) = futures::channel::mpsc::unbounded();
    let sink = EventSink::new(Arc::new(_tx.clone()), None);
    let strategy = PlanExecuteStrategy::new(
        crate::plan_execute::strategy::PlanExecuteConfig::default()
            .max_steps(20)
            .executor(Arc::new(AlwaysFailExecutor))
            .re_planner(Arc::new(AbortRePlanner)),
    );
    strategy.run(runtime, text_input(), &sink).await;
}

#[tokio::test]
async fn max_steps_truncates() {
    let (provider, _scripted_arc) =
        scripted(vec!["1. a\n2. b\n3. c\n4. d\n5. e", "r1", "r2", "r3"]);
    let runtime = minimal_runtime(provider);
    let (_tx, _rx) = futures::channel::mpsc::unbounded();
    let sink = EventSink::new(Arc::new(_tx.clone()), None);
    let strategy = PlanExecuteStrategy::new(
        crate::plan_execute::strategy::PlanExecuteConfig::default()
            .max_steps(2),
    );
    strategy.run(runtime, text_input(), &sink).await;
}

#[tokio::test]
async fn cancel_halts_between_steps() {
    let token: Arc<dyn uva_core::CancelToken> =
        Arc::new(uva_core::AtomicCancelToken::default());
    let arc_dyn = Arc::clone(&token);
    let concrete: Arc<uva_core::AtomicCancelToken> =
        std::sync::Arc::new(uva_core::AtomicCancelToken::default());
    let concrete_clone = Arc::clone(&concrete);
    let _ = arc_dyn; // silence unused
    concrete_clone.cancel();
    let (provider, _scripted_arc) = scripted(vec![]);
    let mut runtime = minimal_runtime(provider);
    runtime.cancel = Arc::clone(&concrete) as Arc<dyn uva_core::CancelToken>;
    let (_tx, _rx) = futures::channel::mpsc::unbounded();
    let sink = EventSink::new(Arc::new(_tx.clone()), None);
    let strategy = PlanExecuteStrategy::new(
        crate::plan_execute::strategy::PlanExecuteConfig::default(),
    );
    strategy.run(runtime, text_input(), &sink).await;
}

#[test]
fn plan_parser_handles_three_prefixes() {
    use crate::plan_execute::defaults::parse_plan;
    let plan = parse_plan("1. alpha\n2. beta\n3) gamma\n4: delta");
    let texts: Vec<&str> = plan.steps.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["alpha", "beta", "gamma", "delta"]);
}

#[test]
fn step_and_plan_constructors() {
    let p = Plan::new(vec![Step::new("a"), Step::new("b")]);
    assert_eq!(p.steps.len(), 2);
    assert_eq!(p.steps[0].text, "a");
}

#[test]
fn step_outcome_summary_strings() {
    assert_eq!(
        StepOutcome::completed("hello").summary(),
        "completed: hello"
    );
    assert_eq!(StepOutcome::failed("nope").summary(), "failed: nope");
    assert_eq!(StepOutcome::cancelled().summary(), "cancelled");
}

#[test]
fn defaults_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<DefaultPlanGenerator>();
    assert_send_sync::<DefaultExecutor>();
    assert_send_sync::<DefaultRePlanner>();
}
