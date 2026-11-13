//! Unit tests for the compaction pass: the guards that stop a pass
//! before it plans, and the plan binding that owns provenance.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::{channel::mpsc, future::BoxFuture};
use serde_json::{Value, json};
use uva_context::AgentState;
use uva_core::{AtomicCancelToken, CancelToken, Error, SystemClock};
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    Message,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    TokenUsage,
};
use uva_session::{
    CompactionError,
    CompactionOutcome,
    CompactionPlan,
    Compactor,
    LogWriter,
    TokenBudget,
    in_memory::InMemorySessionSink,
};
use uva_tool::Risk;

use super::compact;
use crate::{
    AgentCaps,
    agent::{
        approval::ApprovalRequest,
        re_act::{r#loop::ReActLoop, state::RunState},
        sink::EventSink,
    },
    events::AgentEvent,
};

// --- fixtures --------------------------------------------------------

struct IdleProvider;

#[async_trait]
impl ModelProvider for IdleProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "idle"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "idle".into(),
            provider: "test".into(),
            context_window: 1_000,
            max_output_tokens: 256,
            supports_tools: false,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        Err(Error::provider("the run's provider is not sampled here"))
    }
}

#[derive(Default)]
struct CountingProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for CountingProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "counting"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "counting".into(),
            provider: "test".into(),
            context_window: 1_000,
            max_output_tokens: 256,
            supports_tools: false,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(CompletionResponse {
            id: "summary".into(),
            model: "counting".into(),
            content: Content::text("SUMMARY"),
            usage: TokenUsage::default(),
            cached: false,
            replay_state: None,
            stop_reason: None,
        })
    }
}

#[derive(Default)]
struct NeverPlans {
    plans: AtomicUsize,
}

impl Compactor for NeverPlans {
    fn plan(
        &self,
        _history: &[Message],
        _budget: TokenBudget,
    ) -> Option<CompactionPlan> {
        self.plans.fetch_add(1, Ordering::SeqCst);
        None
    }

    fn summarize(
        &self,
        _plan: &CompactionPlan,
        _cancel: Arc<dyn CancelToken>,
    ) -> BoxFuture<'static, Result<CompactionOutcome, CompactionError>> {
        unreachable!("no plan means no summary")
    }
}

fn test_caps(
    compactor: Arc<dyn Compactor>,
    writer: Arc<LogWriter>,
) -> AgentCaps {
    let provider: Arc<dyn ModelProvider> = Arc::new(IdleProvider);
    let mut caps = AgentCaps::for_provider(provider);
    caps.control.compactor = Some(compactor);
    caps.env.log_writer = Some(writer);
    caps
}

async fn test_loop(
    compactor: Arc<dyn Compactor>,
    writer: Arc<LogWriter>,
) -> (ReActLoop, RunState, mpsc::UnboundedReceiver<AgentEvent>) {
    let caps = test_caps(compactor, Arc::clone(&writer));
    let cfg = caps.model.provider.model_config();
    // The transcript the run holds is the compaction pass's only
    // input, so the seeded log is folded into it here, exactly as
    // `drive_setup` does.
    let rows = writer.read().await.expect("read the seeded log");
    let history =
        uva_session::derive_history(&rows).expect("fold the seeded log");
    let surface_seqs = uva_session::try_fold_log_surface(&rows)
        .expect("fold the seeded log")
        .surface_seqs;
    // The loop's own `run` is unused by `compact`; the test drives its
    // own copy so it can hold `&ReActLoop` and `&mut RunState` at once.
    let run_for_loop = RunState::seed(
        AtomicCancelToken::shared(),
        0,
        Arc::new(history.clone()),
        AgentState::from_config(&cfg),
    )
    .with_surface_seqs(surface_seqs.clone());
    let run_for_test = RunState::seed(
        AtomicCancelToken::shared(),
        0,
        Arc::new(history),
        AgentState::from_config(&cfg),
    )
    .with_surface_seqs(surface_seqs);
    let (tx, rx) = mpsc::unbounded();
    let sink = EventSink::new(Arc::new(tx), Some(writer));
    let prompt_context = caps.run_prompt_context(None);
    (
        ReActLoop::from_caps(
            Arc::new(caps),
            cfg,
            run_for_loop,
            sink,
            prompt_context,
        ),
        run_for_test,
        rx,
    )
}

// A log whose four user turns each cost ~1 000 heuristic tokens.
async fn seeded_writer() -> Arc<LogWriter> {
    let sink = Arc::new(InMemorySessionSink::new("compaction"));
    let writer = Arc::new(LogWriter::new(sink, Arc::new(SystemClock)));
    for turn in 0..4 {
        writer
            .append(json!({
                "type": "user_message",
                "data": Message::user(format!(
                    "turn {turn}: {}",
                    "x".repeat(4_000)
                )),
            }))
            .await
            .expect("seed the turn");
    }
    writer.flush().await.expect("flush the seeded rows");
    writer
}

async fn rows_with(writer: &LogWriter, tag: &str) -> Vec<Value> {
    writer
        .read()
        .await
        .expect("read the log")
        .into_iter()
        .filter(|row| row.get("type").and_then(Value::as_str) == Some(tag))
        .collect()
}

// --- tests -----------------------------------------------------------

#[tokio::test]
async fn a_suspended_run_does_not_compact() {
    let writer = seeded_writer().await;
    let spy = Arc::new(NeverPlans::default());
    let (loop_, mut session, mut events) =
        test_loop(spy.clone(), Arc::clone(&writer)).await;
    session.pending_approvals.push(ApprovalRequest::new(
        "a1",
        "c1",
        "shell",
        "hash",
        Risk::High,
        "agent",
    ));

    compact(&loop_, &mut session, 0)
        .await
        .expect("a suspended skip is not fatal");

    assert_eq!(
        spy.plans.load(Ordering::SeqCst),
        0,
        "a suspended run must not even plan"
    );
    assert!(rows_with(&writer, "compaction").await.is_empty());
    assert!(
        events.try_recv().is_err(),
        "a skipped pass publishes nothing"
    );
}

#[tokio::test]
async fn a_repeat_pass_over_the_last_summary_makes_no_second_call() {
    let writer = seeded_writer().await;
    let provider = Arc::new(CountingProvider::default());
    let compactor: Arc<dyn Compactor> = Arc::new(
        uva_session::SummaryCompactor::new(provider.clone(), "counting"),
    );
    let (loop_, mut session, _events) =
        test_loop(compactor, Arc::clone(&writer)).await;

    compact(&loop_, &mut session, 0).await.expect("first pass");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(rows_with(&writer, "compaction").await.len(), 1);

    // The kept turn is still over the tail budget, so the plan still
    // exists; its only shadowable row is the summary it just wrote.
    compact(&loop_, &mut session, 1).await.expect("second pass");
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "rewriting the summary with nothing new is not progress"
    );
    assert_eq!(rows_with(&writer, "compaction").await.len(), 1);
}
