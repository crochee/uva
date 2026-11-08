//! Approval lifecycle: a high-risk call suspends the run before it
//! runs, and the suspension's rows close the turn as suspended.
//! Resume and dispatch fixtures live in the sibling modules.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use elysper_core::SystemClock;
use elysper_provider::ToolUse;
use elysper_session::{LogWriter, in_memory::InMemorySessionSink};
use elysper_tool::{
    Context,
    Tool,
    ToolAnnotations,
    ToolEntry,
    ToolOutput,
    ToolRegistry,
};
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::super::{support::*, *};
use crate::{
    ReActAgent,
    agent::approval::{ApprovalRequest, ResumeToken, Suspension},
};

// A tool whose annotations declare its risk; every execution bumps
// the counter, so "did the held call run?" is observable.
pub(super) struct ProbeTool {
    pub(super) name: &'static str,
    pub(super) runs: Arc<AtomicUsize>,
    pub(super) annotations: Option<ToolAnnotations>,
    pub(super) schema: serde_json::Value,
}

#[async_trait]
impl Tool for ProbeTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "probe"
    }

    fn parameters(&self) -> serde_json::Value {
        self.schema.clone()
    }

    fn annotations(&self) -> Option<ToolAnnotations> {
        self.annotations
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        self.runs.fetch_add(1, Ordering::SeqCst);
        ToolOutput::text("ran")
    }
}

pub(super) fn tool(
    name: &'static str,
    read_only: bool,
    runs: &Arc<AtomicUsize>,
) -> Arc<dyn Tool> {
    tool_with_schema(name, read_only, runs, json!({"type": "object"}))
}

pub(super) fn tool_with_schema(
    name: &'static str,
    read_only: bool,
    runs: &Arc<AtomicUsize>,
    schema: serde_json::Value,
) -> Arc<dyn Tool> {
    Arc::new(ProbeTool {
        name,
        runs: Arc::clone(runs),
        annotations: Some(ToolAnnotations {
            read_only_hint: Some(read_only),
            destructive_hint: Some(!read_only),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
        }),
        schema,
    })
}

pub(super) fn build_registry(tools: Vec<Arc<dyn Tool>>) -> Arc<ToolRegistry> {
    let registry = Arc::new(ToolRegistry::new());
    for tool in tools {
        registry.register_entry(ToolEntry::new(tool));
    }
    registry
}

pub(super) async fn logged_writer() -> Arc<LogWriter> {
    let sink = Arc::new(InMemorySessionSink::new("approval"));
    let writer = Arc::new(LogWriter::new(sink, Arc::new(SystemClock)));
    writer
        .append(json!({"type": "UserInput", "data": {"text": "go"}}))
        .await
        .expect("seed the host's user row");
    writer.flush().await.expect("flush the seeded row");
    writer
}

pub(super) async fn run_agent(
    provider: Arc<ScriptedStreamProvider>,
    registry: Arc<ToolRegistry>,
    writer: Arc<LogWriter>,
    input: AgentInput,
) -> Vec<AgentEvent> {
    let agent = ReActAgent::new(provider, registry).with_log_writer(writer);
    let mut stream = agent.run(input, Arc::new(CancellationToken::new())).await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

pub(super) fn suspension_of(
    events: &[AgentEvent],
) -> Option<(Vec<ApprovalRequest>, ResumeToken)> {
    events.iter().find_map(|event| match event {
        AgentEvent::System(SystemEvent::Suspended(
            Suspension::AwaitingApproval { interrupts, resume },
        )) => Some((interrupts.clone(), resume.clone())),
        _ => None,
    })
}

pub(super) fn end_reason(events: &[AgentEvent]) -> Option<SessionEndReason> {
    events.iter().find_map(|event| match event {
        AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
            Some(reason.clone())
        }
        _ => None,
    })
}

pub(super) async fn rows(writer: &LogWriter) -> Vec<serde_json::Value> {
    writer.read().await.expect("read the log")
}

pub(super) fn call(id: &str, name: &str) -> ToolUse {
    ToolUse {
        id: id.to_string(),
        name: name.to_string(),
        input: json!({}),
    }
}

pub(super) fn scripted(calls: Vec<ToolUse>) -> Arc<ScriptedStreamProvider> {
    Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(calls),
        done_chunk("unused"),
    ]))
}

pub(super) async fn read_only_tools_are_never_asked_about() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("read", true, &runs)]);
    let writer = logged_writer().await;
    let provider = scripted(vec![call("c1", "read")]);
    let events =
        run_agent(provider, registry, writer, AgentInput::text("go")).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1, "a read-only call runs");
    assert!(suspension_of(&events).is_none());
    assert_eq!(end_reason(&events), Some(SessionEndReason::Completed));
}

pub(super) async fn a_high_risk_call_suspends_before_it_runs() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let writer = logged_writer().await;
    let provider = scripted(vec![call("c1", "risky")]);
    let events = run_agent(
        provider,
        registry,
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;

    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "the held call must never run"
    );
    let (interrupts, token) = suspension_of(&events).expect("the run suspends");
    assert_eq!(interrupts.len(), 1);
    assert_eq!(interrupts[0].call_id, "c1");
    assert_eq!(end_reason(&events), Some(SessionEndReason::Suspended));
    assert_eq!(token.pending, vec!["c1".to_string()]);

    let rows = rows(&writer).await;
    let last_seq = rows
        .iter()
        .filter_map(|row| row["seq"].as_u64())
        .max()
        .expect("the suspension wrote rows");
    assert_eq!(token.last_seq, last_seq, "the token names the log tail");

    let asked: Vec<_> = rows
        .iter()
        .filter(|row| row["type"] == "approval_asked")
        .collect();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0]["call_id"], "c1");
    assert_eq!(asked[0]["risk"], "high");

    let turn_suspended = rows.iter().any(|row| {
        row["type"] == "turn"
            && row["data"]["phase"] == "suspended"
            && row["data"]["status"] == "suspended"
            && row["data"]["pending"] == json!(["c1"])
    });
    let step_suspended = rows.iter().any(|row| {
        row["type"] == "step"
            && row["data"]["phase"] == "end"
            && row["data"]["status"] == "suspended"
    });
    let turn_ended = rows.iter().any(|row| {
        row["type"] == "turn"
            && row["data"]["phase"] == "end"
            && row["data"]["reason"] == "suspended"
    });
    assert!(turn_suspended, "the turn records the suspension");
    assert!(step_suspended, "the step ends as suspended");
    assert!(turn_ended, "the turn ends as suspended");
    assert!(
        rows.iter().all(|row| row["type"] != "tool_result"),
        "a held call leaves no result row"
    );
}

pub(super) async fn a_writerless_ask_is_refused_not_suspended() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let provider = scripted(vec![call("c1", "risky")]);
    let agent = ReActAgent::new(provider, registry);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert!(
        suspension_of(&events).is_none(),
        "without a log the call cannot be resumed, so it is refused"
    );
    let denial = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::Model(ContentPart::ToolResult(result)) => Some(result),
            _ => None,
        })
        .expect("the refusal is model-visible");
    assert_eq!(denial.is_error, Some(true));
}
