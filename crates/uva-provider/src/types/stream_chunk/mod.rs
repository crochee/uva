//! The streaming types: [`StreamChunk`] (one event on the SSE
//! stream) and [`SamplingResult`] (the final aggregated response).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{content::ContentPart, models::TokenUsage, tool::ToolUse};

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

/// One event on a provider's stream, as the fold
/// (`uva_provider::assembler::BlockAssembler`) consumes it.
///
/// Decoders translate wire events into these chunks; the fold is the
/// single owner of the turn they add up to.
#[derive(Clone, Debug)]
pub enum StreamChunk {
    /// One delta of the assistant message: text, reasoning, or a
    /// complete non-text part.
    Content(ContentPart),
    /// A usage report. Several may arrive in one turn; the fold keeps
    /// the last one (Anthropic emits progressively authoritative
    /// counts).
    Usage(TokenUsage),
    /// The provider's stop reason (Anthropic `end_turn` / `tool_use` /
    /// `max_tokens`, OpenAI `stop` / `tool_calls` / `length`).
    ///
    /// This chunk is also the **terminal fact** of the stream: the fold
    /// treats its arrival as the end of the step, so a stream that ends
    /// with a stop chunk finalizes as a complete turn.
    Stop(String),
    /// The Anthropic `signature_delta` value for the reasoning block
    /// that just ended.
    ///
    /// It arrives *after* the reasoning text it signs, so the streamed
    /// [`ContentPart::Reasoning`] deltas cannot carry it themselves. The
    /// fold stamps it onto the trailing contiguous reasoning run when it
    /// finalizes, and it is retained even when no reasoning text was
    /// streamed at all (`SamplingResult::reasoning_signature`).
    ///
    /// Dropping it silently breaks interleaved extended thinking:
    /// Anthropic rejects a re-sent thinking block without its signature,
    /// so the turn cannot be continued.
    ReasoningSignature(String),
    ToolCallStart {
        id: String,
        name: String,
        arguments: Value,
    },
    ToolCallDelta {
        id: String,
        arguments_delta: String,
    },
    /// An updated name for a tool call the stream already started.
    ///
    /// OpenAI's wire can send a call's `id` before its
    /// `function.name`, so the [`StreamChunk::ToolCallStart`] names the
    /// call only as far as the first fragment knew. The fold patches
    /// that call's open block, and the finished turn carries the later
    /// name.
    ToolCallName {
        id: String,
        name: String,
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

#[cfg(test)]
mod tests;
