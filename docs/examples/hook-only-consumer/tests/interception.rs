//! The behavioral half of the proof: a real run, driven through the
//! dev-only harness, sees this crate's handler on `tool/pre-execute`.
//!
//! - the handler reads the tool name, the call id and the arguments;
//! - a `Block` short-circuits the tool and leaves a `hook_block` row;
//! - dropping the registration withdraws it.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use futures::StreamExt;
use uva_core::{
    AtomicCancelToken,
    CancelToken,
    Error,
    SystemClock,
    spawn::{BoxFuture, Spawner},
};
use uva_harness::{Agent, AgentEvent, AgentInput, Host, ReActAgent};
use uva_hook::HookRegistry;
use uva_hook_only_consumer::{CallGate, CallLog, SeenCall, install};
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    ToolUse,
};
use uva_session::{
    LogWriter,
    SessionSink,
    in_memory::InMemorySessionSink,
};
use uva_tool::{Tool, ToolOutput, ToolRegistry};

/// Where detached work goes; no runtime crate is in either tree.
struct ThreadSpawner;

impl Spawner for ThreadSpawner {
    fn spawn(&self, task: BoxFuture<()>) {
        thread::spawn(move || futures::executor::block_on(task));
    }

    fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) {
        thread::spawn(f);
    }
}

/// Ask for `echo` once, then answer.
struct ScriptedProvider {
    calls: Arc<AtomicUsize>,
}

#[uva_hook::async_trait]
impl ModelProvider for ScriptedProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "scripted"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "scripted-1".to_string(),
            provider: "scripted".to_string(),
            context_window: 8_192,
            max_output_tokens: 1_024,
            supports_tools: true,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let content = if call == 0 {
            Content::parts(vec![ContentPart::ToolUse(ToolUse {
                id: "call-1".to_string(),
                name: "echo".to_string(),
                input: serde_json::json!({ "text": "hello" }),
            })])
        } else {
            Content::text("done")
        };
        Ok(CompletionResponse {
            content,
            ..CompletionResponse::default()
        })
    }
}

/// Count calls and echo the `text` argument.
struct EchoTool {
    calls: Arc<AtomicUsize>,
}

#[uva_hook::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echo the `text` argument back."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
        })
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _context: &uva_tool::Context,
    ) -> ToolOutput {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = input
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("(missing)");
        ToolOutput::text(format!("echo: {text}"))
    }
}

fn provider() -> Arc<dyn ModelProvider> {
    Arc::new(ScriptedProvider {
        calls: Arc::new(AtomicUsize::new(0)),
    })
}

fn tool_registry() -> (Arc<ToolRegistry>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = ToolRegistry::new();
    registry.register_entry(uva_tool::registry::ToolEntry::new(Arc::new(
        EchoTool {
            calls: Arc::clone(&calls),
        },
    )));
    (Arc::new(registry), calls)
}

fn agent(
    registry: HookRegistry,
    provider: Arc<dyn ModelProvider>,
    tools: Arc<ToolRegistry>,
) -> ReActAgent {
    ReActAgent::new(provider, tools)
        .with_host(Host::default_policy("."))
        .expect("the empty host binds")
        .with_hook_registry(registry)
        .with_spawner(Arc::new(ThreadSpawner))
}

fn run(agent: &ReActAgent) -> Vec<AgentEvent> {
    let cancel: Arc<dyn CancelToken> = AtomicCancelToken::shared();
    let mut stream = futures::executor::block_on(
        agent.run(AgentInput::text("use echo"), cancel),
    );
    futures::executor::block_on(async move {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        events
    })
}

fn answer(events: &[AgentEvent]) -> String {
    let mut answer = String::new();
    for event in events {
        if let AgentEvent::ModelDone(done) = event {
            answer.clone_from(&done.text);
        }
    }
    answer
}

#[test]
fn the_handler_reads_the_tool_call_and_the_tool_runs() {
    let log = Arc::new(CallLog::new());
    let registry = HookRegistry::new();
    let _registration = install(
        &registry,
        CallGate::allowing(Arc::clone(&log)),
    )
    .expect("`tool/pre-execute` is a mount point");
    let (tools, executions) = tool_registry();

    let events = run(&agent(registry, provider(), tools));

    assert_eq!(
        log.calls(),
        vec![SeenCall {
            tool_name: "echo".to_owned(),
            call_id: "call-1".to_owned(),
            arguments: serde_json::json!({ "text": "hello" }).to_string(),
        }],
        "the handler read the tool name, the call id and the arguments"
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "an allowing gate does not stop the call"
    );
    assert_eq!(answer(&events), "done");
}

#[test]
fn a_block_short_circuits_the_tool_and_leaves_a_hook_block_row() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(
        Arc::clone(&sink) as Arc<dyn SessionSink>,
        Arc::new(SystemClock),
    ));
    let log = Arc::new(CallLog::new());
    let registry = HookRegistry::new();
    let _registration =
        install(&registry, CallGate::denying(Arc::clone(&log), "echo"))
            .expect("`tool/pre-execute` is a mount point");
    let (tools, executions) = tool_registry();

    let events = run(
        &agent(registry, provider(), tools).with_log_writer(writer),
    );

    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "the block short-circuits the tool body"
    );
    assert_eq!(log.calls().len(), 1, "the handler saw the refused call");
    let rows = futures::executor::block_on(sink.read()).expect("read the log");
    let blocks: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["type"] == "hook_block")
        .collect();
    assert_eq!(
        blocks.len(),
        1,
        "the external veto writes exactly one hook_block row: {rows:#?}"
    );
    assert_eq!(blocks[0]["data"]["hook"], "sample-call-gate");
    assert_eq!(
        blocks[0]["data"]["reason"],
        "the sample plugin denies `echo`"
    );
    assert_eq!(blocks[0]["data"]["tool"], "echo");
    assert_eq!(blocks[0]["data"]["call_id"], "call-1");
    assert_eq!(answer(&events), "done");
}

#[test]
fn dropping_the_registration_withdraws_it() {
    let log = Arc::new(CallLog::new());
    let registry = HookRegistry::new();
    let registration = install(
        &registry,
        CallGate::denying(Arc::clone(&log), "echo"),
    )
    .expect("`tool/pre-execute` is a mount point");
    drop(registration);
    let (tools, executions) = tool_registry();

    let events = run(&agent(registry, provider(), tools));

    assert!(
        log.calls().is_empty(),
        "the withdrawn registration never runs"
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "with no registration the call proceeds"
    );
    assert_eq!(answer(&events), "done");
}
