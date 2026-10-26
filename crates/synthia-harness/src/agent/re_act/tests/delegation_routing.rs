//! Delegation routing: a delegated tool name resolves to the delegator
//! that advertises it, exactly once, and the call carries the loop's
//! internals (`depth`, tool context, cancel, emitter).

use std::{path::PathBuf, sync::Arc};

use futures::StreamExt;
use serde_json::json;
use synthia_provider::{ContentPart, ToolUse};
use tokio_util::sync::CancellationToken;

use super::{support::*, *};
use crate::agent::re_act::delegation::tests::EchoInterceptor;

fn call(name: &str) -> ToolUse {
    ToolUse {
        id: format!("c_{name}"),
        name: name.to_string(),
        input: json!({"n": 1}),
    }
}

#[tokio::test]
async fn the_call_reaches_only_the_delegator_that_advertises_the_name() {
    let first_seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let second_seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![call("echo_two")]),
        done_chunk("done"),
    ]));
    let (registry, _executions) = echo_registry();
    let agent = ReActAgent::with_descriptor(
        provider,
        registry,
        PathBuf::from("/tmp/delegated"),
        crate::default_descriptor(
            crate::prompt::DEFAULT_SYSTEM_PROMPT.to_string(),
        ),
        Arc::new(PromptContext::default()),
    )
    .with_delegator(Arc::new(EchoInterceptor {
        seen: Some(Arc::clone(&first_seen)),
        ..EchoInterceptor::new("echo_one")
    }))
    .expect("distinct names")
    .with_delegator(Arc::new(EchoInterceptor {
        seen: Some(Arc::clone(&second_seen)),
        ..EchoInterceptor::new("echo_two")
    }))
    .expect("distinct names");

    let mut events = Vec::new();
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_eq!(
        first_seen.lock().clone(),
        Vec::<String>::new(),
        "the delegator that does not own the name must not run"
    );
    assert_eq!(
        second_seen.lock().clone(),
        vec!["echo_two:0:/tmp/delegated".to_string()],
        "the named delegator runs once with the run's depth and tool context"
    );
    let answered = events.iter().any(|event| {
        matches!(event, AgentEvent::Model(ContentPart::ToolResult(result))
            if result.tool_name.as_deref() == Some("echo_two")
                && result.content.iter().any(|part| matches!(part,
                    ContentPart::Text(text)
                        if text.text.starts_with("intercepted:echo_two"))))
    });
    assert!(answered, "the delegated call must commit its result");
}

fn seen_len(seen: &Arc<parking_lot::Mutex<Vec<String>>>) -> usize {
    seen.lock().len()
}

fn advertised_names(
    captured: &[Arc<Vec<synthia_provider::ToolDefinition>>],
    name: &str,
) -> usize {
    captured
        .first()
        .map(|tools| {
            tools
                .iter()
                .filter(|definition| definition.name == name)
                .count()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_host_registered_delegator_is_dispatched() {
    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![call("hosted")]),
        done_chunk("done"),
    ]));
    let (registry, _executions) = echo_registry();
    let mut host = crate::agent::host::Host::new();
    let _registration = host.delegator(Arc::new(EchoInterceptor {
        seen: Some(Arc::clone(&seen)),
        ..EchoInterceptor::new("hosted")
    }));
    let agent = ReActAgent::new(provider.clone(), registry).with_host(host);

    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    assert_eq!(seen_len(&seen), 1, "the host's delegator ran once");
    let captured = provider.captured_tools.lock().await;
    assert_eq!(
        advertised_names(&captured, "hosted"),
        1,
        "the host's delegator rides the tool list once"
    );
}

#[tokio::test]
async fn a_withdrawn_host_delegator_is_no_longer_dispatched() {
    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![call("withdrawn")]),
        done_chunk("done"),
    ]));
    let (registry, _executions) = echo_registry();
    let mut host = crate::agent::host::Host::new();
    let registration = host.delegator(Arc::new(EchoInterceptor {
        seen: Some(Arc::clone(&seen)),
        ..EchoInterceptor::new("withdrawn")
    }));
    let agent = ReActAgent::new(provider, registry).with_host(host);
    drop(registration);

    let mut events = Vec::new();
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_eq!(seen_len(&seen), 0, "the withdrawn delegator must not run");
    let failed = events.iter().any(|event| {
        matches!(event, AgentEvent::Model(ContentPart::ToolResult(result))
            if result.tool_name.as_deref() == Some("withdrawn")
                && result.is_error == Some(true))
    });
    assert!(
        failed,
        "the name is nobody's now, so the call fails like an unknown tool"
    );
}

#[tokio::test]
async fn one_delegator_installed_on_the_agent_and_its_host_runs_once() {
    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let delegator: Arc<dyn crate::agent::DelegatingTool> =
        Arc::new(EchoInterceptor {
            seen: Some(Arc::clone(&seen)),
            ..EchoInterceptor::new("twice_installed")
        });
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![call("twice_installed")]),
        done_chunk("done"),
    ]));
    let (registry, _executions) = echo_registry();
    let mut host = crate::agent::host::Host::new();
    let _registration = host.delegator(Arc::clone(&delegator));
    let agent = ReActAgent::new(provider.clone(), registry)
        .with_host(host)
        .with_delegator(Arc::clone(&delegator))
        .expect("the agent's own install accepts the name");

    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    assert_eq!(
        seen_len(&seen),
        1,
        "the same delegator must not be installed twice"
    );
    let captured = provider.captured_tools.lock().await;
    assert_eq!(
        advertised_names(&captured, "twice_installed"),
        1,
        "the definition must be advertised once"
    );
}
