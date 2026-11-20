//! OpenAI-compatible stream processor.
//!
//! The processor decodes wire lines into chunks and keeps nothing
//! about the turn; `crate::assembler::BlockAssembler` folds the chunks
//! into the finished turn.
//! - `Content(Text)` for `delta.content`
//! - `Content(Reasoning)` for `delta.reasoning_content` (no sniffing)
//! - `ToolCallStart { id, name, arguments }` on the first delta for a tool call
//! - `ToolCallDelta { id, arguments_delta }` on subsequent argument deltas
//! - `ToolCallName { id, name }` when a later fragment completes the
//!   name the start delta did not carry
//! - `ToolCallEnd { id }` when the finish marker closes the open calls
//! - `Usage(TokenUsage)` for a `usage` block, including one that
//!   arrives after the finish marker (some providers seal the choice
//!   first and report usage later)
//! - `Stop(reason)` once per turn, on the delta carrying
//!   `finish_reason`. The decoder keeps consuming to `[DONE]` or EOF
//!   after the seal so a late usage frame is not lost.

mod processor;
mod types;

pub use processor::OpenAIStreamProcessor;
