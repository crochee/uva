//! Regression for the synthetic dispatch hang: a plugin that leaks
//! its `emit` sender into storage that outlives `execute` keeps the
//! channel open forever, so the dispatch must race the executor
//! against the channel (never `join` them). When `execute` resolves,
//! buffered events are drained without blocking and the output
//! returns.
//!
//! The fixture parks the sender in a process-lifetime `OnceLock` (a
//! leaked `Box` / `mem::forget` would keep it alive the same way, but
//! both are banned by repo lint rules), emits one event, then
//! returns a success output.

use std::sync::{Arc, OnceLock};

use futures::{StreamExt, future::BoxFuture};
use tokio_util::sync::CancellationToken;

use super::{support::*, *};
use crate::{
    agent::re_act::synthetic::{SyntheticCall, SyntheticTool},
    events::AgentEvent,
};

static LEAKED_EMIT: OnceLock<
    futures::channel::mpsc::UnboundedSender<AgentEvent>,
> = OnceLock::new();

struct LeakyEmitter;

impl SyntheticTool for LeakyEmitter {
    fn definitions(&self) -> Vec<synthia_provider::ToolDefinition> {
        vec![synthia_provider::ToolDefinition {
            name: "leaky_emit".to_string(),
            description: "leaks its emit sender".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            cache_control: None,
            annotations: None,
        }]
    }

    fn claims(&self, name: &str) -> bool {
        name == "leaky_emit"
    }

    fn execute(
        &self,
        call: SyntheticCall<'_>,
    ) -> BoxFuture<'static, synthia_tool::ToolOutput> {
        // Intentional retain: the sender is never dropped, so the
        // channel's receiver side can never observe a close.
        let _already = LEAKED_EMIT.set(call.emit.clone());
        let emit = call.emit;
        Box::pin(async move {
            let _ = emit.unbounded_send(AgentEvent::text_delta(
                "emitted before execute finished",
            ));
            synthia_tool::ToolOutput::text("synthetic done")
        })
    }
}

#[tokio::test]
async fn leaked_emit_sender_does_not_hang_the_dispatch() {
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![synthia_provider::ToolUse {
            id: "c_leak".to_string(),
            name: "leaky_emit".to_string(),
            input: serde_json::json!({}),
        }]),
        empty_response(),
    ]));
    let (registry, _echo_count) = echo_registry();
    let agent = ReActAgent::new(provider, registry)
        .with_synthetic_tool(Arc::new(LeakyEmitter))
        .expect("first install must succeed");
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    // Under the old `join` semantics this drain never completed: the
    // dispatch waited for the emit channel to close, and the leaked
    // sender guaranteed it never would.
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    let answered = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::Model(synthia_provider::ContentPart::ToolResult(tr))
                if tr.tool_name.as_deref() == Some("leaky_emit")
                    && tr.content.iter().any(|p| {
                        matches!(p,
                            synthia_provider::ContentPart::Text(t)
                                if t.text.contains("synthetic done"))
                    })
        )
    });
    assert!(
        answered,
        "the dispatch must complete and return the output: {events:?}"
    );

    let forwarded = events.iter().any(|e| {
        matches!(e, AgentEvent::Model(part) if part
            .text()
            .is_some_and(|t| t.contains("emitted before execute finished")))
    });
    assert!(
        forwarded,
        "the buffered emit event must reach the run's stream: {events:?}"
    );
}
