//! One assistant turn is one durable row: the harness writes the
//! assembled message (text and tool calls together) once per model
//! call through the session's writer, and text deltas live on the
//! event stream only — a resumed session rebuilds the transcript
//! exactly as the live run produced it.
use std::sync::Arc;

use futures::StreamExt;
use serde_json::{Value, json};
use synthia_core::SystemClock;
use synthia_provider::{
    Content,
    ContentPart,
    Message,
    ModelProvider,
    Role,
    SamplingResult,
    StreamChunk,
    TextContent,
    ToolUse,
};
use synthia_session::{
    LogWriter,
    SessionEvent,
    SessionSink,
    fold_surface,
    in_memory::InMemorySessionSink,
};
use synthia_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use super::super::{support::*, *};

fn text_deltas(deltas: &[&str]) -> Vec<StreamChunk> {
    let mut chunks: Vec<StreamChunk> = deltas
        .iter()
        .map(|d| {
            StreamChunk::Content(ContentPart::Text(TextContent {
                text: (*d).to_string(),
                cache_control: None,
            }))
        })
        .collect();
    chunks.push(StreamChunk::IsDone {
        result: Box::new(SamplingResult {
            stop_reason: Some("end_turn".to_string()),
            ..SamplingResult::default()
        }),
    });
    chunks
}

// Run one session with a log writer wired, and return both the
// captured event stream and the log sink the writer flushed to.
async fn run_logged(
    provider: Arc<dyn ModelProvider>,
    registry: Arc<ToolRegistry>,
) -> (Vec<AgentEvent>, Arc<InMemorySessionSink>) {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let agent = ReActAgent::new(provider, registry).with_log_writer(writer);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }
    (events, sink)
}

async fn log_rows(sink: &Arc<InMemorySessionSink>) -> Vec<Value> {
    sink.read().await.expect("read")
}

fn typed_events(rows: &[Value]) -> Vec<SessionEvent> {
    rows.iter().filter_map(SessionEvent::from_value).collect()
}

fn joined_text(content: &Content) -> String {
    let mut text = String::new();
    for part in content.iter() {
        if let ContentPart::Text(t) = part {
            text.push_str(&t.text);
        }
    }
    text
}

fn parse_message(value: &Value) -> Message {
    serde_json::from_value(value.clone()).expect("message row parses")
}

/// A streamed reply of five text deltas is exactly one assistant
/// fact in the log — one `assistant_message` row holding the
/// assembled text — and the fold rebuilds one assistant message
/// from it, so a resumed session sees the reply as one turn.
pub(super) async fn a_streamed_reply_is_one_row_and_one_message_after_resume() {
    let provider = Arc::new(ScriptedStreamProvider::new(vec![text_deltas(&[
        "Hello", ", ", "stream", "ed", " world",
    ])]));
    let (events, sink) =
        run_logged(provider, Arc::new(ToolRegistry::new())).await;

    // Sanity: the wire really did see five separate deltas.
    let streamed: Vec<&str> = events
        .iter()
        .take_while(|e| !matches!(e, AgentEvent::ModelDone(_)))
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::Text(t)) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(streamed.len(), 5, "five deltas streamed");

    let rows = log_rows(&sink).await;
    let assistant_rows: Vec<&Value> = rows
        .iter()
        .filter(|r| r["type"] == "assistant_message")
        .collect();
    assert_eq!(
        assistant_rows.len(),
        1,
        "one streamed reply is one assistant row, got: {:?}",
        rows.iter()
            .filter_map(|r| r["type"].as_str())
            .collect::<Vec<_>>()
    );
    let message = parse_message(&assistant_rows[0]["data"]);
    assert_eq!(message.role, Role::Assistant);
    assert_eq!(
        joined_text(&message.content),
        "Hello, streamed world",
        "the row carries the assembled text"
    );

    let folded = fold_surface(&typed_events(&rows)).expect("fold");
    assert_eq!(folded.messages.len(), 1, "one surface message");
    let resumed = parse_message(&folded.messages[0]);
    assert_eq!(resumed.role, Role::Assistant);
    assert_eq!(
        joined_text(&resumed.content),
        "Hello, streamed world",
        "resume rebuilds the reply as one message"
    );
}

/// A turn that speaks and then calls a tool lands as one row
/// holding both the text and the `tool_use` part, so the fold
/// never splits the model's turn; the tool's result follows as
/// its own row, and a resumed session rebuilds exactly
/// `[assistant(text + tool_use), tool(result)]`.
pub(super) async fn a_reply_with_a_tool_call_keeps_text_and_tool_use_in_one_row()
 {
    let mut turn1 = text_deltas(&["let me check"]);
    turn1.pop();
    turn1.extend(tool_call_response(vec![ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({"x": 1}),
    }]));
    // One scripted turn only: the follow-up sampling pass takes
    // the provider's empty default, which produces no row, so the
    // fold below is exactly the tool turn.
    let provider = Arc::new(ScriptedStreamProvider::new(vec![turn1]));
    let (registry, _executions) = echo_registry();
    let (_events, sink) = run_logged(provider, registry).await;

    let rows = log_rows(&sink).await;
    let tool_turns: Vec<&Value> = rows
        .iter()
        .filter(|r| r["type"] == "assistant_message")
        .filter(|r| {
            parse_message(&r["data"])
                .content
                .iter()
                .any(|p| matches!(p, ContentPart::ToolUse(_)))
        })
        .collect();
    assert_eq!(
        tool_turns.len(),
        1,
        "exactly one assistant row carries the tool call"
    );
    let message = parse_message(&tool_turns[0]["data"]);
    assert_eq!(joined_text(&message.content), "let me check");
    assert_eq!(
        message
            .content
            .iter()
            .filter(|p| matches!(p, ContentPart::ToolUse(_)))
            .count(),
        1
    );

    // The resumed transcript: one assistant message for the turn
    // (text and tool call together), then the tool's result.
    let folded = fold_surface(&typed_events(&rows)).expect("fold");
    assert_eq!(
        folded.messages.len(),
        2,
        "one assistant message, one tool result; got {:?}",
        folded.messages
    );
    let assistant = parse_message(&folded.messages[0]);
    assert_eq!(assistant.role, Role::Assistant);
    assert_eq!(joined_text(&assistant.content), "let me check");
    assert_eq!(
        assistant
            .content
            .iter()
            .filter(|p| matches!(p, ContentPart::ToolUse(_)))
            .count(),
        1
    );
    let result = parse_message(&folded.messages[1]);
    assert_eq!(result.role, Role::Tool);
    assert_eq!(result.tool_call_id.as_deref(), Some("c1"));
}

/// A tool that failed leaves the failure on its row: the message
/// payload carries the tool's output, and the flag beside it says
/// the call did not succeed. A reader of the log — the web's
/// history projection — must not report a failed tool as a
/// successful one, and the flag is the only thing that says so.
pub(super) async fn a_failed_tool_leaves_its_failure_on_the_row() {
    let mut turn = text_deltas(&["let me try"]);
    turn.pop();
    turn.extend(tool_call_response(vec![ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    }]));
    let provider = Arc::new(ScriptedStreamProvider::new(vec![turn]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        synthia_test_support::FakeTool::failing("echo", "the tool exploded"),
    )));
    let (_events, sink) = run_logged(provider, registry).await;

    let rows = log_rows(&sink).await;
    let result_rows: Vec<&Value> =
        rows.iter().filter(|r| r["type"] == "tool_result").collect();
    assert_eq!(
        result_rows.len(),
        1,
        "one tool result row; got {:?}",
        rows.iter()
            .filter_map(|r| r["type"].as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        result_rows[0]["is_error"],
        json!(true),
        "the row says the tool failed: {:?}",
        result_rows[0]
    );
}

/// Deltas keep streaming as `Model(Text)` events for live
/// rendering, but the log holds no per-delta rows.
pub(super) async fn deltas_still_stream_as_events_but_are_not_persisted() {
    let provider = Arc::new(ScriptedStreamProvider::new(vec![text_deltas(&[
        "a", "b", "c", "d", "e",
    ])]));
    let (events, sink) =
        run_logged(provider, Arc::new(ToolRegistry::new())).await;

    let streamed: Vec<&str> = events
        .iter()
        .take_while(|e| !matches!(e, AgentEvent::ModelDone(_)))
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::Text(t)) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        streamed,
        vec!["a", "b", "c", "d", "e"],
        "live rendering still sees every delta"
    );

    let rows = log_rows(&sink).await;
    assert!(
        rows.iter().all(|r| r["type"] != "assistant_chunk"),
        "no per-delta rows in the log: {:?}",
        rows.iter()
            .filter_map(|r| r["type"].as_str())
            .collect::<Vec<_>>()
    );
}
