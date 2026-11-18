//! Dispatch-intent safety: a non-read-only call records its intent
//! before it runs, and a dispatch row that cannot be flushed stops
//! the call.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uva_approval::{ApprovalMode, ApprovalPolicy};
use uva_core::SystemClock;
use uva_session::{
    LogWriter,
    SessionError,
    SessionSink,
    SessionSnapshot,
    in_memory::InMemorySessionSink,
};

use super::{
    super::{support::*, *},
    approval::*,
};
use crate::ReActAgent;

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
        reason: uva_session::SessionEndReason,
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
        .with_log_writer(writer.clone())
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
    let seed = uva_session::user_message(
        serde_json::to_value(uva_provider::Message::user("go"))
            .expect("a user message serialises"),
        None,
    );
    writer
        .append_event(&seed)
        .await
        .expect("seed the user turn");
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
