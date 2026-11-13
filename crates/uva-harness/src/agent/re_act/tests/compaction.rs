//! Compaction wiring: the step boundary plans, summarises, writes
//! one `Compaction` replace and re-derives the transcript. Without
//! an installed compactor the loop never compacts.

use std::sync::{
    Arc,
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uva_core::{CancelToken, Error, SystemClock};
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
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
    SummaryCompactor,
    TokenBudget,
    derive_history,
    in_memory::InMemorySessionSink,
};
use uva_tool::ToolRegistry;

use super::support::*;
use crate::{
    agent::{Agent, re_act::ReActAgent},
    events::{AgentEvent, SessionEndReason, SystemEvent, WarningKind},
    input::AgentInput,
};

const REASONING: &str = "summary: four turns about x";

// The summariser's provider: records its request and replies with
// fixed text (or a fixed error).
struct SummaryProvider {
    reply: Result<String, String>,
    requests: Mutex<Vec<CompletionRequest>>,
    calls: AtomicUsize,
}

impl SummaryProvider {
    fn replying(text: &str) -> Arc<Self> {
        Arc::new(Self {
            reply: Ok(text.to_string()),
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        })
    }

    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            reply: Err(message.to_string()),
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        })
    }

    fn last_prompt(&self) -> String {
        self.requests
            .lock()
            .expect("requests lock")
            .last()
            .map(|request| {
                request
                    .messages
                    .iter()
                    .filter_map(|message| message.content.extract_text())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }
}

#[async_trait]
impl ModelProvider for SummaryProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "summariser"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "summariser".into(),
            provider: "test".into(),
            context_window: 128_000,
            max_output_tokens: 4096,
            supports_tools: false,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        self.requests.lock().expect("requests lock").push(request);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = self
            .reply
            .as_ref()
            .map_err(|message| Error::provider(message.clone()))?
            .clone();
        Ok(CompletionResponse {
            id: "summary".into(),
            model: "summariser".into(),
            content: Content::text(text),
            usage: TokenUsage::default(),
            cached: false,
            replay_state: None,
            stop_reason: None,
        })
    }
}

// A log whose four user turns each cost about `chars / 4` heuristic
// tokens.
async fn seeded_writer_with(chars: usize) -> Arc<LogWriter> {
    let sink = Arc::new(InMemorySessionSink::new("compaction"));
    let writer = Arc::new(LogWriter::new(sink, Arc::new(SystemClock)));
    for turn in 0..4 {
        writer
            .append(json!({
                "type": "user_message",
                "data": uva_provider::Message::user(format!(
                    "turn {turn}: {}",
                    "x".repeat(chars)
                )),
            }))
            .await
            .expect("seed the turn");
    }
    writer.flush().await.expect("flush the seeded rows");
    writer
}

// ~1 000 tokens per turn: one turn alone outlives the tail budget.
async fn seeded_writer() -> Arc<LogWriter> {
    seeded_writer_with(4_000).await
}

async fn run(
    writer: Arc<LogWriter>,
    compactor: Option<Arc<dyn Compactor>>,
) -> (Vec<AgentEvent>, Arc<CapturingProvider>) {
    let provider = Arc::new(
        CapturingProvider::new(vec![done_chunk("ok")])
            .with_context_window(1_000),
    );
    let dyn_provider: Arc<dyn ModelProvider> = provider.clone();
    let mut agent =
        ReActAgent::new(dyn_provider, Arc::new(ToolRegistry::new()))
            .with_log_writer(Arc::clone(&writer));
    if let Some(compactor) = compactor {
        agent = agent.with_compactor(compactor);
    }
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    (events, provider)
}

async fn run_scripted(
    writer: Arc<LogWriter>,
    compactor: Option<Arc<dyn Compactor>>,
    provider: Arc<CapturingProvider>,
    registry: Arc<ToolRegistry>,
) -> Vec<AgentEvent> {
    let mut agent = ReActAgent::new(provider, registry)
        .with_log_writer(Arc::clone(&writer));
    if let Some(compactor) = compactor {
        agent = agent.with_compactor(compactor);
    }
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

async fn rows(writer: &LogWriter) -> Vec<Value> {
    writer.read().await.expect("read the log")
}

fn kinds<'a>(rows: &'a [Value], tag: &'a str) -> Vec<&'a Value> {
    rows.iter()
        .filter(|row| row.get("type").and_then(Value::as_str) == Some(tag))
        .collect()
}

#[tokio::test]
async fn a_compactor_writes_one_replace_row_with_its_provenance() {
    // ~3 000 tokens per turn with a roomy window: after one
    // compaction the kept tail fits, so iteration 1 neither compacts
    // again nor trips the truncating manager.
    let writer = seeded_writer_with(12_000).await;
    let summariser = SummaryProvider::replying(REASONING);
    let provider: Arc<dyn ModelProvider> = summariser.clone();
    let compactor: Arc<dyn Compactor> =
        Arc::new(SummaryCompactor::new(provider, "summariser"));
    let (events, _provider) = run(Arc::clone(&writer), Some(compactor)).await;

    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Completed,
            })
        )),
        "the run must complete"
    );
    assert_eq!(
        summariser.calls.load(Ordering::SeqCst),
        1,
        "one compaction pass, one summary call"
    );
    assert!(
        matches!(
            events.iter().find(|event| matches!(
                event,
                AgentEvent::System(SystemEvent::CompactionStarted { .. })
            )),
            Some(AgentEvent::System(SystemEvent::CompactionStarted {
                start: 0,
                end: 3,
                ..
            }))
        ),
        "the live event must name the shadowed span"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::System(SystemEvent::CompactionFinished {
                outcome: uva_session::CompactionLifecycleOutcome::Committed,
                shadowed_messages: 3,
                ..
            })
        )),
        "the finished event reports the committed span"
    );

    let log = rows(&writer).await;
    let replacements = kinds(&log, "compaction");
    assert_eq!(replacements.len(), 1, "exactly one replace row");
    let row = replacements[0];
    assert_eq!(row["surface_op"]["start"], json!(0));
    assert_eq!(row["surface_op"]["end"], json!(3));
    assert_eq!(row["data"]["summary"], json!(REASONING));
    assert_eq!(
        row["data"]["provider"],
        json!("summariser"),
        "the row names the provider that wrote the summary"
    );
    assert_eq!(row["data"]["model"], json!("summariser"));
    assert!(
        row["data"]["usage"].is_object(),
        "the row carries the provider's usage report"
    );
    let cited = row["surface_op"]["source_event_seqs"]
        .as_array()
        .expect("cited seqs")
        .clone();
    assert_eq!(cited.len(), 3, "every shadowed row is cited");
    let replacement_seq = row["seq"].as_u64().expect("seq");
    for seq in &cited {
        let seq = seq.as_u64().expect("seq");
        assert!(
            seq < replacement_seq,
            "a replace cannot cite its own or a later row: {seq} >= {replacement_seq}"
        );
    }

    // The replace folds, and the derived history is the summary plus
    // the kept tail.
    let history = derive_history(&log).expect("the log still folds");
    assert_eq!(
        history.len(),
        3,
        "summary + the newest turn + the assistant reply"
    );
    assert!(
        history[0]
            .content
            .extract_text()
            .is_some_and(|text| { text.contains("summary: four turns") })
    );

    let prompt = summariser.last_prompt();
    assert!(
        prompt.contains("turn 0:"),
        "the summariser reads the shadowed span"
    );
    assert!(
        !prompt.contains("turn 3:"),
        "the kept tail is not summarised"
    );
}

#[tokio::test]
async fn without_a_compactor_no_compaction_row_is_written() {
    let writer = seeded_writer().await;
    let (events, _provider) = run(Arc::clone(&writer), None).await;

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        })
    )));
    let log = rows(&writer).await;
    assert!(
        kinds(&log, "compaction").is_empty(),
        "no compactor means the surface is never rewritten"
    );
    assert_eq!(
        derive_history(&log).expect("fold").len(),
        5,
        "four user turns plus the assistant reply"
    );
}

#[tokio::test]
async fn a_failing_summariser_warns_and_the_run_completes() {
    let writer = seeded_writer().await;
    let summariser = SummaryProvider::failing("summariser down");
    let provider: Arc<dyn ModelProvider> = summariser.clone();
    let compactor: Arc<dyn Compactor> =
        Arc::new(SummaryCompactor::new(provider, "summariser"));
    let (events, _provider) = run(Arc::clone(&writer), Some(compactor)).await;

    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Completed,
            })
        )),
        "a summariser failure must not fail the turn"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::System(SystemEvent::Warning {
                kind: WarningKind::ContextCompaction,
                message,
                ..
            }) if message.contains("summariser down")
        )),
        "the failure must be visible"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::System(SystemEvent::CompactionFinished {
                outcome: uva_session::CompactionLifecycleOutcome::Failed,
                ..
            })
        )),
        "the live event reports the failed attempt"
    );

    let log = rows(&writer).await;
    assert!(
        kinds(&log, "compaction").is_empty(),
        "a failed summary writes no replace"
    );
    assert_eq!(
        derive_history(&log).expect("fold").len(),
        5,
        "the surface is unchanged"
    );
    let ends = kinds(&log, "compaction_end");
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0]["outcome"], json!("failed"));
    let error = ends[0]["data"]["error"]
        .as_str()
        .expect("the failure is recorded");
    assert!(
        error.contains("summariser down"),
        "the row names the provider failure: {error}"
    );
}

// A compactor that reports nothing for provenance, proving the
// harness writes its own bound plan instead.
struct MisreportingCompactor;

impl Compactor for MisreportingCompactor {
    fn plan(
        &self,
        history: &[Message],
        _budget: TokenBudget,
    ) -> Option<CompactionPlan> {
        let range = 0..history.len().saturating_sub(1);
        if range.is_empty() {
            return None;
        }
        Some(CompactionPlan {
            range: range.clone(),
            shadowed_seqs: Vec::new(),
            keep_recent: 1,
            shadowed: history[range].to_vec(),
            shadowed_tokens: 0,
        })
    }

    fn summarize(
        &self,
        _plan: &CompactionPlan,
        _cancel: Arc<dyn CancelToken>,
    ) -> futures::future::BoxFuture<
        'static,
        Result<CompactionOutcome, CompactionError>,
    > {
        Box::pin(async {
            Ok(CompactionOutcome {
                summary: "CUSTOM SUMMARY".to_string(),
                // Deliberately wrong: the harness must not write it.
                shadowed_seqs: vec![9_999],
                shadowed_tokens: 1,
                provider: None,
                model: None,
                usage: None,
            })
        })
    }
}

#[tokio::test]
async fn the_durable_replace_cites_the_bound_plan_not_the_outcome() {
    let writer = seeded_writer().await;
    let compactor: Arc<dyn Compactor> = Arc::new(MisreportingCompactor);
    let (events, _provider) = run(Arc::clone(&writer), Some(compactor)).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        })
    )));

    let log = rows(&writer).await;
    let replacements = kinds(&log, "compaction");
    assert_eq!(replacements.len(), 1);
    let row = replacements[0];
    assert_eq!(
        row["surface_op"]["source_event_seqs"],
        json!([1, 2, 3]),
        "the fold's seqs win over the compactor's echo"
    );
    let tokens = row["data"]["shadowed_token_count"]
        .as_u64()
        .expect("token count");
    assert!(
        tokens > 1_000,
        "the plan's pre-prune price is written, not the echo: {tokens}"
    );
    assert_eq!(row["data"]["provider"], Value::Null);
    assert_eq!(row["data"]["model"], Value::Null);
    assert!(
        derive_history(&log).is_ok(),
        "the row the harness wrote is foldable"
    );
}

#[tokio::test]
async fn the_next_request_carries_the_summary_and_not_the_shadowed_turns() {
    // ~3 000 tokens per turn with a roomy window: after one
    // compaction the kept tail fits, so iteration 1 neither compacts
    // again nor trips the truncating manager.
    let writer = seeded_writer_with(12_000).await;
    let summariser = SummaryProvider::replying(REASONING);
    let provider: Arc<dyn ModelProvider> = summariser.clone();
    let compactor: Arc<dyn Compactor> =
        Arc::new(SummaryCompactor::new(provider, "summariser"));
    let (registry, _executions) = echo_registry();
    let calls = Arc::new(
        CapturingProvider::new(vec![
            tool_call_response(vec![ToolUse {
                id: "c1".to_string(),
                name: "echo".to_string(),
                input: json!({"text": "hi"}),
            }]),
            done_chunk("ok"),
        ])
        .with_context_window(10_000),
    );

    let events = run_scripted(
        Arc::clone(&writer),
        Some(compactor),
        Arc::clone(&calls),
        registry,
    )
    .await;
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        })
    )));

    let captured = calls.captured.lock().await;
    assert!(
        captured.len() >= 2,
        "the tool turn must be followed by a second request"
    );
    let next = captured.last().expect("next request");
    assert_eq!(next[0].role, Role::System);
    let summary_at = next
        .iter()
        .position(|message| {
            message
                .content
                .extract_text()
                .is_some_and(|text| text.contains("summary: four turns"))
        })
        .expect("the next request must carry the summary");
    assert_eq!(next[summary_at].role, Role::Assistant);
    for message in next.iter() {
        let text = message.content.extract_text().unwrap_or_default();
        assert!(
            !text.contains("turn 0:") && !text.contains("turn 2:"),
            "a shadowed turn reached the next request: {text}"
        );
    }
    assert!(
        next.iter().any(|message| {
            message
                .content
                .extract_text()
                .is_some_and(|text| text.contains("turn 3:"))
        }),
        "the kept tail stays in the request"
    );
}

#[tokio::test]
async fn a_replace_citing_a_later_seq_does_not_fold() {
    let writer = seeded_writer().await;
    writer
        .append(json!({
            "type": "compaction",
            "data": {"summary": "bad"},
            "surface_op": {
                "start": 0,
                "end": 1,
                "source_event_seqs": [9_999],
            },
        }))
        .await
        .expect("append the bad replace");
    writer.flush().await.expect("flush");

    let log = rows(&writer).await;
    assert!(
        derive_history(&log).is_err(),
        "the fold rejects a cite that precedes nothing"
    );
}
