//! The streaming types: [`StreamChunk`] (one event on the SSE
//! stream) and [`SamplingResult`] (the final aggregated response).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use elysper_core::Error;

use super::{
    completion::CompletionResponse,
    content::{Content, ContentPart, TextContent},
    models::TokenUsage,
    tool::ToolUse,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SamplingResult {
    pub text: String,
    pub tool_calls: Vec<ToolUse>,
    pub reasoning: String,
    /// Anthropic `signature_delta` value attached to the most recent
    /// reasoning block, propagated so the agent can preserve
    /// cross-turn reasoning continuity.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reasoning_signature: Option<String>,
    pub usage: TokenUsage,
    /// Provider's stop reason (e.g. Anthropic `end_turn`,
    /// `tool_use`, `max_tokens`, OpenAI `stop`,
    /// `tool_calls`, `length`). Optional because not all
    /// providers surface it (and not all responses have
    /// one, e.g. mid-stream usage-only updates).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stop_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub enum StreamChunk {
    Content(ContentPart),
    Usage(TokenUsage),
    Stop(String),
    ToolCallStart {
        id: String,
        name: String,
        arguments: Value,
    },
    ToolCallDelta {
        id: String,
        arguments_delta: String,
    },
    ToolCallEnd {
        id: String,
    },
    IsDone {
        result: Box<SamplingResult>,
    },
}

impl From<ContentPart> for StreamChunk {
    fn from(part: ContentPart) -> Self {
        StreamChunk::Content(part)
    }
}

impl From<String> for StreamChunk {
    fn from(stop: String) -> Self {
        StreamChunk::Stop(stop)
    }
}

impl SamplingResult {
    /// Assemble the provider-level [`CompletionResponse`] view of this
    /// sampling — the finalize step both adapters' streaming paths
    /// share.
    ///
    /// A terminal stop that produced no text, no reasoning, and no
    /// tool calls is rejected with the typed empty-completion error
    /// (dsh `EMPTY_RESPONSE` parity): the attempt produced nothing
    /// durable and MUST NOT read as a successful empty turn.
    pub fn into_completion_response(
        self,
        model: String,
    ) -> Result<CompletionResponse, Error> {
        let SamplingResult {
            text,
            tool_calls,
            reasoning: _,
            reasoning_signature: _,
            usage,
            stop_reason,
        } = self;

        let content = if tool_calls.is_empty() {
            Content::Single(ContentPart::Text(TextContent {
                text,
                cache_control: None,
            }))
        } else {
            Content::Multi(
                tool_calls
                    .iter()
                    .cloned()
                    .map(ContentPart::ToolUse)
                    .collect(),
            )
        };

        CompletionResponse {
            id: ulid::Ulid::generate().to_string(),
            model,
            content,
            usage,
            cached: false,
            stop_reason,
            replay_state: None,
        }
        .ensure_non_empty()
    }
}

#[cfg(test)]
mod tests;
