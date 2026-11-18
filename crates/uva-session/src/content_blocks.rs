//! Provider content blocks, read off a raw payload.
//!
//! The provider owns the wire shape of a content block — a `tool_use`
//! block with its `id`, a `tool_result` block with the `tool_use_id`
//! it answers. This module is where the session reads that shape:
//! one reader, so the surface fold and the tool pairing cannot drift
//! on what a block is.
//!
//! The read stays **raw** on purpose: a payload written in an older
//! dialect does not decode into the provider's own types, and the
//! fold must still find the calls inside it. The provider's typed
//! accessors ([`crate::tool_pairing::first_tool_use_id`]) read the
//! same blocks from a decoded message — the two agree because both
//! read the same shape, from one place.

use serde_json::Value;

/// Collect every tool-use block's call id, in the order the blocks
/// appear.
///
/// The blocks are read straight out of the JSON a log row carries:
/// `{"type": "tool_use", "id": "…"}`. Nested arrays and objects are
/// walked too, so a call is found wherever it sits in the payload.
pub(crate) fn collect_tool_use_ids(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if matches!(
                map.get("type").and_then(Value::as_str),
                Some("tool_use")
            ) && let Some(id) = map.get("id").and_then(Value::as_str)
            {
                out.push(id.to_string());
            }
            for nested in map.values() {
                collect_tool_use_ids(nested, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_tool_use_ids(item, out);
            }
        }
        _ => {}
    }
}

/// Collect every tool-result block's call id, in the order the blocks
/// appear.
///
/// A tool-result block is `{"type": "tool_result", "tool_use_id":
/// "…"}` — it names the call it answers.
pub(crate) fn collect_tool_result_ids(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if matches!(
                map.get("type").and_then(Value::as_str),
                Some("tool_result")
            ) && let Some(id) =
                map.get("tool_use_id").and_then(Value::as_str)
            {
                out.push(id.to_string());
            }
            for nested in map.values() {
                collect_tool_result_ids(nested, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_tool_result_ids(item, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_use_ids_are_read_in_block_order() {
        let payload = json!({
            "role": "assistant",
            "content": [
                { "type": "text", "text": "calling" },
                { "type": "tool_use", "id": "call-1", "name": "shell", "input": {} },
                { "type": "tool_use", "id": "call-2", "name": "read", "input": {} },
            ],
        });
        let mut ids = Vec::new();
        collect_tool_use_ids(&payload, &mut ids);
        assert_eq!(ids, vec!["call-1".to_string(), "call-2".to_string()]);
    }

    #[test]
    fn tool_result_ids_are_read_from_the_block_they_answer() {
        let payload = json!({
            "role": "tool",
            "content": [{ "type": "tool_result", "tool_use_id": "call-1", "content": [] }],
        });
        let mut ids = Vec::new();
        collect_tool_result_ids(&payload, &mut ids);
        assert_eq!(ids, vec!["call-1".to_string()]);
    }

    #[test]
    fn a_block_without_its_key_is_not_a_call() {
        let payload = json!({ "type": "tool_use", "name": "shell" });
        let mut ids = Vec::new();
        collect_tool_use_ids(&payload, &mut ids);
        assert!(ids.is_empty());
    }
}
