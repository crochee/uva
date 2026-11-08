use std::sync::Arc;

use elysper_provider::{ContentPart, ToolUse};
use elysper_test_support::FakeTool;
use elysper_tool::ToolRegistry;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::super::{support::*, *};

pub(super) async fn call_to_a_hidden_tool_is_refused_without_executing_it() {
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "call_secret".to_string(),
            name: "secret".to_string(),
            input: json!({}),
        }]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(
        elysper_tool::ToolEntry::new(Arc::new(FakeTool::new(
            "secret",
            "SENTINEL-EXECUTED",
        )))
        .with_is_hidden(true),
    );

    let events = run_and_collect(
        provider,
        registry,
        CancellationToken::new(),
        AgentInput::text("go"),
    )
    .await;

    let results: Vec<&elysper_provider::ToolResult> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Model(ContentPart::ToolResult(result)) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1, "the call must be answered: {events:?}");
    assert_eq!(results[0].tool_use_id, "call_secret");
    assert_eq!(
        results[0].is_error,
        Some(true),
        "a hidden tool's call is an error, not a silent no-op"
    );
    let body = serde_json::to_string(&results[0].content).unwrap();
    assert!(
        !body.contains("SENTINEL-EXECUTED"),
        "the hidden tool must never run: {body}"
    );
}
