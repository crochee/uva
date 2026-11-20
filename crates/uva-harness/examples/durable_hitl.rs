//! Durable HITL smoke: a high-risk tool call suspends the run before it
//! runs, the request and the run's structural rows are durable in the
//! session log, and a later run resumes the *same* session from that
//! log with `AgentInput::resume_approving_all` — the approved call
//! executes exactly once and the run completes.
//!
//! There is no snapshot to save or load: the log is the resume source,
//! so the example doubles as the contract ("resume is a log replay")
//! written down as something that runs.
//!
//! Run with `cargo run --example durable_hitl -p uva-harness`.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use uva_core::{Error, SystemClock};
use uva_harness::{
    Agent,
    AgentEvent,
    AgentInput,
    ReActAgent,
    SessionEndReason,
    Suspension,
    SystemEvent,
};
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    Message,
    ModelConfig,
    ProviderConfig,
    SamplingResult,
    StreamChunk,
    TextContent,
    TokenUsage,
    traits::ModelProvider,
};
use uva_session::{LogWriter, in_memory::InMemorySessionSink, user_message};
use uva_tool::{
    Context,
    Tool,
    ToolAnnotations,
    ToolEntry,
    ToolOutput,
    ToolRegistry,
};

// One scripted sampling turn: either a tool call or a final answer.
enum Turn {
    ToolCall {
        id: &'static str,
        name: &'static str,
        input: serde_json::Value,
    },
    Text(&'static str),
}

/// Scripted provider that records the transcript of every call, so the
/// example can prove the resumed run replays the log rather than
/// starting from a fresh history.
#[derive(Default)]
struct RecordingProvider {
    scripted: AsyncMutex<Vec<Turn>>,
    seen: AsyncMutex<Vec<String>>,
    calls: AtomicUsize,
}

impl RecordingProvider {
    fn new(scripted: Vec<Turn>) -> Self {
        Self {
            scripted: AsyncMutex::new(scripted),
            seen: AsyncMutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
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
        _cancel_token: Option<Arc<dyn uva_core::CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<SamplingResult, Error> {
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

        let turn = {
            let mut scripted = self.scripted.lock().await;
            if scripted.is_empty() {
                Turn::Text("done")
            } else {
                scripted.remove(0)
            }
        };
        let mut assembler = uva_provider::BlockAssembler::new();
        let chunks = match turn {
            Turn::ToolCall { id, name, input } => vec![
                StreamChunk::ToolCallStart {
                    id: id.to_string(),
                    name: name.to_string(),
                    arguments: serde_json::Value::String(input.to_string()),
                },
                StreamChunk::ToolCallEnd { id: id.to_string() },
                Self::done_chunk(),
            ],
            Turn::Text(text) => {
                vec![Self::text_chunk(text), Self::done_chunk()]
            }
        };
        for chunk in chunks {
            assembler.push(chunk.clone());
            on_delta(chunk);
        }
        let (turn, _parts, _incomplete) = assembler.finalize_assistant();
        Ok(turn)
    }
}

/// A tool whose annotations make it high risk, so the default approval
/// policy holds it before it runs. Every execution bumps the counter,
/// which is how "did the approved call run?" is answered.
struct DeployTool {
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for DeployTool {
    fn name(&self) -> &str {
        "deploy"
    }

    fn description(&self) -> &str {
        "deploy the site"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        })
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        self.runs.fetch_add(1, Ordering::SeqCst);
        ToolOutput::text("deployed")
    }
}

async fn drain(
    stream: &mut (impl futures::Stream<Item = AgentEvent> + Unpin),
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

fn suspension_of(events: &[AgentEvent]) -> Option<Suspension> {
    events.iter().find_map(|event| match event {
        AgentEvent::System(SystemEvent::Suspended(suspension)) => {
            Some(suspension.clone())
        }
        _ => None,
    })
}

fn end_reason(events: &[AgentEvent]) -> Option<SessionEndReason> {
    events.iter().find_map(|event| match event {
        AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
            Some(reason.clone())
        }
        _ => None,
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // 1. The session's one writer. The host persisted this turn's user
    //    message; the run folds it back out of the log.
    let sink = Arc::new(InMemorySessionSink::new("durable-hitl"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let seed = user_message(
        serde_json::to_value(Message::user("deploy"))
            .expect("a user message serialises"),
        None,
    );
    writer.append_event(&seed).await.expect("seed the user row");
    writer.flush().await.expect("flush the seeded row");

    let runs = Arc::new(AtomicUsize::new(0));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(ToolEntry::new(Arc::new(DeployTool {
        runs: Arc::clone(&runs),
    })));

    // 2. First run: the model asks for the high-risk call, the policy
    //    holds it, and the suspension's rows close the turn durably.
    let first: Arc<dyn ModelProvider> =
        Arc::new(RecordingProvider::new(vec![Turn::ToolCall {
            id: "c1",
            name: "deploy",
            input: json!({}),
        }]));
    let agent = ReActAgent::new(first, Arc::clone(&registry))
        .with_log_writer(writer.clone());
    let mut stream = agent
        .run(
            AgentInput::text("deploy"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    let events = drain(&mut stream).await;

    let Suspension::AwaitingApproval { interrupts, resume } =
        suspension_of(&events).expect("the high-risk call holds");
    assert_eq!(interrupts.len(), 1, "one call is held");
    assert_eq!(interrupts[0].call_id, "c1");
    assert_eq!(end_reason(&events), Some(SessionEndReason::Suspended));
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "a held call must not execute"
    );

    let asked = writer
        .read()
        .await
        .expect("read the log")
        .into_iter()
        .filter(|row| row["type"] == "approval_asked")
        .count();
    assert_eq!(asked, 1, "the request is durable, not just broadcast");

    // 3. Resume the same session from the log: approving the interrupt
    //    executes the held call exactly once.
    let second = Arc::new(RecordingProvider::new(vec![Turn::Text("deployed")]));
    let provider: Arc<dyn ModelProvider> =
        Arc::clone(&second) as Arc<dyn ModelProvider>;
    let resumed = ReActAgent::new(provider, Arc::clone(&registry))
        .with_log_writer(writer.clone());
    let mut stream = resumed
        .run(
            AgentInput::text("continue").resume_approving_all(resume),
            Arc::new(CancellationToken::new()),
        )
        .await;
    let resumed_events = drain(&mut stream).await;

    assert_eq!(
        end_reason(&resumed_events),
        Some(SessionEndReason::Completed),
        "the approved run completes"
    );
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the approved call executes exactly once"
    );

    let seen = second.seen.lock().await;
    assert!(
        seen[0].contains("deploy"),
        "the resumed run replays the log's turns; saw: {}",
        seen[0]
    );

    println!("OK: suspended on approval, resumed from the log, deployed once");
}
