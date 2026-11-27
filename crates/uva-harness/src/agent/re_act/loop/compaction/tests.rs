//! Unit tests for the compaction pass: the guards that stop a pass
//! before it plans, and the plan binding that owns provenance.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::{channel::mpsc, future::BoxFuture};
use serde_json::{Value, json};
use uva_approval::ApprovalRequest;
use uva_context::AgentState;
use uva_core::{AtomicCancelToken, CancelToken, Error, SystemClock};
use uva_events::AgentEvent;
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    Message,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    Role,
    TokenUsage,
    ToolUse,
};
use uva_session::{
    CompactionError,
    CompactionOutcome,
    CompactionPlan,
    Compactor,
    LogWriter,
    SessionEvent,
    SummaryInput,
    TokenBudget,
    in_memory::InMemorySessionSink,
};
use uva_tool::Risk;

use super::compact;
use crate::{
    AgentCaps,
    agent::{
        re_act::{r#loop::ReActLoop, state::RunState, transcript::Transcript},
        sink::EventSink,
    },
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
        _input: &SummaryInput,
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
    let history = uva_session::ProvenancedHistory::from_log(&rows)
        .expect("fold the seeded log");
    // The loop's own `run` is unused by `compact`; the test drives its
    // own copy so it can hold `&ReActLoop` and `&mut RunState` at once.
    let run_for_loop = RunState::seed(
        AtomicCancelToken::shared(),
        Transcript::from_projection(history.clone()),
        AgentState::from_config(&cfg),
    );
    let run_for_test = RunState::seed(
        AtomicCancelToken::shared(),
        Transcript::from_projection(history),
        AgentState::from_config(&cfg),
    );
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
        let row = uva_session::user_message(
            serde_json::to_value(Message::user(format!(
                "turn {turn}: {}",
                "x".repeat(4_000)
            )))
            .expect("a user message serialises"),
            None,
        );
        writer.append_event(&row).await.expect("seed the turn");
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

// A compactor that compacts exactly the span it is told to.
//
// The pass's own wiring is under test here — the range it writes and
// the fold that reads it back — not the summariser's policy.
struct FixedSpan {
    range: std::ops::Range<usize>,
}

impl Compactor for FixedSpan {
    fn plan(
        &self,
        _history: &[Message],
        _budget: TokenBudget,
    ) -> Option<CompactionPlan> {
        Some(CompactionPlan {
            range: self.range.clone(),
            shadowed_seqs: Vec::new(),
            keep_recent: 0,
            shadowed: Vec::new(),
            shadowed_tokens: 0,
        })
    }

    fn summarize(
        &self,
        _plan: &CompactionPlan,
        _input: &SummaryInput,
        _cancel: Arc<dyn CancelToken>,
    ) -> BoxFuture<'static, Result<CompactionOutcome, CompactionError>> {
        Box::pin(async {
            Ok(CompactionOutcome {
                summary: "SUMMARY".to_string(),
                shadowed_seqs: Vec::new(),
                shadowed_tokens: 0,
                provider: None,
                model: None,
                usage: None,
            })
        })
    }
}

// A steered run's log: the steer lands while the tool batch is in
// flight, so its row sits between the assistant's `tool_use` and the
// result that answers it.
async fn steered_writer() -> Arc<LogWriter> {
    let sink = Arc::new(InMemorySessionSink::new("steered"));
    let writer = Arc::new(LogWriter::new(sink, Arc::new(SystemClock)));
    let parts = vec![ContentPart::ToolUse(ToolUse {
        id: "call-1".into(),
        name: "read".into(),
        input: json!({"path": "a.rs"}),
    })];
    for row in [
        uva_session::user_message(
            serde_json::to_value(Message::user("start"))
                .expect("a user message serialises"),
            None,
        ),
        uva_session::assistant_message(
            serde_json::to_value(Message::new(
                Role::Assistant,
                Content::parts(parts),
            ))
            .expect("an assistant message serialises"),
        ),
        uva_session::user_message(
            serde_json::to_value(Message::user("steer: focus on the tests"))
                .expect("a user message serialises"),
            None,
        ),
        SessionEvent::ToolResult {
            seq: 0,
            changed: None,
            checkpoint: None,
            ts: String::new(),
            data: serde_json::to_value(Message::tool(
                Content::text("ok"),
                "call-1",
            ))
            .expect("a tool message serialises"),
            is_error: false,
            tool_call_id: Some("call-1".to_string()),
            surface_op: None,
        },
    ] {
        writer.append_event(&row).await.expect("seed the row");
    }
    writer.flush().await.expect("flush the seeded rows");
    writer
}

// The tool calls the messages ask for and the ones they answer.
fn tool_ids(messages: &[Message]) -> (Vec<String>, Vec<String>) {
    let mut asked = Vec::new();
    let mut answered = Vec::new();
    for message in messages {
        for part in message.content.iter() {
            match part {
                ContentPart::ToolUse(call) => asked.push(call.id.clone()),
                ContentPart::ToolResult(result) => {
                    answered.push(result.tool_use_id.clone())
                }
                _ => {}
            }
        }
        if let Some(id) = message.tool_call_id.as_ref() {
            answered.push(id.clone());
        }
    }
    (asked, answered)
}

// A resumed steered run that compacts must leave a transcript every
// provider accepts: the `tool_result` that answers the shadowed
// `tool_use` goes with it, and the steer survives.
//
// The run is *resumed*: its transcript comes from the fold, so the
// range the pass writes is read back in the fold's own coordinates.
// A fold that read the range in the log's coordinates would shadow
// the steer instead of the tool result, and the resumed run would
// carry a `tool_result` no `tool_use` answers.
#[tokio::test]
async fn a_resumed_steered_run_compacts_without_orphaning_a_tool_result() {
    let writer = steered_writer().await;
    let compactor: Arc<dyn Compactor> = Arc::new(FixedSpan { range: 1..3 });
    let (loop_, mut session, _events) =
        test_loop(compactor, Arc::clone(&writer)).await;

    // The seeded fold is in the model-visible order: the steer waits
    // for the result that answers the open call.
    assert_eq!(
        session.transcript.surface_seqs(),
        &[1, 2, 4, 3],
        "the resumed transcript is in the model-visible order"
    );

    compact(&loop_, &mut session, 0)
        .await
        .expect("the pass runs");
    assert_eq!(
        rows_with(&writer, "compaction").await.len(),
        1,
        "the pass wrote its replace"
    );

    // The durable log folds to the same surface the live transcript
    // holds: the tool_use and its result are shadowed, the steer
    // survives.
    let rows = writer.read().await.expect("read the log");
    let surface =
        uva_session::try_fold_log_surface(&rows).expect("the log folds");
    // The compaction row is the seventh: four steered rows, the audit
    // start row, the summary row, then the replace.
    assert_eq!(surface.surface_seqs, vec![1, 7, 3]);
    assert_eq!(
        session.transcript.surface_seqs(),
        surface.surface_seqs,
        "the live surface and the resumed one are the same surface"
    );
    let (asked, answered) = tool_ids(
        &surface
            .messages
            .iter()
            .filter_map(uva_session::surface_message)
            .collect::<Vec<_>>(),
    );
    assert!(
        answered.is_empty(),
        "the shadowed tool_use takes its result with it: {answered:?}"
    );
    assert!(asked.is_empty(), "the tool_use is shadowed too");

    // The request the next step assembles carries the same shape.
    let request = loop_
        .build_request(&mut session, 1)
        .await
        .expect("the request assembles");
    let (asked, answered) = tool_ids(request.as_ref());
    assert!(
        answered.is_empty(),
        "the request must not carry an orphaned tool result: {answered:?}"
    );
    assert!(asked.is_empty());
}
