//! Durable resume smoke: a run whose input carries a `thread_id`
//! persists a snapshot under `<thread_id>:<agent_name>` when it
//! ends non-`Completed` (here: cancelled), and a later run on the
//! same thread loads that snapshot instead of a fresh history.
//!
//! Run with `cargo run --example durable_hitl -p synthia-harness`.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt;
use synthia_core::Error;
use synthia_harness::{
    Agent,
    AgentInput,
    agent::checkpoint::{Checkpointer, InMemoryCheckpointer},
};
use synthia_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ProviderConfig,
    SamplingResult,
    StreamChunk,
    TextContent,
    TokenUsage,
    traits::ModelProvider,
};
use synthia_tool::registry::ToolRegistry;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

/// Scripted provider that records the transcript of every call, so
/// the example can prove the second run resumed from the snapshot.
#[derive(Default)]
struct RecordingProvider {
    scripted: AsyncMutex<Vec<&'static str>>,
    seen: AsyncMutex<Vec<String>>,
    calls: AtomicUsize,
    /// Fires this token on the first sampling call, so a run
    /// cancels *during* the turn rather than before it. A token
    /// already fired when `run` is called ends the session at
    /// setup, before there is a transcript worth persisting.
    cancel_on_sample: Option<CancellationToken>,
    fired: AtomicBool,
}

impl RecordingProvider {
    fn new(scripted: Vec<&'static str>) -> Self {
        Self {
            scripted: AsyncMutex::new(scripted),
            seen: AsyncMutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            cancel_on_sample: None,
            fired: AtomicBool::new(false),
        }
    }

    fn cancelling(
        scripted: Vec<&'static str>,
        token: CancellationToken,
    ) -> Self {
        Self {
            cancel_on_sample: Some(token),
            ..Self::new(scripted)
        }
    }

    fn text_chunk(text: &str) -> StreamChunk {
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: text.to_string(),
            cache_control: None,
        }))
    }

    fn done_chunk() -> StreamChunk {
        StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: String::new(),
                usage: TokenUsage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    ..Default::default()
                },
                ..Default::default()
            }),
        }
    }
}

#[async_trait]
impl ModelProvider for RecordingProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "recording"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "fake".to_string(),
            provider: "recording".to_string(),
            context_window: 128_000,
            max_output_tokens: 4096,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        unreachable!("streaming path should not call complete()")
    }

    async fn complete_with_stream(
        &self,
        request: CompletionRequest,
        _cancel_token: Option<Arc<dyn synthia_core::CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<CompletionResponse, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let transcript = request
            .messages
            .iter()
            .map(|m| {
                let role = m.role.as_str();
                let body = match &m.content {
                    Content::Single(ContentPart::Text(t)) => t.text.clone(),
                    other => format!("{other:?}"),
                };
                format!("{role}:{body}")
            })
            .collect::<Vec<_>>()
            .join(" | ");
        self.seen.lock().await.push(transcript);

        // Fire the cancel token after the transcript is recorded, so
        // the turn has done real work before it is interrupted —
        // which is what makes the snapshot worth resuming from.
        if !self.fired.swap(true, Ordering::SeqCst)
            && let Some(token) = self.cancel_on_sample.as_ref()
        {
            token.cancel();
        }

        let text = self
            .scripted
            .lock()
            .await
            .first()
            .copied()
            .unwrap_or("done");
        on_delta(Self::text_chunk(text));
        on_delta(Self::done_chunk());
        Ok(CompletionResponse {
            id: format!("resp-{}", self.calls.load(Ordering::SeqCst)),
            model: "fake".to_string(),
            content: Content::Single(ContentPart::Text(TextContent {
                text: text.to_string(),
                cache_control: None,
            })),
            usage: TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                ..Default::default()
            },
            cached: false,
            replay_state: None,
            stop_reason: None,
        })
    }
}

async fn drain(
    stream: &mut (impl futures::Stream<Item = synthia_harness::AgentEvent> + Unpin),
) {
    while let Some(ev) = stream.next().await {
        let _ = ev;
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cp = Arc::new(InMemoryCheckpointer::default());

    // 1. Cancelled run: the finalize path persists a snapshot under
    //    `<thread_id>:<agent_name>` because the run ended
    //    non-`Completed`. The cancel fires during the turn — a token
    //    already fired at `run` would end the session at setup, with
    //    no transcript to persist.
    let cancel = CancellationToken::new();
    let cancelled_provider =
        Arc::new(RecordingProvider::cancelling(vec!["ok"], cancel.clone()));
    let agent = synthia_harness::ReActAgent::new(
        cancelled_provider,
        Arc::new(ToolRegistry::new()),
    )
    .with_checkpointer(Arc::clone(&cp) as Arc<dyn Checkpointer>);
    let mut stream = agent
        .run(
            AgentInput::text("first visit").thread_id("thread-1"),
            Arc::new(cancel),
        )
        .await;
    drain(&mut stream).await;

    // 2. The snapshot is present under the documented key.
    let snapshot = cp
        .load("thread-1:agent")
        .expect("load")
        .expect("cancelled run must persist a snapshot");
    assert_eq!(snapshot.thread_id, "thread-1");
    assert_eq!(snapshot.agent_id, "agent");

    // 3. A later run on the same thread resumes from the snapshot:
    //    its first provider call sees the snapshot's transcript
    //    (plus this run's fresh user turn), proving the durable
    //    history replaced the freshly prepared one.
    let resumed_provider = Arc::new(RecordingProvider::new(vec!["resumed"]));
    let agent2 = synthia_harness::ReActAgent::new(
        Arc::clone(&resumed_provider) as Arc<dyn ModelProvider>,
        Arc::new(ToolRegistry::new()),
    )
    .with_checkpointer(Arc::clone(&cp) as Arc<dyn Checkpointer>);
    let mut stream2 = agent2
        .run(
            AgentInput::text("continue").thread_id("thread-1"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    drain(&mut stream2).await;

    let seen = resumed_provider.seen.lock().await;
    assert_eq!(
        resumed_provider.calls.load(Ordering::SeqCst),
        1,
        "resumed run must sample exactly once"
    );
    assert_eq!(seen.len(), 1, "provider transcript log must have one entry");
    assert!(
        seen[0].contains("first visit"),
        "resumed history must contain the persisted turn; got: {}",
        seen[0]
    );

    // 4. The resumed run completed, so its key is deleted again.
    assert!(
        cp.load("thread-1:agent").unwrap().is_none(),
        "Completed run must delete its resume key"
    );

    println!("OK: durable resume persisted + replayed on thread-1");
}
