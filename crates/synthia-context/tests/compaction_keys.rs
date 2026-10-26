//! A durable compaction is only as good as the names both sides
//! agree on: the manager names the rows it shadowed, and the ledger
//! looks those names up among the rows the run actually wrote.
//!
//! This is the cross-crate half of the key scheme — the manager
//! reports [`synthia_session::ledger_key::key_of`] on the messages
//! it shadows, and the ledger indexes the same key on the rows it
//! recorded. When the two disagree the checkpoint cannot prove the
//! span, and the failure is silent: the compaction is recorded
//! log-only and the shadowed rows are replayed.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use synthia_context::{
    context_manager::{AgentState, ContextManager},
    summarizing::{CompactionRecord, SummariseFn, SummarizingContextManager},
};
use synthia_provider::{
    Content,
    ContentPart,
    Message,
    Role,
    TextContent,
    ToolUse,
};
use synthia_session::SurfaceLedger;

/// The assistant turn that made the call.
fn assistant_turn() -> Message {
    Message::new(
        Role::Assistant,
        Content::parts(vec![
            ContentPart::Text(TextContent {
                text: "working".to_string(),
                cache_control: None,
            }),
            ContentPart::ToolUse(ToolUse {
                id: "call-1".to_string(),
                name: "read".to_string(),
                input: json!({}),
            }),
        ]),
    )
}

/// The tool result that answers the call.
fn tool_result() -> Message {
    Message::tool(
        Content::parts(vec![ContentPart::Text(TextContent {
            text: "raw output bytes".to_string(),
            cache_control: None,
        })]),
        "call-1",
    )
}

/// The rows the run wrote: the turn that made the call, and the
/// result that answers it. The loop's system prompt and user turn
/// are never logged, so the durable surface holds only these two.
fn run_rows() -> Vec<Value> {
    vec![
        json!({"type": "assistant_message", "data": assistant_turn()}),
        json!({"type": "tool_result", "data": tool_result()}),
    ]
}

/// A summariser that always answers with the same short text.
fn summarise_that_returns(text: &str) -> SummariseFn {
    let text = text.to_string();
    Arc::new(move |_input: &str| {
        let text = text.clone();
        Box::pin(async move { Some(text) })
    })
}

/// The manager's record and the ledger's rows must name the same
/// rows: the record's keys resolve against the ledger, so the
/// checkpoint can write the span the durable fold will reproduce.
#[tokio::test]
async fn the_managers_keys_resolve_against_the_ledger() {
    let records: Arc<Mutex<Vec<CompactionRecord>>> =
        Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&records);
    let manager =
        SummarizingContextManager::new(summarise_that_returns("short"))
            .with_compaction_emitter(move |record| {
                sink.lock().expect("records mutex").push(record);
            });

    let mut messages = vec![
        Message::system("sys"),
        Message::user("go"),
        assistant_turn(),
        tool_result(),
        Message::assistant("done"),
    ];
    let mut state = AgentState::with_window(3000);
    manager.prepare(&mut messages, &mut state).await;

    let captured = records.lock().expect("records mutex").clone();
    assert_eq!(captured.len(), 1, "exactly one compaction splice");
    let record = &captured[0];
    assert!(
        record.source_keys.iter().all(|key| key.is_some()),
        "every shadowed row carries a durable identity: {:?}",
        record.source_keys,
    );

    // The rows the run wrote, indexed by the ledger.
    let ledger = SurfaceLedger::new();
    for (index, row) in run_rows().iter().enumerate() {
        ledger.record(index as u64 + 1, row);
    }

    let resolved = ledger
        .resolve(&record.source_keys)
        .expect("the manager's keys resolve against the ledger");
    assert_eq!(
        resolved.source_event_seqs,
        vec![1, 2],
        "the shadowed rows are the turn that made the call and the \
         result that answers it",
    );
}
