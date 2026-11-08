//! Parallel / sequential tool execution + descriptor-pinning
//! regression tests.

use std::sync::{Arc, atomic::Ordering};

use async_trait::async_trait;
use elysper_tool::{Context, ToolOutput, ToolRegistry};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{support::*, *};

#[tokio::test]
async fn unsafe_tools_run_serially_even_when_safe_present() {
    UNSAFE_RUNNING.store(0, Ordering::SeqCst);
    MAX_CONCURRENT_UNSAFE.store(0, Ordering::SeqCst);

    let tool_use_safe = ToolUse {
        id: "s".into(),
        name: "safe".into(),
        input: json!({}),
    };
    let tool_use_unsafe = ToolUse {
        id: "u".into(),
        name: "unsafe".into(),
        input: json!({}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![tool_use_safe, tool_use_unsafe]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(elysper_tool::ToolEntry::new(Arc::new(
        CountingSafeTool,
    )));
    registry.register_entry(elysper_tool::ToolEntry::new(Arc::new(
        CountingUnsafeTool,
    )));
    let _events = run_and_collect(
        provider,
        registry,
        CancellationToken::new(),
        AgentInput::text("m"),
    )
    .await;
    assert_eq!(
        MAX_CONCURRENT_UNSAFE.load(Ordering::SeqCst),
        1,
        "unsafe tool must never overlap with itself"
    );
}

#[tokio::test]
async fn sequential_round_aborts_after_first_error_and_skips_remaining() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Counter proves later calls never run.
    static LATER_CALLS: AtomicUsize = AtomicUsize::new(0);
    static EARLY_CALLS: AtomicUsize = AtomicUsize::new(0);

    struct EarlyErrorTool;
    #[async_trait]
    impl elysper_tool::Tool for EarlyErrorTool {
        fn name(&self) -> &str {
            "early"
        }

        fn description(&self) -> &str {
            "early"
        }

        fn parameters(&self) -> serde_json::Value {
            json!({"type":"object"})
        }

        fn mode(&self) -> elysper_tool::traits::ExecutionMode {
            elysper_tool::traits::ExecutionMode::Sequential
        }

        async fn call(&self, _: serde_json::Value, _: &Context) -> ToolOutput {
            EARLY_CALLS.fetch_add(1, Ordering::SeqCst);
            // Use the public `error` helper so we get
            // the correct `metadata` / `truncated_by`
            // defaults; we then override `is_error =
            // Some(true)` on the returned value.
            let mut out = ToolOutput::error("boom");
            out.is_error = Some(true);
            out
        }
    }

    struct LaterTool;
    #[async_trait]
    impl elysper_tool::Tool for LaterTool {
        fn name(&self) -> &str {
            "later"
        }

        fn description(&self) -> &str {
            "later"
        }

        fn parameters(&self) -> serde_json::Value {
            json!({"type":"object"})
        }

        fn mode(&self) -> elysper_tool::traits::ExecutionMode {
            elysper_tool::traits::ExecutionMode::Sequential
        }

        async fn call(&self, _: serde_json::Value, _: &Context) -> ToolOutput {
            LATER_CALLS.fetch_add(1, Ordering::SeqCst);
            ToolOutput::text("later-ok")
        }
    }

    let early = ToolUse {
        id: "e".into(),
        name: "early".into(),
        input: json!({}),
    };
    let later = ToolUse {
        id: "l".into(),
        name: "later".into(),
        input: json!({}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![early, later]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry
        .register_entry(elysper_tool::ToolEntry::new(Arc::new(EarlyErrorTool)));
    registry.register_entry(elysper_tool::ToolEntry::new(Arc::new(LaterTool)));

    let events = run_and_collect(
        provider,
        registry,
        CancellationToken::new(),
        AgentInput::text("m"),
    )
    .await;

    // The "early" tool fired exactly once (abort happened
    // on its first error).
    assert_eq!(
        EARLY_CALLS.load(Ordering::SeqCst),
        1,
        "early tool must be invoked exactly once"
    );
    // The "later" tool NEVER fired (round aborted).
    assert_eq!(
        LATER_CALLS.load(Ordering::SeqCst),
        0,
        "later tool must NOT be invoked after early error"
    );

    // Wire side: both tool_use_ids must surface as
    // ToolResults so the LLM's history stays well-formed.
    let tool_results: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => Some(tr),
            _ => None,
        })
        .collect();
    let ids: Vec<&str> = tool_results
        .iter()
        .map(|tr| tr.tool_use_id.as_str())
        .collect();
    assert!(
        ids.contains(&"e") && ids.contains(&"l"),
        "both tool_use_ids must appear on the wire; got {ids:?}"
    );

    // The "later" tool's synthetic result must name why it never
    // ran — a skipped-after-sibling-error call has an UNKNOWN
    // outcome (the model must verify external state before
    // retrying), which is exactly what the message says.
    let later_result = tool_results
        .iter()
        .find(|tr| tr.tool_use_id == "l")
        .expect("'l' result must exist");
    let later_text = later_result
        .content
        .iter()
        .filter_map(|c| match c {
            ContentPart::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        later_text.contains("skipped after sibling sequential tool error"),
        "the skip must name its cause; got {later_text:?}"
    );
    assert!(
        later_text.contains("verify external state before retrying"),
        "an unknown-outcome skip must say so; got {later_text:?}"
    );
    assert!(
        !later_text.contains("cancelled"),
        "a sibling-error skip is not a cancellation; got {later_text:?}"
    );
    assert!(
        later_result.is_error.unwrap_or(false),
        "synthetic result must be flagged as error so LLM sees it as failure"
    );
}
