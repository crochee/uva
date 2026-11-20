//! The decoder's contract: a wire event sequence in, a chunk sequence
//! out. The fold's own suite (`crate::assembler::tests`) covers what
//! those chunks add up to, so nothing here reaches into a turn the
//! decoder no longer builds.

use super::*;
use crate::{
    BlockAssembler,
    streaming::anthropic::events::AnthropicStreamEvent,
    types::{ContentPart, StreamChunk, TokenUsage},
};

fn event(json: &str) -> AnthropicStreamEvent {
    serde_json::from_str(json).expect("valid event")
}

fn simple_event(r#type: &str) -> AnthropicStreamEvent {
    AnthropicStreamEvent {
        r#type: r#type.to_string(),
        content_block: None,
        index: None,
        delta: None,
        stop_reason: None,
        message: None,
        usage: None,
    }
}

fn fold(chunks: Vec<StreamChunk>) -> (crate::SamplingResult, bool) {
    let mut assembler = BlockAssembler::new();
    for chunk in chunks {
        assembler.push(chunk);
    }
    let (result, _parts, incomplete) = assembler.finalize_assistant();
    (result, incomplete)
}

fn usage_chunks(chunks: &[StreamChunk]) -> Vec<TokenUsage> {
    chunks
        .iter()
        .filter_map(|chunk| match chunk {
            StreamChunk::Usage(usage) => Some(usage.clone()),
            _ => None,
        })
        .collect()
}

/// A captured real-wire turn decodes to one chunk sequence: the
/// reasoning deltas, the signature as its own chunk, the tool call's
/// start/deltas/end, the usage report and the stop reason — and no
/// finished turn.
///
/// The fixture is a capture of the Anthropic-compatible endpoint this
/// project is configured against. Three defects only real bytes
/// exposed: an OBJECT-form `content_block.input` (`{}`), a
/// `message_delta.delta` carrying only `stop_reason`, and the `{}`
/// placeholder that must not seed the argument buffer.
#[test]
fn a_captured_tool_call_turn_decodes_to_its_chunk_sequence() {
    let raw =
        include_str!("../../../../tests/fixtures/anthropic_tool_stream.sse");
    let mut processor = StreamProcessor::new();
    let mut chunks = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(data) = line.strip_prefix("data: ")
            && let Ok(event) =
                serde_json::from_str::<AnthropicStreamEvent>(data)
        {
            chunks.extend(processor.process_event(&event));
        }
    }

    assert!(
        !chunks
            .iter()
            .any(|c| matches!(c, StreamChunk::IsDone { .. })),
        "a decoder never builds a finished turn; chunks were {chunks:?}"
    );
    assert!(
        chunks.iter().any(|c| matches!(
            c,
            StreamChunk::Content(ContentPart::Reasoning(r)) if r.text.contains("shell command")
        )),
        "reasoning arrives as content chunks; chunks were {chunks:?}"
    );
    assert!(
        chunks.iter().any(|c| matches!(
            c,
            StreamChunk::ReasoningSignature(sig)
                if sig == "c3054ee8db5a268e5776b248f58f4272665da233687f633ee5f11e49bae4ce0e"
        )),
        "the signature is its own chunk; chunks were {chunks:?}"
    );

    let start = chunks
        .iter()
        .find_map(|c| match c {
            StreamChunk::ToolCallStart {
                id,
                name,
                arguments,
            } => Some((id.clone(), name.clone(), arguments.clone())),
            _ => None,
        })
        .expect("the tool call is announced");
    assert_eq!(start.0, "call_c7b17d26e0be47e1a542e5be");
    assert_eq!(start.1, "shell");
    let deltas: String = chunks
        .iter()
        .filter_map(|c| match c {
            StreamChunk::ToolCallDelta {
                arguments_delta, ..
            } => Some(arguments_delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas, "{\"command\": \"echo AAA\"}",
        "argument fragments accumulate verbatim; got {deltas:?}"
    );
    assert!(
        chunks.iter().any(|c| matches!(
            c,
            StreamChunk::ToolCallEnd { id }
                if id == "call_c7b17d26e0be47e1a542e5be"
        )),
        "the call is closed; chunks were {chunks:?}"
    );

    let usage = usage_chunks(&chunks)
        .pop()
        .expect("the wire usage is reported");
    assert_eq!(usage.prompt_tokens, 197);
    assert_eq!(usage.completion_tokens, 50);
    assert_eq!(usage.total_tokens, 247);
    assert!(
        matches!(chunks.last(), Some(StreamChunk::Stop(reason)) if reason == "tool_use"),
        "the stop reason is the terminal chunk; chunks were {chunks:?}"
    );

    // Folding the same chunks lands the turn the endpoint meant.
    let (turn, incomplete) = fold(chunks);
    assert!(!incomplete, "the stop chunk finished the step");
    assert_eq!(turn.tool_calls.len(), 1);
    assert_eq!(turn.tool_calls[0].name, "shell");
    assert_eq!(
        turn.tool_calls[0].input,
        serde_json::json!({"command": "echo AAA"}),
        "the `{{}}` placeholder must not seed the buffer"
    );
    assert_eq!(turn.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(
        turn.reasoning_signature.as_deref(),
        Some(
            "c3054ee8db5a268e5776b248f58f4272665da233687f633ee5f11e49bae4ce0e"
        )
    );
    assert_eq!(turn.usage.prompt_tokens, 197);
    assert_eq!(turn.usage.completion_tokens, 50);
}

/// The opening frame reports the prompt half, the terminal delta the
/// final output half. Each usage chunk carries the merge so far — the
/// fold keeps the last one, so the returned turn cannot lose the
/// opening input count.
#[test]
fn usage_frames_grow_one_merged_usage_chunk() {
    let mut processor = StreamProcessor::new();
    let first = processor.process_event(&event(
        r#"{"type":"message_start","message":{"usage":{"input_tokens":120,"output_tokens":1,"cache_read_input_tokens":80,"cache_creation_input_tokens":20}}}"#,
    ));
    let second = processor.process_event(&event(
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":30}}"#,
    ));

    let usages = usage_chunks(&first);
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].prompt_tokens, 120);
    assert_eq!(usages[0].completion_tokens, 1);

    let usages = usage_chunks(&second);
    assert_eq!(usages.len(), 1, "the terminal frame emits the merge");
    let usage = &usages[0];
    assert_eq!(usage.prompt_tokens, 120, "the opening input survives");
    assert_eq!(usage.completion_tokens, 30, "the terminal output wins");
    assert_eq!(usage.total_tokens, 150);
    assert_eq!(usage.cached_prompt_tokens, Some(80));
    assert_eq!(usage.cache_read_tokens, Some(80));
    assert_eq!(usage.cache_write_tokens, Some(20));
    assert_eq!(usage.reasoning_tokens, None);
}

/// A provider-reported cache miss is a value: zero stays a zero-valued
/// bucket rather than disappearing into `None`.
#[test]
fn a_reported_cache_zero_stays_a_zero_bucket() {
    let mut processor = StreamProcessor::new();
    let chunks = processor.process_event(&event(
        r#"{"type":"message_start","message":{"usage":{"input_tokens":5,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
    ));
    let usage = &usage_chunks(&chunks)[0];
    assert_eq!(usage.cached_prompt_tokens, Some(0));
    assert_eq!(usage.cache_read_tokens, Some(0));
    assert_eq!(usage.cache_write_tokens, Some(0));
}

/// The residual the think extractor withholds becomes content, not a
/// line on a terminal result the decoder no longer builds. In the
/// ordinary case — text already streamed — the old terminal fold
/// dropped it, so it never reached the projection or the turn.
#[test]
fn residual_think_text_is_emitted_as_content() {
    let mut processor = StreamProcessor::new();
    let streamed = processor.process_event(&event(
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"answer <thin"}}"#,
    ));
    assert!(
        streamed
            .iter()
            .all(|c| matches!(c, StreamChunk::Content(_))),
        "the visible prefix streams as content; got {streamed:?}"
    );

    // The real order: the terminal delta carries the stop reason, then
    // the stream closes.
    let closing = processor.process_event(&event(
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
    ));
    let carry: String = closing
        .iter()
        .filter_map(|c| match c {
            StreamChunk::Content(ContentPart::Text(t)) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(carry, "<thin", "the withheld tail leaves as content");
    assert!(
        matches!(closing.last(), Some(StreamChunk::Stop(_))),
        "the terminal chunk closes the sequence after the carry; got {closing:?}"
    );
    let after = processor.process_event(&event(r#"{"type":"message_stop"}"#));
    assert!(after.is_empty(), "nothing follows the terminal chunk");

    let (turn, _) = fold(streamed.into_iter().chain(closing).collect());
    assert_eq!(
        turn.text, "answer <thin",
        "the carry lands in the finished turn"
    );
}

/// `message_stop` without a terminal delta still closes the step with
/// the wire's historical default, and it emits `Stop` — never `IsDone`.
#[test]
fn message_stop_without_a_reason_emits_the_end_turn_stop() {
    let mut processor = StreamProcessor::new();
    let chunks = processor.process_event(&simple_event("message_stop"));
    assert_eq!(
        chunks.len(),
        1,
        "one terminal chunk, no finished turn: {chunks:?}"
    );
    assert!(
        matches!(&chunks[0], StreamChunk::Stop(reason) if reason == "end_turn")
    );
}

/// The terminal delta's stop reason is the chunk the fold turns into
/// the step's stop reason; a second terminal delta must not append a
/// second one.
#[test]
fn terminal_delta_emits_the_stop_chunk_once() {
    let mut processor = StreamProcessor::new();
    let first = processor.process_event(&event(
        r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#,
    ));
    assert!(
        matches!(first.last(), Some(StreamChunk::Stop(r)) if r == "max_tokens")
    );

    let second = processor.process_event(&event(r#"{"type":"message_delta"}"#));
    let closing = processor.process_event(&simple_event("message_stop"));
    assert!(
        !second
            .iter()
            .chain(&closing)
            .any(|c| matches!(c, StreamChunk::Stop(_))),
        "the step reports its stop reason once; got {second:?} / {closing:?}"
    );
}

/// `signature_delta` is its own chunk: it arrives after the reasoning
/// text it signs, so it cannot ride the reasoning chunks.
#[test]
fn signature_delta_is_its_own_chunk() {
    let mut processor = StreamProcessor::new();
    let chunks = processor.process_event(&event(
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-abc"}}"#,
    ));
    assert_eq!(chunks.len(), 1);
    assert!(
        matches!(&chunks[0], StreamChunk::ReasoningSignature(sig) if sig == "sig-abc")
    );
}

/// A tool block's arguments travel as start + deltas + end, with the
/// object-form placeholder (`{}`) leaving the buffer empty.
#[test]
fn tool_use_emits_start_delta_end() {
    let mut processor = StreamProcessor::new();
    let start = processor.process_event(&event(
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"get_weather","input":{}}}"#,
    ));
    assert!(matches!(
        &start[0],
        StreamChunk::ToolCallStart { id, name, arguments }
            if id == "t1" && name == "get_weather" && arguments == &serde_json::json!("")
    ));
    let delta = processor.process_event(&event(
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"location\":\"Beijing\"}"}}"#,
    ));
    assert!(matches!(
        &delta[0],
        StreamChunk::ToolCallDelta { id, arguments_delta }
            if id == "t1" && arguments_delta == "{\"location\":\"Beijing\"}"
    ));
    let end = processor.process_event(&simple_event("content_block_stop"));
    // `simple_event` carries no index, so use a real frame.
    assert!(end.is_empty());
    let end = processor
        .process_event(&event(r#"{"type":"content_block_stop","index":0}"#));
    assert!(matches!(&end[0], StreamChunk::ToolCallEnd { id } if id == "t1"));
}

/// `thinking` and `redacted_thinking` blocks surface as reasoning
/// content; the redacted marker is fixed and unsigned.
#[test]
fn thinking_blocks_emit_reasoning_content() {
    let mut processor = StreamProcessor::new();
    let chunks = processor.process_event(&event(
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"step one"}}"#,
    ));
    assert!(matches!(
        &chunks[0],
        StreamChunk::Content(ContentPart::Reasoning(r))
            if r.text == "step one" && r.signature.is_none()
    ));
    let chunks = processor.process_event(&event(
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking"}}"#,
    ));
    assert!(matches!(
        &chunks[0],
        StreamChunk::Content(ContentPart::Reasoning(r))
            if r.text == "[Redacted by safety filter]" && r.signature.is_none()
    ));
}

/// `new()` and `default()` behave alike: unknown events produce no
/// chunks.
#[test]
fn unknown_events_produce_no_chunks() {
    for processor in [StreamProcessor::new(), StreamProcessor::default()] {
        let mut processor = processor;
        for name in ["ping", "x", "wat"] {
            assert!(processor.process_event(&simple_event(name)).is_empty());
        }
    }
}
