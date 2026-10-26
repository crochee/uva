//! The log writer's step-boundary flush: a run's structural rows
//! are durable when the run ends, and each row carries the ordinal
//! the log holds it under.

use std::sync::Arc;

use futures::StreamExt;
use synthia_core::SystemClock;
use synthia_session::{
    LogWriter,
    SessionEvent,
    SessionSink,
    in_memory::InMemorySessionSink,
};
use synthia_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use super::super::{support::*, *};

/// A finished run leaves no row in the writer's buffer: the step
/// boundary and the final flush wrote them all, and each row's own
/// `seq` is the ordinal the reader reproduces.
pub(super) async fn a_completed_run_flushes_its_structural_rows() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let provider =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("all done")]));

    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_log_writer(writer);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    assert!(
        !rows.is_empty(),
        "the run's structural rows must reach the sink"
    );
    let seqs: Vec<u64> = rows
        .iter()
        .map(|row| row["seq"].as_u64().expect("stamped ordinal"))
        .collect();
    assert_eq!(
        seqs,
        (1..=seqs.len() as u64).collect::<Vec<u64>>(),
        "the ordinals are contiguous and the rows carry their own"
    );
}

fn typed(rows: &[serde_json::Value]) -> Vec<SessionEvent> {
    rows.iter().filter_map(SessionEvent::from_value).collect()
}

/// One run is one turn: the log carries exactly one start before the
/// first step and one end after the last, and a repair walking the
/// finished log finds nothing to close.
pub(super) async fn a_completed_run_brackets_its_steps_with_one_turn() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let provider =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("all done")]));

    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_log_writer(writer);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    let turns: Vec<&serde_json::Value> =
        rows.iter().filter(|row| row["type"] == "turn").collect();
    assert_eq!(
        turns.len(),
        2,
        "a completed run writes one turn start and one turn end: {turns:?}"
    );
    assert_eq!(turns[0]["data"]["kind"], "start");
    assert_eq!(turns[0]["data"]["turn"], 0);
    assert_eq!(turns[1]["data"]["kind"], "end");
    assert_eq!(turns[1]["data"]["turn"], 0);
    assert_eq!(turns[1]["data"]["reason"], "completed");

    let start = rows
        .iter()
        .position(|row| row["type"] == "turn" && row["data"]["kind"] == "start")
        .expect("a turn start");
    let end = rows
        .iter()
        .position(|row| row["type"] == "turn" && row["data"]["kind"] == "end")
        .expect("a turn end");
    let first_step = rows
        .iter()
        .position(|row| row["type"] == "step")
        .expect("a step start");
    let last_step = rows
        .iter()
        .rposition(|row| row["type"] == "step")
        .expect("a step start");
    assert!(
        start < first_step,
        "the turn opens before its first step: {rows:#?}"
    );
    assert!(
        last_step < end,
        "the turn closes after its last step: {rows:#?}"
    );
    assert!(
        synthia_session::interrupted_turn_closers(&typed(&rows)).is_empty(),
        "a completed run leaves no open turn to repair"
    );
}

/// The crash shape the acceptance step exercises: the log holds a
/// turn start, an open step, the assistant's `tool_use` and the
/// dispatch intent — then the process dies. Repair closes the call,
/// the step and the turn, and the projection the next request is
/// built from has no unanswered `tool_use`.
pub(super) async fn a_crashed_tool_call_repairs_to_a_balanced_tail() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let call = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: serde_json::json!({}),
    };
    let assistant = SessionEvent::AssistantMessage {
        seq: 0,
        ts: String::new(),
        data: serde_json::to_value(Message::new(
            synthia_provider::Role::Assistant,
            Content::Multi(vec![ContentPart::ToolUse(call)]),
        ))
        .expect("assistant row serializes"),
        surface_op: None,
    };
    for event in [
        synthia_session::turn_start(0),
        synthia_session::step_start(0, 0),
        assistant,
        synthia_session::tool_dispatch("c1", "echo", "hash"),
    ] {
        writer
            .append(serde_json::to_value(&event).expect("event serializes"))
            .await
            .expect("append the crashed tail");
    }
    writer.flush().await.expect("flush the crashed tail");

    let repaired = synthia_context::repair_session(Arc::clone(&writer))
        .await
        .expect("repair the interrupted tail");
    assert_eq!(
        repaired, 3,
        "the tool result, the step end and the turn end close the tail"
    );

    let rows = sink.read().await.expect("read the repaired log");
    assert!(
        rows.iter().any(|row| {
            row["type"] == "tool_result"
                && row["data"]["call_id"] == "c1"
                && row["data"]["error"] == synthia_session::TOOL_OUTCOME_UNKNOWN
        }),
        "the dangling dispatch gets an unknown-outcome result: {rows:#?}"
    );
    assert!(
        synthia_session::interrupted_turn_closers(&typed(&rows)).is_empty(),
        "the repaired log is balanced"
    );

    // The next request is built from the projection: every `tool_use`
    // the assistant emitted must have a matching result, or the
    // provider rejects the request.
    let messages = synthia_context::memory::project_typed_messages(&rows, None)
        .expect("project the repaired log");
    let mut uses = Vec::new();
    let mut results = Vec::new();
    for message in &messages {
        // A tool row projects to a `tool` message keyed by the call it
        // answers; a repair-synthesised row has no `ToolResult` part,
        // only that key and the error text.
        if message.role == synthia_provider::Role::Tool {
            if let Some(call_id) = &message.tool_call_id {
                results.push(call_id.clone());
            }
            continue;
        }
        for part in message.content.iter() {
            if let ContentPart::ToolUse(call) = part {
                uses.push(call.id.clone());
            }
        }
    }
    assert_eq!(uses, vec!["c1".to_string()]);
    assert_eq!(
        results, uses,
        "the projection answers every tool_use the assistant emitted"
    );
}

/// Cancelling mid-run is not a crash: the turn this run opened is
/// closed exactly once, with the cancel reason.
pub(super) async fn a_cancelled_run_closes_its_turn_with_the_cancel_reason() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let cancel = CancellationToken::new();
    let provider = Arc::new(CancellingStreamProvider::new(cancel.clone()));

    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_log_writer(writer);
    let mut stream = agent.run(AgentInput::text("hi"), Arc::new(cancel)).await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    let turns: Vec<&serde_json::Value> =
        rows.iter().filter(|row| row["type"] == "turn").collect();
    assert_eq!(turns.len(), 2, "one turn start and one turn end: {turns:?}");
    assert_eq!(turns[0]["data"]["kind"], "start");
    assert_eq!(turns[1]["data"]["kind"], "end");
    assert_eq!(turns[1]["data"]["reason"], "cancelled");
}

/// A run that fails setup never reaches a step, but it is still a
/// turn: it opens one and closes it once as `error`.
pub(super) async fn a_run_that_fails_setup_closes_its_turn_as_error() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    writer
        .append(serde_json::json!({
            "type": "UserInput",
            "data": { "text": "from-log" },
        }))
        .await
        .expect("seed the host's user row");
    writer.flush().await.expect("flush the seeded row");

    let provider =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("never")]));
    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_log_writer(writer);
    // A history the log does not support: setup refuses the run.
    let input = AgentInput::multi_with_history(
        vec![Message::user("from-input")],
        vec![ContentPart::Text(TextContent {
            text: "next".to_string(),
            cache_control: None,
        })],
    );
    let mut stream = agent.run(input, Arc::new(CancellationToken::new())).await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    let turns: Vec<&serde_json::Value> =
        rows.iter().filter(|row| row["type"] == "turn").collect();
    assert_eq!(
        turns.len(),
        2,
        "the failed run opens and closes one turn: {turns:?}"
    );
    assert_eq!(turns[0]["data"]["kind"], "start");
    assert_eq!(turns[1]["data"]["kind"], "end");
    assert_eq!(turns[1]["data"]["reason"], "error");
}

/// The resume window's crash shape: the suspension closed the first
/// turn, the resume reopened it and recorded the decision and the
/// dispatch intent, then the process died before the result. The log
/// must read as an open turn — repair closes the call and the turn
/// instead of the next resume refusing an unbalanced tail.
pub(super) async fn a_crash_in_the_resume_window_repairs_the_reopened_turn() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    for event in [
        synthia_session::turn_start(0),
        synthia_session::turn_suspended(vec!["c1".to_string()]),
        synthia_session::step_end_suspended(),
        synthia_session::turn_end_suspended(),
        // The resume reopens the turn before it records the intent.
        synthia_session::turn_start(0),
        synthia_session::approval_decided(
            "approval-c1",
            "c1",
            "approved",
            "resume",
            None,
        ),
        synthia_session::tool_dispatch("c1", "echo", "hash"),
    ] {
        writer
            .append(serde_json::to_value(&event).expect("event serializes"))
            .await
            .expect("append the resume tail");
    }
    writer.flush().await.expect("flush the resume tail");

    let repaired = synthia_context::repair_session(Arc::clone(&writer))
        .await
        .expect("repair the interrupted resume");
    assert_eq!(
        repaired, 2,
        "the reopened turn has no open step: the result and the turn end close it"
    );

    let rows = sink.read().await.expect("read the repaired log");
    assert!(
        rows.iter().any(|row| {
            row["type"] == "tool_result"
                && row["data"]["call_id"] == "c1"
                && row["data"]["error"] == synthia_session::TOOL_OUTCOME_UNKNOWN
        }),
        "the dispatched call gets an unknown-outcome result: {rows:#?}"
    );
    let last_turn = rows
        .iter()
        .rfind(|row| row["type"] == "turn")
        .expect("a turn row");
    assert_eq!(last_turn["data"]["kind"], "end");
    assert_eq!(last_turn["data"]["reason"], "interrupted");
    assert!(
        synthia_session::interrupted_turn_closers(&typed(&rows)).is_empty(),
        "the repaired tail is balanced for the next resume"
    );
}
