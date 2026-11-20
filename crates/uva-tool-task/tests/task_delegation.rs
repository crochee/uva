//! End-to-end delegation tests: a parent `ReActAgent` with the
//! `task` tool registered in its tool registry runs a registered peer
//! as a child and commits the child's answer as the tool result.
//!
//! The tool is an ordinary registry entry — no interceptor, no loop
//! seam — so the tool list the model sees is the registry's, and the
//! depth cap is the tool's own bookkeeping.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::Mutex as TokioMutex;
use uva_agent::{Agent, AgentEntry, AgentInput, AgentRegistry};
use uva_core::{AtomicCancelToken, CancelToken, Error, registry::Registry};
use uva_events::AgentEvent;
use uva_harness::{PromptContext, ReActAgent};
use uva_provider::{
    CompletionResponse,
    ContentPart,
    ProviderConfig,
    Role,
    SamplingResult,
    StreamChunk,
    TextContent,
    TokenUsage,
    ToolDefinition,
    ToolUse,
    traits::ModelProvider,
    types::{CompletionRequest, ModelConfig},
};
use uva_tool::{Tool, ToolRegistry};
use uva_tool_task::{MAX_SUBAGENT_DEPTH, TaskTool};

/// Provider that records every `CompletionRequest` (messages +
/// tool definitions) and replays a scripted chunk sequence —
/// used to assert what the loop actually sends to the model.
struct CapturingProvider {
    captured_tools: Arc<TokioMutex<Vec<Arc<Vec<ToolDefinition>>>>>,
    scripted: Arc<TokioMutex<Vec<Vec<StreamChunk>>>>,
}

impl CapturingProvider {
    fn new(scripted: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            captured_tools: Arc::new(TokioMutex::new(Vec::new())),
            scripted: Arc::new(TokioMutex::new(scripted)),
        }
    }
}

#[async_trait]
impl ModelProvider for CapturingProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "capturing"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "capturing".into(),
            provider: "test".into(),
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
        unreachable!("streaming path")
    }

    async fn complete_with_stream(
        &self,
        request: CompletionRequest,
        _cancel: Option<Arc<dyn CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<SamplingResult, Error> {
        self.captured_tools.lock().await.push(request.tools.clone());
        let chunks = {
            let mut g = self.scripted.lock().await;
            if g.is_empty() {
                vec![StreamChunk::IsDone {
                    result: Box::new(SamplingResult::default()),
                }]
            } else {
                g.remove(0)
            }
        };
        let mut assembler = uva_provider::BlockAssembler::new();
        for c in chunks {
            assembler.push(c.clone());
            on_delta(c);
        }
        let (turn, _parts, _incomplete) = assembler.finalize_assistant();
        Ok(turn)
    }
}

/// Provider stub for the child agent: echoes the trailing user
/// prompt (skipping the runtime-context snapshot frame) back as
/// the child's answer so tests can assert prompt propagation.
#[derive(Clone)]
struct EchoChildProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ModelProvider for EchoChildProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "echo-child"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "echo-child".to_string(),
            provider: "test".to_string(),
            context_window: 128_000,
            max_output_tokens: 1024,
            supports_tools: false,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        unreachable!("streaming path")
    }

    async fn complete_with_stream(
        &self,
        request: CompletionRequest,
        _cancel_token: Option<Arc<dyn CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<SamplingResult, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let snapshot_prefix = "Current runtime context. \
            This snapshot supersedes earlier runtime-context \
            snapshots.";
        let user_text = request
            .messages
            .iter()
            .rev()
            .filter(|m| m.role == Role::User)
            .find_map(|m| match &m.content {
                uva_provider::Content::Single(ContentPart::Text(t)) => {
                    if t.text.starts_with(snapshot_prefix) {
                        None
                    } else {
                        Some(t.text.clone())
                    }
                }
                _ => None,
            })
            .unwrap_or_default();
        let text = format!("child-answered: {user_text}");
        let turn = SamplingResult {
            text,
            tool_calls: vec![],
            reasoning: String::new(),
            reasoning_signature: None,
            usage: TokenUsage::default(),
            stop_reason: None,
        };
        on_delta(StreamChunk::IsDone {
            result: Box::new(turn.clone()),
        });
        Ok(turn)
    }
}

/// A peer that answers with a fixed text and records how often it ran.
struct StubPeer {
    answer: String,
    runs: Arc<AtomicUsize>,
    descriptor: uva_core::agent::AgentDescriptor,
}

impl StubPeer {
    fn new(answer: &str, runs: Arc<AtomicUsize>) -> Self {
        Self {
            answer: answer.to_string(),
            runs,
            descriptor: {
                let mut descriptor = uva_harness::default_descriptor(
                    "a peer that answers with a fixed text".to_string(),
                );
                descriptor.name = "child".to_string();
                descriptor
            },
        }
    }
}

#[async_trait]
impl Agent for StubPeer {
    fn descriptor(&self) -> &uva_core::agent::AgentDescriptor {
        &self.descriptor
    }

    async fn run(
        &self,
        _input: AgentInput,
        _cancel: Arc<dyn CancelToken>,
    ) -> std::pin::Pin<
        Box<dyn futures::Stream<Item = AgentEvent> + Send + 'static>,
    > {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let answer = self.answer.clone();
        Box::pin(futures::stream::iter(vec![AgentEvent::Model(
            ContentPart::Text(TextContent {
                text: answer,
                cache_control: None,
            }),
        )]))
    }
}

impl uva_core::registry::RegistryItem for StubPeer {
    fn name(&self) -> &str {
        "child"
    }

    fn description(&self) -> &str {
        "a peer that answers with a fixed text"
    }
}

fn task_call(id: &str, agent: &str, prompt: &str) -> Vec<StreamChunk> {
    vec![StreamChunk::IsDone {
        result: Box::new(SamplingResult {
            text: String::new(),
            tool_calls: vec![ToolUse {
                id: id.into(),
                name: "task".into(),
                input: serde_json::json!({
                    "agent": agent,
                    "prompt": prompt
                }),
            }],
            reasoning: String::new(),
            reasoning_signature: None,
            usage: TokenUsage::default(),
            ..Default::default()
        }),
    }]
}

fn text_only(text: &str) -> Vec<StreamChunk> {
    vec![StreamChunk::IsDone {
        result: Box::new(SamplingResult {
            text: text.into(),
            tool_calls: vec![],
            reasoning: String::new(),
            reasoning_signature: None,
            usage: TokenUsage::default(),
            ..Default::default()
        }),
    }]
}

async fn echo_child_registry() -> AgentRegistry {
    let registry = AgentRegistry::new();
    let child = ReActAgent::with_descriptor(
        Arc::new(EchoChildProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        }),
        Arc::new(ToolRegistry::new()),
        PathBuf::new(),
        uva_harness::default_descriptor("child instructions".to_string()),
        Arc::new(PromptContext::default()),
    );
    let child = child.with_name("child");
    registry
        .put(AgentEntry::new(Arc::new(child)))
        .await
        .expect("register child");
    registry
}

/// A registry holding the `task` tool over `peers`.
fn registry_with_task(peers: Arc<AgentRegistry>) -> Arc<ToolRegistry> {
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(TaskTool::new(
        peers,
    ))));
    registry
}

#[tokio::test]
async fn task_tool_runs_a_peer_and_commits_its_answer() {
    // Parent: first completion issues ONE `task` tool call to
    // peer `"child"`; second completion is text-only so the
    // loop terminates. The child echoes its user prompt.
    let parent_provider = Arc::new(CapturingProvider::new(vec![
        task_call("call_task_1", "child", "explore the topic"),
        text_only("done"),
    ]));

    let child_registry = Arc::new(echo_child_registry().await);

    let parent = ReActAgent::with_descriptor(
        parent_provider.clone(),
        registry_with_task(Arc::clone(&child_registry)),
        PathBuf::new(),
        uva_harness::default_descriptor("parent instructions".to_string()),
        Arc::new(PromptContext::default()),
    );

    let mut events = Vec::new();
    let mut stream = parent
        .run(
            AgentInput::text("hi"),
            Arc::new(AtomicCancelToken::shared()),
        )
        .await;
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }
    drop(stream);

    // The parent tool list must have carried the `task` tool
    // definition into the LLM request.
    let captured_tools = parent_provider.captured_tools.lock().await;
    let first_tools = captured_tools.first().expect("parent request tools");
    let tool_names: Vec<&str> =
        first_tools.iter().map(|d| d.name.as_str()).collect();
    assert!(
        tool_names.contains(&"task"),
        "parent tool list must include the task tool; got {tool_names:?}"
    );
    drop(captured_tools);

    // The child's final text must reach the parent history as
    // the `task` tool result.
    let tool_result_text = events.iter().any(|e| {
        matches!(e, AgentEvent::Model(ContentPart::ToolResult(tr))
        if tr.tool_name.as_deref() == Some("task")
            && tr.content.iter().any(|p| {
                matches!(p, ContentPart::Text(t)
                    if t.text.contains("child-answered: explore the topic"))
            }))
    });
    assert!(
        tool_result_text,
        "child answer must be committed as the task tool result"
    );

    // The parent's final output must be the second completion's
    // text.
    assert!(
        events.iter().any(|e| {
            matches!(e, AgentEvent::Model(ContentPart::Text(t))
                    if t.text == "done")
        }),
        "parent must finish with its own completion text"
    );
}

#[tokio::test]
async fn task_tool_unknown_agent_returns_error_result() {
    let parent_provider = Arc::new(CapturingProvider::new(vec![
        task_call("call_task_x", "ghost", "hi"),
        text_only("recovered"),
    ]));

    let parent = ReActAgent::with_descriptor(
        parent_provider.clone(),
        registry_with_task(Arc::new(AgentRegistry::new())),
        PathBuf::new(),
        uva_harness::default_descriptor(String::new()),
        Arc::new(PromptContext::default()),
    );

    let mut saw_error_result = false;
    let mut stream = parent
        .run(
            AgentInput::text("hi"),
            Arc::new(AtomicCancelToken::shared()),
        )
        .await;
    while let Some(ev) = stream.next().await {
        if let AgentEvent::Model(ContentPart::ToolResult(tr)) = &ev
            && tr.tool_name.as_deref() == Some("task")
            && tr.content.iter().any(|p| {
                matches!(p, ContentPart::Text(t)
                        if t.text.contains("unknown peer agent `ghost`"))
            })
        {
            saw_error_result = true;
        }
    }
    drop(stream);
    assert!(
        saw_error_result,
        "unknown agent must surface as an error tool result"
    );
}

#[tokio::test]
async fn task_tool_depth_guard_refuses_spawn_at_limit() {
    // The depth cap is the tool's own bookkeeping: every dispatch
    // records the child it started under the child's session id, and
    // a dispatch whose parent session id is a recorded child runs at
    // that child's depth. A run at `MAX_SUBAGENT_DEPTH` must refuse
    // further delegation with an error tool result instead of
    // recursing forever.
    let peers = Arc::new(AgentRegistry::new());
    let runs = Arc::new(AtomicUsize::new(0));
    peers
        .put(AgentEntry::new(Arc::new(StubPeer::new(
            "deep work",
            Arc::clone(&runs),
        ))))
        .await
        .expect("register the peer");

    let tool = TaskTool::new(Arc::clone(&peers));
    let mut parent_session = "root".to_string();
    for _ in 0..MAX_SUBAGENT_DEPTH {
        let context = uva_tool::Context::new(
            parent_session.clone(),
            std::env::temp_dir(),
        );
        let output = tool
            .call(
                serde_json::json!({"agent": "child", "prompt": "go deeper"}),
                &context,
            )
            .await;
        assert!(
            !output.is_error.unwrap_or(false),
            "the child runs below the cap: {output:?}"
        );
        let text = output
            .content
            .iter()
            .filter_map(|part| part.text())
            .collect::<String>();
        let child_session = text
            .rsplit_once("[subagent session: ")
            .and_then(|(_, tail)| tail.split(']').next())
            .expect("the result names the child session")
            .to_string();
        parent_session = child_session;
    }

    // The next dispatch would run at `MAX_SUBAGENT_DEPTH`: refused.
    let context =
        uva_tool::Context::new(parent_session.clone(), std::env::temp_dir());
    let output = tool
        .call(
            serde_json::json!({"agent": "child", "prompt": "too deep"}),
            &context,
        )
        .await;
    assert!(
        output.is_error.unwrap_or(false),
        "the depth-limited dispatch must fail: {output:?}"
    );
    let text = output
        .content
        .iter()
        .filter_map(|part| part.text())
        .collect::<String>();
    assert!(
        text.contains("maximum sub-agent depth"),
        "the refusal names the reason: {text}"
    );
    assert_eq!(
        runs.load(Ordering::SeqCst),
        MAX_SUBAGENT_DEPTH,
        "the refused dispatch runs no child"
    );
}

#[tokio::test]
async fn task_tool_is_absent_when_it_is_not_registered() {
    // No `task` entry in the registry → the `task` definition must
    // NOT appear in the tool list; the loop stays a plain harness.
    let parent_provider = Arc::new(CapturingProvider::new(vec![
        task_call("call_task_n", "child", "hi"),
        text_only("ok"),
    ]));

    let parent = ReActAgent::with_descriptor(
        parent_provider.clone(),
        Arc::new(ToolRegistry::new()),
        PathBuf::new(),
        uva_harness::default_descriptor(String::new()),
        Arc::new(PromptContext::default()),
    );

    let mut events = Vec::new();
    let mut stream = parent
        .run(
            AgentInput::text("hi"),
            Arc::new(AtomicCancelToken::shared()),
        )
        .await;
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }
    drop(stream);

    let captured_tools = parent_provider.captured_tools.lock().await;
    let first_tools = captured_tools.first().expect("parent request tools");
    assert!(
        !first_tools.iter().any(|d| d.name == "task"),
        "task tool must be absent when it is not registered"
    );
    let _ = events;
}

/// The tool is a registry entry: the registry's own dispatch reaches
/// it, and the tool's definition rides the model's tool list.
#[tokio::test]
async fn the_registry_dispatches_the_task_tool() {
    let peers = Arc::new(AgentRegistry::new());
    let runs = Arc::new(AtomicUsize::new(0));
    peers
        .put(AgentEntry::new(Arc::new(StubPeer::new(
            "registry answer",
            Arc::clone(&runs),
        ))))
        .await
        .expect("register the peer");

    let registry = registry_with_task(Arc::clone(&peers));
    let context =
        uva_tool::Context::new("s1".to_string(), std::env::temp_dir());
    let mut stream = registry.dispatch_one(
        ToolUse {
            id: "call-1".to_string(),
            name: "task".to_string(),
            input: serde_json::json!({"agent": "child", "prompt": "hi"}),
        },
        &context,
    );
    let mut output = None;
    while let Some(item) = stream.next().await {
        if let uva_tool::StreamOutput::Result(result) = item {
            output = Some(result);
        }
    }
    let output = output.expect("the registry yields exactly one result");
    assert!(
        !output.is_error.unwrap_or(false),
        "the registry dispatches the tool: {output:?}"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the peer ran once");
}
