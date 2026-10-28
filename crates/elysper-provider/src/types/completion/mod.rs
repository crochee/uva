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
    /// opaque provider-native state the caller wants the
    /// adapter to restore on a later call (Anthropic prompt
    /// cache id, OpenAI response id, etc.). The runtime
    /// threads it through unchanged; the adapter validates
    /// and either consumes or drops it. Default: `None`
    /// (no replay state to restore).
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
    /// opaque provider-native state the adapter minted
    /// for this response (Anthropic prompt cache id,
    /// OpenAI response id, etc.). The runtime may pass
    /// it back to the adapter on a later request; the
    /// adapter decides whether to restore the state.
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
