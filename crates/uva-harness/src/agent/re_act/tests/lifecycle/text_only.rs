//! One full ReAct loop pass: streamed chunks → tool dispatch →
//! max-iteration warning → cancel. Every test in this file
//! drives a single session and asserts the resulting event
//! sequence or the per-iteration usage accounting.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uva_provider::{
    ContentPart,
    SamplingResult,
    StreamChunk,
    TextContent,
    TokenUsage,
    ToolUse,
};
use uva_test_support::FakeTool;
use uva_tool::ToolRegistry;

use super::super::{support::*, *};

pub(super) async fn run_emits_full_event_lifecycle_for_text_only_response() {
    let provider = Arc::new(ScriptedStreamProvider::new(vec![vec![
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: "hello ".to_string(),
            cache_control: None,
        })),
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: "world".to_string(),
            cache_control: None,
        })),
        StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: String::new(),
                tool_calls: vec![],
                reasoning: String::new(),
                reasoning_signature: None,
                usage: TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    cached_prompt_tokens: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                },
                ..Default::default()
            }),
        },
    ]]));
    let events = run_and_collect(
        provider,
        Arc::new(ToolRegistry::new()),
        CancellationToken::new(),
        AgentInput::text("hello"),
    )
    .await;

    // SessionStarted → Progress → Model(text) → Model(text) →
    // Usage → ModelDone → SessionEnded.
    let kinds: Vec<&str> = events.iter().map(|e| e.kind()).collect();
    assert_eq!(
        kinds,
        vec![
            "System",
            "System",
            "Model",
            "Model",
            "System",
            "ModelDone",
            "System",
        ]
    );

    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::System(SystemEvent::SessionStarted { .. })
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        })
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::System(SystemEvent::Progress { .. })
    )));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ModelDone(_))));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::System(SystemEvent::Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..
        })
    )));
}

pub(super) async fn isdone_text_does_not_duplicate_streamed_chunks() {
    // Real Anthropic / OpenAI providers stream text via
    // `Content(Text)` chunks AND include the consolidated
    // final text in `IsDone.result.text`. Without dedup
    // the consumer would see the same text twice (once
    // per chunk, once via IsDone). This test reproduces
    // that exact pattern and asserts the wire sees each
    // text fragment exactly once.
    let provider = Arc::new(ScriptedStreamProvider::new(vec![vec![
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: "hello ".to_string(),
            cache_control: None,
        })),
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: "world".to_string(),
            cache_control: None,
        })),
        StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                // Same text as the streamed chunks—the
                // canonical duplicate.
                text: "hello world".to_string(),
                tool_calls: vec![],
                reasoning: String::new(),
                reasoning_signature: None,
                usage: TokenUsage {
                    prompt_tokens: 1,
                    completion_tokens: 2,
                    total_tokens: 3,
                    cached_prompt_tokens: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                },
                ..Default::default()
            }),
        },
    ]]));

    let events = run_and_collect(
        provider,
        Arc::new(ToolRegistry::new()),
        CancellationToken::new(),
        AgentInput::text("hi"),
    )
    .await;

    let text_events: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::Text(tc)) => Some(tc.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text_events,
        vec!["hello ".to_string(), "world".to_string()],
        "IsDone text must NOT be re-emitted as a wire event when streamed chunks already covered it; got {text_events:?}"
    );
}

pub(super) async fn isdone_text_emitted_only_when_no_streamed_chunks() {
    // Mirror of the previous test: when the provider
    // batches everything into IsDone (no streamed chunks),
    // the IsDone text MUST surface on the wire—otherwise
    // non-streaming providers (or streaming providers with
    // no content events) would produce a silent assistant
    // turn.
    let provider = Arc::new(ScriptedStreamProvider::new(vec![vec![
        StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: "batched-response".to_string(),
                tool_calls: vec![],
                reasoning: String::new(),
                reasoning_signature: None,
                usage: TokenUsage::default(),
                ..Default::default()
            }),
        },
    ]]));

    let events = run_and_collect(
        provider,
        Arc::new(ToolRegistry::new()),
        CancellationToken::new(),
        AgentInput::text("hi"),
    )
    .await;

    let text_events: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::Text(tc)) => Some(tc.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text_events,
        vec!["batched-response".to_string()],
        "non-streaming IsDone must surface its text on the wire; got {text_events:?}"
    );
}

pub(super) async fn isdone_tool_calls_dedup_against_streamed_tool_call_end() {
    // Anthropic and OpenAI both emit `ToolCallStart` +
    // `ToolCallEnd` AND re-list the same tool calls in
    // `IsDone.result.tool_calls`. Without dedup, the
    // history message would carry two identical
    // ContentPart::ToolUse entries for the same id—
    // corrupting the LLM context. This test asserts each
    // tool_use_id appears on the wire exactly once.
    let tool_use = ToolUse {
        id: "call_dup".to_string(),
        name: "noop".to_string(),
        input: json!({}),
    };
    // Patch the IsDone to also include the same tool
    // call (re-listing). We rebuild the chunks directly
    // because `tool_call_response` emits
    // `TokenUsage::default()` in IsDone—fine, but we
    // need tool_calls populated too.
    let chunks: Vec<Vec<StreamChunk>> = vec![vec![
        StreamChunk::ToolCallStart {
            id: tool_use.id.clone(),
            name: tool_use.name.clone(),
            arguments: Value::String("{}".to_string()),
        },
        StreamChunk::ToolCallEnd {
            id: tool_use.id.clone(),
        },
        StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: String::new(),
                // Same id as the streamed chunk—must be deduped.
                tool_calls: vec![tool_use.clone()],
                reasoning: String::new(),
                reasoning_signature: None,
                usage: TokenUsage::default(),
                ..Default::default()
            }),
        },
    ]];
    let provider = Arc::new(ScriptedStreamProvider::new(chunks));

    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        "noop", "done",
    ))));

    let events = run_and_collect(
        provider,
        registry,
        CancellationToken::new(),
        AgentInput::text("dedup"),
    )
    .await;

    let wire_tool_use_ids: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolUse(tu)) => Some(tu.id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        wire_tool_use_ids,
        vec!["call_dup"],
        "tool_use_id must appear on the wire exactly once even when IsDone re-lists it; got {wire_tool_use_ids:?}"
    );
}
