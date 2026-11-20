//! Anthropic SSE stream processor.
//!
//! # Module Layout
//!
//! - [`events`]: The raw SSE event structs
//!   ([`events::AnthropicStreamEvent`],
//!   [`events::AnthropicStreamContentBlock`],
//!   [`events::AnthropicStreamDelta`], and the usage objects
//!   [`events::AnthropicStreamMessage`] /
//!   [`events::AnthropicStreamUsage`]).
//! - [`processor`]: [`processor::StreamProcessor`], the decode-only
//!   processor that turns those events into chunks. It keeps only
//!   decode scratch — the wire-index → tool-buffer map, the think
//!   extractor, and the usage frames merged so far — and never builds
//!   a finished turn.
//! - [`tests`]: Unit tests covering the chunk sequence:
//!   `text_delta` → `Content(Text)`, `thinking_delta` →
//!   `Content(Reasoning)`, the tool-use start/delta/end sequence,
//!   usage chunks, the signature chunk, and `parse_tool_input` edge
//!   cases (empty string → empty object, invalid JSON → raw string).

mod events;
mod processor;

pub use events::{
    AnthropicStreamContentBlock,
    AnthropicStreamDelta,
    AnthropicStreamEvent,
};
pub use processor::StreamProcessor;
