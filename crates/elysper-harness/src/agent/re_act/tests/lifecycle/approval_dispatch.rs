//! Dispatch-intent safety: a high-risk delegated call is blocked, a
//! non-read-only call records its intent before it runs, and a
//! dispatch row that cannot be flushed stops the call.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use elysper_core::SystemClock;
use elysper_session::{
    LogWriter,
    SessionError,
    SessionSink,
    SessionSnapshot,
    in_memory::InMemorySessionSink,
};
use elysper_tool::{ToolAnnotations, ToolOutput, ToolRegistry};
use tokio_util::sync::CancellationToken;

use super::{
    super::{support::*, *},
    approval::*,
};
use crate::{
    DelegatingTool,
    DelegationCall,
    ReActAgent,
    agent::approval::{ApprovalMode, ApprovalPolicy},
};

struct DangerousDelegator {
    runs: Arc<AtomicUsize>,
}

impl DelegatingTool for DangerousDelegator {
    fn definitions(&self) -> Vec<elysper_provider::ToolDefinition> {
        vec![elysper_provider::ToolDefinition {
            name: "delegate".to_string(),
            description: "delegate".to_string(),
            input_schema: json!({"type": "object"}),
            cache_control: None,
            annotations: None,
        }]
    }

    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
        })
    }

    fn execute(
        &self,
        _call: DelegationCall<'_>,
    ) -> futures::future::BoxFuture<'static, ToolOutput> {
        let runs = Arc::clone(&self.runs);
        Box::pin(async move {
            runs.fetch_add(1, Ordering::SeqCst);
            ToolOutput::text("delegated")
        })
    }
}

// A sink that refuses the dispatch row: the writer's flush fails, and
// the call must not run without its intent row.
struct RejectingDispatchSink {
    inner: InMemorySessionSink,
}

#[async_trait]
impl SessionSink for RejectingDispatchSink {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn alloc_seq(&self) -> u64 {
        self.inner.alloc_seq()
    }

    async fn append(&self, event: &Value) -> Result<(), SessionError> {
        if event.get("type").and_then(Value::as_str) == Some("tool_dispatch") {
            return Err(SessionError::AppendFailed(
                "dispatch row refused".to_string(),
            ));
        }
        self.inner.append(event).await
    }

    async fn read(&self) -> Result<Vec<Value>, SessionError> {
        self.inner.read().await
    }

    async fn read_from(&self, from: u64) -> Result<Vec<Value>, SessionError> {
        self.inner.read_from(from).await
    }

    async fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        self.inner.snapshot().await
    }

    async fn close(
        &self,
        reason: elysper_session::SessionEndReason,
    ) -> Result<(), SessionError> {
        self.inner.close(reason).await
    }
}

fn never_policy() -> ApprovalPolicy {
    ApprovalPolicy {
        mode: ApprovalMode::Never,
        ..ApprovalPolicy::default()
    }
}

pub(super) async fn a_high_risk_delegated_call_is_blocked() {
    let runs = Arc::new(AtomicUsize::new(0));
    let writer = logged_writer().await;
    let provider = scripted(vec![call("c1", "delegate")]);
    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_log_writer(writer)
        .with_delegator(Arc::new(DangerousDelegator {
            runs: Arc::clone(&runs),
        }))
        .expect("the delegator installs");
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "a high-risk delegated call never runs"
    );
    assert!(
        suspension_of(&events).is_none(),
        "it is blocked, not suspended: there is no child resume route"
    );
    let denial = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::Model(ContentPart::ToolResult(result)) => Some(result),
            _ => None,
        })
        .expect("the block is a model-visible result");
    assert_eq!(denial.is_error, Some(true));
    assert!(
        denial.content.iter().any(|part| matches!(part,
            ContentPart::Text(text)
                if text.text.contains("high-risk delegated"))),
        "the block names the reason: {:?}",
        denial.content
    );
    assert_eq!(end_reason(&events), Some(SessionEndReason::Completed));
}

pub(super) async fn dispatch_is_recorded_for_high_and_unknown_calls() {
    let runs = Arc::new(AtomicUsize::new(0));
    let plain = Arc::new(ProbeTool {
        name: "plain",
        runs: Arc::clone(&runs),
        annotations: None,
        schema: json!({"type": "object"}),
    });
    let registry = build_registry(vec![tool("risky", false, &runs), plain]);
    let writer = logged_writer().await;
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![call("c1", "risky"), call("c2", "plain")]),
        done_chunk("done"),
    ]));
    let agent = ReActAgent::new(provider, registry)
        .with_log_writer(Arc::clone(&writer))
        .with_approval_policy(never_policy());
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let rows = rows(&writer).await;
    let dispatched: Vec<&str> = rows
        .iter()
        .filter(|row| row["type"] == "tool_dispatch")
        .filter_map(|row| row["call_id"].as_str())
        .collect();
    assert!(
        dispatched.contains(&"c1") && dispatched.contains(&"c2"),
        "High and Unknown both leave an intent row: {dispatched:?}"
    );
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "the allowed calls still run"
    );
}

pub(super) async fn a_failing_dispatch_flush_prevents_execution() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let sink = Arc::new(RejectingDispatchSink {
        inner: InMemorySessionSink::new("approval"),
    });
    let writer = Arc::new(LogWriter::new(sink, Arc::new(SystemClock)));
    writer
        .append(json!({"type": "UserInput", "data": {"text": "go"}}))
        .await
        .expect("seed the host's user row");
    writer.flush().await.expect("flush the seeded row");

    let provider = scripted(vec![call("c1", "risky")]);
    let agent = ReActAgent::new(provider, registry)
        .with_log_writer(writer)
        .with_approval_policy(never_policy());
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "the call must not run when its intent row is not durable"
    );
    match end_reason(&events) {
        Some(SessionEndReason::Error(message)) => assert!(
            message.contains("log flush failed"),
            "the flush failure ends the run: {message}"
        ),
        other => panic!("expected a flush error, got {other:?}"),
    }
}
