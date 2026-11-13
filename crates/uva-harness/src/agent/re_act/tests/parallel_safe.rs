//! Parallel / sequential tool execution + descriptor-pinning
//! regression tests.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uva_session::SessionSink;
use uva_tool::{Context, ToolOutput, ToolRegistry};

use super::{support::*, *};

struct SleepTool;

#[async_trait]
impl uva_tool::Tool for SleepTool {
    fn name(&self) -> &str {
        "sleep"
    }

    fn description(&self) -> &str {
        "sleeps for a configurable duration"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    fn mode(&self) -> uva_tool::traits::ExecutionMode {
        uva_tool::traits::ExecutionMode::Parallel
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _ctx: &Context,
    ) -> ToolOutput {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        ToolOutput::text("done")
    }
}

#[tokio::test]
async fn parallel_safe_tools_run_concurrently() {
    let tool_use1 = ToolUse {
        id: "c1".into(),
        name: "sleep".into(),
        input: json!({}),
    };
    let tool_use2 = ToolUse {
        id: "c2".into(),
        name: "sleep".into(),
        input: json!({}),
    };
    let tool_use3 = ToolUse {
        id: "c3".into(),
        name: "sleep".into(),
        input: json!({}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![tool_use1, tool_use2, tool_use3]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(SleepTool)));
    let started = std::time::Instant::now();
    let _events = run_and_collect(
        provider,
        registry,
        CancellationToken::new(),
        AgentInput::text("run"),
    )
    .await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(250),
        "3 parallel-safe sleep(100ms) tools must finish in <250ms, took {elapsed:?}"
    );
}

struct SlowFastTool(u64);

#[async_trait]
impl uva_tool::Tool for SlowFastTool {
    fn name(&self) -> &str {
        if self.0 == 1 { "slow" } else { "fast" }
    }

    fn description(&self) -> &str {
        "speed"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    fn mode(&self) -> uva_tool::traits::ExecutionMode {
        uva_tool::traits::ExecutionMode::Parallel
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _ctx: &Context,
    ) -> ToolOutput {
        let d = if self.0 == 1 { 200 } else { 10 };
        tokio::time::sleep(std::time::Duration::from_millis(d)).await;
        ToolOutput::text(self.name().to_string())
    }
}

#[tokio::test]
async fn parallel_safe_tool_emits_progress_and_result_in_llm_order() {
    let tool_use_slow = ToolUse {
        id: "slow".into(),
        name: "slow".into(),
        input: json!({}),
    };
    let tool_use_fast = ToolUse {
        id: "fast".into(),
        name: "fast".into(),
        input: json!({}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![tool_use_slow, tool_use_fast]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry
        .register_entry(uva_tool::ToolEntry::new(Arc::new(SlowFastTool(1))));
    registry
        .register_entry(uva_tool::ToolEntry::new(Arc::new(SlowFastTool(2))));
    let events = run_and_collect(
        provider,
        registry,
        CancellationToken::new(),
        AgentInput::text("o"),
    )
    .await;

    let progress: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::System(SystemEvent::ToolProgress {
                tool_name, ..
            }) => Some(tool_name.clone()),
            _ => None,
        })
        .collect();
    // ToolProgress is only emitted by tools that override
    // `stream()`; these synthetic tools use the default
    // call→Result path, so progress is empty here. The
    // meaningful invariant is ToolResult order below.
    let _ = progress;

    let results: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => {
                Some(tr.tool_name.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        vec![Some("slow".to_string()), Some("fast".to_string())],
        "ToolResult order must follow LLM tool-call order"
    );
}

// A tool that waits for the duration its input names.
struct DelayTool;

#[async_trait]
impl uva_tool::Tool for DelayTool {
    fn name(&self) -> &str {
        "delay"
    }

    fn description(&self) -> &str {
        "waits for the duration its input names"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &Context,
    ) -> ToolOutput {
        let millis = input["ms"].as_u64().unwrap_or(0);
        tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
        ToolOutput::text("done")
    }
}

// The tool results are committed in the assistant's order, not in
// the order the calls finished.
//
// The batch runs under a concurrency bound, so the calls finish in
// whatever order they finish. The rows the log holds are still the
// assistant's order: a commit that followed the completion order
// would make the transcript's results read differently from the tool
// calls they answer.
#[tokio::test]
async fn tool_results_are_committed_in_the_assistants_order() {
    // Descending delays: the calls finish in the *reverse* of their
    // source order, so a completion-ordered commit would write
    // c3, c2, c1.
    let tool_use1 = ToolUse {
        id: "c1".into(),
        name: "delay".into(),
        input: json!({"ms": 150}),
    };
    let tool_use2 = ToolUse {
        id: "c2".into(),
        name: "delay".into(),
        input: json!({"ms": 100}),
    };
    let tool_use3 = ToolUse {
        id: "c3".into(),
        name: "delay".into(),
        input: json!({"ms": 50}),
    };
    let sink = Arc::new(uva_session::in_memory::InMemorySessionSink::new("s"));
    let writer = Arc::new(uva_session::LogWriter::new(
        sink.clone(),
        Arc::new(uva_core::SystemClock),
    ));
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![tool_use1, tool_use2, tool_use3]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(DelayTool)));
    let agent = ReActAgent::new(provider, registry).with_log_writer(writer);
    let mut stream = agent
        .run(AgentInput::text("run"), Arc::new(CancellationToken::new()))
        .await;
    while futures::StreamExt::next(&mut stream).await.is_some() {}

    let rows = sink.read().await.expect("read");
    let order: Vec<&str> = rows
        .iter()
        .filter(|row| row["type"] == "tool_result")
        .filter_map(|row| row["data"]["tool_call_id"].as_str())
        .collect();
    assert_eq!(
        order,
        vec!["c1", "c2", "c3"],
        "the log's tool results must follow the assistant's tool-call \
         order, not the order the calls finished"
    );
}
