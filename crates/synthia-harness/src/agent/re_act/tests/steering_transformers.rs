//! Steering wiring: guards, hooks, sanitizers, hints, output
//! transformers, and the context-manager + chunk persistence
//! contracts.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use synthia_context::AgentState;
use synthia_provider::ContentPart;
use synthia_steering::Steering;
use synthia_test_support::FakeTool;
use synthia_tool::ToolRegistry;

use super::{support::*, *};
#[tokio::test]
async fn output_transformer_rewrites_committed_result() {
    struct Uppercase;
    #[async_trait]
    impl synthia_steering::OutputTransformer for Uppercase {
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
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        FakeTool::new("echo", "quiet"),
    )));
    let steering =
        Arc::new(Steering::noop().with_output_transformer(Arc::new(Uppercase)));

    let events = run_and_collect_with_steering(
        provider,
        registry,
        steering,
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
