//! Anthropic Messages API provider implementation.
//!
//! Submodule layout:
//!
//! - `types`: the wire types sent to / received from
//!   `POST /v1/messages` (`AnthropicRequest`, `AnthropicMessage`,
//!   `AnthropicContentBlock`, `AnthropicResponse`, etc.) plus the
//!   `deserialize_tool_result_content` adapter.
//! - `provider`: the `AnthropicProvider` struct itself, its
//!   constructors, and the request/response transformation helpers
//!   (`transform_request`, `transform_message`, `transform_part`,
//!   `reorder_anthropic_messages`, `sanitize_tool_id`,
//!   `parse_response`, `make_request`).
//! - `traits_impl`: `impl ModelProvider for AnthropicProvider`
//!   (`initialize`, `complete`, `complete_with_stream`, `embed`).
//!   `complete` runs parse + the empty-terminal guard inside the
//!   retry loop; `complete_with_stream` runs on the crate's shared
//!   SSE pump (cancellation + idle watchdog), folds the emitted chunks
//!   and returns that turn (rejecting one with no durable content).
//! - `token`: `estimate_tokens` and `impl TokenCounter for
//!   `AnthropicProvider`.
//! - `usage`: the one mapping from an Anthropic usage report onto the
//!   canonical `TokenUsage` buckets. The non-streaming parse and the
//!   streaming decoder both call it, so a conversation reports the same
//!   totals in both modes.

mod provider;
mod token;
mod traits_impl;
mod types;
pub(crate) mod usage;

pub use provider::AnthropicProvider;
pub use token::estimate_tokens;
