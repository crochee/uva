use std::sync::Arc;

use elysper_provider::{SamplingResult, StreamChunk, ToolUse};
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::super::{support::*, *};

pub(super) async fn declared_output_schema_warns_when_never_submitted() {
    let provider =
        Arc::new(CapturingProvider::new(vec![vec![StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: "I will answer in prose instead".to_string(),
                ..Default::default()
            }),
        }]]));
    let (registry, _calls) = echo_registry();
    let schema = json!({
        "type": "object",
        "properties": { "answer": { "type": "string" } },
        "required": ["answer"]
    });
    let agent = ReActAgent::new(provider, registry).with_output_schema(schema);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    let warning = events.iter().find_map(|e| match e {
        AgentEvent::System(SystemEvent::Warning {
            kind: WarningKind::StructuredOutput,
            message,
            ..
        }) => Some(message.clone()),
        _ => None,
    });
    let message = warning.expect(
        "a declared schema that the run never submitted must warn; \
         events were: {events:#?}",
    );
    assert!(
        message.contains("structured_output"),
        "the warning must name what was missing; got: {message}"
    );
}

/// A run that never declared a schema stays silent—the warning is
/// about a declared-but-unsatisfied contract, not about prose answers
/// in general.
pub(super) async fn no_output_schema_means_no_warning() {
    let provider =
        Arc::new(CapturingProvider::new(vec![vec![StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: "plain prose answer".to_string(),
                ..Default::default()
            }),
        }]]));
    let (registry, _calls) = echo_registry();
    let agent = ReActAgent::new(provider, registry);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::System(SystemEvent::Warning {
                kind: WarningKind::StructuredOutput,
                ..
            })
        )),
        "no schema declared, so there is no contract to miss"
    );
}

/// A run that *does* submit a schema-valid answer stays silent.
///
/// The other half of [`declared_output_schema_warns_when_never_submitted`]:
/// the warning must key off the `structured_output` tool's own success
/// result, not merely off the schema being declared, or it would fire on
/// every well-behaved run.
pub(super) async fn satisfied_output_schema_warns_not() {
    // Pass 1: the model fills the schema. Pass 2: it stops.
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "so1".to_string(),
            name: elysper_tool::STRUCTURED_OUTPUT_TOOL_NAME.to_string(),
            input: json!({ "answer": "42" }),
        }]),
        empty_response(),
    ]));
    let (registry, _calls) = echo_registry();
    let schema = json!({
        "type": "object",
        "properties": { "answer": { "type": "string" } },
        "required": ["answer"]
    });
    let agent = ReActAgent::new(provider, registry).with_output_schema(schema);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::System(SystemEvent::Warning {
                kind: WarningKind::StructuredOutput,
                ..
            })
        )),
        "a satisfied schema must not warn; events were: {events:#?}"
    );
}
