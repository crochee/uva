//! The [`Content`] / [`ContentPart`] enums, the data structs they
//! reference (`TextContent` / `ImageContent` / `AudioContent`), the
//! `ImageDetail` / `AudioFormat` small enums, all `From` /
//! `IntoIterator` impls, and the `is_tool_use` / `text` /
//! `cache_control` accessors.

use serde::{Deserialize, Serialize};

use super::tool::{ResourceLink, ToolResult, ToolUse};
use crate::cache_mark::CacheControlMark;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
// `Single` holds a full `ContentPart` inline (≈296 bytes for the
// largest variant `Reasoning`) so the common single-part path
// avoids a heap allocation. `Multi` only holds a 24-byte `Vec`
// header. Boxing the large variant would defeat the optimisation,
// so the size difference is intentional.
#[allow(clippy::large_enum_variant)]
pub enum Content {
    Single(ContentPart),
    Multi(Vec<ContentPart>),
}

impl Content {
    pub fn text(text: impl Into<String>) -> Self {
        Content::Single(ContentPart::Text(TextContent {
            text: text.into(),
            cache_control: None,
        }))
    }

    pub fn parts(mut parts: Vec<ContentPart>) -> Self {
        if parts.len() == 1 {
            Content::Single(parts.remove(0))
        } else {
            Content::Multi(parts)
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &ContentPart> {
        self.into_iter()
    }

    pub fn has_tool_use(&self) -> bool {
        self.into_iter().any(ContentPart::is_tool_use)
    }

    pub fn has_text(&self) -> bool {
        self.into_iter().any(|p| matches!(p, ContentPart::Text(..)))
    }

    pub fn extract_text(&self) -> Option<String> {
        let texts: Vec<String> = self
            .into_iter()
            .filter_map(|p| {
                if let ContentPart::Text(tc) = p {
                    Some(tc.text.clone())
                } else {
                    None
                }
            })
            .collect();
        if texts.is_empty() {
            None
        } else {
            Some(texts.join(""))
        }
    }

    pub fn extract_tool_uses(&self) -> Vec<ToolUse> {
        self.into_iter()
            .filter_map(|p| {
                if let ContentPart::ToolUse(tu) = p {
                    Some(tu.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}

impl From<String> for Content {
    fn from(s: String) -> Self {
        Content::text(s)
    }
}

impl From<&str> for Content {
    fn from(s: &str) -> Self {
        Content::text(s.to_string())
    }
}

impl IntoIterator for Content {
    type IntoIter = std::vec::IntoIter<Self::Item>;
    type Item = ContentPart;

    fn into_iter(self) -> Self::IntoIter {
        match self {
            Content::Single(part) => vec![part].into_iter(),
            Content::Multi(parts) => parts.into_iter(),
        }
    }
}

impl<'a> IntoIterator for &'a Content {
    type IntoIter = std::slice::Iter<'a, ContentPart>;
    type Item = &'a ContentPart;

    fn into_iter(self) -> Self::IntoIter {
        match self {
            Content::Single(part) => std::slice::from_ref(part).iter(),
            Content::Multi(parts) => parts.iter(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TextContent {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cache_control: Option<CacheControlMark>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReasoningContent {
    pub text: String,
    /// Anthropic `signature_delta` value attached to the most recent
    /// reasoning block. Required to preserve cross-turn reasoning
    /// continuity when the upstream Provider is Anthropic.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImageContent {
    pub data: String,
    pub mime_type: String,
    pub detail: Option<ImageDetail>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AudioContent {
    pub data: String,
    pub mime_type: String,
    pub format: Option<AudioFormat>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text(TextContent),
    Image(ImageContent),
    Audio(AudioContent),
    ToolUse(ToolUse),
    ToolResult(ToolResult),
    Reasoning(ReasoningContent),
    Resource(ResourceLink),
}

impl ContentPart {
    pub fn is_tool_use(&self) -> bool {
        matches!(self, ContentPart::ToolUse(..))
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            ContentPart::Text(tc) => Some(&tc.text),
            _ => None,
        }
    }

    /// Mutable access to the text content of a `Text` variant.
    ///
    /// Returns `None` for non-`Text` variants.
    pub fn text_mut(&mut self) -> Option<&mut String> {
        match self {
            ContentPart::Text(tc) => Some(&mut tc.text),
            _ => None,
        }
    }

    /// Get the [`CacheControlMark`] on this part when it is a `Text`
    /// variant with a mark set. Returns `None` for non-`Text` variants
    /// or when no mark is present.
    pub fn cache_control(&self) -> Option<&CacheControlMark> {
        match self {
            ContentPart::Text(tc) => tc.cache_control.as_ref(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageDetail {
    Low,
    High,
    Auto,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    Wav,
    Mp3,
    Flac,
}

/// `true` when a media part's `data` field holds a remote URL rather
/// than inline bytes.
///
/// Classification is by **shape**, never by length: a real base64 PNG
/// is tens of kilobytes, so any length threshold classifies it as a
/// URL and produces a request the provider rejects. Only an explicit
/// `http://` / `https://` scheme means "remote".
///
/// Gated on the adapters: these helpers exist to render a *request*, and
/// a build with neither adapter has no wire to render onto. A
/// `cfg`-gated definition is honest about that; an `allow(dead_code)`
/// would instead hide genuinely orphaned code in every configuration.
#[cfg(any(feature = "anthropic", feature = "openai"))]
fn is_remote_url(data: &str) -> bool {
    let data = data.trim_start();
    data.starts_with("http://") || data.starts_with("https://")
}

/// The inline base64 payload of a media part, with any `data:` envelope
/// removed. Callers must have checked [`is_remote_url`] first.
#[cfg(any(feature = "anthropic", feature = "openai"))]
fn inline_base64(data: &str) -> &str {
    match data.strip_prefix("data:") {
        Some(rest) => rest
            .split_once(',')
            .map(|(_, payload)| payload)
            .unwrap_or(rest),
        None => data,
    }
}

/// Each method is gated on the adapter that actually calls it, so a
/// consumer who compiles exactly one adapter gets no dead-code warning
/// for the other's helper. `is_remote_url` is shared: `to_url` needs it
/// too.
#[cfg(any(feature = "anthropic", feature = "openai"))]
impl ImageContent {
    /// `true` when this part references a remote image rather than
    /// carrying inline bytes.
    pub(crate) fn is_remote_url(&self) -> bool {
        is_remote_url(&self.data)
    }

    /// The payload bytes as base64, without the `data:` envelope.
    /// `None` for a remote URL.
    ///
    /// Anthropic's `image.source` takes the payload directly; OpenAI's
    /// `image_url` wants the whole `data:` URL, so only this adapter
    /// needs the split.
    #[cfg(feature = "anthropic")]
    pub(crate) fn inline_base64(&self) -> Option<&str> {
        (!self.is_remote_url()).then(|| inline_base64(&self.data))
    }

    /// The inline form OpenAI accepts for an image: a `data:` URL. A
    /// `data:` input is passed through unchanged; a bare base64 string is
    /// wrapped with this part's media type. Remote URLs are returned
    /// as-is.
    #[cfg(feature = "openai")]
    pub(crate) fn to_url(&self) -> String {
        if self.is_remote_url() || self.data.starts_with("data:") {
            self.data.clone()
        } else {
            format!("data:{};base64,{}", self.mime_type, self.data)
        }
    }
}

#[cfg(any(feature = "anthropic", feature = "openai"))]
impl AudioContent {
    /// `true` when this part references remote audio rather than
    /// carrying inline bytes.
    pub(crate) fn is_remote_url(&self) -> bool {
        is_remote_url(&self.data)
    }

    /// The payload bytes as base64, without the `data:` envelope.
    /// `None` for a remote URL.
    pub(crate) fn inline_base64(&self) -> Option<&str> {
        (!self.is_remote_url()).then(|| inline_base64(&self.data))
    }

    /// The wire label for this part's audio container, falling back to
    /// the media type's subtype when no explicit format was recorded.
    /// `None` when neither yields a known container.
    pub(crate) fn format_label(&self) -> Option<&'static str> {
        let explicit = self.format.as_ref().map(|f| match f {
            AudioFormat::Wav => "wav",
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Flac => "flac",
        });
        explicit.or(match self.mime_type.as_str() {
            "audio/wav" | "audio/wave" | "audio/x-wav" => Some("wav"),
            "audio/mpeg" | "audio/mp3" | "audio/mpeg3" => Some("mp3"),
            "audio/flac" | "audio/x-flac" => Some("flac"),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests;
