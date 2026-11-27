//! The durable row says what the call changed.
//!
//! A tool that changes a file reports the change in its output's
//! metadata (the same report the protocol's diff block and the live
//! frame carry). The **live** diff is gone by the time a client reads
//! the log, so the row keeps the paths: the log answers "what did
//! this call touch" on its own.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uva_core::SystemClock;
use uva_session::{LogWriter, SessionSink, in_memory::InMemorySessionSink};
use uva_tool::{Context, Tool, ToolOutput};

use super::{support::*, *};

/// A tool that changes a file: it reports the change the way the
/// file tools do.
struct EditingTool;

#[async_trait]
impl Tool for EditingTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "changes a file"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        ToolOutput::text("Replaced the text on line 2 of '/srv/app.rs'.")
            .with_metadata(
                "diff",
                json!({
                    "path": "/srv/app.rs",
                    "old_text": "old",
                    "new_text": "new",
                }),
            )
    }
}

/// The tool result row carries the files the call changed.
#[tokio::test]
async fn the_tool_result_row_carries_the_changed_paths() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "c1".to_string(),
            name: "edit".to_string(),
            input: json!({"file_path": "app.rs"}),
        }]),
        done_chunk("done"),
    ]));
    let registry = Arc::new(uva_tool::ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(EditingTool)));
    let agent = ReActAgent::new(provider, registry).with_log_writer(writer);

    let mut stream = agent
        .run(
            AgentInput::text("edit the file"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    let row = rows
        .iter()
        .find(|row| row["type"] == "tool_result")
        .expect("the tool result is committed");
    assert_eq!(
        row["changed"],
        json!(["/srv/app.rs"]),
        "the row says what the call changed: {row}"
    );
}

/// A call that changed nothing says nothing: the member is absent,
/// not an empty list.
#[tokio::test]
async fn a_call_that_changed_nothing_says_nothing() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "c1".to_string(),
            name: "echo".to_string(),
            input: json!({}),
        }]),
        done_chunk("done"),
    ]));
    let (registry, _executions) = echo_registry();
    let agent = ReActAgent::new(provider, registry).with_log_writer(writer);

    let mut stream = agent
        .run(AgentInput::text("echo"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    let row = rows
        .iter()
        .find(|row| row["type"] == "tool_result")
        .expect("the tool result is committed");
    assert!(
        row.get("changed").is_none(),
        "the row says nothing about changes it made: {row}"
    );
}
