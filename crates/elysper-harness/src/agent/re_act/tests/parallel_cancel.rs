//! Parallel / sequential tool execution + descriptor-pinning
//! regression tests.

use std::sync::Arc;

use elysper_tool::ToolRegistry;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{support::*, *};

#[tokio::test]
async fn parallel_call_respects_cancellation() {
    // Cancel must prevent the second LLM iteration from
    // starting; the first iteration's tool calls complete
    // (long ones at most ~800ms each, but they run in
    // parallel) and then the loop notices the cancelled
    // token before issuing the next sampling pass. Bound:
    // 800ms (parallel long tools) + 200ms slack + next-iter
    // detection = well under 3s.
    let tool_use_a = ToolUse {
        id: "a".into(),
        name: "long".into(),
        input: json!({}),
    };
    let tool_use_b = ToolUse {
        id: "b".into(),
        name: "long".into(),
        input: json!({}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![tool_use_a, tool_use_b]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(elysper_tool::ToolEntry::new(Arc::new(LongTool)));
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        cancel_clone.cancel();
    });
    let started = std::time::Instant::now();
    let _events =
        run_and_collect(provider, registry, cancel, AgentInput::text("c"))
            .await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "session must abort promptly on cancel even with long parallel tools"
    );
}
