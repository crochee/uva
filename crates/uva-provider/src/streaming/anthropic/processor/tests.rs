use super::*;
use crate::streaming::anthropic::events::{
    AnthropicStreamContentBlock,
    AnthropicStreamDelta,
    AnthropicStreamEvent,
};

// -- new() / default() -------------------------------------------

/// `StreamProcessor::new()` MUST initialize all fields to
/// empty/sensible defaults.
#[test]
fn new_initializes_all_fields() {
    let mut p = StreamProcessor::new();
    // No public getters, but we exercise the
    // default-state by sending a no-op event.
    let ev = AnthropicStreamEvent {
        r#type: "ping".to_string(),
        content_block: None,
        index: None,
        delta: None,
        stop_reason: None,
    };
    let chunks = p.process_event(&ev);
    assert!(chunks.is_empty(), "ping event produces no chunks");
}

/// `StreamProcessor::default()` MUST match `new()`.
#[test]
fn default_matches_new() {
    // Both produce processors that return empty chunks
    // for unknown events.
    let mut p = StreamProcessor::default();
    let ev = AnthropicStreamEvent {
        r#type: "x".to_string(),
        content_block: None,
        index: None,
        delta: None,
        stop_reason: None,
    };
    assert!(p.process_event(&ev).is_empty());
}

// -- text_delta --------------------------------------------------

/// `process_event` MUST emit `Content(Text)` chunks for
/// `text_delta` events and accumulate the text.
#[test]
fn text_delta_emits_content_text_chunk() {
    let mut p = StreamProcessor::new();
    let ev = AnthropicStreamEvent {
        r#type: "content_block_delta".to_string(),
        content_block: None,
        index: Some(0),
        delta: Some(AnthropicStreamDelta {
            r#type: "text_delta".to_string(),
            text: Some("hello".to_string()),
            thinking: None,
            partial_json: None,
            signature: None,
            stop_reason: None,
        }),
        stop_reason: None,
    };
    let chunks = p.process_event(&ev);
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        StreamChunk::Content(ContentPart::Text(t)) => {
            assert_eq!(t.text, "hello");
        }
        other => panic!("expected Content(Text), got {other:?}"),
    }
}

/// `process_event` MUST accumulate text across multiple
/// `text_delta` events (used for streaming text output).
#[test]
fn text_delta_accumulates_across_events() {
    let mut p = StreamProcessor::new();
    for word in ["hello", " ", "world"] {
        let ev = AnthropicStreamEvent {
            r#type: "content_block_delta".to_string(),
            content_block: None,
            index: Some(0),
            delta: Some(AnthropicStreamDelta {
                r#type: "text_delta".to_string(),
                text: Some(word.to_string()),
                thinking: None,
                partial_json: None,
                signature: None,
                stop_reason: None,
            }),
            stop_reason: None,
        };
        p.process_event(&ev);
    }
    // Finalize to inspect the aggregated text.
    let chunks = finalize(&mut p);
    match chunks.into_iter().next().unwrap() {
        StreamChunk::IsDone { result } => {
            assert_eq!(result.text, "hello world");
        }
        other => panic!("expected IsDone, got {other:?}"),
    }
}

// -- thinking_delta ----------------------------------------------

/// `process_event` MUST emit `Content(Reasoning)` chunks for
/// `thinking_delta` events and accumulate the reasoning.
#[test]
fn thinking_delta_emits_content_reasoning_chunk() {
    let mut p = StreamProcessor::new();
    let ev = AnthropicStreamEvent {
        r#type: "content_block_delta".to_string(),
        content_block: None,
        index: Some(0),
        delta: Some(AnthropicStreamDelta {
            r#type: "thinking_delta".to_string(),
            text: None,
            thinking: Some("step 1".to_string()),
            partial_json: None,
            signature: None,
            stop_reason: None,
        }),
        stop_reason: None,
    };
    let chunks = p.process_event(&ev);
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        StreamChunk::Content(ContentPart::Reasoning(r)) => {
            assert_eq!(r.text, "step 1");
            assert_eq!(r.signature, None);
        }
        other => panic!("expected Content(Reasoning), got {other:?}"),
    }
}

/// `signature_delta` MUST fold into the next reasoning
/// chunk's `signature` field (no separate stream chunk).
#[test]
fn signature_delta_sets_next_reasoning_signature() {
    let mut p = StreamProcessor::new();
    // 1) signature_delta (no stream chunk emitted).
    let ev1 = AnthropicStreamEvent {
        r#type: "content_block_delta".to_string(),
        content_block: None,
        index: Some(0),
        delta: Some(AnthropicStreamDelta {
            r#type: "signature_delta".to_string(),
            text: None,
            thinking: None,
            partial_json: None,
            signature: Some("sig-abc".to_string()),
            stop_reason: None,
        }),
        stop_reason: None,
    };
    let chunks1 = p.process_event(&ev1);
    assert!(
        chunks1.is_empty(),
        "signature_delta emits no chunk (got {:?})",
        chunks1
    );
    // 2) thinking_delta picks up the signature.
    let ev2 = AnthropicStreamEvent {
        r#type: "content_block_delta".to_string(),
        content_block: None,
        index: Some(0),
        delta: Some(AnthropicStreamDelta {
            r#type: "thinking_delta".to_string(),
            text: None,
            thinking: Some("r".to_string()),
            partial_json: None,
            signature: None,
            stop_reason: None,
        }),
        stop_reason: None,
    };
    let chunks2 = p.process_event(&ev2);
    match &chunks2[0] {
        StreamChunk::Content(ContentPart::Reasoning(r)) => {
            assert_eq!(r.signature, Some("sig-abc".to_string()));
        }
        _ => panic!("expected Content(Reasoning)"),
    }
}

// -- tool_use lifecycle ------------------------------------------

/// `content_block_start` (tool_use) MUST emit `ToolCallStart`
/// and buffer the input.
#[test]
fn tool_use_start_emits_tool_call_start() {
    let mut p = StreamProcessor::new();
    let ev = AnthropicStreamEvent {
        r#type: "content_block_start".to_string(),
        content_block: Some(AnthropicStreamContentBlock {
            r#type: "tool_use".to_string(),
            id: Some("t-1".to_string()),
            name: Some("bash".to_string()),
            input: Some(serde_json::json!({"cmd": "ls"})),
            thinking: None,
        }),
        index: Some(0),
        delta: None,
        stop_reason: None,
    };
    let chunks = p.process_event(&ev);
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        StreamChunk::ToolCallStart {
            id,
            name,
            arguments,
        } => {
            assert_eq!(id, "t-1");
            assert_eq!(name, "bash");
            assert_eq!(
                *arguments,
                serde_json::json!("{\"cmd\":\"ls\"}".to_string())
            );
        }
        other => panic!("expected ToolCallStart, got {other:?}"),
    }
}

/// `input_json_delta` MUST emit `ToolCallDelta` and
/// append to the buffer.
#[test]
fn input_json_delta_accumulates() {
    let mut p = StreamProcessor::new();
    // Start the tool_use first.
    p.process_event(&AnthropicStreamEvent {
        r#type: "content_block_start".to_string(),
        content_block: Some(AnthropicStreamContentBlock {
            r#type: "tool_use".to_string(),
            id: Some("t-1".to_string()),
            name: Some("bash".to_string()),
            input: Some(serde_json::Value::String("".to_string())),
            thinking: None,
        }),
        index: Some(0),
        delta: None,
        stop_reason: None,
    });
    // Now send two deltas.
    let ev = AnthropicStreamEvent {
        r#type: "content_block_delta".to_string(),
        content_block: None,
        index: Some(0),
        delta: Some(AnthropicStreamDelta {
            r#type: "input_json_delta".to_string(),
            text: None,
            thinking: None,
            partial_json: Some("{\"cmd\":\"l".to_string()),
            signature: None,
            stop_reason: None,
        }),
        stop_reason: None,
    };
    let chunks = p.process_event(&ev);
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        StreamChunk::ToolCallDelta {
            id,
            arguments_delta,
        } => {
            assert_eq!(id, "t-1");
            assert_eq!(arguments_delta, "{\"cmd\":\"l");
        }
        _ => panic!("expected ToolCallDelta"),
    }
}

/// `content_block_stop` MUST emit `ToolCallEnd` and
/// finalize the tool_use (populating
/// `SamplingResult.tool_calls` on `message_stop`).
#[test]
fn content_block_stop_finalizes_tool_use() {
    let mut p = StreamProcessor::new();
    // Start.
    p.process_event(&AnthropicStreamEvent {
        r#type: "content_block_start".to_string(),
        content_block: Some(AnthropicStreamContentBlock {
            r#type: "tool_use".to_string(),
            id: Some("t-1".to_string()),
            name: Some("bash".to_string()),
            input: Some(serde_json::json!({"cmd": "ls"})),
            thinking: None,
        }),
        index: Some(0),
        delta: None,
        stop_reason: None,
    });
    // Stop.
    let chunks = p.process_event(&AnthropicStreamEvent {
        r#type: "content_block_stop".to_string(),
        content_block: None,
        index: Some(0),
        delta: None,
        stop_reason: None,
    });
    assert_eq!(chunks.len(), 1);
    assert!(
        matches!(&chunks[0], StreamChunk::ToolCallEnd { id } if id == "t-1")
    );
}

// -- message_stop / finalize -------------------------------------

/// `message_stop` MUST emit `IsDone { result }` with the
/// default stop_reason `"end_turn"` when none was set.
#[test]
fn message_stop_default_stop_reason_is_end_turn() {
    let mut p = StreamProcessor::new();
    let chunks = p.process_event(&AnthropicStreamEvent {
        r#type: "message_stop".to_string(),
        content_block: None,
        index: None,
        delta: None,
        stop_reason: None,
    });
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        StreamChunk::IsDone { result } => {
            assert_eq!(result.stop_reason, Some("end_turn".to_string()));
        }
        _ => panic!("expected IsDone"),
    }
}

/// `message_stop` MUST respect an explicit stop_reason from
/// a prior `message_delta` event.
#[test]
fn message_delta_then_stop_propagates_reason() {
    let mut p = StreamProcessor::new();
    p.process_event(&AnthropicStreamEvent {
        r#type: "message_delta".to_string(),
        content_block: None,
        index: None,
        delta: None,
        stop_reason: Some("tool_use".to_string()),
    });
    let chunks = p.process_event(&AnthropicStreamEvent {
        r#type: "message_stop".to_string(),
        content_block: None,
        index: None,
        delta: None,
        stop_reason: None,
    });
    match &chunks[0] {
        StreamChunk::IsDone { result } => {
            assert_eq!(result.stop_reason, Some("tool_use".to_string()));
        }
        _ => panic!("expected IsDone"),
    }
}

/// `message_stop` MUST drain any residual ThinkExtractor
/// text into the final SamplingResult.
#[test]
fn message_stop_drains_residual_text() {
    let mut p = StreamProcessor::new();
    // Send a text_delta that contains an unbalanced
    // `<think>` marker so the ThinkExtractor is still
    // holding text at message_stop.
    p.process_event(&AnthropicStreamEvent {
        r#type: "content_block_delta".to_string(),
        content_block: None,
        index: Some(0),
        delta: Some(AnthropicStreamDelta {
            r#type: "text_delta".to_string(),
            text: Some("unbalanced think<".to_string()),
            thinking: None,
            partial_json: None,
            signature: None,
            stop_reason: None,
        }),
        stop_reason: None,
    });
    let chunks = p.process_event(&AnthropicStreamEvent {
        r#type: "message_stop".to_string(),
        content_block: None,
        index: None,
        delta: None,
        stop_reason: None,
    });
    match &chunks[0] {
        StreamChunk::IsDone { result } => {
            // The drain flushed the residual text into the
            // final result.
            assert!(
                result.text.contains("think"),
                "expected text drain (got {:?})",
                result.text
            );
        }
        _ => panic!("expected IsDone"),
    }
}

// -- unknown event types -----------------------------------------

/// `process_event` MUST silently drop unknown event
/// types (no chunks emitted, no panic).
#[test]
fn unknown_event_type_emits_no_chunks() {
    let mut p = StreamProcessor::new();
    for t in ["ping", "x", "message_start", "wat"] {
        let chunks = p.process_event(&AnthropicStreamEvent {
            r#type: t.to_string(),
            content_block: None,
            index: None,
            delta: None,
            stop_reason: None,
        });
        assert!(chunks.is_empty(), "event {t:?} produced {:?}", chunks);
    }
}

// -- helpers -----------------------------------------------------

/// Helper: drain a processor with a `message_stop` and
/// return the produced chunks.
fn finalize(p: &mut StreamProcessor) -> Vec<StreamChunk> {
    p.process_event(&AnthropicStreamEvent {
        r#type: "message_stop".to_string(),
        content_block: None,
        index: None,
        delta: None,
        stop_reason: None,
    })
}
/// A REAL tool-call turn, replayed from a capture of the
/// Anthropic-compatible endpoint this project is configured against
/// (`tests/fixtures/anthropic_tool_stream.sse`).
///
/// Three defects only a real capture could expose, all of which made
/// every tool call from that endpoint vanish:
///
/// 1. `content_block.input` is an OBJECT on the wire (`{}`), not a
///    string, so the typed struct rejected the whole frame and the
///    caller's `if let Ok(event)` dropped it silently.
/// 2. `message_delta.delta` carries ONLY `stop_reason` — no `type` —
///    so requiring `type` dropped that frame too, which is why every
///    real turn reported the `end_turn` default rather than the
///    endpoint's actual `tool_use`.
/// 3. The `{}` placeholder must NOT seed the argument buffer, or the
///    fragments accumulate into `{}{"command":…}`.
///
/// Fixture rather than a synthetic event stream on purpose: hand-built
/// events encoded the same wrong assumptions as the parser, so they
/// agreed with it while the real wire did not.
#[test]
fn a_captured_tool_call_turn_is_parsed_completely() {
    let raw =
        include_str!("../../../../tests/fixtures/anthropic_tool_stream.sse");
    let mut p = StreamProcessor::new();
    let mut chunks = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(data) = line.strip_prefix("data: ")
            && let Ok(event) = serde_json::from_str::<
                crate::streaming::AnthropicStreamEvent,
            >(data)
        {
            chunks.extend(p.process_event(&event));
        }
    }

    let result = chunks
        .iter()
        .find_map(|c| match c {
            StreamChunk::IsDone { result } => Some((**result).clone()),
            _ => None,
        })
        .expect("the stream terminates");

    assert_eq!(
        result.tool_calls.len(),
        1,
        "the captured turn carried one tool_use"
    );
    let call = &result.tool_calls[0];
    assert_eq!(call.name, "shell");
    assert_eq!(
        call.input,
        serde_json::json!({"command": "echo AAA"}),
        "the argument fragments must accumulate into clean JSON, not \
         be preceded by the `{{}}` placeholder"
    );
    assert_eq!(
        result.stop_reason.as_deref(),
        Some("tool_use"),
        "the endpoint's nested stop_reason must be read"
    );
    // The thinking block that preceded the call survives too.
    assert!(
        result.reasoning.contains("shell command"),
        "the turn's thinking block must be preserved: {:?}",
        result.reasoning
    );
    assert!(
        chunks
            .iter()
            .any(|c| matches!(c, StreamChunk::ToolCallStart { .. })),
        "a live consumer needs the call announced as it starts"
    );
}
