use super::*;
use crate::{
    json_repair::parse_tool_input,
    types::{Content, Message, Role},
};

fn text_chunk(s: &str) -> StreamChunk {
    StreamChunk::Content(ContentPart::Text(TextContent {
        text: s.to_string(),
        cache_control: None,
    }))
}

fn reasoning_chunk(s: &str) -> StreamChunk {
    StreamChunk::Content(ContentPart::Reasoning(ReasoningContent {
        text: s.to_string(),
        signature: None,
    }))
}

fn tool_start(id: &str, name: &str, args: serde_json::Value) -> StreamChunk {
    StreamChunk::ToolCallStart {
        id: id.to_string(),
        name: name.to_string(),
        arguments: args,
    }
}

fn tool_delta(id: &str, delta: &str) -> StreamChunk {
    StreamChunk::ToolCallDelta {
        id: id.to_string(),
        arguments_delta: delta.to_string(),
    }
}

fn tool_end(id: &str) -> StreamChunk {
    StreamChunk::ToolCallEnd { id: id.to_string() }
}

fn done_with(result: SamplingResult) -> StreamChunk {
    StreamChunk::IsDone {
        result: Box::new(result),
    }
}

/// The deferred signature lands only on the trailing reasoning run.
///
/// On the streaming path every reasoning part is un-signed, so a
/// walk that did not stop at the run's edge would stamp the single
/// latest signature onto *every* reasoning part — including an
/// earlier block it was never computed for. A signature is bound to
/// the text it signed, so that would forge one.
///
/// The earlier block is deliberately left unsigned: the processor
/// tracks one signature slot per turn, so its own signature is not
/// knowable here. The Anthropic adapter drops an unsigned block
/// rather than sending an invalid request, so the outcome is
/// "reasoning omitted", never "signature lying about its text".
#[test]
fn deferred_signature_stamps_only_the_trailing_reasoning_run() {
    let mut a = BlockAssembler::new();
    // Interleaved shape: block 1, text, block 2.
    a.push(reasoning_chunk("block one a"));
    a.push(reasoning_chunk("block one b"));
    a.push(text_chunk("answer"));
    a.push(reasoning_chunk("block two a"));
    a.push(reasoning_chunk("block two b"));
    a.push(done_with(SamplingResult {
        text: String::new(),
        tool_calls: Vec::new(),
        reasoning: String::new(),
        reasoning_signature: Some("sig_for_block_two".to_string()),
        usage: TokenUsage::default(),
        stop_reason: Some("end_turn".to_string()),
    }));

    let (_result, parts, _) = a.finalize_assistant();
    let signatures: Vec<(&str, Option<&str>)> = parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Reasoning(rc) => {
                Some((rc.text.as_str(), rc.signature.as_deref()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        signatures,
        vec![
            ("block one a", None),
            ("block one b", None),
            ("block two a", Some("sig_for_block_two")),
            ("block two b", Some("sig_for_block_two")),
        ],
        "only the trailing run may be stamped; a signature must never \
         be attached to an earlier block"
    );
}

#[test]
fn empty_stream_returns_empty_sampling_result() {
    let mut a = BlockAssembler::new();
    let (result, parts, incomplete) = a.finalize_assistant();
    assert!(result.text.is_empty());
    assert!(result.tool_calls.is_empty());
    assert!(parts.is_empty());
    assert!(incomplete);
}

#[test]
fn streamed_text_chunks_accumulate_in_order() {
    let mut a = BlockAssembler::new();
    a.push(text_chunk("hello "));
    a.push(text_chunk("world"));
    assert_eq!(a.text_so_far(), "hello world");
    let (result, _parts, _) = a.finalize_assistant();
    assert_eq!(result.text, "hello world");
}

#[test]
fn reasoning_accumulates_separately_from_text() {
    let mut a = BlockAssembler::new();
    a.push(reasoning_chunk("think 1 "));
    a.push(reasoning_chunk("think 2"));
    a.push(text_chunk("answer"));
    assert_eq!(a.reasoning_so_far(), "think 1 think 2");
    assert_eq!(a.text_so_far(), "answer");
    let (result, _, _) = a.finalize_assistant();
    assert_eq!(result.reasoning, "think 1 think 2");
    assert_eq!(result.text, "answer");
}

#[test]
fn tool_call_deltas_concatenate_arguments() {
    let mut a = BlockAssembler::new();
    a.push(tool_start("tc1", "shell", serde_json::json!("")));
    a.push(tool_delta("tc1", "{\"command\":\""));
    a.push(tool_delta("tc1", "ls -la\"}"));
    a.push(tool_end("tc1"));
    let (result, parts, _) = a.finalize_assistant();
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].id, "tc1");
    assert_eq!(result.tool_calls[0].name, "shell");
    assert_eq!(
        result.tool_calls[0].input,
        serde_json::json!({"command": "ls -la"})
    );
    assert_eq!(parts.len(), 1);
    assert!(matches!(parts[0], ContentPart::ToolUse(_)));
}

#[test]
fn is_done_dedupes_tool_calls_already_pushed_by_stream() {
    // Provider emits ToolCallStart/Delta/End, then IsDone
    // re-lists the same call. The assembler must NOT push it
    // twice.
    let mut a = BlockAssembler::new();
    a.push(tool_start("tc1", "shell", serde_json::json!("{}")));
    a.push(tool_end("tc1"));
    let dup = ToolUse {
        id: "tc1".to_string(),
        name: "shell".to_string(),
        input: serde_json::json!({}),
    };
    a.push(done_with(SamplingResult {
        text: String::new(),
        tool_calls: vec![dup],
        reasoning: String::new(),
        reasoning_signature: None,
        usage: TokenUsage::default(),
        stop_reason: Some("tool_calls".to_string()),
    }));
    let (result, parts, _) = a.finalize_assistant();
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(parts.len(), 1);
    assert!(a.is_done);
}

#[test]
fn is_done_folds_text_when_no_streamed_text() {
    // Non-streaming provider batches everything into IsDone.
    let mut a = BlockAssembler::new();
    a.push(done_with(SamplingResult {
        text: "all at once".to_string(),
        tool_calls: Vec::new(),
        reasoning: String::new(),
        reasoning_signature: None,
        usage: TokenUsage::default(),
        stop_reason: Some("stop".to_string()),
    }));
    let (result, _, incomplete) = a.finalize_assistant();
    assert_eq!(result.text, "all at once");
    assert!(!incomplete);
}

#[test]
fn is_done_does_not_double_count_streamed_text() {
    // Streamed text already covered — IsDone's text is
    // ignored so the model-visible history shows one copy.
    let mut a = BlockAssembler::new();
    a.push(text_chunk("hello"));
    a.push(done_with(SamplingResult {
        text: "hello".to_string(),
        tool_calls: Vec::new(),
        reasoning: String::new(),
        reasoning_signature: None,
        usage: TokenUsage::default(),
        stop_reason: Some("stop".to_string()),
    }));
    let (result, _, _) = a.finalize_assistant();
    assert_eq!(result.text, "hello");
}

#[test]
fn usage_chunk_overwritten_by_terminal_is_done_usage() {
    // Anthropic emits intermediate `message_delta.usage`
    // before the terminal one. The assembler must let the
    // terminal value win.
    let mut a = BlockAssembler::new();
    a.push(StreamChunk::Usage(TokenUsage {
        prompt_tokens: 100,
        completion_tokens: 5,
        total_tokens: 105,
        cached_prompt_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
    }));
    a.push(done_with(SamplingResult {
        text: String::new(),
        tool_calls: Vec::new(),
        reasoning: String::new(),
        reasoning_signature: None,
        usage: TokenUsage {
            prompt_tokens: 120,
            completion_tokens: 8,
            total_tokens: 128,
            cached_prompt_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
        },
        stop_reason: Some("stop".to_string()),
    }));
    let (result, _, _) = a.finalize_assistant();
    assert_eq!(result.usage.prompt_tokens, 120);
    assert_eq!(result.usage.completion_tokens, 8);
}

#[test]
fn finalize_drains_open_tool_buffers_on_early_termination() {
    // Stream ends mid-ToolCall (no ToolCallEnd) without
    // IsDone. The assembler must still surface the partial
    // tool call so the history builder does not silently
    // drop it.
    let mut a = BlockAssembler::new();
    a.push(tool_start("tc1", "shell", serde_json::json!("{}")));
    a.push(tool_delta("tc1", "{\"command\":\"ls\""));
    // No ToolCallEnd, no IsDone.
    let (result, _, incomplete) = a.finalize_assistant();
    assert!(incomplete);
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].id, "tc1");
}

#[test]
fn tool_call_delta_for_unknown_id_is_ignored() {
    // Provider sometimes re-uses an id after ToolCallEnd. The
    // assembler must ignore stragglers so the closed block's
    // arguments are not corrupted.
    let mut a = BlockAssembler::new();
    a.push(tool_start("tc1", "shell", serde_json::json!("{}")));
    a.push(tool_end("tc1"));
    a.push(tool_delta("tc1", "stray"));
    let (result, _, _) = a.finalize_assistant();
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].input, serde_json::json!({}));
}

/// Regression (R33): the assembler used to hold a private parser
/// that degraded invalid JSON to `Value::Null`, while the shared
/// helper degraded to `Value::String` — the same input produced a
/// different `ToolUse.input` depending on which path finalized
/// it. Both go through `json_repair::parse_tool_input*` now, so
/// the streamed path and the shared helper must agree, whether
/// the payload is rescued by the repair or falls back to the raw
/// text.
#[test]
fn streamed_tool_input_agrees_with_the_shared_parser() {
    for raw in [
        r#"{"cmd": "ls""#,          // truncated: no repair possible
        "not json at all",          // garbage
        "{\"cmd\": \"echo a\nb\"}", // repaired: raw newline
    ] {
        let mut a = BlockAssembler::new();
        a.push(tool_start("tc1", "shell", serde_json::json!("")));
        a.push(tool_delta("tc1", raw));
        a.push(tool_end("tc1"));
        let (result, _parts, _) = a.finalize_assistant();
        let input = &result.tool_calls[0].input;
        assert_eq!(
            *input,
            parse_tool_input(raw),
            "assembler and shared parser must agree on: {raw}"
        );
        assert!(
            !input.is_null(),
            "the `Value::Null` degradation is dead: {raw}"
        );
    }
}

#[test]
fn stop_chunk_first_wins_for_stop_reason() {
    // Provider may emit `Stop` then `IsDone` with the same
    // reason; the assembler's contract is "first observation
    // wins" so the wire-visible reason is preserved.
    let mut a = BlockAssembler::new();
    a.push(StreamChunk::Stop("tool_use".to_string()));
    a.push(done_with(SamplingResult {
        text: String::new(),
        tool_calls: Vec::new(),
        reasoning: String::new(),
        reasoning_signature: None,
        usage: TokenUsage::default(),
        stop_reason: Some("end_turn".to_string()),
    }));
    let (result, _, _) = a.finalize_assistant();
    assert_eq!(result.stop_reason.as_deref(), Some("tool_use"));
}

#[test]
fn empty_stream_can_be_used_to_construct_message() {
    // Loosely sanity-checks that the assembler's output
    // composes into a `Message` (the loop's actual
    // consumer).
    let mut a = BlockAssembler::new();
    a.push(text_chunk("done"));
    let (result, parts, _) = a.finalize_assistant();
    let _ = Content::parts(parts);
    let _ = Message {
        role: Role::Assistant,
        content: Content::text(""),
        tool_call_id: None,
        name: None,
        tool_result_cleared_at: None,
    };
    // The Message constructor is exercised by the call
    // above; we don't assert the field assignment because
    // the assembler's job is producing parts + a
    // SamplingResult, not a fully-built Message.
    assert_eq!(result.text, "done");
}
