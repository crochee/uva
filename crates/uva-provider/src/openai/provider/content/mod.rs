//! The 2 content methods on
//! [`super::core::OpenAICompatibleProvider`]:
//!
//! - [`OpenAICompatibleProvider::transform_content`] —
//!   dispatches on `Content::Single` vs `Content::Multi` to
//!   map each `ContentPart` via `transform_part`.
//! - [`OpenAICompatibleProvider::transform_part`] — maps a
//!   single [`crate::types::ContentPart`] to an
//!   [`super::types::OpenAIContentPart`]. Text/Image map 1:1;
//!   `Audio` becomes `input_audio` only for the containers this API
//!   family accepts (`wav`/`mp3`) and otherwise degrades to a text
//!   note; `ToolUse` keeps `id`/`name`/`input`;
//!   `ToolResult` becomes a flat text representation via
//!   `format!("{:?}", tr.content)` (the OpenAI API does not
//!   preserve the structured result); `Reasoning` is dropped (no
//!   request-side slot exists); `Resource` is projected by
//!   [`ResourceLink::prompt_projection`] — a bounded note carrying the
//!   name, the media type, and any small inline `text/*` payload, never
//!   the base64.
//!
//! [`ResourceLink::prompt_projection`]: crate::types::ResourceLink::prompt_projection

use super::{
    super::types::{OpenAIContentPart, OpenAIImageUrl},
    core::OpenAICompatibleProvider,
};

impl OpenAICompatibleProvider {
    pub(in crate::openai) fn transform_content(
        &self,
        content: &crate::types::Content,
    ) -> Vec<OpenAIContentPart> {
        match content {
            crate::types::Content::Single(part) => {
                self.transform_part(part).into_iter().collect()
            }
            crate::types::Content::Multi(parts) => parts
                .iter()
                .filter_map(|p| self.transform_part(p))
                .collect(),
        }
    }

    /// Project one canonical part onto the OpenAI-compatible wire
    /// vocabulary, or `None` when this API family has no request-side
    /// slot for it.
    ///
    /// Only [`ContentPart::Reasoning`] is such a part. Reasoning is an
    /// *output* concept here: `delta.reasoning_content` is read off the
    /// response (see `openai_streaming`), but no `/chat/completions`
    /// request body defines a `reasoning` content block — real OpenAI
    /// rejects an unknown `type`, and the gateways that do expose
    /// reasoning expose it on the way out only. Anthropic is the
    /// opposite: it *requires* the signed `thinking` block to be echoed
    /// back, which is why the same part survives
    /// `anthropic::transform_part` as a `ThinkingBlock`.
    ///
    /// The history keeps the reasoning part either way; dropping it
    /// happens here, at the last moment before the wire, where the
    /// adapter owns the wire shape.
    ///
    /// [`ContentPart::Reasoning`]: crate::types::ContentPart::Reasoning
    pub(in crate::openai) fn transform_part(
        &self,
        part: &crate::types::ContentPart,
    ) -> Option<OpenAIContentPart> {
        let mapped = match part {
            crate::types::ContentPart::Text(tc) => OpenAIContentPart::Text {
                text: tc.text.clone(),
            },
            crate::types::ContentPart::Image(ic) => {
                OpenAIContentPart::ImageUrl {
                    image_url: OpenAIImageUrl {
                        url: ic.to_url(),
                        detail: ic.detail.as_ref().map(|d| match d {
                            crate::types::ImageDetail::Low => "low".to_string(),
                            crate::types::ImageDetail::High => {
                                "high".to_string()
                            }
                            crate::types::ImageDetail::Auto => {
                                "auto".to_string()
                            }
                        }),
                    },
                }
            }
            crate::types::ContentPart::Audio(ac) => {
                // `input_audio` requires inline bytes in a container
                // this API family knows (`wav` / `mp3` only), and it has
                // no URL slot. Neither a wider container such as `flac`
                // nor a remote URL can be expressed, so each degrades to
                // a text note instead of a body the provider rejects.
                let Some(input_audio) = super::input_audio(ac) else {
                    return Some(OpenAIContentPart::Text {
                        text: format!("[Audio: {}]", ac.data),
                    });
                };
                OpenAIContentPart::InputAudio { input_audio }
            }
            crate::types::ContentPart::ToolUse(tu) => {
                OpenAIContentPart::ToolUse {
                    id: tu.id.clone(),
                    name: tu.wire_name(),
                    input: tu.wire_input(),
                }
            }
            crate::types::ContentPart::ToolResult(tr) => {
                OpenAIContentPart::ToolResult {
                    id: tr.tool_use_id.clone(),
                    content: format!("{:?}", tr.content),
                    is_error: tr.is_error.unwrap_or(false),
                }
            }
            crate::types::ContentPart::Reasoning(_) => return None,
            crate::types::ContentPart::Resource(link) => {
                OpenAIContentPart::Text {
                    text: link.prompt_projection(),
                }
            }
        };
        Some(mapped)
    }
}

#[cfg(test)]
mod tests;
