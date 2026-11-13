//! The tool-message transform method on
//! [`super::core::OpenAICompatibleProvider`]:
//!
//! - [`OpenAICompatibleProvider::transform_tool_message`] —
//!   builds the `OpenAIMessage` for a `role == "tool"`
//!   message. It walks the content looking for
//!   `ContentPart::ToolResult(tr)`, extracts the text body
//!   (joined with `\n`) and the first media part
//!   (image/audio from `tr.content`); `tool_call_id` is
//!   `msg.tool_call_id` or, as a fallback, the first
//!   `tr.tool_use_id` found in the content.

use super::{
    super::types::{OpenAIContentPart, OpenAIImageUrl, OpenAIMessage},
    core::OpenAICompatibleProvider,
};

impl OpenAICompatibleProvider {
    pub(in crate::openai) fn transform_tool_message(
        &self,
        msg: &crate::types::Message,
    ) -> OpenAIMessage {
        fn extract_text_from_content(
            content: &[crate::types::ContentPart],
        ) -> String {
            let texts: Vec<String> = content
                .iter()
                .filter_map(|p| {
                    if let crate::types::ContentPart::Text(tc) = p {
                        Some(tc.text.clone())
                    } else {
                        None
                    }
                })
                .collect();
            texts.join("\n")
        }

        fn extract_media_parts(
            content: &[&crate::types::ContentPart],
        ) -> Vec<OpenAIContentPart> {
            content
                .iter()
                .filter_map(|p| match *p {
                    crate::types::ContentPart::Image(ic) => {
                        Some(OpenAIContentPart::ImageUrl {
                            image_url: OpenAIImageUrl {
                                url: ic.to_url(),
                                detail: ic.detail.as_ref().map(|d| match d {
                                    crate::types::ImageDetail::Low => {
                                        "low".to_string()
                                    }
                                    crate::types::ImageDetail::High => {
                                        "high".to_string()
                                    }
                                    crate::types::ImageDetail::Auto => {
                                        "auto".to_string()
                                    }
                                }),
                            },
                        })
                    }
                    crate::types::ContentPart::Audio(ac) => {
                        // Same narrowing as `transform_part`:
                        // `input_audio` accepts only `wav`/`mp3` inline
                        // bytes, so anything else (a `flac` upload, a
                        // remote URL) has no slot here and the part is
                        // dropped from the tool result rather than
                        // producing an invalid body.
                        super::input_audio(ac).map(|input_audio| {
                            OpenAIContentPart::InputAudio { input_audio }
                        })
                    }
                    crate::types::ContentPart::ToolResult(tr) => {
                        let inner: Vec<&crate::types::ContentPart> =
                            tr.content.iter().collect();
                        let media = extract_media_parts(&inner);
                        if !media.is_empty() {
                            media.into_iter().next()
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .collect()
        }

        let content_parts: Vec<&crate::types::ContentPart> = match &msg.content
        {
            crate::types::Content::Single(p) => vec![p],
            crate::types::Content::Multi(ps) => ps.iter().collect(),
        };

        // A tool message's content is EITHER the canonical
        // `ToolResult` wrapper (what the session sink's projection
        // writes) OR the tool's own result parts, unwrapped — which is
        // exactly what the harness commits for a live run:
        // `Message::tool(Content::parts(tr.content.clone()), call_id)`.
        //
        // Reading only the wrapper left `content` empty for every
        // harness-built message, so the wire body was
        // `{"role":"tool","content":""}` and the model was told the tool
        // had produced nothing. It then reported exactly that back to
        // the user, while the real output sat in the session log
        // (`ContentPart::ToolResult` reaches that log by a different
        // path) — which is why the transcript looked correct.
        let mut texts: Vec<String> = Vec::new();
        let mut tool_use_id: Option<String> = None;
        for part in &content_parts {
            match part {
                crate::types::ContentPart::ToolResult(tr) => {
                    if tool_use_id.is_none() {
                        tool_use_id = Some(tr.tool_use_id.clone());
                    }
                    texts.push(extract_text_from_content(&tr.content));
                }
                crate::types::ContentPart::Text(tc) => {
                    texts.push(tc.text.clone())
                }
                // Everything else (media, tool_use, reasoning,
                // resource) carries no text body; media is projected
                // by `extract_media_parts` below.
                _ => {}
            }
        }
        let content_str = texts
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n");

        let mut parts: Vec<OpenAIContentPart> = if content_str.is_empty() {
            Vec::new()
        } else {
            vec![OpenAIContentPart::Text { text: content_str }]
        };
        parts.extend(extract_media_parts(&content_parts));

        OpenAIMessage {
            role: "tool".to_string(),
            content: if parts.is_empty() {
                Some(vec![OpenAIContentPart::Text {
                    text: String::new(),
                }])
            } else {
                Some(parts)
            },
            tool_calls: None,
            tool_call_id: msg.tool_call_id.clone().or(tool_use_id),
            name: msg.name.clone(),
            reasoning_content: None,
            reasoning: None,
        }
    }
}

#[cfg(test)]
mod tests;
