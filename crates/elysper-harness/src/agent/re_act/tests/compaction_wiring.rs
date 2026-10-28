//! Context-manager and compaction wiring: a caller-supplied manager is
//! the one the loop uses, and an installed `SummaryCompactor` is the
//! one that rewrites the transcript.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use elysper_context::{AgentState, ContextManager};
use elysper_core::{Error, SystemClock};
use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ModelConfig,
    ProviderConfig,
    SamplingResult,
    StreamChunk,
    TextContent,
    TokenUsage,
    traits::ModelProvider,
};
use elysper_session::{
    LogWriter,
    SummaryCompactor,
    derive_history,
    in_memory::InMemorySessionSink,
};
use elysper_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use super::{support::*, *};

struct SpyManager {
    prepares: Arc<AtomicUsize>,
}

#[async_trait]
impl ContextManager for SpyManager {
    async fn prepare(
        &self,
        _messages: &mut Vec<Message>,
        _state: &mut AgentState,
    ) {
        self.prepares
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn answer_script() -> ScriptedStreamProvider {
    ScriptedStreamProvider::new(vec![vec![
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: "done".to_string(),
            cache_control: None,
        })),
        StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: "done".to_string(),
                ..SamplingResult::default()
            }),
        },
    ]])
}

async fn run_to_completion(agent: &ReActAgent) -> Vec<SessionEndReason> {
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut reasons = Vec::new();
    while let Some(event) = stream.next().await {
        if let AgentEvent::System(SystemEvent::SessionEnded { reason }) = event
        {
            reasons.push(reason);
        }
    }
    reasons
}

#[tokio::test]
async fn caller_supplied_manager_runs_when_set_via_with_context_manager() {
    let prepares = Arc::new(AtomicUsize::new(0));
    let spy: Arc<dyn ContextManager> = Arc::new(SpyManager {
        prepares: Arc::clone(&prepares),
    });
    let provider: Arc<dyn ModelProvider> = Arc::new(answer_script());

    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_context_manager(spy);

    let reasons = run_to_completion(&agent).await;
    assert_eq!(
        reasons,
        vec![SessionEndReason::Completed],
        "the run must complete"
    );
    assert_eq!(
        prepares.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the caller-supplied manager must be the one used by the loop"
    );
}

// The summariser's provider: replies with fixed text and counts its
// calls, so "did the installed compactor run?" is observable.
struct SummaryProvider {
    calls: AtomicUsize,
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
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(CompletionResponse {
            id: "summary".into(),
            model: "summariser".into(),
            content: Content::text("summary of the shadowed span"),
            usage: TokenUsage::default(),
            cached: false,
            replay_state: None,
            stop_reason: None,
        })
    }
}

// Four user turns of ~1 000 heuristic tokens each.
async fn seeded_writer() -> Arc<LogWriter> {
    let sink = Arc::new(InMemorySessionSink::new("compaction-wiring"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    for turn in 0..4 {
        writer
            .append(json!({
                "type": "user_message",
                "data": elysper_provider::Message::user(format!(
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

#[tokio::test]
async fn an_installed_summary_compactor_rewrites_the_transcript() {
    let writer = seeded_writer().await;
    let summariser = Arc::new(SummaryProvider {
        calls: AtomicUsize::new(0),
    });
    let compactor = Arc::new(SummaryCompactor::new(
        Arc::clone(&summariser) as Arc<dyn ModelProvider>,
        "summariser",
    ));
    let provider: Arc<dyn ModelProvider> = Arc::new(
        CapturingProvider::new(vec![done_chunk("ok")])
            .with_context_window(1_000),
    );

    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_log_writer(Arc::clone(&writer))
        .with_compactor(compactor);

    let reasons = run_to_completion(&agent).await;
    assert_eq!(
        reasons,
        vec![SessionEndReason::Completed],
        "a compaction pass must not fail the turn"
    );
    assert_eq!(
        summariser.calls.load(Ordering::SeqCst),
        1,
        "the installed compactor is the one that ran"
    );

    let rows = writer.read().await.expect("read the log");
    let replaces: Vec<_> = rows
        .iter()
        .filter(|row| row["type"] == "compaction")
        .collect();
    assert_eq!(replaces.len(), 1, "one replace row: {replaces:?}");

    let history = derive_history(&rows).expect("the log still folds");
    assert!(
        history.len() < 5,
        "the replace shadows the seeded turns: {} messages left",
        history.len()
    );
    assert!(
        history[0]
            .content
            .extract_text()
            .is_some_and(|text| text.contains("summary of the shadowed span")),
        "the derived history opens on the summary"
    );
}
