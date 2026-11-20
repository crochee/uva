//! The decoder's contract: SSE lines in, a chunk sequence out. The
//! fold's own suite (`crate::assembler::tests`) covers what those
//! chunks add up to, so nothing here reaches into a turn the decoder
//! no longer builds.

use super::*;
use crate::{
    BlockAssembler,
    types::{ContentPart, StreamChunk},
};

fn fold(chunks: Vec<StreamChunk>) -> (crate::SamplingResult, bool) {
    let mut assembler = BlockAssembler::new();
    for chunk in chunks {
        assembler.push(chunk);
    }
    let (result, _parts, incomplete) = assembler.finalize_assistant();
    (result, incomplete)
}

fn process_all(
    p: &mut OpenAIStreamProcessor,
    lines: &[&str],
) -> Vec<StreamChunk> {
    lines.iter().flat_map(|line| p.process_line(line)).collect()
}

#[test]
fn new_and_default_start_empty() {
    assert!(OpenAIStreamProcessor::new().process_line("").is_empty());
    assert!(OpenAIStreamProcessor::default().process_line("").is_empty());
}

/// Non-JSON lines are silent no-ops — empty keepalives, whitespace, SSE
/// comments and corrupt JSON must not terminate the stream.
#[test]
fn non_json_lines_are_silently_dropped() {
    let mut p = OpenAIStreamProcessor::new();
    for line in ["", "   \t  ", ": heartbeat", "{not json", "data: {choices}"] {
        assert!(p.process_line(line).is_empty(), "line {line:?}");
    }
    let chunks =
        p.process_line(r#"{"choices":[{"delta":{"content":"hello"}}]}"#);
    assert!(matches!(
        &chunks[0],
        StreamChunk::Content(ContentPart::Text(t)) if t.text == "hello"
    ));
}

/// Content and reasoning deltas each emit their own chunk; an empty
/// delta emits nothing.
#[test]
fn content_and_reasoning_emit_content_chunks() {
    let mut p = OpenAIStreamProcessor::new();
    assert!(
        p.process_line(
            r#"{"choices":[{"delta":{"content":""},"finish_reason":null}]}"#
        )
        .is_empty()
    );
    let chunks = p.process_line(
        r#"{"choices":[{"delta":{"reasoning_content":"think","content":"answer"},"finish_reason":null}]}"#,
    );
    assert_eq!(chunks.len(), 2);
    assert!(matches!(
        &chunks[0],
        StreamChunk::Content(ContentPart::Reasoning(r)) if r.text == "think"
    ));
    assert!(matches!(
        &chunks[1],
        StreamChunk::Content(ContentPart::Text(t)) if t.text == "answer"
    ));
}

/// Inline `<think>…</think>` markers split into reasoning + text; a
/// marker split across deltas is assembled from the extractor's carry.
#[test]
fn inline_think_markers_split_into_reasoning_and_text() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = process_all(
        &mut p,
        &[
            r#"{"choices":[{"delta":{"content":"hello <thin"}}]}"#,
            r#"{"choices":[{"delta":{"content":"k>reason</think>done"}}]}"#,
        ],
    );
    let reasoning: String = chunks
        .iter()
        .filter_map(|c| match c {
            StreamChunk::Content(ContentPart::Reasoning(r)) => {
                Some(r.text.as_str())
            }
            _ => None,
        })
        .collect();
    let text: String = chunks
        .iter()
        .filter_map(|c| match c {
            StreamChunk::Content(ContentPart::Text(t)) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "reason");
    assert_eq!(text, "hello done");
}

/// The extractor's withheld tail leaves as content when the finish
/// marker seals the turn — it used to be folded into a terminal result
/// that the fold discarded whenever text had already streamed.
#[test]
fn residual_think_text_is_emitted_as_content_at_the_seal() {
    let mut p = OpenAIStreamProcessor::new();
    p.process_line(r#"{"choices":[{"delta":{"content":"answer <thin"}}]}"#);
    let chunks =
        p.process_line(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#);
    let carry: String = chunks
        .iter()
        .filter_map(|c| match c {
            StreamChunk::Content(ContentPart::Text(t)) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(carry, "<thin");
    assert!(matches!(chunks.last(), Some(StreamChunk::Stop(r)) if r == "stop"));
}

/// First tool delta announces the call; later ones append arguments.
#[test]
fn tool_deltas_emit_start_then_delta() {
    let mut p = OpenAIStreamProcessor::new();
    let start = p.process_line(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"loc"}}]}}]}"#,
    );
    assert!(matches!(
        &start[0],
        StreamChunk::ToolCallStart { id, name, arguments }
            if id == "call_1"
                && name == "get_weather"
                && arguments == &serde_json::json!("{\"loc")
    ));
    let delta = p.process_line(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ation\":\"Beijing\"}"}}]}}]}"#,
    );
    assert!(matches!(
        &delta[0],
        StreamChunk::ToolCallDelta { id, arguments_delta }
            if id == "call_1" && arguments_delta == "ation\":\"Beijing\"}"
    ));
}

/// `id` and `function.name` arrive in separate fragments; a later
/// fragment's name is a `ToolCallName` update for the call the start
/// chunk already announced.
#[test]
fn a_later_fragment_emits_a_tool_call_name_update() {
    let mut p = OpenAIStreamProcessor::new();
    let start = p.process_line(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"arguments":""}}]}}]}"#,
    );
    assert!(matches!(
        &start[0],
        StreamChunk::ToolCallStart { name, .. } if name == "unknown"
    ));

    let update = p.process_line(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"read_file"}}]}}]}"#,
    );
    assert_eq!(update.len(), 1, "one chunk, the name update: {update:?}");
    assert!(matches!(
        &update[0],
        StreamChunk::ToolCallName { id, name }
            if id == "call_1" && name == "read_file"
    ));
}

/// A first delta without an id synthesizes `call_{index}` so every
/// consumer has a stable id to key the call by.
#[test]
fn tool_call_id_falls_back_to_the_index() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = process_all(
        &mut p,
        &[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":2,"type":"function","function":{"name":"get_weather"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ],
    );
    assert!(matches!(
        &chunks[0],
        StreamChunk::ToolCallStart { id, .. } if id == "call_2"
    ));
    let (turn, _) = fold(chunks);
    assert_eq!(turn.tool_calls.len(), 1);
    assert_eq!(turn.tool_calls[0].id, "call_2");
    assert_eq!(turn.tool_calls[0].name, "get_weather");
}

/// `tool_calls: []` is a heartbeat, not a termination signal.
#[test]
fn empty_tool_calls_array_is_a_no_op() {
    let mut p = OpenAIStreamProcessor::new();
    p.process_line(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"get_weather","arguments":"{\"loc\":"}}]}}]}"#,
    );
    assert!(
        p.process_line(r#"{"choices":[{"delta":{"tool_calls":[]}}]}"#)
            .is_empty()
    );
    let chunks = p.process_line(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ation\":\"Beijing\"}"}}]}}]}"#,
    );
    assert!(
        matches!(&chunks[0], StreamChunk::ToolCallDelta { id, .. } if id == "call_x")
    );
}

/// Only the first choice of a multi-choice frame is decoded.
#[test]
fn multi_choice_takes_only_the_first_choice() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(
        r#"{"choices":[{"delta":{"content":"first"},"index":0},{"delta":{"content":"second"},"index":1}]}"#,
    );
    assert_eq!(chunks.len(), 1);
    assert!(matches!(
        &chunks[0],
        StreamChunk::Content(ContentPart::Text(t)) if t.text == "first"
    ));
}

/// The authoritative usage is a sibling of `choices`; a usage-only
/// final frame (empty `choices`) must still emit it.
#[test]
fn top_level_usage_emits_a_usage_chunk() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(
        r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":2,"total_tokens":13}}"#,
    );
    assert_eq!(chunks.len(), 1);
    assert!(matches!(
        &chunks[0],
        StreamChunk::Usage(u) if u.prompt_tokens == 11
            && u.completion_tokens == 2
            && u.total_tokens == 13
    ));
}

/// A gateway that nests usage inside the choice is tolerated.
#[test]
fn choice_nested_usage_is_tolerated() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(
        r#"{"choices":[{"delta":{},"finish_reason":null,"usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}}]}"#,
    );
    assert!(matches!(
        &chunks[0],
        StreamChunk::Usage(u) if u.prompt_tokens == 100
            && u.completion_tokens == 50
    ));
}

/// The finish marker closes the open calls and emits the terminal
/// `Stop`; a subsequent `[DONE]` adds nothing.
#[test]
fn finish_marker_closes_tools_and_emits_stop_once() {
    let mut p = OpenAIStreamProcessor::new();
    p.process_line(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"bash","arguments":"{}"}}]}}]}"#,
    );
    let chunks = process_all(
        &mut p,
        &[
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ],
    );
    assert_eq!(
        chunks
            .iter()
            .filter(|c| matches!(c, StreamChunk::ToolCallEnd { .. }))
            .count(),
        1
    );
    assert_eq!(
        chunks
            .iter()
            .filter(|c| matches!(c, StreamChunk::Stop(_)))
            .count(),
        1
    );
    assert!(
        matches!(chunks.last(), Some(StreamChunk::Stop(r)) if r == "tool_calls")
    );
    let (turn, incomplete) = fold(chunks);
    assert!(!incomplete, "the stop chunk completed the turn");
    assert_eq!(turn.stop_reason.as_deref(), Some("tool_calls"));
}

/// Once the finish marker sealed the turn, content and a repeated
/// finish marker add nothing, but a late usage frame is still decoded.
#[test]
fn a_late_usage_frame_is_decoded_after_the_seal() {
    let mut p = OpenAIStreamProcessor::new();
    let finish =
        p.process_line(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#);
    assert!(matches!(finish.last(), Some(StreamChunk::Stop(_))));

    let after = p.process_line(
        r#"{"choices":[{"delta":{"content":"!"},"finish_reason":"stop"}]}"#,
    );
    assert!(
        after.is_empty(),
        "content after the seal is ignored: {after:?}"
    );

    let late = p.process_line(
        r#"{"choices":[],"usage":{"prompt_tokens":41,"completion_tokens":7,"total_tokens":48}}"#,
    );
    assert!(matches!(
        &late[0],
        StreamChunk::Usage(u) if u.prompt_tokens == 41
    ));
    assert!(
        !late.iter().any(|c| matches!(c, StreamChunk::Stop(_))),
        "the late usage frame must not close the turn again: {late:?}"
    );
    assert!(
        p.process_line("[DONE]").is_empty(),
        "the end-of-stream marker must not close the sealed turn again"
    );
}

/// A truncated stream (the sentinel, no finish marker) reports no stop
/// reason. It emits no terminal chunk, and the fold still drains the
/// tool buffer the stream never closed.
#[test]
fn done_without_a_finish_marker_emits_no_stop_and_the_fold_drains() {
    let mut p = OpenAIStreamProcessor::new();
    let start = process_all(
        &mut p,
        &[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"get_weather","arguments":"{\"location\":\"Beijing\"}"}}]}}]}"#,
            "[DONE]",
        ],
    );
    assert!(
        !start.iter().any(|c| matches!(c, StreamChunk::Stop(_))),
        "no finish marker means no stop reason: {start:?}"
    );
    let (turn, incomplete) = fold(start);
    assert!(incomplete, "a stream without a stop reason is truncated");
    assert_eq!(turn.tool_calls.len(), 1);
    assert_eq!(turn.tool_calls[0].id, "call_x");
    assert_eq!(
        turn.tool_calls[0].input,
        serde_json::json!({"location": "Beijing"})
    );
    assert_eq!(turn.stop_reason, None);

    // Everything after the sentinel belongs to nobody.
    assert!(
        p.process_line(r#"{"choices":[{"delta":{"content":"x"}}]}"#)
            .is_empty()
    );
}
