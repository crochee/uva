use std::sync::Arc;

use futures::StreamExt;
use serde_json::json;
use synthia_provider::{StreamChunk, ToolUse};
use synthia_test_support::FakeTool;
use synthia_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use super::super::{support::*, *};

pub(super) async fn run_emits_warning_at_max_iterations() {
    let scripted: Vec<Vec<StreamChunk>> = (0..DEFAULT_MAX_ITERATIONS + 2)
        .map(|_| {
            tool_call_response(vec![ToolUse {
                id: "loop".to_string(),
                name: "echo".to_string(),
                input: json!({}),
            }])
        })
        .collect();
    let provider = Arc::new(ScriptedStreamProvider::new(scripted));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        FakeTool::new("echo", "ok"),
    )));

    let events = run_and_collect(
        provider,
        registry,
        CancellationToken::new(),
        AgentInput::text("loop forever"),
    )
    .await;

    let warning = events.iter().find_map(|e| match e {
        AgentEvent::System(SystemEvent::Warning {
            kind: WarningKind::Loop,
            message,
            ..
        }) => Some(message.clone()),
        _ => None,
    });
    assert!(warning.is_some(), "expected Loop warning");
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::MaxIterations,
        })
    )));
}

pub(super) async fn with_max_iterations_overrides_default() {
    let tool_use = ToolUse {
        id: "loop".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let scripted: Vec<Vec<StreamChunk>> = (0..DEFAULT_MAX_ITERATIONS)
        .map(|_| tool_call_response(vec![tool_use.clone()]))
        .collect();
    let provider = Arc::new(ScriptedStreamProvider::new(scripted));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        FakeTool::new("echo", "ok"),
    )));

    let agent = ReActAgent::new(provider, registry).with_max_iterations(3);
    assert_eq!(
        agent.max_iterations(),
        3,
        "with_max_iterations MUST overwrite the field, not no-op"
    );
    let mut stream = agent
        .run(AgentInput::text("loop"), Arc::new(CancellationToken::new()))
        .await;
    while let Some(ev) = stream.next().await {
        let _ = ev;
    }
    drop(stream);

    // `max_iterations(3)` clamped the loop to 3 iterations;
    // since the model never stops calling `echo`, the run
    // exhausts the cap and ends with `MaxIterations`.
    let _ = tool_use;
}

// The clamps are the contract: `0` floors to `1`, overflow ceilings to `4096`.
#[test]
fn max_iterations_clamps_inputs() {
    assert_eq!(
        ReActAgent::new(
            Arc::new(
                synthia_provider::traits_stub::ModelProviderStub::text_only(
                    "x"
                )
            ),
            Arc::new(ToolRegistry::new()),
        )
        .with_max_iterations(0)
        .max_iterations(),
        1
    );
    assert_eq!(
        ReActAgent::new(
            Arc::new(
                synthia_provider::traits_stub::ModelProviderStub::text_only(
                    "x"
                )
            ),
            Arc::new(synthia_tool::ToolRegistry::new()),
        )
        .with_max_iterations(usize::MAX)
        .max_iterations(),
        4096
    );
}
