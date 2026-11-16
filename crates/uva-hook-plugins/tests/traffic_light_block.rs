//! A traffic-light veto is a durable `hook_block` row.
//!
//! The gate is registered on the contract's external registry (the
//! same path a third-party plugin takes), the loop asks it before the
//! tool runs, and the vetoed call never executes.

#![cfg(feature = "traffic-light")]

use std::sync::Arc;

use futures::StreamExt;
use serde_json::Value;
use uva_core::{AtomicCancelToken, SystemClock};
use uva_harness::{Agent, AgentInput, ReActAgent};
use uva_hook::{HookRegistry, seam};
use uva_hook_plugins::traffic_light::TrafficLight;
use uva_provider::{
    ContentPart,
    ModelProvider,
    SamplingResult,
    StreamChunk,
    TextContent,
};
use uva_session::{LogWriter, SessionSink, in_memory::InMemorySessionSink};
use uva_test_support::ReplayProvider;
use uva_tool::{Context, Risk, Tool, ToolEntry, ToolOutput, ToolRegistry};

struct DangerTool {
    executions: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for DangerTool {
    fn name(&self) -> &str {
        "danger"
    }

    fn description(&self) -> &str {
        "a high-risk test tool"
    }

    fn parameters(&self) -> Value {
        serde_json::json!({ "type": "object" })
    }

    fn risk(&self, _input: &Value) -> Risk {
        Risk::High
    }

    async fn call(&self, _input: Value, _context: &Context) -> ToolOutput {
        self.executions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ToolOutput::text("ran")
    }
}

fn tool_call_script(name: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallStart {
            id: "c1".to_string(),
            name: name.to_string(),
            arguments: serde_json::Value::String("{}".to_string()),
        },
        StreamChunk::ToolCallEnd {
            id: "c1".to_string(),
        },
        StreamChunk::IsDone {
            result: Box::new(SamplingResult::default()),
        },
    ]
}

fn final_script(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: text.to_string(),
            cache_control: None,
        })),
        StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: text.to_string(),
                ..SamplingResult::default()
            }),
        },
    ]
}

#[tokio::test]
async fn a_high_risk_call_is_blocked_and_logged() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));

    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tools = Arc::new(ToolRegistry::new());
    tools.register_entry(ToolEntry::new(Arc::new(DangerTool {
        executions: executions.clone(),
    })));

    let registry = HookRegistry::new();
    let _registration = registry
        .register_gate(
            seam::TOOL_PRE,
            Arc::new(TrafficLight::new(tools.clone())),
        )
        .expect("the gate registers");

    let provider = Arc::new(ReplayProvider::new(vec![
        tool_call_script("danger"),
        final_script("noted"),
    ])) as Arc<dyn ModelProvider>;
    let agent = ReActAgent::new(provider, tools)
        .with_hook_registry(registry)
        .with_log_writer(writer);

    let mut stream = agent
        .run(AgentInput::text("run danger"), AtomicCancelToken::shared())
        .await;
    while stream.next().await.is_some() {}

    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the vetoed call never runs"
    );

    let rows = sink.read().await.expect("read the log");
    let blocks: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["type"] == "hook_block")
        .collect();
    assert_eq!(
        blocks.len(),
        1,
        "the veto writes exactly one row: {rows:#?}"
    );
    assert_eq!(blocks[0]["data"]["hook"], "traffic-light");
    assert_eq!(blocks[0]["data"]["tool"], "danger");
    assert_eq!(blocks[0]["data"]["call_id"], "c1");
    assert_eq!(
        blocks[0]["data"]["reason"],
        "traffic-light: `danger` is high risk"
    );
}
