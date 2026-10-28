//! Tool-projection memo: the loop hands the *same*
//! `Vec<ToolDefinition>` allocation to the provider on
//! consecutive iterations when nothing in the tool set
//! changed, and produces a fresh allocation when the
//! transcript-driven `Deferred` promotion changes the
//! projection.

use std::sync::Arc;

use serde_json::json;
use elysper_test_support::FakeTool;
use elysper_tool::{ToolExposure, ToolRegistry};
use tokio_util::sync::CancellationToken;

use super::{support::*, *};

#[tokio::test]
async fn repeated_iterations_share_one_tool_definition_allocation() {
    let use_echo = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![use_echo]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(elysper_tool::ToolEntry::new(Arc::new(
        FakeTool::new("echo", "echoed"),
    )));
    registry.register_entry(elysper_tool::ToolEntry::dynamic(
        "read".to_string(),
        "read a file".to_string(),
        json!({"type": "object"}),
    ));

    let _events = run_and_collect(
        provider.clone(),
        Arc::clone(&registry),
        CancellationToken::new(),
        AgentInput::text("go"),
    )
    .await;

    let captured = provider.captured_tools.lock().await;
    assert_eq!(captured.len(), 2, "one request per iteration");
    assert!(
        Arc::ptr_eq(&captured[0], &captured[1]),
        "an unchanged tool set must reuse the projected allocation"
    );
    // The catalog is still the full one — the memo must not narrow it.
    assert_eq!(captured[0].len(), 2);
    drop(captured);
}

#[tokio::test]
async fn deferred_promotion_invalidates_the_projection_memo() {
    let call_deep = ToolUse {
        id: "c1".to_string(),
        name: "deep".to_string(),
        input: json!({"q": "hello"}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![call_deep]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(
        elysper_tool::ToolEntry::dynamic(
            "deep".to_string(),
            "advertised by name until first call".to_string(),
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        )
        .with_exposure(ToolExposure::Deferred),
    );

    let _events = run_and_collect(
        provider.clone(),
        Arc::clone(&registry),
        CancellationToken::new(),
        AgentInput::text("go"),
    )
    .await;

    let captured = provider.captured_tools.lock().await;
    assert_eq!(captured.len(), 2);
    assert!(
        !Arc::ptr_eq(&captured[0], &captured[1]),
        "promotion changes the projection, so it must not be memo-served"
    );
    assert_eq!(
        captured[0][0].input_schema,
        json!({"type": "object", "additionalProperties": true}),
        "before the call: the permissive placeholder"
    );
    assert_eq!(
        captured[1][0].input_schema,
        json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        "after the call: the real schema"
    );
}

#[tokio::test]
async fn deferred_tool_called_in_input_history_is_promoted_on_the_first_request()
 {
    let real_schema =
        json!({"type": "object", "properties": {"q": {"type": "string"}}});
    let provider = Arc::new(CapturingProvider::new(vec![empty_response()]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(
        elysper_tool::ToolEntry::dynamic(
            "deep".to_string(),
            "advertised by name until first call".to_string(),
            real_schema.clone(),
        )
        .with_exposure(ToolExposure::Deferred),
    );

    let mut input = AgentInput::text("go");
    input.history = vec![Message::new(
        elysper_provider::Role::Assistant,
        Content::parts(vec![ContentPart::ToolUse(ToolUse {
            id: "c0".to_string(),
            name: "deep".to_string(),
            input: json!({"q": "x"}),
        })]),
    )];

    let _events = run_and_collect(
        provider.clone(),
        Arc::clone(&registry),
        CancellationToken::new(),
        input,
    )
    .await;

    let captured = provider.captured_tools.lock().await;
    assert_eq!(
        captured[0][0].input_schema, real_schema,
        "a tool named by the input history must be promoted before the first request"
    );
}
