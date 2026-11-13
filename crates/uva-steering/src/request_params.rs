//! [`RequestParams`] — the request parameters a `before_request`
//! hook may rewrite.

use std::sync::Arc;

use uva_provider::{CompletionRequest, Message, ToolChoice, ToolDefinition};

/// The parameters of one model request.
///
/// The type is the whole rewrite surface of a request-rewriting
/// plugin: the model, the sampling temperature, the response budget,
/// the tool choice, and the tool schemas. Two things are
/// deliberately absent:
///
/// - there is no `system` field, so the system prompt is out of
///   reach by type, not by convention;
/// - the message list is a read-only view — [`RequestParams::messages`]
///   hands out a `&[Message]` and there is no setter — so a plugin
///   can read the transcript but cannot rewrite it.
///
/// [`RequestParams::from_request`] snapshots a request's parameters;
/// [`RequestParams::apply_to`] writes them back.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestParams {
    /// The model to call.
    pub model: String,
    /// The sampling temperature; `None` is the provider's default.
    pub temperature: Option<f64>,
    /// The response budget in tokens; `None` is the provider's
    /// default.
    pub max_tokens: Option<usize>,
    /// Which tool the model may call.
    pub tool_choice: ToolChoice,
    /// The tool schemas the model is shown.
    pub tools: Arc<Vec<ToolDefinition>>,
    messages: Arc<Vec<Message>>,
}

impl RequestParams {
    /// Snapshot one request's parameters.
    #[must_use]
    pub fn from_request(request: &CompletionRequest) -> Self {
        Self {
            model: request.model.clone(),
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            tool_choice: request.tool_choice.clone(),
            tools: Arc::clone(&request.tools),
            messages: Arc::clone(&request.messages),
        }
    }

    /// The request's messages — read-only.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Write these parameters back onto a request. The message list
    /// is left alone: it is not part of the rewrite surface.
    pub fn apply_to(&self, request: &mut CompletionRequest) {
        request.model = self.model.clone();
        request.temperature = self.temperature;
        request.max_tokens = self.max_tokens;
        request.tool_choice = self.tool_choice.clone();
        request.tools = Arc::clone(&self.tools);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uva_provider::{
        CompletionRequest,
        Message,
        ToolChoice,
        ToolDefinition,
    };

    use super::*;

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "base-model".to_string(),
            messages: Arc::new(vec![Message::user("hello")]),
            tools: Arc::new(vec![ToolDefinition {
                name: "echo".to_string(),
                description: "echo".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
                cache_control: None,
                annotations: None,
            }]),
            tool_choice: ToolChoice::Auto,
            temperature: Some(0.5),
            max_tokens: Some(128),
            ..CompletionRequest::default()
        }
    }

    #[test]
    fn the_params_carry_the_requests_own_values() {
        let request = request();
        let params = RequestParams::from_request(&request);
        assert_eq!(params.model, "base-model");
        assert_eq!(params.temperature, Some(0.5));
        assert_eq!(params.max_tokens, Some(128));
        assert_eq!(params.tool_choice, ToolChoice::Auto);
        assert_eq!(params.tools.len(), 1);
    }

    #[test]
    fn the_messages_are_a_read_only_view() {
        let request = request();
        let params = RequestParams::from_request(&request);
        assert_eq!(params.messages().len(), 1);
        // The view is a slice: there is no way to reach a mutable
        // message through it.
        let _: &[Message] = params.messages();
    }

    #[test]
    fn applying_the_params_rewrites_the_request_but_not_its_messages() {
        let mut request = request();
        let mut params = RequestParams::from_request(&request);
        params.model = "other-model".to_string();
        params.temperature = Some(0.1);
        params.max_tokens = None;
        params.tool_choice = ToolChoice::None;
        params.apply_to(&mut request);
        assert_eq!(request.model, "other-model");
        assert_eq!(request.temperature, Some(0.1));
        assert_eq!(request.max_tokens, None);
        assert_eq!(request.tool_choice, ToolChoice::None);
        // The transcript is not part of the rewrite surface.
        assert_eq!(request.messages.len(), 1);
        assert_eq!(
            request.messages[0].content.extract_text().as_deref(),
            Some("hello")
        );
    }
}
