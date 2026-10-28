//! The response-parsing method on
//! [`super::core::AnthropicProvider`]:
//!
//! - [`AnthropicProvider::parse_response`] — takes an
//!   [`super::super::types::AnthropicResponse`] and produces
//!   a [`crate::types::CompletionResponse`]. When
//!   `stop_reason` is `"tool_use"`, tool-use blocks are
//!   extracted; otherwise the text content is joined.

use super::{super::types::AnthropicContentBlock, core::AnthropicProvider};
use crate::types::{
    CompletionResponse,
    Content,
    ContentPart,
    TextContent,
    TokenUsage,
    ToolUse,
};

impl AnthropicProvider {
    pub(in crate::anthropic) fn parse_response(
        &self,
        resp: &super::super::types::AnthropicResponse,
        model: &str,
    ) -> CompletionResponse {
        let content = if resp.stop_reason.as_deref() == Some("tool_use") {
            let mut tool_uses: Vec<_> = resp
                .content
                .iter()
                .filter_map(|c| match c {
                    AnthropicContentBlock::ToolUse {
                        id, name, input, ..
                    } => Some(ContentPart::ToolUse(ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    })),
                    _ => None,
                })
                .collect();
            // Upstream inconsistency guard: when
            // `stop_reason == "tool_use"` but the
            // content has no `tool_use` blocks
            // (only text — e.g. a model that
            // mentioned a tool but did not emit it),
            // fall back to joining the text rather
            // than dropping the response. Without
            // this fallback the agent would see an
            // empty response and crash.
            if tool_uses.is_empty() {
                let text = resp
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        AnthropicContentBlock::Text { text, .. } => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Content::Single(ContentPart::Text(TextContent {
                    text,
                    cache_control: None,
                }))
            } else if tool_uses.len() == 1 {
                // Every entry in `tool_uses` was
                // collected as `ContentPart::ToolUse`
                // (line 33), so unwrap is sound here.
                if let ContentPart::ToolUse(tu) = tool_uses.remove(0) {
                    Content::Single(ContentPart::ToolUse(tu))
                } else {
                    // Defensive: the inner type is
                    // invariant, but rustc cannot
                    // prove the destructuring
                    // without this branch.
                    Content::Single(ContentPart::Text(TextContent {
                        text: String::new(),
                        cache_control: None,
                    }))
                }
            } else {
                Content::Multi(tool_uses)
            }
        } else {
            let text = resp
                .content
                .iter()
                .filter_map(|c| match c {
                    AnthropicContentBlock::Text { text, .. } => {
                        Some(text.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Content::Single(ContentPart::Text(TextContent {
                text,
                cache_control: None,
            }))
        };

        CompletionResponse {
            id: resp.id.clone(),
            model: model.to_string(),
            content,
            usage: TokenUsage {
                prompt_tokens: resp.usage.input_tokens,
                completion_tokens: resp.usage.output_tokens,
                total_tokens: resp.usage.input_tokens
                    + resp.usage.output_tokens,
                cached_prompt_tokens: resp.usage.cache_read_input_tokens,
                cache_read_tokens: resp.usage.cache_read_input_tokens,
                cache_write_tokens: resp.usage.cache_creation_input_tokens,
                // Anthropic bills thinking blocks as output
                // tokens and does not report a separate
                // reasoning count in `message.usage`.
                reasoning_tokens: None,
            },
            // `cached` means "cache HIT" — a positive number
            // of tokens served from cache. Anthropic reports
            // `cache_read_input_tokens = Some(0)` for a cache
            // miss on a cache-eligible prompt; treating
            // `is_some()` as "cached" would mis-classify
            // these as hits. We require `Some(n)` with
            // `n > 0` to flip the flag.
            cached: resp.usage.cache_read_input_tokens.is_some_and(|n| n > 0),
            stop_reason: resp.stop_reason.clone(),
            replay_state: None,
        }
    }
}

#[cfg(test)]
mod tests;
