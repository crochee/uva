//! Wire types for the OpenAI Chat Completions / Embeddings API.
//!
//! Visibility is crate-private because callers should go through
//! [`super::provider::OpenAICompatibleProvider`].

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub(super) struct OpenAIRequest {
    pub(super) model: String,
    pub(super) messages: Vec<OpenAIMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<Vec<OpenAITool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_tokens: Option<usize>,
    pub(super) stream: bool,
    pub(super) store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) extra_body:
        Option<std::collections::HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning_split: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct OpenAIMessage {
    pub(super) role: String,
    #[serde(serialize_with = "serialize_content")]
    #[serde(deserialize_with = "deserialize_content")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) content: Option<Vec<OpenAIContentPart>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_calls: Option<Vec<OpenAIToolUse>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    #[serde(default, rename = "reasoning_content")]
    pub(super) reasoning_content: Option<String>,
    #[serde(default, rename = "reasoning")]
    pub(super) reasoning: Option<String>,
}

pub(super) fn serialize_content<S>(
    content: &Option<Vec<OpenAIContentPart>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match content {
        None => serializer.serialize_none(),
        Some(vec) if vec.is_empty() => serializer.serialize_none(),
        Some(vec)
            if vec.len() == 1
                && let OpenAIContentPart::Text { text } = &vec[0] =>
        {
            serializer.serialize_str(text)
        }
        Some(vec) => vec.serialize(serializer),
    }
}

pub(super) fn deserialize_content<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<OpenAIContentPart>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum ContentStringOrArray {
        Null(()),
        String(String),
        Array(Vec<OpenAIContentPart>),
    }

    let content = ContentStringOrArray::deserialize(deserializer)?;
    match content {
        ContentStringOrArray::Null(()) => Ok(None),
        ContentStringOrArray::String(s) => {
            if s.is_empty() {
                Ok(None)
            } else {
                Ok(Some(vec![OpenAIContentPart::Text { text: s }]))
            }
        }
        ContentStringOrArray::Array(arr) => Ok(Some(arr)),
    }
}

/// Inner object of an `image_url` content part.
///
/// The Chat Completions API nests the URL under an `image_url` key —
/// `{"type":"image_url","image_url":{"url":…}}` — it is not a sibling
/// of `type`. Emitting the flat form makes every strict
/// OpenAI-compatible endpoint reject the whole request with
/// `422 missing field image_url`.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct OpenAIImageUrl {
    pub(super) url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) detail: Option<String>,
}

/// Inner object of an `input_audio` content part. Same nesting rule as
/// [`OpenAIImageUrl`]: `{"type":"input_audio","input_audio":{…}}`.
///
/// `data` is raw base64 (no `data:` envelope) and `format` is the
/// container name — this API family has no `mime_type` field here.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct OpenAIInputAudio {
    pub(super) data: String,
    pub(super) format: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(super) enum OpenAIContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: OpenAIImageUrl },
    #[serde(rename = "input_audio")]
    InputAudio { input_audio: OpenAIInputAudio },
    #[serde(rename = "tool_call")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        id: String,
        content: String,
        is_error: bool,
    },
    #[serde(rename = "reasoning")]
    Reasoning { text: String },
}

#[derive(Debug, Serialize)]
pub(super) struct OpenAITool {
    pub(super) r#type: String,
    pub(super) function: OpenAIFunction,
}

#[derive(Debug, Serialize)]
pub(super) struct OpenAIFunction {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct OpenAIToolUse {
    pub(super) id: String,
    pub(super) r#type: String,
    pub(super) function: OpenAIToolUseFunction,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct OpenAIToolUseFunction {
    pub(super) name: String,
    pub(super) arguments: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct OpenAIResponse {
    pub(super) id: String,
    pub(super) model: String,
    pub(super) choices: Vec<OpenAIChoice>,
    pub(super) usage: OpenAIUsage,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct OpenAIChoice {
    pub(super) message: OpenAIMessage,
    #[serde(default)]
    pub(super) finish_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct OpenAIPromptTokensDetails {
    #[serde(default)]
    pub(crate) cached_tokens: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct OpenAICompletionTokensDetails {
    #[serde(default)]
    pub(crate) reasoning_tokens: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct OpenAIUsage {
    pub(super) prompt_tokens: usize,
    pub(super) completion_tokens: usize,
    pub(super) total_tokens: usize,
    #[serde(default)]
    pub(super) prompt_tokens_details: Option<OpenAIPromptTokensDetails>,
    #[serde(default)]
    pub(super) completion_tokens_details: Option<OpenAICompletionTokensDetails>,
}

#[derive(Debug, Serialize)]
pub(super) struct OpenAIEmbeddingRequest {
    pub(super) model: String,
    pub(super) input: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAIEmbeddingResponse {
    pub(super) data: Vec<OpenAIEmbeddingData>,
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAIEmbeddingData {
    pub(super) embedding: Vec<f32>,
}

#[cfg(test)]
mod tests;
