//! The Prometheus tool counters the loop's dispatch feeds.
//!
//! The loop drains a tool's stream itself, so `uva-tool`'s registry
//! dispatch — the other writer of these counters — never sees the
//! calls a run makes. Without the loop's own recording, a running
//! server would expose no tool metrics at all, and `/metrics` would
//! answer with an empty exposition.
//!
//! The assertions read the **exposition** (`gather_text`), not the
//! counter objects: what a Prometheus scrape sees is the thing under
//! test.

use std::sync::Arc;

use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uva_provider::{ToolUse, traits::ModelProvider};
use uva_test_support::FakeTool;
use uva_tool::ToolRegistry;

use super::{support::*, *};

// The tool's name in the counters. A name of its own, so the
// process-wide registry cannot carry another test's count into this
// one.
const PROBE_TOOL: &str = "metrics_probe";

// One labeled counter's value, read from the exposition.
fn counter_value(text: &str, series: &str) -> Option<f64> {
    text.lines()
        .find(|line| line.starts_with(series))
        .and_then(|line| line.rsplit(' ').next())
        .and_then(|value| value.parse::<f64>().ok())
}

// A registry holding the probe tool.
fn probe_registry() -> Arc<ToolRegistry> {
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        PROBE_TOOL, "probed",
    ))));
    registry
}

// Run one turn that calls the probe tool once and then answers.
async fn run_one_tool_call(
    provider: Arc<dyn ModelProvider>,
    registry: Arc<ToolRegistry>,
) -> Vec<AgentEvent> {
    let agent = ReActAgent::new(provider, registry);
    let mut stream = agent
        .run(
            AgentInput::text("call the probe tool"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn a_tool_call_the_loop_serves_is_counted_in_the_exposition() {
    let before = String::from_utf8(uva_telemetry::gather_text())
        .expect("the exposition is UTF-8");
    let ok_before = counter_value(
        &before,
        &format!(
            "tool_executions_total{{outcome=\"ok\",tool=\"{PROBE_TOOL}\"}}"
        ),
    )
    .unwrap_or(0.0);

    let call = ToolUse {
        id: "c1".to_string(),
        name: PROBE_TOOL.to_string(),
        input: json!({}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![call]),
        empty_response(),
    ]));

    run_one_tool_call(provider, probe_registry()).await;

    let after = String::from_utf8(uva_telemetry::gather_text())
        .expect("the exposition is UTF-8");
    let ok_after = counter_value(
        &after,
        &format!(
            "tool_executions_total{{outcome=\"ok\",tool=\"{PROBE_TOOL}\"}}"
        ),
    )
    .unwrap_or(0.0);

    assert_eq!(
        ok_after,
        ok_before + 1.0,
        "the loop's own dispatch counted the call it served:\n{after}"
    );
    assert!(
        after.contains(&format!(
            "tool_execution_duration_seconds_count{{tool=\"{PROBE_TOOL}\"}}"
        )),
        "the call's duration was observed:\n{after}"
    );
}
