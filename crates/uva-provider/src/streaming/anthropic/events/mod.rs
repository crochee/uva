//! The 3 raw Anthropic SSE event structs. Used by
//! [`super::processor::StreamProcessor`].

use serde::Deserialize;

use crate::types::TokenUsage;

/// Top-level Anthropic SSE event (1 per SSE message). Only
/// the fields needed by either stream processor are modeled.
#[derive(Debug, Deserialize)]
pub struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(default)]
    pub content_block: Option<AnthropicStreamContentBlock>,
    #[serde(default)]
    pub index: Option<usize>,
    #[serde(default)]
    pub delta: Option<AnthropicStreamDelta>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// The opening frame (`message_start`) carries its usage object
    /// here. Only the usage object is modelled; the rest of the frame
    /// (id, model, role, stop_sequence, content) has no reader.
    #[serde(default)]
    pub(crate) message: Option<AnthropicStreamMessage>,
    /// The terminal frame (`message_delta`) carries its usage object
    /// at the top level, a sibling of `delta`.
    #[serde(default)]
    pub(crate) usage: Option<AnthropicStreamUsage>,
}

/// The `message_start.message` frame, reduced to the one field the
/// stream decodes.
#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicStreamMessage {
    #[serde(default)]
    pub usage: Option<AnthropicStreamUsage>,
}

/// One streaming usage report.
///
/// Every field is optional because the two frames that carry it report
/// different halves of the turn: `message_start` has the prompt-side
/// counts (and the output count so far), while the terminal
/// `message_delta` has the final output count and may omit the
/// prompt-side fields entirely. An absent field means "this frame did
/// not report it"; a reported zero is the value zero.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct AnthropicStreamUsage {
    #[serde(default)]
    pub input_tokens: Option<usize>,
    #[serde(default)]
    pub output_tokens: Option<usize>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<usize>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<usize>,
}

impl AnthropicStreamUsage {
    /// Overlay every field this frame actually reported.
    ///
    /// Absent fields keep the earlier frame's value. A last-writer-wins
    /// replacement would drop the prompt count the opening frame
    /// carried, because the terminal delta normally reports only
    /// `output_tokens`.
    pub(super) fn merge(&mut self, next: &Self) {
        if next.input_tokens.is_some() {
            self.input_tokens = next.input_tokens;
        }
        if next.output_tokens.is_some() {
            self.output_tokens = next.output_tokens;
        }
        if next.cache_read_input_tokens.is_some() {
            self.cache_read_input_tokens = next.cache_read_input_tokens;
        }
        if next.cache_creation_input_tokens.is_some() {
            self.cache_creation_input_tokens = next.cache_creation_input_tokens;
        }
    }

    /// The accumulated report in the canonical buckets.
    ///
    /// Missing prompt/output counts become zero (they are the required
    /// buckets of [`TokenUsage`]); the shared mapping keeps the
    /// optional cache buckets exactly as the provider reported them.
    #[must_use]
    pub(super) fn to_token_usage(&self) -> TokenUsage {
        crate::anthropic::usage::token_usage(
            self.input_tokens.unwrap_or(0),
            self.output_tokens.unwrap_or(0),
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens,
        )
    }
}

/// Content-block descriptor carried by `content_block_start` events.
#[derive(Debug, Deserialize)]
pub struct AnthropicStreamContentBlock {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// A `tool_use` block's starting input.
    ///
    /// Typed as a JSON VALUE, not a string: the real wire sends an
    /// **object** (`"input": {}`) — and declares the field `map[unknown]`
    /// — so a `String` here made serde reject the entire
    /// `content_block_start` event, and the caller's
    /// `if let Ok(event)` silently dropped it. That is how every tool call
    /// from a real Anthropic endpoint went missing. The streaming protocol
    /// then sends the arguments separately as `input_json_delta`
    /// fragments, so this value is normally an empty object.
    ///
    /// A `String` is still accepted because some gateways prefill the
    /// arguments as text.
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    #[serde(default)]
    pub thinking: Option<String>,
}

/// Delta descriptor carried by `content_block_delta` events.
#[derive(Debug, Deserialize)]
pub struct AnthropicStreamDelta {
    /// The delta's discriminator. It is NOT always present: the real
    /// `message_delta` frame carries only `stop_reason`
    /// (`"delta":{"stop_reason":"tool_use"}`), so requiring this field
    /// made serde reject that event and the caller's `if let Ok(event)`
    /// dropped it — which is why every real turn reported the `end_turn`
    /// default instead of the endpoint's actual stop reason.
    #[serde(rename = "type", default)]
    pub r#type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub partial_json: Option<String>,
    /// Anthropic `signature_delta` value emitted alongside the final
    /// `thinking_delta` for the same content block. Used to preserve
    /// reasoning continuity across turns.
    #[serde(default)]
    pub signature: Option<String>,
    /// The turn's stop reason, which the real wire nests INSIDE
    /// `message_delta.delta` — `{"type":"message_delta","delta":
    /// {"stop_reason":"tool_use"}}` — not at the event's top level.
    ///
    /// Modelling it only at the top level meant the value was never read
    /// from a real endpoint: every turn reported the `end_turn` default,
    /// so a `tool_use` stop was indistinguishable from a plain reply.
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[cfg(test)]
mod tests;
