//! `DemoEchoProvider` — the keyless demo/CI provider
//! (`[providers.<name>] type = "demo"`, alias `"fake"`).
//!
//! It echoes the last user message back, so a full end-to-end demo
//! (chat → SSE → durable session) runs with no API key and no
//! network. Gated behind `test-utils` so a production build cannot
//! select it by typo: without the feature the same `type` is reported
//! as an unsupported type, exactly as any other unknown string.

use std::sync::Arc;

use synthia::{
    core::{CancelToken, Error},
    provider::{
        CompletionRequest,
        CompletionResponse,
        Content,
        ContentPart,
        ModelConfig,
        ProviderConfig,
        Role,
        SamplingResult,
        StreamChunk,
        TextContent,
        TokenUsage,
        traits::ModelProvider,
    },
};

/// Stateless echo provider.
#[derive(Debug, Default)]
pub struct DemoEchoProvider;

fn echo_text(request: &CompletionRequest) -> String {
    // The harness injects a "Current runtime context" snapshot as a
    // user-role message each turn; echoing that back would show the
    // model's own context block instead of the user's words.
    let is_user_turn = |text: &str| !text.contains("runtime-context snapshots");
    let mut fallback = None;
    let mut chosen = None;
    for message in request.messages.iter().rev() {
        if message.role != Role::User {
            continue;
        }
        let Some(text) = message.content.extract_text() else {
            continue;
        };
        if fallback.is_none() {
            fallback = Some(text.clone());
        }
        if is_user_turn(&text) {
            chosen = Some(text);
            break;
        }
    }
    match chosen.or(fallback) {
        Some(text) if !text.trim().is_empty() => {
            format!("demo echo: {text}")
        }
        _ => "demo echo: (no user message)".to_string(),
    }
}

fn response(text: String) -> CompletionResponse {
    CompletionResponse {
        id: "demo".to_string(),
        model: "demo".to_string(),
        content: Content::Single(ContentPart::Text(TextContent {
            text,
            cache_control: None,
        })),
        usage: TokenUsage::default(),
        cached: false,
        stop_reason: None,
        replay_state: None,
    }
}

#[async_trait::async_trait]
impl ModelProvider for DemoEchoProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "demo"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "demo".to_string(),
            provider: "demo".to_string(),
            context_window: 128_000,
            max_output_tokens: 4096,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        Ok(response(echo_text(&request)))
    }

    async fn complete_with_stream(
        &self,
        request: CompletionRequest,
        _cancel_token: Option<Arc<dyn CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<CompletionResponse, Error> {
        let text = echo_text(&request);
        on_delta(StreamChunk::Content(ContentPart::Text(TextContent {
            text: text.clone(),
            cache_control: None,
        })));
        on_delta(StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: text.clone(),
                tool_calls: Vec::new(),
                reasoning: String::new(),
                reasoning_signature: None,
                usage: TokenUsage::default(),
                ..Default::default()
            }),
        });
        Ok(response(text))
    }
}

#[cfg(test)]
mod tests {
    use synthia::provider::Message;

    use super::*;

    fn request(messages: Vec<Message>) -> CompletionRequest {
        CompletionRequest {
            messages: Arc::new(messages),
            ..CompletionRequest::default()
        }
    }

    #[tokio::test]
    async fn echoes_the_last_user_message() {
        let provider = DemoEchoProvider;
        let response = provider
            .complete(request(vec![
                Message::system("sys"),
                Message::user("hello"),
                Message::assistant("ignored"),
            ]))
            .await
            .unwrap();
        assert_eq!(
            response.content.extract_text().as_deref(),
            Some("demo echo: hello")
        );
    }

    #[tokio::test]
    async fn streams_the_same_text() {
        let provider = DemoEchoProvider;
        let seen: Arc<std::sync::Mutex<Vec<StreamChunk>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let on_delta: Box<dyn FnMut(StreamChunk) + Send> =
            Box::new(move |chunk| sink.lock().unwrap().push(chunk));
        let response = provider
            .complete_with_stream(
                request(vec![Message::user("ping")]),
                None,
                on_delta,
            )
            .await
            .unwrap();
        assert_eq!(
            response.content.extract_text().as_deref(),
            Some("demo echo: ping")
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "one text delta + the terminal IsDone"
        );
    }

    #[tokio::test]
    async fn skips_the_injected_runtime_context_snapshot() {
        let provider = DemoEchoProvider;
        let response = provider
            .complete(request(vec![
                Message::user("real turn"),
                Message::user(
                    "Current runtime context. This snapshot supersedes \
                     earlier runtime-context snapshots.\n\nWorking directory: /tmp",
                ),
            ]))
            .await
            .unwrap();
        assert_eq!(
            response.content.extract_text().as_deref(),
            Some("demo echo: real turn")
        );
    }

    #[tokio::test]
    async fn no_user_message_is_not_an_error() {
        let provider = DemoEchoProvider;
        let response = provider.complete(request(vec![])).await.unwrap();
        assert_eq!(
            response.content.extract_text().as_deref(),
            Some("demo echo: (no user message)")
        );
    }
}
