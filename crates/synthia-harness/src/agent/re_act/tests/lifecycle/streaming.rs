use std::sync::Arc;

use futures::StreamExt;
use serde_json::json;
use synthia_provider::{
    ContentPart,
    SamplingResult,
    StreamChunk,
    TokenUsage,
    ToolUse,
};
use tokio_util::sync::CancellationToken;

use super::super::{support::*, *};

pub(super) async fn streamed_text_deltas_commit_as_one_block() {
    // Two text deltas, then a tool call so a second sampling pass
    // follows with this assistant turn in history.
    let turn1 = vec![
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: "Hello ".to_string(),
            cache_control: None,
        })),
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: "world".to_string(),
            cache_control: None,
        })),
        StreamChunk::ToolCallStart {
            id: "c1".to_string(),
            name: "echo".to_string(),
            arguments: json!({}),
        },
        StreamChunk::ToolCallEnd {
            id: "c1".to_string(),
        },
        StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: String::new(),
                tool_calls: vec![ToolUse {
                    id: "c1".to_string(),
                    name: "echo".to_string(),
                    input: json!({}),
                }],
                usage: TokenUsage::default(),
                stop_reason: Some("tool_use".to_string()),
                ..Default::default()
            }),
        },
    ];
    let provider =
        Arc::new(CapturingProvider::new(vec![turn1, empty_response()]));
    let (registry, _calls) = echo_registry();
    let agent =
        ReActAgent::new(provider.clone(), registry).with_max_iterations(2);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    // Precondition: the wire really did see two separate deltas, so one
    // joined part below is coalescing rather than a single chunk. Only
    // the first sampling pass counts—turn 2 streams `empty_response`'s
    // text, which is not part of this turn.
    let first_pass = events
        .iter()
        .take_while(|e| !matches!(e, AgentEvent::ModelDone(_)))
        .collect::<Vec<_>>();
    let streamed_text: Vec<&str> = first_pass
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::Text(t)) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        streamed_text,
        vec!["Hello ", "world"],
        "the stream must publish each delta as it arrives"
    );

    let captured = provider.captured.lock().await;
    assert!(
        captured.len() >= 2,
        "need a second request; got {}",
        captured.len()
    );
    // The assistant turn, not the whole request: the system prompt is
    // also a `Text` part, and the user message is another.
    let assistant = captured[1]
        .iter()
        .rev()
        .find(|m| {
            m.role == synthia_provider::Role::Assistant
                && m.content
                    .iter()
                    .any(|p| matches!(p, ContentPart::ToolUse(_)))
        })
        .expect("turn 1's assistant message must be in request 2");
    let parts: Vec<&ContentPart> = assistant.content.iter().collect();
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["Hello world"],
        "the deltas must commit as one joined part; parts were: {parts:?}"
    );
    assert!(
        matches!(parts.last(), Some(ContentPart::ToolUse(_))),
        "and the tool call must follow the joined text; parts: {parts:?}"
    );
}

// -- : durable resume (checkpointer wiring) ------------------
