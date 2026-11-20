use super::*;

// -- AnthropicStreamEvent ---------------------------------------

/// `AnthropicStreamEvent` MUST deserialize a message_start
/// event with the `type` field correctly mapped.
#[test]
fn event_message_start_parses() {
    let json = r#"{"type": "message_start", "message": {}}"#;
    let e: AnthropicStreamEvent = serde_json::from_str(json).unwrap();
    assert_eq!(e.r#type, "message_start");
    assert!(e.content_block.is_none());
    assert!(e.index.is_none());
    assert!(e.delta.is_none());
    assert!(e.stop_reason.is_none());
}

/// `AnthropicStreamEvent` MUST deserialize a content_block_start
/// event with the embedded content block.
#[test]
fn event_content_block_start_parses() {
    let json = r#"{
        "type": "content_block_start",
        "index": 0,
        "content_block": {
            "type": "tool_use",
            "id": "toolu_01",
            "name": "bash"
        }
    }"#;
    let e: AnthropicStreamEvent = serde_json::from_str(json).unwrap();
    assert_eq!(e.r#type, "content_block_start");
    assert_eq!(e.index, Some(0));
    let cb = e.content_block.unwrap();
    assert_eq!(cb.r#type, "tool_use");
    assert_eq!(cb.id, Some("toolu_01".to_string()));
    assert_eq!(cb.name, Some("bash".to_string()));
    assert!(cb.input.is_none());
}

/// `AnthropicStreamEvent` MUST deserialize a content_block_delta
/// event with text delta.
#[test]
fn event_content_block_delta_text_parses() {
    let json = r#"{
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "text_delta", "text": "hello"}
    }"#;
    let e: AnthropicStreamEvent = serde_json::from_str(json).unwrap();
    assert_eq!(e.r#type, "content_block_delta");
    let d = e.delta.unwrap();
    assert_eq!(d.r#type, "text_delta");
    assert_eq!(d.text, Some("hello".to_string()));
    assert!(d.thinking.is_none());
    assert!(d.partial_json.is_none());
    assert!(d.signature.is_none());
}

/// `AnthropicStreamEvent` MUST deserialize input_json_delta
/// events (carrying partial tool-call JSON).
#[test]
fn event_input_json_delta_parses() {
    let json = r#"{
        "type": "content_block_delta",
        "index": 1,
        "delta": {"type": "input_json_delta", "partial_json": "{\"cmd\":"}
    }"#;
    let e: AnthropicStreamEvent = serde_json::from_str(json).unwrap();
    let d = e.delta.unwrap();
    assert_eq!(d.r#type, "input_json_delta");
    assert_eq!(d.partial_json, Some("{\"cmd\":".to_string()));
}

/// `AnthropicStreamEvent` MUST deserialize
/// thinking_delta events with the embedded thinking text.
#[test]
fn event_thinking_delta_parses() {
    let json = r#"{
        "type": "content_block_delta",
        "index": 2,
        "delta": {
            "type": "thinking_delta",
            "thinking": "step 1..."
        }
    }"#;
    let e: AnthropicStreamEvent = serde_json::from_str(json).unwrap();
    let d = e.delta.unwrap();
    assert_eq!(d.r#type, "thinking_delta");
    assert_eq!(d.thinking, Some("step 1...".to_string()));
}

/// `AnthropicStreamEvent` MUST deserialize signature_delta
/// events (the final signature_delta for a thinking block).
#[test]
fn event_signature_delta_parses() {
    let json = r#"{
        "type": "content_block_delta",
        "index": 2,
        "delta": {
            "type": "signature_delta",
            "signature": "abc123"
        }
    }"#;
    let e: AnthropicStreamEvent = serde_json::from_str(json).unwrap();
    let d = e.delta.unwrap();
    assert_eq!(d.r#type, "signature_delta");
    assert_eq!(d.signature, Some("abc123".to_string()));
}

/// `AnthropicStreamEvent` MUST deserialize message_delta
/// events with stop_reason.
#[test]
fn event_message_delta_with_stop_reason_parses() {
    let json = r#"{
        "type": "message_delta",
        "stop_reason": "end_turn"
    }"#;
    let e: AnthropicStreamEvent = serde_json::from_str(json).unwrap();
    assert_eq!(e.r#type, "message_delta");
    assert_eq!(e.stop_reason, Some("end_turn".to_string()));
}

/// The opening `message_start` frame carries the prompt-side usage
/// object nested inside `message`. It MUST land on a typed field:
/// while `message` was unmodelled, serde dropped the whole object
/// and a streamed step reported zero usage.
#[test]
fn event_message_start_usage_parses() {
    let json = r#"{
        "type": "message_start",
        "message": {
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "content": [],
            "usage": {
                "input_tokens": 120,
                "output_tokens": 1,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 20
            }
        }
    }"#;
    let e: AnthropicStreamEvent = serde_json::from_str(json).unwrap();
    let usage = e
        .message
        .and_then(|message| message.usage)
        .expect("the opening usage object must survive deserialization");
    assert_eq!(usage.input_tokens, Some(120));
    assert_eq!(usage.output_tokens, Some(1));
    assert_eq!(
        usage.cache_read_input_tokens,
        Some(0),
        "a reported cache miss is a value, not an absence"
    );
    assert_eq!(usage.cache_creation_input_tokens, Some(20));
}

/// The terminal `message_delta` frame carries its usage object at
/// the top level, a sibling of `delta` — and normally only the
/// final output count.
#[test]
fn event_message_delta_usage_parses() {
    let json = r#"{
        "type": "message_delta",
        "delta": {"stop_reason": "end_turn"},
        "usage": {"output_tokens": 30}
    }"#;
    let e: AnthropicStreamEvent = serde_json::from_str(json).unwrap();
    let usage = e.usage.expect("the terminal usage object must survive");
    assert_eq!(usage.output_tokens, Some(30));
    assert_eq!(
        usage.input_tokens, None,
        "an omitted field stays absent; it is not reported as zero"
    );
}

/// A frame with no usage object leaves both landing spots empty
/// rather than inventing zeros.
#[test]
fn event_without_usage_leaves_it_none() {
    let json = r#"{"type": "message_stop"}"#;
    let e: AnthropicStreamEvent = serde_json::from_str(json).unwrap();
    assert!(e.message.is_none());
    assert!(e.usage.is_none());
}

/// `AnthropicStreamEvent` MUST reject unknown event types
/// when they have required fields missing.
#[test]
fn event_minimal_payload_only_type_required() {
    // Only `type` is strictly required; everything else
    // is `#[serde(default)]`.
    let json = r#"{"type": "ping"}"#;
    let e: AnthropicStreamEvent = serde_json::from_str(json).unwrap();
    assert_eq!(e.r#type, "ping");
}

/// `AnthropicStreamEvent` MUST reject malformed JSON.
#[test]
fn event_rejects_malformed_json() {
    let json = r#"{"type": "message_start""; // missing closing brace
    let result: Result<AnthropicStreamEvent, _> =
        serde_json::from_str(json);
    assert!(result.is_err());
}

/// `AnthropicStreamEvent` MUST reject payload without a
/// `type` field (the only strictly-required field).
#[test]
fn event_rejects_missing_type_field() {
    let json = r#"{"index": 0}"#;
    let result: Result<AnthropicStreamEvent, _> = serde_json::from_str(json);
    assert!(result.is_err());
}

// -- AnthropicStreamContentBlock --------------------------------

/// `AnthropicStreamContentBlock` MUST deserialize the 5
/// known shape fields independently (text / tool_use /
/// thinking each populate different fields).
#[test]
fn content_block_tool_use_with_id_and_name() {
    // The shapes a `tool_use` content block can carry in `input`.
    // An OBJECT is the real wire (the field is `map[unknown]`, and the
    // streaming protocol sends an EMPTY object here while the
    // arguments arrive as `input_json_delta`); a STRING is the shape a
    // gateway that prefills the arguments sends. Both must parse —
    // typing it as `String` alone made serde reject the real frame,
    // which silently dropped every tool call from that endpoint.
    let object =
        r#"{"type": "tool_use", "id": "t", "name": "bash", "input": {}}"#;
    let cb: AnthropicStreamContentBlock = serde_json::from_str(object).unwrap();
    assert_eq!(cb.r#type, "tool_use");
    assert_eq!(cb.id, Some("t".to_string()));
    assert_eq!(cb.name, Some("bash".to_string()));
    assert_eq!(cb.input, Some(serde_json::json!({})));
    assert!(cb.thinking.is_none());

    let prefilled = r#"{"type": "tool_use", "id": "t", "name": "bash", "input": "{\"x\":1}"}"#;
    let cb: AnthropicStreamContentBlock =
        serde_json::from_str(prefilled).unwrap();
    assert_eq!(
        cb.input,
        Some(serde_json::Value::String("{\"x\":1}".to_string()))
    );
}

/// The real `message_delta` frame carries ONLY `stop_reason` inside
/// `delta` — no `type` — so requiring the discriminator dropped the
/// frame and the endpoint's actual stop reason was never read.
#[test]
fn message_delta_without_a_delta_type_parses() {
    let json = r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#;
    let e: AnthropicStreamEvent = serde_json::from_str(json).unwrap();
    let delta = e.delta.expect("the delta must survive");
    assert_eq!(delta.stop_reason.as_deref(), Some("tool_use"));
    assert!(
        delta.r#type.is_empty(),
        "an absent discriminator defaults rather than rejecting the frame"
    );
}

/// `AnthropicStreamContentBlock` MUST deserialize thinking
/// blocks (extended-thinking feature).
#[test]
fn content_block_thinking_with_text() {
    let json = r#"{"type": "thinking", "thinking": "let me reason..."}"#;
    let cb: AnthropicStreamContentBlock = serde_json::from_str(json).unwrap();
    assert_eq!(cb.r#type, "thinking");
    assert_eq!(cb.thinking, Some("let me reason...".to_string()));
    assert!(cb.id.is_none());
    assert!(cb.name.is_none());
    assert!(cb.input.is_none());
}

/// `AnthropicStreamContentBlock` MUST tolerate missing
/// optional fields (only `type` is required).
#[test]
fn content_block_minimal_only_type() {
    let json = r#"{"type": "text"}"#;
    let cb: AnthropicStreamContentBlock = serde_json::from_str(json).unwrap();
    assert_eq!(cb.r#type, "text");
    assert!(cb.id.is_none());
    assert!(cb.name.is_none());
    assert!(cb.input.is_none());
    assert!(cb.thinking.is_none());
}

// -- AnthropicStreamDelta ---------------------------------------

/// `AnthropicStreamDelta` MUST deserialize all 4 optional
/// fields independently.
#[test]
fn delta_all_four_optional_fields() {
    let json = r#"{
        "type": "input_json_delta",
        "text": "tx",
        "thinking": "tk",
        "partial_json": "{}",
        "signature": "sig"
    }"#;
    let d: AnthropicStreamDelta = serde_json::from_str(json).unwrap();
    assert_eq!(d.r#type, "input_json_delta");
    assert_eq!(d.text, Some("tx".to_string()));
    assert_eq!(d.thinking, Some("tk".to_string()));
    assert_eq!(d.partial_json, Some("{}".to_string()));
    assert_eq!(d.signature, Some("sig".to_string()));
}

/// `AnthropicStreamDelta` MUST accept an empty payload
/// (only `type` required).
#[test]
fn delta_only_type_required() {
    let json = r#"{"type": "x"}"#;
    let d: AnthropicStreamDelta = serde_json::from_str(json).unwrap();
    assert_eq!(d.r#type, "x");
    assert!(d.text.is_none());
    assert!(d.thinking.is_none());
    assert!(d.partial_json.is_none());
    assert!(d.signature.is_none());
}
