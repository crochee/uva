//! Host wiring: guards, hooks, sanitizers, hints, output
//! transformers, and the context-manager + chunk persistence
//! contracts.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use elysper_context::AgentState;
use elysper_provider::ContentPart;
use elysper_test_support::FakeTool;
use elysper_tool::ToolRegistry;

use super::{support::*, *};
use crate::agent::host::Host;
#[tokio::test]
async fn output_transformer_rewrites_committed_result() {
    struct Uppercase;
    #[async_trait]
    impl elysper_steering::OutputTransformer for Uppercase {
        async fn transform(
            &self,
            output: String,
            _tool_name: &str,
            _state: &AgentState,
        ) -> String {
            output.to_uppercase()
        }
    }

    let tool_use = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![tool_use.clone()]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(elysper_tool::ToolEntry::new(Arc::new(
        FakeTool::new("echo", "quiet"),
    )));
    let mut host = Host::new();
    let _transformer = host.output_transformer(Arc::new(Uppercase));

    let events = run_and_collect_with_host(
        provider,
        registry,
        host,
        AgentInput::text("run echo"),
    )
    .await;

    let result_text = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => {
                Some(tr.content.clone())
            }
            _ => None,
        })
        .next()
        .expect("tool result event");
    assert!(
        result_text.iter().any(|p| {
            matches!(p, ContentPart::Text(t) if t.text == "QUIET")
        })
    );
}
