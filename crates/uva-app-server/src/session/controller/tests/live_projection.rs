//! The live projection and the durable log must agree about a call.
//!
//! A tool call is answered twice: the harness publishes the answer
//! on the live event stream (the projection a `follow` client
//! renders until the durable row lands) and writes it as a durable
//! `tool_result` row (what the same client reads after a reload).
//! Both come from one commit, so both must say the same thing --
//! otherwise the live transcript shows a call that failed as still
//! running, and the reloaded one shows it failed.
//!
//! The scenario is a tool that fails: its error is the answer, and
//! the durable row carries the failure flag beside the payload.

use std::{sync::Arc, time::Duration};

use serde_json::json;
use uva::{
    harness::AgentEvent,
    provider::{ContentPart, Message, StreamChunk},
    test_support::FakeTool,
    tool::{ToolEntry, ToolRegistry},
};

use super::{
    super::{AgentRunStreamFactory, SessionOp},
    support::{ScriptedStreamProvider, make_manager_and_controller_with_deps},
};

fn tool_call_chunks(id: &str, name: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallStart {
            id: id.to_string(),
            name: name.to_string(),
            arguments: json!({}),
        },
        StreamChunk::ToolCallEnd { id: id.to_string() },
        StreamChunk::IsDone {
            result: Box::default(),
        },
    ]
}

/// A failed tool is answered on both paths, and both answers say
/// `failed`.
///
/// The live projection learns the answer from the harness's tool
/// result event; the durable row carries the same failure flag
/// beside the same payload. A client that renders the live
/// transcript and a client that reloads the log must not disagree
/// about the call.
#[tokio::test]
async fn a_failed_call_reads_failed_in_the_live_projection_and_in_the_log() {
    let provider =
        Arc::new(ScriptedStreamProvider::new(vec![tool_call_chunks(
            "c1", "boom",
        )]));
    let boom: Arc<dyn uva::tool::Tool> =
        Arc::new(FakeTool::failing("boom", "denied by policy"));
    let registry = {
        let registry = ToolRegistry::new();
        registry.register_entry(ToolEntry::new(Arc::clone(&boom)));
        registry
    };
    let mut deps = super::support::test_deps();
    deps.provider = provider;
    deps.tool_registry = Arc::new(registry);

    let (controller, manager, _temp) = make_manager_and_controller_with_deps(
        Duration::from_secs(60),
        Arc::new(AgentRunStreamFactory),
        deps,
    )
    .await;

    let mut events = controller.subscribe();
    controller
        .submit(SessionOp::Prompt {
            content: "go".to_string(),
            priority: 1,
        })
        .await
        .unwrap();

    // The harness flushes the durable row before it publishes the
    // answer, so the two paths can be compared once the answer
    // arrives.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let item = events.recv().await.expect("the broadcast is live");
            let crate::event_stream::BroadcastEvent::Run { event, .. } = item
            else {
                continue;
            };
            if matches!(
                event.as_ref(),
                AgentEvent::Model(ContentPart::ToolResult(_))
            ) {
                break;
            }
        }
    })
    .await
    .expect("the tool result reached the subscriber");

    let live = controller.live_snapshot();
    let call = live
        .tool_calls
        .iter()
        .find(|call| call.call_id == "c1")
        .expect("the live projection holds the call");
    assert_eq!(
        call.status, "failed",
        "the live projection must not read an answered call as running"
    );
    assert_eq!(call.output.as_deref(), Some("denied by policy"));

    let rows = manager.sink("alice", "s1").read().await.unwrap();
    let row = rows
        .iter()
        .find(|row| row["type"] == "tool_result")
        .expect("the durable row");
    assert_eq!(
        row["is_error"],
        json!(true),
        "the durable row says the call failed"
    );
    let message: Message = serde_json::from_value(row["data"].clone())
        .expect("the row carries the tool message");
    let durable_text = message
        .content
        .iter()
        .filter_map(|part| part.text())
        .collect::<String>();

    // The same call, read through the two paths: the live
    // projection and the durable row name the same outcome and
    // carry the same text.
    assert_eq!(
        call.status == "failed",
        row["is_error"] == json!(true),
        "the two paths must agree about the call's outcome"
    );
    assert_eq!(
        call.output.as_deref(),
        Some(durable_text.as_str()),
        "the two paths must carry the same text"
    );
}
