//! Anthropic SSE stream processor.
//!
//! # Module Layout
//!
//! - [`events`]: The 3 raw SSE event structs
//!   ([`events::AnthropicStreamEvent`],
//!   [`events::AnthropicStreamContentBlock`],
//!   [`events::AnthropicStreamDelta`]).
//! - [`processor`]: [`processor::StreamProcessor`] + its private
//!   [`processor::ToolUseBuffer`] + the free helper
//!   [`processor::delta_usage`]. Tool-call arguments are finalized
//!   through [`crate::json_repair::parse_tool_input_logged`].
//! - [`tests`]: Unit tests covering
//!   `text_delta` → `Content(Text)`, `thinking_delta` →
//!   `Content(Reasoning)`, the full `ToolUse` start/delta/end +
//!   `IsDone` round-trip with parsed JSON, `IsDone` carries
//!   accumulated text, and `parse_tool_input` edge cases
//!   (empty string → empty object, invalid JSON → raw string).

mod events;
mod processor;

pub use events::{
    AnthropicStreamContentBlock,
    AnthropicStreamDelta,
    AnthropicStreamEvent,
};
pub use processor::StreamProcessor;

#[cfg(test)]
mod tests;
