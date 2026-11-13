//! Per-agent tool restriction: `denied_tools`, `allow_list`,
//! and the delegation seam. Drives one loop per scenario and
//! asserts the advertised tool list + the post-call result.

use std::sync::Arc;

use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uva_provider::{
    SamplingResult,
    StreamChunk,
    TokenUsage,
    ToolDefinition,
    traits::ModelProvider,
};
use uva_test_support::FakeTool;
use uva_tool::ToolRegistry;

use super::{support::*, *};

async fn run_and_collect_with_restriction(
    provider: Arc<dyn ModelProvider>,
    registry: Arc<ToolRegistry>,
    restriction: uva_tool::ToolRestriction,
    input: AgentInput,
) -> Vec<AgentEvent> {
    let agent =
        ReActAgent::new(provider, registry).with_tool_restriction(restriction);
    let mut stream = agent.run(input, Arc::new(CancellationToken::new())).await;
    let mut out = Vec::new();
    while let Some(ev) = stream.next().await {
        out.push(ev);
    }
    out
}

fn restricted_test_registry() -> Arc<ToolRegistry> {
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        "echo", "echoed",
    ))));
    registry.register_entry(uva_tool::ToolEntry::dynamic(
        "read".to_string(),
        "read a file".to_string(),
        json!({"type": "object"}),
    ));
    registry
}

#[tokio::test]
async fn denied_tool_is_hidden_and_its_call_is_refused() {
    let ignored = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ignored]),
        empty_response(),
    ]));

    let events = run_and_collect_with_restriction(
        provider.clone(),
        restricted_test_registry(),
        uva_tool::ToolRestriction::deny(["echo"]),
        AgentInput::text("use echo"),
    )
    .await;

    // Neither request advertises the denied tool; `read` still is.
    let captured = provider.captured_tools.lock().await;
    assert!(!captured.is_empty(), "the run made at least one request");
    for request_tools in captured.iter() {
        let names: Vec<&str> =
            request_tools.iter().map(|d| d.name.as_str()).collect();
        assert!(
            !names.contains(&"echo"),
            "a denied tool must not be advertised: {names:?}"
        );
        assert!(
            names.contains(&"read"),
            "the rest of the catalog stays advertised: {names:?}"
        );
    }
    drop(captured);

    // The call the model made anyway comes back as an error naming the
    // restriction.
    let results: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr))
                if tr.tool_name.as_deref() == Some("echo") =>
            {
                Some(tr.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1, "the call must still get a result");
    assert!(results[0].is_error.unwrap_or(false));
    let text = match &results[0].content[0] {
        ContentPart::Text(t) => t.text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    assert!(
        text.contains("[denied by tool restriction]"),
        "the refusal must name the restriction: {text}"
    );

    // A policy denial is a `Guard` warning, and the session completes.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::System(SystemEvent::Warning {
                kind: WarningKind::Guard,
                ..
            })
        )),
        "the refusal must be visible as a Guard warning"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        }))
    ));
}

#[tokio::test]
async fn allow_list_narrows_the_advertised_set() {
    let provider = Arc::new(CapturingProvider::new(vec![empty_response()]));

    let _events = run_and_collect_with_restriction(
        provider.clone(),
        restricted_test_registry(),
        uva_tool::ToolRestriction::allow(["read"]),
        AgentInput::text("go"),
    )
    .await;

    let captured = provider.captured_tools.lock().await;
    let names: Vec<&str> =
        captured[0].iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["read"],
        "an allow-list admits exactly its members"
    );
}

#[tokio::test]
async fn restriction_without_the_delegator_hides_it() {
    use crate::agent::re_act::delegation::tests::EchoInterceptor;

    let provider = Arc::new(CapturingProvider::new(vec![empty_response()]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::dynamic(
        "read".to_string(),
        "read a file".to_string(),
        json!({"type": "object"}),
    ));

    let agent = ReActAgent::new(provider.clone(), registry)
        .with_delegator(Arc::new(EchoInterceptor::new("intercepted_tool")))
        .expect("first install must succeed")
        .with_tool_restriction(uva_tool::ToolRestriction::allow(["read"]));
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let captured = provider.captured_tools.lock().await;
    let names: Vec<&str> =
        captured[0].iter().map(|d| d.name.as_str()).collect();
    assert!(
        !names.contains(&"intercepted_tool"),
        "the delegator is not in the allow-list, so its tool must not be \
         advertised: {names:?}"
    );
    assert!(
        names.contains(&"read"),
        "the allow-listed registry tool stays advertised: {names:?}"
    );
}

#[tokio::test]
async fn delegator_definition_advertised_and_call_routed() {
    use crate::agent::re_act::delegation::tests::EchoInterceptor;

    let provider = Arc::new(CapturingProvider::new(vec![
        vec![StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: String::new(),
                tool_calls: vec![ToolUse {
                    id: "c_ix".into(),
                    name: "intercepted_tool".into(),
                    input: json!({"n": 1}),
                }],
                reasoning: String::new(),
                reasoning_signature: None,
                usage: TokenUsage::default(),
                ..Default::default()
            }),
        }],
        vec![StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: "done".into(),
                tool_calls: vec![],
                reasoning: String::new(),
                reasoning_signature: None,
                usage: TokenUsage::default(),
                ..Default::default()
            }),
        }],
    ]));
    let registry = echo_registry().0;

    let agent = ReActAgent::new(provider.clone(), registry)
        .with_delegator(Arc::new(EchoInterceptor::new("intercepted_tool")))
        .expect("first install must succeed");
    let mut events = Vec::new();
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }
    drop(stream);

    let captured = provider.captured_tools.lock().await;
    let names: Vec<&str> =
        captured[0].iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"intercepted_tool"),
        "delegator definition rides the tool list: {names:?}"
    );
    drop(captured);

    let intercepted_result = events.iter().any(|e| {
        matches!(e, AgentEvent::Model(ContentPart::ToolResult(tr))
        if tr.tool_name.as_deref() == Some("intercepted_tool")
            && tr.content.iter().any(|p| {
                matches!(p, ContentPart::Text(t)
                    if t.text.contains("intercepted:intercepted_tool"))
            }))
    });
    assert!(
        intercepted_result,
        "the advertised call must be answered by the delegator: {events:?}"
    );
}

#[test]
fn with_delegator_rejects_duplicate_name() {
    use crate::agent::re_act::delegation::tests::EchoInterceptor;

    let agent = ReActAgent::new(
        Arc::new(CapturingProvider::new(vec![])),
        echo_registry().0,
    )
    .with_delegator(Arc::new(EchoInterceptor::new("shared_tool")))
    .expect("first install must succeed");

    let result =
        agent.with_delegator(Arc::new(EchoInterceptor::new("shared_tool")));
    let err = match result {
        Ok(_) => panic!("duplicate delegator name must surface as Err"),
        Err(name) => name,
    };
    assert_eq!(err, "shared_tool");
}

#[tokio::test]
async fn the_agent_offers_the_tools_of_the_registry_it_was_built_with() {
    let provider =
        Arc::new(CapturingProvider::new(vec![done_chunk("all done")]));
    let other = Arc::new(ToolRegistry::new());
    other.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        "echo", "echoed",
    ))));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        "write", "wrote",
    ))));

    let agent = ReActAgent::new(provider.clone(), registry);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let names: Vec<String> = provider
        .captured_tools
        .lock()
        .await
        .first()
        .map(|defs: &Arc<Vec<ToolDefinition>>| {
            defs.iter().map(|d| d.name.clone()).collect()
        })
        .unwrap_or_default();
    assert!(
        names.iter().any(|n| n == "write"),
        "the agent's own registry's tools must reach the model; got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "echo"),
        "another registry's tools must not reach the model; got {names:?}"
    );
}
