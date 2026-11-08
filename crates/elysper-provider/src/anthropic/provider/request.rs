//! The request builder method on
//! [`super::core::AnthropicProvider`]:
//!
//! - [`AnthropicProvider::make_request`] — transforms the
//!   [`crate::types::CompletionRequest`] into a
//!   JSON-serialized body, then returns a
//!   `reqwest::RequestBuilder` POSTing to
//!   `{base_url}/v1/messages` with the
//!   `anthropic-version`, `content-type`, and
//!   `anthropic-beta` headers plus optional `x-api-key`
//!   auth. The outgoing body is logged at
//!   `tracing::debug!` level under the
//!   `elysper_provider::anthropic::debug` target
//!   (downgraded from `info!` to avoid leaking user
//!   prompts / system prompts / API keys into production
//!   logs at default verbosity).

use elysper_core::Error;

use super::core::AnthropicProvider;
use crate::types::CompletionRequest;

impl AnthropicProvider {
    /// The one POST builder for `/v1/messages`.
    ///
    /// `.json(body)` already sets `content-type: application/json`;
    /// adding it again appends a second header, and at least one
    /// Anthropic-compatible proxy (DeepSeek) answers a duplicated
    /// content-type with `415 Expected request with Content-Type:
    /// application/json` even though the value is right — so this is
    /// the only place the headers are set, and a test pins the count.
    pub(in crate::anthropic) fn messages_request(
        &self,
        body: &super::super::types::AnthropicRequest,
    ) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .json(body)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "prompt-caching-2024-07-31");
        if let Some(key) = &self.api_key {
            req = req.header("x-api-key", key);
        }
        req
    }

    pub(in crate::anthropic) async fn make_request(
        &self,
        request: &CompletionRequest,
    ) -> Result<reqwest::RequestBuilder, Error> {
        let url = format!("{}/v1/messages", self.base_url);
        let body = self.transform_request(request);
        let body_json = serde_json::to_string(&body).unwrap_or_default();

        tracing::debug!(target: "elysper_provider::anthropic::debug",
            url = %url,
            body_len = body_json.len(),
            "Anthropic outgoing request body"
        );

        Ok(self.messages_request(&body))
    }
}
