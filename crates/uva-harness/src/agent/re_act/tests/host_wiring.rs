//! Host wiring: the per-agent plugin host reaches the loop's
//! decision points, and its decisions are the ones the loop obeys.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uva_core::SystemClock;
use uva_events::{SessionEndReason, SystemEvent, WarningKind};
use uva_provider::{ContentPart, Role, ToolUse};
use uva_session::{
    LogWriter,
    SessionEvent,
    SessionSink,
    in_memory::InMemorySessionSink,
};
use uva_steering::{HookDecision, HookEvent, RequestParams};
use uva_tool::ToolOutput;

use super::{support::*, *};
use crate::agent::host::{
    Gate,
    Hook,
    HookContext,
    Host,
    PromptSection,
    RequestTransformer,
    ResultTransformer,
};

// An observer that vetoes the `echo` tool.
struct EchoBlocker;

#[async_trait]
impl Hook for EchoBlocker {
    fn name(&self) -> &str {
        "echo-blocker"
    }

    async fn observe(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        match event {
            HookEvent::BeforeTool { tool_name, .. } if tool_name == "echo" => {
                HookDecision::Block {
                    reason: "echo is not allowed here".to_string(),
                }
            }
            _ => HookDecision::Allow,
        }
    }
}

// A gate installed through the contract crate's external registry.
struct RegistryBlocker;

#[async_trait]
impl Gate for RegistryBlocker {
    fn name(&self) -> &str {
        "registry-blocker"
    }

    async fn check(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        match event {
            HookEvent::BeforeTool { tool_name, .. } if tool_name == "echo" => {
                Ok(HookDecision::Block {
                    reason: "the registry denied echo".to_string(),
                })
            }
            _ => Ok(HookDecision::Allow),
        }
    }
}

// A gate that ends the run instead of blocking the call.
struct RegistryTerminator;

#[async_trait]
impl Gate for RegistryTerminator {
    fn name(&self) -> &str {
        "registry-terminator"
    }

    async fn check(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        match event {
            HookEvent::BeforeTool { tool_name, .. } if tool_name == "echo" => {
                Ok(HookDecision::Terminate {
                    reason: "the registry ended the run".to_string(),
                })
            }
            _ => Ok(HookDecision::Allow),
        }
    }
}

// A gate that cannot answer, on an observational point.
struct FailingObserverGate;

#[async_trait]
impl Gate for FailingObserverGate {
    fn name(&self) -> &str {
        "registry-observer"
    }

    async fn check(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        Err("the observer has no answer".to_string())
    }
}

// A gate that asks for an approval before every call.
struct ApprovalAsker;

#[async_trait]
impl Gate for ApprovalAsker {
    fn name(&self) -> &str {
        "approval-asker"
    }

    async fn check(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        Ok(HookDecision::Ask {
            reason: "this call needs an approval".to_string(),
        })
    }
}

// An observer that ends the run before it starts.
struct RunStopper;

#[async_trait]
impl Hook for RunStopper {
    fn name(&self) -> &str {
        "run-stopper"
    }

    async fn observe(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        HookDecision::Terminate {
            reason: "policy ended the run".to_string(),
        }
    }
}

// A transformer that rewrites the request's model.
struct ModelRewriter;

#[async_trait]
impl RequestTransformer for ModelRewriter {
    fn name(&self) -> &str {
        "model-rewriter"
    }

    async fn transform(
        &self,
        params: &mut RequestParams,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        params.model = "rewritten-model".to_string();
        Ok(())
    }
}

fn echo_tool_call() -> ToolUse {
    ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    }
}

#[tokio::test]
async fn a_host_observer_blocks_a_tool_call() {
    let mut host = Host::new();
    let _observer = host.hook(Arc::new(EchoBlocker));

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![echo_tool_call()]),
        done_chunk("blocked noted"),
    ]));
    let (registry, _executions) = echo_registry();
    let agent = ReActAgent::new(provider, registry)
        .with_host(host)
        .expect("bind the host");

    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    let committed = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Model(ContentPart::ToolResult(result)) => Some(result),
            _ => None,
        })
        .next()
        .expect("the vetoed call must still commit a tool result");
    assert_eq!(committed.is_error, Some(true));
    assert!(
        committed
            .content
            .iter()
            .any(|part| matches!(part, ContentPart::Text(text)
                if text.text.starts_with("[blocked by policy]"))),
        "the denial must carry the policy prefix: {:?}",
        committed.content
    );
}

// A hook veto is a durable fact: the log carries a `hook_block` row
// naming the hook that refused the call and the reason it gave, so
// a reloaded session says which policy blocked the call — the
// model-visible tool result only says that *something* did.
#[tokio::test]
async fn a_hook_veto_writes_a_hook_block_row() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let mut host = Host::new();
    let _observer = host.hook(Arc::new(EchoBlocker));

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![echo_tool_call()]),
        done_chunk("blocked noted"),
    ]));
    let (registry, _executions) = echo_registry();
    let agent = ReActAgent::new(provider, registry)
        .with_host(host)
        .expect("bind the host")
        .with_log_writer(writer);

    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    let blocks: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["type"] == "hook_block")
        .collect();
    assert_eq!(
        blocks.len(),
        1,
        "the veto writes exactly one hook_block row: {rows:#?}"
    );
    assert_eq!(blocks[0]["data"]["hook"], "echo-blocker");
    assert_eq!(blocks[0]["data"]["reason"], "echo is not allowed here");
    assert_eq!(blocks[0]["data"]["tool"], "echo");
    assert_eq!(blocks[0]["data"]["call_id"], "c1");
}

// An external registration goes through the same arbitration and the
// same durable row: the contract crate's registry is not a second,
// quieter interception path.
#[tokio::test]
async fn an_external_registry_veto_writes_a_hook_block_row() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let registry = uva_hook::HookRegistry::new();
    let _registration = registry
        .register_gate(uva_hook::seam::TOOL_PRE, Arc::new(RegistryBlocker))
        .expect("the external gate registers");

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![echo_tool_call()]),
        done_chunk("blocked noted"),
    ]));
    let (tool_registry, executions) = echo_registry();
    let agent = ReActAgent::new(provider, tool_registry)
        .with_hook_registry(registry)
        .with_log_writer(writer);

    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    assert_eq!(*executions.lock(), 0, "the vetoed call does not run");
    let rows = sink.read().await.expect("read");
    let blocks: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["type"] == "hook_block")
        .collect();
    assert_eq!(
        blocks.len(),
        1,
        "the external veto writes exactly one hook_block row: {rows:#?}"
    );
    assert_eq!(blocks[0]["data"]["hook"], "registry-blocker");
    assert_eq!(blocks[0]["data"]["reason"], "the registry denied echo");
    assert_eq!(blocks[0]["data"]["tool"], "echo");
    assert_eq!(blocks[0]["data"]["call_id"], "c1");
}

// `Terminate` refuses the call too — the model reads
// `[terminated by policy]` — so it owes the same durable row. Without
// it a reloaded session could not answer "who ended this call".
#[tokio::test]
async fn an_external_registry_termination_writes_a_hook_block_row() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let registry = uva_hook::HookRegistry::new();
    let _registration = registry
        .register_gate(uva_hook::seam::TOOL_PRE, Arc::new(RegistryTerminator))
        .expect("the external gate registers");

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![echo_tool_call()]),
        done_chunk("terminated noted"),
    ]));
    let (tool_registry, executions) = echo_registry();
    let agent = ReActAgent::new(provider, tool_registry)
        .with_hook_registry(registry)
        .with_log_writer(writer);

    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    assert_eq!(*executions.lock(), 0, "the refused call does not run");
    let rows = sink.read().await.expect("read");
    let blocks: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["type"] == "hook_block")
        .collect();
    assert_eq!(
        blocks.len(),
        1,
        "the termination writes exactly one hook_block row: {rows:#?}"
    );
    assert_eq!(blocks[0]["data"]["hook"], "registry-terminator");
    assert_eq!(blocks[0]["data"]["reason"], "the registry ended the run");
    assert_eq!(blocks[0]["data"]["tool"], "echo");
    assert_eq!(blocks[0]["data"]["call_id"], "c1");
}

// `llm/result` accepts no denial, so a gate that cannot answer there
// cannot refuse the pass — but the failure must not vanish into
// silence either: the dispatch reports it and the run publishes a
// `hook` warning naming the gate (the session controller persists a
// warning as a durable row, like every other harness warning).
#[tokio::test]
async fn a_gate_failure_at_an_observational_point_surfaces_a_warning() {
    let registry = uva_hook::HookRegistry::new();
    let _registration = registry
        .register_gate(
            uva_hook::seam::LLM_RESULT,
            Arc::new(FailingObserverGate),
        )
        .expect("the external gate registers");

    let provider = Arc::new(CapturingProvider::new(vec![done_chunk("done")]));
    let (tool_registry, _executions) = echo_registry();
    let agent =
        ReActAgent::new(provider, tool_registry).with_hook_registry(registry);

    let mut stream = agent
        .run(
            AgentInput::text("say done"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    let messages: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::System(SystemEvent::Warning {
                kind: WarningKind::Hook,
                message,
                ..
            }) => Some(message.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        messages.len(),
        1,
        "the failure is reported once, as a hook warning: {events:#?}"
    );
    assert!(messages[0].contains("registry-observer"), "{}", messages[0]);
    assert!(
        messages[0].contains("the observer has no answer"),
        "{}",
        messages[0]
    );
}

// The `hook_block` row is log-only: the model-visible surface is
// the same with the row and without it.
#[tokio::test]
async fn a_hook_block_row_leaves_the_folded_surface_unchanged() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let mut host = Host::new();
    let _observer = host.hook(Arc::new(EchoBlocker));

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![echo_tool_call()]),
        done_chunk("blocked noted"),
    ]));
    let (registry, _executions) = echo_registry();
    let agent = ReActAgent::new(provider, registry)
        .with_host(host)
        .expect("bind the host")
        .with_log_writer(writer);

    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    let events: Vec<SessionEvent> =
        rows.iter().filter_map(SessionEvent::from_value).collect();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, SessionEvent::HookBlock { .. })),
        "the run wrote a hook_block row: {rows:#?}"
    );
    let without: Vec<SessionEvent> = events
        .iter()
        .filter(|event| !matches!(event, SessionEvent::HookBlock { .. }))
        .cloned()
        .collect();
    let with_row = uva_session::fold_surface(&events).expect("fold");
    let without_row = uva_session::fold_surface(&without).expect("fold");
    assert_eq!(
        with_row.messages, without_row.messages,
        "the log-only row must not change the transcript the model sees"
    );
    assert_eq!(with_row.surface_seqs, without_row.surface_seqs);
}

#[tokio::test]
async fn a_host_gate_ask_without_a_log_is_refused() {
    let mut host = Host::new();
    let _gate = host.gate(Arc::new(ApprovalAsker));

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![echo_tool_call()]),
        done_chunk("refused noted"),
    ]));
    let (registry, executions) = echo_registry();
    let agent = ReActAgent::new(provider, registry)
        .with_host(host)
        .expect("bind the host");

    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::System(SystemEvent::Suspended(_))
        )),
        "no log means no resumable suspension: the call is refused"
    );
    let denial = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::Model(ContentPart::ToolResult(result)) => Some(result),
            _ => None,
        })
        .expect("the refusal is a model-visible result");
    assert_eq!(denial.is_error, Some(true));
    assert!(
        denial.content.iter().any(|part| matches!(part,
            ContentPart::Text(text)
                if text.text.contains("no session log"))),
        "the refusal names the missing log: {:?}",
        denial.content
    );
    assert_eq!(
        events.iter().find_map(|event| match event {
            AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                Some(reason.clone())
            }
            _ => None,
        }),
        Some(SessionEndReason::Completed)
    );
    assert_eq!(*executions.lock(), 0, "the refused call does not run");
}

#[tokio::test]
async fn a_host_observer_ends_the_run_before_the_first_sample() {
    let mut host = Host::new();
    let _observer = host.hook(Arc::new(RunStopper));

    let provider =
        Arc::new(CapturingProvider::new(vec![done_chunk("never sampled")]));
    let (registry, _executions) = echo_registry();
    let agent = ReActAgent::new(provider, registry)
        .with_host(host)
        .expect("bind the host");

    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::Model(ContentPart::ToolResult(_))
        )),
        "the run must end before the first sample"
    );
    let reason = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                Some(reason.clone())
            }
            _ => None,
        })
        .expect("the run must end with a reason");
    match reason {
        SessionEndReason::Error(message) => assert!(
            message.contains("policy ended the run"),
            "the end reason must carry the policy's reason: {message}"
        ),
        other => panic!("the run must end with the policy's reason: {other:?}"),
    }
}

#[tokio::test]
async fn the_host_rewrites_the_request_parameters() {
    let mut host = Host::new();
    let _transformer = host.request_transformer(Arc::new(ModelRewriter));

    let provider = Arc::new(CapturingProvider::new(vec![done_chunk("done")]));
    let (registry, _executions) = echo_registry();
    let agent = ReActAgent::new(provider.clone(), registry)
        .with_host(host)
        .expect("bind the host");

    let mut stream = agent
        .run(
            AgentInput::text("hello"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    let models = provider.captured_models.lock().await;
    assert_eq!(models.first().map(String::as_str), Some("rewritten-model"));
}

// The whole link, end to end: a plugin registers a prompt section on the
// host, one run goes out, and the model's system message carries it.
#[tokio::test]
async fn a_host_prompt_section_reaches_the_model() {
    let mut host = Host::new();
    let _section = host.prompt_section(PromptSection {
        title: "Audit".to_string(),
        body: "Always cite the source.".to_string(),
    });

    let provider = Arc::new(CapturingProvider::new(vec![done_chunk("done")]));
    let (registry, _executions) = echo_registry();
    let agent = ReActAgent::new(provider.clone(), registry)
        .with_host(host)
        .expect("bind the host");

    let mut stream = agent
        .run(
            AgentInput::text("hello"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    let captured = provider.captured.lock().await;
    let system = &captured[0][0];
    assert_eq!(
        system.role,
        Role::System,
        "the assembled system prompt must be the first request message"
    );
    let text = match &system.content {
        uva_provider::Content::Single(ContentPart::Text(t)) => &t.text,
        _ => panic!("expected a single text system message"),
    };
    assert!(
        text.contains("## Audit"),
        "the plugin section's heading must reach the model; got:\n{text}"
    );
    assert_eq!(
        text.matches("Always cite the source.").count(),
        1,
        "the section must render exactly once; got:\n{text}"
    );
}

// A transformer that replaces the tool result the model reads.
struct ResultRewriter;

#[async_trait]
impl ResultTransformer for ResultRewriter {
    fn name(&self) -> &str {
        "result-rewriter"
    }

    async fn transform(
        &self,
        output: &mut ToolOutput,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        *output = ToolOutput::text("rewritten by the plugin");
        Ok(())
    }
}

// Transformers run before the loop commits the result, so the log holds
// the rewritten text. The audit row is provenance for that: exactly one
// row, naming the rewriter, with two different digests.
#[tokio::test]
async fn a_host_result_rewrite_leaves_one_audit_row() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let mut host = Host::new();
    let _transformer = host.result_transformer(Arc::new(ResultRewriter));

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![echo_tool_call()]),
        done_chunk("done"),
    ]));
    let (registry, _executions) = echo_registry();
    let agent = ReActAgent::new(provider.clone(), registry)
        .with_host(host)
        .expect("bind the host")
        .with_log_writer(writer);

    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    let audit: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["type"] == "tool_result_transformed")
        .collect();
    assert_eq!(audit.len(), 1, "one rewrite is one row: {rows:#?}");
    let row = SessionEvent::from_value(audit[0]).expect("the row parses");
    let SessionEvent::ToolResultTransformed {
        transformers,
        tool_name,
        call_id,
        before,
        after,
        ..
    } = &row
    else {
        panic!("the row is an audit row: {row:?}");
    };
    assert_eq!(transformers, &["result-rewriter"]);
    assert_eq!(tool_name, "echo");
    assert_eq!(call_id, "c1");
    assert_ne!(before, after, "the audit must name two versions");

    // The model read the rewritten text, and the log's own
    // `tool_result` row holds the same text: the transformers run
    // before the commit.
    let last = provider.captured.lock().await;
    let sent = format!("{:?}", last.last().expect("the second request"));
    assert!(
        sent.contains("rewritten by the plugin"),
        "the model reads the rewrite: {sent}"
    );
}

// The ordering the audit row rests on: the durable `tool_result` row is
// committed from the *transformed* output, so the log says exactly what
// the model read. A regression that committed before the transform
// would surface here.
#[tokio::test]
async fn the_tool_result_row_holds_the_post_transform_text() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let mut host = Host::new();
    let _transformer = host.result_transformer(Arc::new(ResultRewriter));

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![echo_tool_call()]),
        done_chunk("done"),
    ]));
    let (registry, _executions) = echo_registry();
    let agent = ReActAgent::new(provider, registry)
        .with_host(host)
        .expect("bind the host")
        .with_log_writer(writer);

    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    let row = rows
        .iter()
        .find(|row| row["type"] == "tool_result")
        .expect("the tool result is committed");
    let payload = row["data"].to_string();
    assert!(
        payload.contains("rewritten by the plugin"),
        "the log holds what the model read: {payload}"
    );
    assert!(
        !payload.contains("echoed"),
        "the log does not hold the pre-transform output: {payload}"
    );
}

// A run whose host holds no result transformer writes no audit row and
// takes no snapshot: `execute_gated` checks `has_result_rewriters`
// before it touches the output.
#[tokio::test]
async fn a_run_without_result_transformers_writes_no_audit_row() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![echo_tool_call()]),
        done_chunk("done"),
    ]));
    let (registry, _executions) = echo_registry();
    let agent = ReActAgent::new(provider, registry).with_log_writer(writer);

    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    assert!(
        rows.iter()
            .all(|row| row["type"] != "tool_result_transformed"),
        "nothing rewrote the result, so nothing is audited: {rows:#?}"
    );
    assert!(
        rows.iter().any(|row| row["type"] == "tool_result"),
        "the tool's own row is still the log's record"
    );
}
