//! The completion request / response types + [`ToolChoice`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{
    content::{Content, ContentPart},
    message::Message,
    models::TokenUsage,
    tool::ToolDefinition,
};
use crate::cache_policy::CachePolicy;

/// `messages` and `tools` are `Arc<Vec<T>>` so that reference equality
/// (`Arc::ptr_eq`) can be used to short-circuit cache policy
/// re-application and to signal that the prompt cache prefix is
/// unchanged between calls. `Arc<Vec<T>>` derefs to `Vec<T>`, so read
/// accesses (`request.tools.iter()`, `request.tools[i]`, etc.) work
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Arc<Vec<Message>>,
    pub tools: Arc<Vec<ToolDefinition>>,
    pub tool_choice: ToolChoice,
    pub temperature: Option<f64>,
    pub max_tokens: Option<usize>,
    pub stop_sequences: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub extra_body:
        Option<std::collections::HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cache_policy: Option<CachePolicy>,
    /// R29 (dsh `assistantProvenance.replayState` parity):
    /// opaque provider-native continuity state the caller wants the
    /// adapter to restore on this call — the seam dsh calls
    /// `assistantProvenance.replayState`.
    ///
    /// **Status:** the shipped adapters mint no native state —
    /// the OpenAI adapter speaks Chat Completions (no
    /// `previous_response_id`, which belongs to the Responses
    /// API) and Anthropic Messages has no replayable response
    /// id. Reasoning continuity for Anthropic is carried
    /// structurally instead: the thinking `signature` rides the
    /// reasoning part itself, and the harness voids signatures a
    /// compaction invalidated (its span always starts at the
    /// surface head, so every kept-tail message's prefix contains
    /// the shadowed span — the rule is exact, not an
    /// approximation). An adapter that does mint native state
    /// (a Responses-API adapter's `previous_response_id`, a
    /// per-message prefix digest) writes it on the response,
    /// sees it back here, and validates or drops it. Default:
    /// `None` (no replay state to restore).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub replay_state: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Specific {
        name: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub id: String,
    pub model: String,
    pub content: Content,
    pub usage: TokenUsage,
    pub cached: bool,
    /// Provider's stop reason (e.g. Anthropic `end_turn`,
    /// `tool_use`, `max_tokens`, OpenAI `stop`,
    /// `tool_calls`, `length`). Optional because not all
    /// providers surface it and not all paths read it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stop_reason: Option<String>,
    /// R29 (dsh `assistantProvenance.replayState` parity):
    /// opaque provider-native continuity state the adapter minted
    /// for this response — the response half of
    /// [`CompletionRequest::replay_state`].
    ///
    /// **Status:** no shipped adapter mints state (see the request
    /// half for what carries continuity instead). The field is the
    /// seam for an adapter that does; the runtime threads it through
    /// unchanged and never interprets it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub replay_state: Option<serde_json::Value>,
}

impl Default for CompletionResponse {
    fn default() -> Self {
        Self {
            id: String::new(),
            model: String::new(),
            content: Content::Single(ContentPart::Text(
                super::content::TextContent {
                    text: String::new(),
                    cache_control: None,
                },
            )),
            usage: TokenUsage::default(),
            cached: false,
            stop_reason: None,
            replay_state: None,
        }
    }
}

#[cfg(test)]
mod tests;
