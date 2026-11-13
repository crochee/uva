use super::*;

// -- new() / default() -------------------------------------------

/// `OpenAIStreamProcessor::new()` MUST produce a processor
/// that ignores input until `process_line` is called.
#[test]
fn new_constructs_empty_processor() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line("");
    assert!(chunks.is_empty());
}

/// `OpenAIStreamProcessor::default()` MUST match `new()`.
#[test]
fn default_matches_new() {
    let mut p = OpenAIStreamProcessor::default();
    // Empty input → empty chunks.
    assert!(p.process_line("").is_empty());
}

// -- [DONE] sentinel ---------------------------------------------

/// `process_line("[DONE]")` MUST emit a single `IsDone`
/// chunk with no stop reason (no `finish_reason` was seen).
#[test]
fn done_sentinel_emits_is_done_with_no_stop_reason() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line("[DONE]");
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        StreamChunk::IsDone { result } => {
            assert_eq!(result.stop_reason, None);
        }
        _ => panic!("expected IsDone"),
    }
}

/// After `IsDone` is emitted, further `process_line` calls
/// MUST silently return empty (the `finished` flag).
#[test]
fn events_after_is_done_are_ignored() {
    let mut p = OpenAIStreamProcessor::new();
    p.process_line("[DONE]");
    // Any subsequent line MUST be a no-op.
    let chunks = p.process_line(r#"{"choices":[{"delta":{"content":"x"}}]}"#);
    assert!(chunks.is_empty());
}

// -- malformed JSON ----------------------------------------------

/// Malformed JSON MUST silently produce no chunks (no panic,
/// no error propagation).
#[test]
fn malformed_json_silently_ignored() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line("{bad json");
    assert!(chunks.is_empty());
}

/// Empty input MUST produce no chunks (not [DONE], not
/// an error).
#[test]
fn empty_input_produces_no_chunks() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line("");
    assert!(chunks.is_empty());
}

/// Whitespace-only input MUST be trimmed and treated as
/// empty (the `data.trim()` call).
#[test]
fn whitespace_only_input_produces_no_chunks() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line("   \n  \t ");
    assert!(chunks.is_empty());
}

// -- content delta ------------------------------------------------

/// A `delta.content` chunk MUST emit `Content(Text)` and
/// accumulate text.
#[test]
fn content_delta_emits_text_chunk() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(
        r#"{"choices":[{"delta":{"content":"hello"},"finish_reason":null}]}"#,
    );
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        StreamChunk::Content(ContentPart::Text(t)) => {
            assert_eq!(t.text, "hello");
        }
        other => panic!("expected Content(Text), got {other:?}"),
    }
}

/// Empty `delta.content` MUST NOT emit a chunk (the
/// `!content.is_empty()` guard).
#[test]
fn empty_content_delta_emits_no_chunk() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(
        r#"{"choices":[{"delta":{"content":""},"finish_reason":null}]}"#,
    );
    assert!(chunks.is_empty());
}

/// Multiple `delta.content` chunks MUST accumulate into the
/// final `SamplingResult.text`.
#[test]
fn content_accumulates_across_lines() {
    let mut p = OpenAIStreamProcessor::new();
    for chunk in ["hello", " ", "world"] {
        let json = format!(
            r#"{{"choices":[{{"delta":{{"content":"{chunk}"}},"finish_reason":null}}]}}"#,
        );
        p.process_line(&json);
    }
    let chunks = p.process_line("[DONE]");
    match &chunks[0] {
        StreamChunk::IsDone { result } => {
            assert_eq!(result.text, "hello world");
        }
        _ => panic!("expected IsDone"),
    }
}

// -- reasoning_content delta -------------------------------------

/// `delta.reasoning_content` MUST emit `Content(Reasoning)`
/// chunks.
#[test]
fn reasoning_content_delta_emits_reasoning_chunk() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(
        r#"{"choices":[{"delta":{"reasoning_content":"thinking..."},"finish_reason":null}]}"#,
    );
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        StreamChunk::Content(ContentPart::Reasoning(r)) => {
            assert_eq!(r.text, "thinking...");
            assert_eq!(r.signature, None);
        }
        other => panic!("expected Content(Reasoning), got {other:?}"),
    }
}

/// `reasoning_content` and `content` in the same chunk
/// MUST emit BOTH chunks (separately).
#[test]
fn reasoning_and_content_in_same_chunk() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(
        r#"{"choices":[{"delta":{"reasoning_content":"think","content":"answer"},"finish_reason":null}]}"#,
    );
    assert_eq!(chunks.len(), 2);
    let reasoning_chunk = chunks
        .iter()
        .find(|c| matches!(c, StreamChunk::Content(ContentPart::Reasoning(_))));
    let text_chunk = chunks
        .iter()
        .find(|c| matches!(c, StreamChunk::Content(ContentPart::Text(_))));
    assert!(reasoning_chunk.is_some(), "no reasoning chunk");
    assert!(text_chunk.is_some(), "no text chunk");
}

// -- tool_calls delta ---------------------------------------------

/// First `delta.tool_calls` MUST emit `ToolCallStart`.
#[test]
fn tool_call_first_delta_emits_tool_call_start() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(
        r#"{
            "choices":[{
                "delta":{"tool_calls":[{
                    "id":"call-1",
                    "function":{"name":"bash","arguments":"{\"cmd\":\"ls\"}"},
                    "index":0
                }]},
                "finish_reason":null
            }]
        }"#,
    );
    // 1 ToolCallStart, no other chunks.
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        StreamChunk::ToolCallStart {
            id,
            name,
            arguments,
        } => {
            assert_eq!(id, "call-1");
            assert_eq!(name, "bash");
            assert_eq!(
                *arguments,
                serde_json::json!("{\"cmd\":\"ls\"}".to_string())
            );
        }
        other => panic!("expected ToolCallStart, got {other:?}"),
    }
}

/// Subsequent `delta.tool_calls` deltas (with the same
/// index) MUST emit `ToolCallDelta`, NOT a second
/// `ToolCallStart`.
#[test]
fn tool_call_subsequent_delta_emits_tool_call_delta() {
    let mut p = OpenAIStreamProcessor::new();
    // First delta: start.
    p.process_line(
        r#"{
            "choices":[{
                "delta":{"tool_calls":[{
                    "id":"call-1",
                    "function":{"name":"bash","arguments":""},
                    "index":0
                }]},
                "finish_reason":null
            }]
        }"#,
    );
    // Second delta: continuation.
    let chunks = p.process_line(
        r#"{
            "choices":[{
                "delta":{"tool_calls":[{
                    "id":null,
                    "function":{"arguments":"{\"cmd\":\"l"},
                    "index":0
                }]},
                "finish_reason":null
            }]
        }"#,
    );
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        StreamChunk::ToolCallDelta {
            id,
            arguments_delta,
        } => {
            assert_eq!(id, "call-1");
            assert_eq!(arguments_delta, "{\"cmd\":\"l");
        }
        other => panic!("expected ToolCallDelta, got {other:?}"),
    }
}

// -- usage block --------------------------------------------------

/// The usage block MUST be read from the chunk's **top level**, where
/// every real provider puts it. This is the shape DeepSeek sends on
/// the `finish_reason` chunk (verified against the live endpoint) and
/// the shape OpenAI documents for `stream_options.include_usage`.
#[test]
fn top_level_usage_on_the_finish_chunk_emits_usage() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(
        r#"{
            "choices":[{"delta":{"content":""},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":6,"completion_tokens":1,"total_tokens":7}
        }"#,
    );
    // A Usage chunk plus the terminal IsDone.
    let usage = chunks
        .iter()
        .find_map(|c| match c {
            StreamChunk::Usage(u) => Some(u.clone()),
            _ => None,
        })
        .expect("top-level usage must produce a Usage chunk");
    assert_eq!(usage.prompt_tokens, 6);
    assert_eq!(usage.completion_tokens, 1);
    assert_eq!(usage.total_tokens, 7);
    let done = chunks
        .iter()
        .find_map(|c| match c {
            StreamChunk::IsDone { result } => Some((**result).clone()),
            _ => None,
        })
        .expect("IsDone must fire on finish_reason");
    // The terminal result must carry the same counts, or every
    // budget guard and the usage chip read zero.
    assert_eq!(done.usage.prompt_tokens, 6);
    assert_eq!(done.usage.completion_tokens, 1);
}

/// OpenAI's documented final frame for `include_usage` carries an
/// EMPTY `choices` array. The processor must still surface the usage
/// rather than returning early.
#[test]
fn usage_only_chunk_with_empty_choices_still_emits_usage() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(
        r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":2,"total_tokens":13}}"#,
    );
    let usage = chunks
        .iter()
        .find_map(|c| match c {
            StreamChunk::Usage(u) => Some(u.clone()),
            _ => None,
        })
        .expect("a usage-only terminator must not be dropped");
    assert_eq!(usage.prompt_tokens, 11);
    // And it must survive into the terminal result.
    let done = p.process_line("[DONE]");
    let result = done
        .iter()
        .find_map(|c| match c {
            StreamChunk::IsDone { result } => Some((**result).clone()),
            _ => None,
        })
        .expect("IsDone must fire on [DONE]");
    assert_eq!(result.usage.prompt_tokens, 11);
    assert_eq!(result.usage.completion_tokens, 2);
}

/// A gateway that nests `usage` inside the choice is still honoured,
/// so tolerating the non-standard shape costs nothing.
#[test]
fn choice_nested_usage_is_tolerated() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(
        r#"{"choices":[{"delta":{},"finish_reason":null,"usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}}]}"#,
    );
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        StreamChunk::Usage(u) => {
            assert_eq!(u.prompt_tokens, 100);
            assert_eq!(u.completion_tokens, 50);
            assert_eq!(u.total_tokens, 150);
        }
        other => panic!("expected Usage, got {other:?}"),
    }
}

// -- finish_reason ------------------------------------------------

/// `finish_reason` MUST emit `ToolCallEnd` chunks for each
/// buffered tool call, then `IsDone` with the reason.
#[test]
fn finish_reason_drains_tool_calls_and_emits_is_done() {
    let mut p = OpenAIStreamProcessor::new();
    // First start a tool call (no finish_reason yet).
    p.process_line(
        r#"{
            "choices":[{
                "delta":{"tool_calls":[{
                    "id":"call-1",
                    "function":{"name":"bash","arguments":"{\"cmd\":\"ls\"}"},
                    "index":0
                }]},
                "finish_reason":null
            }]
        }"#,
    );
    // Now send finish_reason.
    let chunks = p.process_line(
        r#"{
            "choices":[{"delta":{},"finish_reason":"tool_calls"}]
        }"#,
    );
    // ToolCallEnd + IsDone.
    assert_eq!(chunks.len(), 2);
    assert!(
        matches!(&chunks[0], StreamChunk::ToolCallEnd { id } if id == "call-1")
    );
    match &chunks[1] {
        StreamChunk::IsDone { result } => {
            assert_eq!(result.stop_reason, Some("tool_calls".to_string()));
            assert_eq!(result.tool_calls.len(), 1);
            assert_eq!(result.tool_calls[0].name, "bash");
        }
        _ => panic!("expected IsDone as second chunk"),
    }
}

// -- multi-choice handling ----------------------------------------

/// Multi-choice response MUST process only the FIRST
/// choice and silently drop the rest.
#[test]
fn multi_choice_processes_only_first() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(
        r#"{
            "choices":[
                {"delta":{"content":"first"},"finish_reason":null},
                {"delta":{"content":"second"},"finish_reason":null}
            ]
        }"#,
    );
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        StreamChunk::Content(ContentPart::Text(t)) => {
            assert_eq!(t.text, "first");
        }
        _ => panic!("expected Content(Text)"),
    }
}

/// Empty `choices` array MUST produce no chunks (no panic).
#[test]
fn empty_choices_array_produces_no_chunks() {
    let mut p = OpenAIStreamProcessor::new();
    let chunks = p.process_line(r#"{"choices":[]}"#);
    assert!(chunks.is_empty());
}

// -- orphan tool buffers on [DONE] -------------------------------

/// When `[DONE]` arrives with live `tool_buffers`
/// (truncated stream), the `IsDone.tool_calls` MUST still
/// include them (silent drain, no `ToolCallEnd` chunks).
#[test]
fn done_sentinel_drains_orphan_tool_buffers() {
    let mut p = OpenAIStreamProcessor::new();
    // Start a tool call but never close it.
    p.process_line(
        r#"{
            "choices":[{
                "delta":{"tool_calls":[{
                    "id":"call-orphan",
                    "function":{"name":"bash","arguments":"{\"cmd\":\"l"},
                    "index":0
                }]},
                "finish_reason":null
            }]
        }"#,
    );
    // Stream ends via [DONE] (no finish_reason).
    let chunks = p.process_line("[DONE]");
    match &chunks[0] {
        StreamChunk::IsDone { result } => {
            // The orphan was drained into tool_calls even
            // though no ToolCallEnd was emitted.
            assert_eq!(result.tool_calls.len(), 1);
            assert_eq!(result.tool_calls[0].id, "call-orphan");
        }
        _ => panic!("expected IsDone"),
    }
}
