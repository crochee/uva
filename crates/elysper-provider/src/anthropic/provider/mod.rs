//! `AnthropicProvider` — the stateful struct + its
//! request/response transformation helpers. The
//! `ModelProvider` trait impl lives in `traits_impl`.
//!
//! # Module Layout
//!
//! - `core`: [`core::AnthropicProvider`] struct +
//!   `new` / `with_api_key` / `with_base_url` constructors.
//! - `transform`: 5 transform methods
//!   ([`transform::AnthropicProvider::transform_request`],
//!   `transform_message`, `transform_part`,
//!   `reorder_anthropic_messages`, `sanitize_tool_id`).
//! - `parse`:
//!   [`parse::AnthropicProvider::parse_response`].
//! - `request`:
//!   [`request::AnthropicProvider::make_request`].

mod core;
mod parse;
mod request;
mod transform;

pub use core::AnthropicProvider;

#[cfg(test)]
mod tests;
