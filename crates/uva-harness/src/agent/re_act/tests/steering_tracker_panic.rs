//! Host wiring: guards, hooks, sanitizers, hints, output
//! transformers, and the context-manager + chunk persistence
//! contracts.

use std::sync::Arc;

use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{support::*, *};
use crate::agent::host::Host;
#[tokio::test]
async fn system_prompt_hint_is_appended_once_per_session() {
    use uva_context::AgentState;
    use uva_steering::{Hint, HintMessage, HintPriority, hint::InjectionPoint};

    const MARKER: &str = "SYSTEM-HINT-MARKER";

    struct AlwaysHint;
    impl Hint for AlwaysHint {
        fn name(&self) -> &str {
            "always"
        }

        fn should_trigger(&self, _state: &AgentState) -> bool {
            true
        }

        fn generate(&self, _state: &AgentState) -> HintMessage {
            HintMessage {
                content: MARKER.to_string(),
                priority: HintPriority::Normal,
            }
        }

        fn injection_point(&self) -> InjectionPoint {
            InjectionPoint::SystemPrompt
        }
    }

    // Two iterations: a tool call, then a final answer — so the hint is
    // consulted (and, before the fix, re-appended) twice.
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "call_1".to_string(),
            name: "echo".to_string(),
            input: json!({}),
        }]),
        empty_response(),
    ]));
    let (registry, _calls) = echo_registry();

    let mut host = Host::new();
    let _hint = host.hint(Arc::new(AlwaysHint));

    let agent = ReActAgent::new(provider.clone(), registry)
        .with_host(host)
        .expect("bind the host");
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let captured = provider.captured.lock().await;
    assert!(
        captured.len() >= 2,
        "need at least two sampling passes to test re-append; got {}",
        captured.len()
    );
    // The last request shows the system prompt as it accumulated.
    let system_text = captured
        .last()
        .unwrap()
        .first()
        .map(|m| m.content.extract_text().unwrap_or_default())
        .unwrap_or_default();
    let count = system_text.matches(MARKER).count();
    assert!(
        count >= 1,
        "the hint must actually be appended at all; system message was: \
         {system_text}"
    );
    assert_eq!(
        count, 1,
        "a SystemPrompt hint must be appended exactly once per session; \
         system message was: {system_text}"
    );
}
