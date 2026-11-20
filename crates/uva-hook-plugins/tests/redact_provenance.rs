//! Redaction reaches the log through the loop's provenance row.
//!
//! This drives a real run: the tool returns a credential, the
//! `redact` transformer rewrites the result before the loop commits
//! it, and the loop writes one `tool_result_transformed` row naming
//! the transformer with two different digests.

#![cfg(feature = "redact")]

use std::sync::Arc;

use futures::StreamExt;
use uva_core::{AtomicCancelToken, SystemClock};
use uva_harness::{Agent, AgentInput, Host, ReActAgent};
use uva_hook::{HookRegistry, MountPoint, RegistrationOrigin};
use uva_hook_plugins::redact::RedactTransformer;
use uva_provider::{
    ContentPart,
    ModelProvider,
    SamplingResult,
    StreamChunk,
    TextContent,
};
use uva_session::{
    LogWriter,
    SessionEvent,
    SessionSink,
    in_memory::InMemorySessionSink,
};
use uva_test_support::{FakeTool, ReplayProvider};
use uva_tool::{ToolEntry, ToolRegistry};

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
async fn a_redacted_result_leaves_one_provenance_row() {
    const SECRET: &str = "sk-abcdefgh12345678";
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));

    let tools = Arc::new(ToolRegistry::new());
    tools.register_entry(ToolEntry::new(Arc::new(FakeTool::new(
        "echo",
        &format!("the credential is {SECRET}"),
    ))));

    let provider = Arc::new(ReplayProvider::new(vec![
        tool_call_script("echo"),
        final_script("done"),
    ])) as Arc<dyn ModelProvider>;
    let registry = HookRegistry::new();
    let _registration = registry
        .register_result_transformer_at(
            MountPoint::ToolPost,
            Arc::new(RedactTransformer::default()),
            RegistrationOrigin::InProcess,
        )
        .expect("a result transformer belongs on `tool/post-execute`");
    let agent = ReActAgent::new(provider, tools)
        .with_host(Host::new_with_registry(registry))
        .expect("bind the host")
        .with_log_writer(writer);

    let mut stream = agent
        .run(
            AgentInput::text("echo a secret"),
            AtomicCancelToken::shared(),
        )
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read the log");
    let audit: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["type"] == "tool_result_transformed")
        .collect();
    assert_eq!(audit.len(), 1, "one rewrite is one row: {rows:#?}");
    let audit_row = SessionEvent::from_value(audit[0]).expect("the row parses");
    let SessionEvent::ToolResultTransformed {
        transformers,
        tool_name,
        call_id,
        before,
        after,
        ..
    } = &audit_row
    else {
        panic!("the row is an audit row: {audit_row:?}");
    };
    assert_eq!(transformers, &["redact"]);
    assert_eq!(tool_name, "echo");
    assert_eq!(call_id, "c1");
    assert_ne!(before, after, "the audit names two versions: {rows:#?}");

    let result = rows
        .iter()
        .find(|row| row["type"] == "tool_result")
        .expect("the tool result is committed")
        .to_string();
    assert!(
        result.contains("[REDACTED:openai-key]"),
        "the committed result is the redacted one: {result}"
    );
    assert!(
        !result.contains(SECRET),
        "the raw credential never reaches the log: {result}"
    );
}
