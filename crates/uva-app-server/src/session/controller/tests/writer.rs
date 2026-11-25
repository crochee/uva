//! One writer per session: the write discipline the controller
//! owes its log.
//!
//! Every durable row a session gains — the run's own rows, the op
//! loop's out-of-band rows (feedback), a fork's inherited rows —
//! goes through the session's one writer. That is what keeps the
//! log fully stamped, so the fold numbers each row by its own
//! ordinal and a compaction checkpoint's citations name the rows
//! they shadow. It is also what backs the broadcast promise: the
//! harness flushes each model fact before its event is emitted.

use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use parking_lot::Mutex;
use serde_json::{Value, json};
use uva::{
    harness::AgentEvent,
    provider::{
        ContentPart,
        StreamChunk,
        TextContent,
        ToolResult,
        traits::ModelProvider,
    },
    session::{
        LogWriter,
        SessionSink,
        SurfaceLedger,
        manager::{SERVER_DEFAULT_USER_ID, SessionRegistry},
        try_fold_log_surface,
    },
    test_support::FakeTool,
};

use super::{
    super::{AgentRunStreamFactory, RunStreamFactory, SessionOp},
    support::{
        RecordedCall,
        VecFactory,
        make_manager_and_controller,
        make_manager_and_controller_with_deps,
        wait_for_log_row,
    },
};
use crate::{
    routes::sessions::{ForkSessionRequest, fork_session},
    state::AppState,
};

fn text_event(body: &str) -> AgentEvent {
    AgentEvent::Model(ContentPart::Text(TextContent {
        text: body.to_string(),
        cache_control: None,
    }))
}

fn tool_result_event(call_id: &str) -> AgentEvent {
    AgentEvent::Model(ContentPart::ToolResult(ToolResult::new(
        call_id,
        "raw output",
    )))
}

/// The op loop writes a feedback row alongside the run's own rows.
/// The two must not drift apart: every row carries the ordinal its
/// writer stamped, the fold numbers the log by those ordinals, and
/// a compaction checkpoint citing them resolves — the strict fold
/// passes and the shadowed span is replaced, not replayed.
#[tokio::test]
async fn a_feedback_row_does_not_break_the_run_ordinals() {
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn RunStreamFactory> = Arc::new(VecFactory::new(
        vec![tool_result_event("c9"), text_event("done")],
        Arc::clone(&calls),
    ));
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // The op loop's out-of-band row lands before the run's own.
    controller
        .submit(SessionOp::Feedback {
            message_id: "m1".to_string(),
            thumbs_up: true,
        })
        .await
        .unwrap();
    wait_for_log_row(&manager, "feedback").await;

    controller
        .submit(SessionOp::Prompt {
            content: "go".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    wait_for_log_row(&manager, "done").await;

    let sink = manager.sink("alice", "s1");
    let rows = sink.read().await.unwrap();
    // The feedback row is typed: its own tag, the rating in typed
    // fields, and the ordinal the writer handed it.
    let feedback = rows
        .iter()
        .find(|row| row["type"] == "feedback")
        .expect("the feedback row is a typed log-only row");
    assert_eq!(feedback["message_id"], "m1", "the rated message");
    assert_eq!(feedback["thumbs_up"], true, "the rating");
    assert!(
        feedback.get("kind").is_none(),
        "the row carries no ad-hoc `kind` field: {feedback:?}"
    );
    assert!(
        rows.iter()
            .all(|row| row.get("seq").and_then(Value::as_u64).is_some()),
        "every event row carries the ordinal its writer stamped; \
         got {rows:?}"
    );

    let tool_seq = rows
        .iter()
        .find(|row| {
            row.pointer("/data/tool_call_id").and_then(Value::as_str)
                == Some("c9")
        })
        .and_then(|row| row.get("seq").and_then(Value::as_u64))
        .expect("the tool row is stamped");

    // The ledger a run seeds from this history must agree with the
    // fold: the writer numbers the seed the way the fold does.
    let writer = manager.writer("alice", "s1");
    let ledger = Arc::new(SurfaceLedger::new());
    writer.set_ledger(Arc::clone(&ledger));
    writer.seed_ledger(&rows);
    let resolved = ledger
        .resolve(&[Some("tool:c9".to_string())])
        .expect("the tool row resolves in the seeded ledger");
    assert_eq!(
        resolved.source_event_seqs,
        vec![tool_seq],
        "the seed and the row's own stamp name the same ordinal"
    );

    // A checkpoint citing those seqs is a legitimate compaction:
    // the strict fold accepts it and the shadowed span disappears.
    writer
        .append_event(&uva::session::SessionEvent::Compaction {
            seq: 0,
            ts: String::new(),
            data: json!({"summary": "the tool turn, compacted"}),
            surface_op: uva::session::SurfaceOp::Replace {
                start: resolved.start,
                end: resolved.end,
                source_event_seqs: resolved.source_event_seqs.clone(),
            },
        })
        .await
        .unwrap();
    writer.flush().await.unwrap();

    let folded = try_fold_log_surface(&sink.read().await.unwrap())
        .expect("the compaction resolves against the folded log");
    let flattened = serde_json::to_string(&folded.messages).unwrap();
    assert!(
        flattened.contains("the tool turn, compacted"),
        "the summary replaces the shadowed span; got {flattened}"
    );
    assert!(
        !flattened.contains("raw output"),
        "the shadowed tool row must not replay; got {flattened}"
    );
}

/// A provider that answers the first pass with a tool call and
/// then parks: the run never ends, so no tail flush can land the
/// tool-result row — only the harness's own flush before the
/// event can.
struct ToolThenParkProvider {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl ModelProvider for ToolThenParkProvider {
    async fn initialize(
        &mut self,
        _config: uva::provider::ProviderConfig,
    ) -> Result<(), uva::core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "tool-then-park"
    }

    fn model_config(&self) -> uva::provider::types::ModelConfig {
        uva::provider::types::ModelConfig {
            name: "fake".to_string(),
            provider: "scripted".to_string(),
            context_window: 128_000,
            max_output_tokens: 4096,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
        }
    }

    async fn complete(
        &self,
        _request: uva::provider::CompletionRequest,
    ) -> Result<uva::provider::CompletionResponse, uva::core::Error> {
        unreachable!("streaming path should not call complete()")
    }

    async fn complete_with_stream(
        &self,
        _request: uva::provider::CompletionRequest,
        _cancel_token: Option<Arc<dyn uva::core::CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<uva::provider::SamplingResult, uva::core::Error> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let chunks = [
                StreamChunk::ToolCallStart {
                    id: "c1".to_string(),
                    name: "echo".to_string(),
                    arguments: json!({"text": "proof"}),
                },
                StreamChunk::ToolCallEnd {
                    id: "c1".to_string(),
                },
                StreamChunk::IsDone {
                    result: Box::default(),
                },
            ];
            let mut assembler = uva::provider::BlockAssembler::new();
            for chunk in chunks {
                assembler.push(chunk.clone());
                on_delta(chunk);
            }
            let (turn, _parts, _incomplete) = assembler.finalize_assistant();
            return Ok(turn);
        }
        // The second pass parks: the run never ends, so the tail
        // flush never runs.
        std::future::pending::<()>().await;
        unreachable!("pending never resolves")
    }
}

/// A tool result a subscriber sees is already on disk: the
/// harness writes the `tool_result` row and flushes it BEFORE
/// emitting the tool-result event, so the broadcast is a promise
/// the log already keeps. The run parks after the tool turn, so
/// no later flush could have written the row instead.
#[tokio::test]
async fn a_tool_result_row_is_on_disk_before_its_event_is_broadcast() {
    let provider = Arc::new(ToolThenParkProvider {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });

    let echo: Arc<dyn uva::tool::Tool> =
        Arc::new(FakeTool::new("echo", "echoed proof"));
    let registry = {
        let registry = uva::tool::ToolRegistry::new();
        registry.register_entry(uva::tool::ToolEntry::new(Arc::clone(&echo)));
        registry
    };
    let mut deps = super::support::test_deps();
    deps.provider = provider;
    deps.tool_registry = Arc::new(registry);

    let (controller, manager, _temp) = make_manager_and_controller_with_deps(
        Duration::from_secs(60),
        Arc::new(AgentRunStreamFactory),
        deps,
    )
    .await;

    let mut rx = controller.subscribe();
    controller
        .submit(SessionOp::Prompt {
            content: "go".to_string(),
            priority: 1,
        })
        .await
        .unwrap();

    let sink = manager.sink("alice", "s1");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let item = rx.recv().await.expect("the broadcast is live");
            let crate::event_stream::BroadcastEvent::Run { event, .. } = item
            else {
                continue;
            };
            let AgentEvent::Model(ContentPart::ToolResult(result)) =
                event.as_ref()
            else {
                continue;
            };
            assert_eq!(
                result.tool_use_id, "c1",
                "the observed fact is the echo call's result"
            );
            let rows = sink.read().await.unwrap();
            assert!(
                rows.iter().any(|row| {
                    row["type"] == "tool_result"
                        && row.to_string().contains("echoed proof")
                }),
                "the tool-result row must read back before its event \
                 is broadcast; got {rows:?}"
            );
            break;
        }
    })
    .await
    .expect("the tool result was broadcast within the deadline");
}

/// A provider that parks inside its first call: the run never ends,
/// so no later flush could have written the row instead.
struct ParkProvider {
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl ModelProvider for ParkProvider {
    async fn initialize(
        &mut self,
        _config: uva::provider::ProviderConfig,
    ) -> Result<(), uva::core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "park"
    }

    fn model_config(&self) -> uva::provider::types::ModelConfig {
        uva::provider::types::ModelConfig {
            name: "fake".to_string(),
            provider: "scripted".to_string(),
            context_window: 128_000,
            max_output_tokens: 4096,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
        }
    }

    async fn complete(
        &self,
        _request: uva::provider::CompletionRequest,
    ) -> Result<uva::provider::CompletionResponse, uva::core::Error> {
        unreachable!("streaming path should not call complete()")
    }

    async fn complete_with_stream(
        &self,
        _request: uva::provider::CompletionRequest,
        _cancel_token: Option<Arc<dyn uva::core::CancelToken>>,
        _on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<uva::provider::SamplingResult, uva::core::Error> {
        // The model is being asked. Everything it is about to see is
        // already on disk — the harness flushed the writer at the step
        // boundary, before this call went out.
        self.entered.notify_one();
        std::future::pending::<()>().await;
        unreachable!("pending never resolves")
    }
}

/// The user's prompt is on disk before the model sees it. The
/// harness flushes the writer at the step boundary — before the
/// provider call — so the message the user already saw in the UI
/// survives a crash in the window between the prompt and the answer.
/// The provider parks inside that call, so no later flush could have
/// written the row instead.
#[tokio::test]
async fn the_user_prompt_is_on_disk_before_the_model_call() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(ParkProvider {
        entered: Arc::clone(&entered),
    });
    let mut deps = super::support::test_deps();
    deps.provider = provider;

    let (controller, manager, _temp) = make_manager_and_controller_with_deps(
        Duration::from_secs(60),
        Arc::new(AgentRunStreamFactory),
        deps,
    )
    .await;

    controller
        .submit(SessionOp::Prompt {
            content: "the question".to_string(),
            priority: 1,
        })
        .await
        .unwrap();

    // The provider is inside its call: the model has been asked, and
    // the run is parked there.
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the provider was called");

    let rows = manager.sink("alice", "s1").read().await.unwrap();
    assert!(
        rows.iter().any(|row| {
            super::support::prompt_text_of(row).as_deref()
                == Some("the question")
        }),
        "the prompt must be on disk while the model is still blocked; \
         got {rows:?}"
    );
}

/// A fork's inherited rows are re-stamped by the child's writer:
/// the parent's ordinals never cross into the child's log, and the
/// child's own future continues from the child's counter.
#[tokio::test]
async fn a_forked_session_restamps_its_inherited_rows() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let sessions = SessionRegistry::new(dir.path().join("sessions"));
    let state =
        Arc::new(AppState::for_test(sessions, dir.path().to_path_buf()).await);

    // The parent's rows carry ordinals only the parent ever used —
    // a verbatim copy would put 5 and 6 into the child's log.
    let parent_dir = dir
        .path()
        .join("sessions")
        .join(SERVER_DEFAULT_USER_ID)
        .join("origin");
    std::fs::create_dir_all(&parent_dir).expect("parent dir");
    let body: String = [
        json!({
            "seq": 5,
            "type": "UserInput",
            "ts": "2026-10-25T08:00:00+00:00",
            "data": {"text": "hello"},
        }),
        json!({
            "seq": 6,
            "type": "Model",
            "ts": "2026-10-25T08:00:05+00:00",
            "data": {"text": "hi"},
        }),
    ]
    .iter()
    .map(|row| format!("{row}\n"))
    .collect();
    std::fs::write(parent_dir.join("events.jsonl"), body)
        .expect("parent transcript");

    let forked = fork_session(
        Arc::clone(&state),
        "origin".to_string(),
        ForkSessionRequest {
            from_stream_index: None,
        },
    )
    .await
    .expect("fork");

    let child_rows = state
        .session_manager
        .sink(SERVER_DEFAULT_USER_ID, &forked.session_id)
        .read()
        .await
        .expect("the child's log reads back");
    let seqs: Vec<Option<u64>> = child_rows
        .iter()
        .map(|row| row.get("seq").and_then(Value::as_u64))
        .collect();
    assert_eq!(
        seqs,
        vec![Some(1), Some(2)],
        "the child's rows carry the child's own ordinals, not the \
         parent's 5 and 6; got {child_rows:?}"
    );
    // The copy preserves when the events happened: the child's
    // `created_at` is the conversation's first event, not the fork.
    let ts: Vec<Option<&str>> = child_rows
        .iter()
        .map(|row| row.get("ts").and_then(Value::as_str))
        .collect();
    assert_eq!(
        ts,
        vec![
            Some("2026-10-25T08:00:00+00:00"),
            Some("2026-10-25T08:00:05+00:00")
        ],
        "the inherited rows keep the parent's timestamps; got {child_rows:?}"
    );

    // The child's future continues its own sequence.
    let writer = state
        .session_manager
        .writer(SERVER_DEFAULT_USER_ID, &forked.session_id);
    let next = writer
        .append_event(&uva::session::user_message(
            serde_json::to_value(uva::provider::Message::user("after"))
                .expect("a user message serialises"),
            None,
        ))
        .await
        .expect("the child's next row");
    writer.flush().await.expect("flush");
    assert_eq!(
        next, 3,
        "the child's counter follows its own re-stamped rows"
    );
}

/// A fork's inherited compaction row keeps meaning what it meant:
/// its citations are remapped to the child's own ordinals, so the
/// child's fold still replaces the span the parent's checkpoint
/// shadowed.
#[tokio::test]
async fn a_forked_session_remaps_the_inherited_compaction() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let sessions = SessionRegistry::new(dir.path().join("sessions"));
    let state =
        Arc::new(AppState::for_test(sessions, dir.path().to_path_buf()).await);

    let parent_dir = dir
        .path()
        .join("sessions")
        .join(SERVER_DEFAULT_USER_ID)
        .join("origin");
    std::fs::create_dir_all(&parent_dir).expect("parent dir");
    let rows = [
        json!({
            "seq": 5,
            "type": "UserInput",
            "ts": "2026-10-25T08:00:00+00:00",
            "data": {"text": "hello"},
        }),
        json!({
            "seq": 7,
            "type": "Model",
            "ts": "2026-10-25T08:00:05+00:00",
            "data": {
                "type": "tool_result",
                "tool_use_id": "c9",
                "content": [{"type": "text", "text": "raw output"}],
            },
        }),
        json!({
            "seq": 8,
            "type": "compaction",
            "ts": "2026-10-25T08:00:10+00:00",
            "surface_op": {
                "start": 1,
                "end": 2,
                "source_event_seqs": [7],
            },
            "data": {"summary": "the tool turn, compacted"},
        }),
    ];
    let body: String = rows.iter().map(|row| format!("{row}\n")).collect();
    std::fs::write(parent_dir.join("events.jsonl"), body)
        .expect("parent transcript");

    let forked = fork_session(
        Arc::clone(&state),
        "origin".to_string(),
        ForkSessionRequest {
            from_stream_index: None,
        },
    )
    .await
    .expect("fork");

    let child_rows = state
        .session_manager
        .sink(SERVER_DEFAULT_USER_ID, &forked.session_id)
        .read()
        .await
        .expect("the child's log reads back");
    assert_eq!(child_rows.len(), 3);
    // The tool row is the child's ordinal 2; the checkpoint's
    // citation must name it, not the parent's 7.
    assert_eq!(
        child_rows[2].pointer("/surface_op/source_event_seqs"),
        Some(&json!([2])),
        "the citation is remapped to the child's own ordinals; got \
         {child_rows:?}"
    );
    let folded = try_fold_log_surface(&child_rows)
        .expect("the remapped compaction resolves in the child's log");
    let flattened = serde_json::to_string(&folded.messages).unwrap();
    assert!(
        flattened.contains("the tool turn, compacted")
            && !flattened.contains("raw output"),
        "the shadowed span stays shadowed after the fork; got {flattened}"
    );
}

/// The writer is the only thing that can tell a compaction
/// checkpoint where its shadowed rows are: it stamps every row with
/// the ordinal the reader will reproduce and feeds the same rows,
/// under the same ordinals, to the ledger. The controller never sees
/// a handle of its own that could bypass that.
#[tokio::test]
async fn the_writer_feeds_the_ledger_with_log_ordinals() {
    let sink: Arc<dyn SessionSink> =
        Arc::new(uva::session::in_memory::InMemorySessionSink::new("s1"));
    let ledger = Arc::new(SurfaceLedger::new());
    let writer = Arc::new(
        LogWriter::new(Arc::clone(&sink), Arc::new(uva::core::SystemClock))
            .with_ledger(Arc::clone(&ledger)),
    );
    // The rows are the writer's: it stamps each one with the ordinal
    // the reader will reproduce and feeds the same ordinals to the
    // ledger. The tool row is the typed event a tool result now
    // takes; the legacy `Model` envelope it replaced folded to the
    // same surface row.
    writer
        .append_event(&uva::session::user_message(
            serde_json::to_value(uva::provider::Message::user("hi"))
                .expect("a user message serialises"),
            None,
        ))
        .await
        .unwrap();
    writer
        .append_event(&uva::session::SessionEvent::ToolResult {
            seq: 0,
            ts: String::new(),
            data: serde_json::to_value(uva::provider::Message::tool(
                uva::provider::Content::text("out"),
                "c1",
            ))
            .expect("a tool message serialises"),
            is_error: false,
            tool_call_id: Some("c1".to_string()),
            surface_op: None,
        })
        .await
        .unwrap();
    writer.flush().await.unwrap();

    assert_eq!(ledger.len(), 2);
    let resolved = ledger
        .resolve(&[Some("tool:c1".to_string())])
        .expect("the tool row resolves");
    assert_eq!(resolved.start, 1, "the tool row is the second surface row");
    assert_eq!(resolved.source_event_seqs, vec![2]);

    // A typed row is stamped with the ordinal it will have in the
    // log, which is what a checkpoint cites.
    let event = uva::session::SessionEvent::CompactionEnd {
        seq: 0,
        ts: String::new(),
        token: "t".to_string(),
        outcome: "failed".to_string(),
        data: json!({}),
    };
    writer.append_event(&event).await.unwrap();
    writer.flush().await.unwrap();
    let rows = sink.read().await.unwrap();
    assert_eq!(rows[2]["seq"], 3);
    assert!(
        rows[2]["ts"].as_str().is_some_and(|ts| !ts.is_empty()),
        "the wall clock is stamped too"
    );
    // The log is fully stamped, so the strict fold reads each row's
    // own ordinal — the same numbers the ledger recorded.
    try_fold_log_surface(&rows).expect("a fully stamped log folds");
}

/// The ledger must record the ordinal the **sink's counter** gave
/// the row, not `previous + 1`.
///
/// The controller's op loop appends out-of-band — `Feedback` rows,
/// and the shutdown marker, which can land while a run is still
/// writing. A locally counted value would file the next run row one
/// slot too low, and a checkpoint citing it would fail
/// `validate_replace`'s `cited >= self_seq` check: the lenient fold
/// then replays the span the checkpoint had replaced, and the strict
/// fold errors. The out-of-band rows go through the same writer, so
/// the interleave below is the production shape: one ordinal source,
/// and the ledger, the fold and the counter agree.
#[tokio::test]
async fn the_writer_takes_its_ordinal_from_the_sink_not_a_local_counter() {
    let sink: Arc<dyn SessionSink> =
        Arc::new(uva::session::in_memory::InMemorySessionSink::new("s1"));
    let writer = Arc::new(LogWriter::new(
        Arc::clone(&sink),
        Arc::new(uva::core::SystemClock),
    ));
    // Seed the writer as the run task does: the previous run left
    // one row, and the run seeds its ledger from what is on disk.
    writer
        .append_event(&uva::session::user_message(
            json!({
                "role": "user",
                "content": {"text": "old"},
            }),
            None,
        ))
        .await
        .unwrap();
    writer.flush().await.unwrap();

    let ledger = Arc::new(SurfaceLedger::new());
    writer.set_ledger(Arc::clone(&ledger));
    let existing = sink.read().await.unwrap();
    writer.seed_ledger(&existing);

    writer
        .append_event(&uva::session::user_message(
            json!({
                "role": "user",
                "content": {"text": "run row"},
            }),
            None,
        ))
        .await
        .unwrap();

    // The op loop's out-of-band row lands between two of the run's
    // own — through the same writer, so it draws from the same
    // counter and the two cannot collide.
    writer
        .append_event(&uva::session::SessionEvent::Feedback {
            seq: 0,
            ts: String::new(),
            message_id: "m1".to_string(),
            thumbs_up: true,
        })
        .await
        .unwrap();

    writer
        .append_event(&uva::session::SessionEvent::ToolResult {
            seq: 0,
            ts: String::new(),
            data: serde_json::to_value(uva::provider::Message::tool(
                uva::provider::Content::text("out"),
                "c9",
            ))
            .expect("a tool message serialises"),
            is_error: false,
            tool_call_id: Some("c9".to_string()),
            surface_op: None,
        })
        .await
        .unwrap();
    writer.flush().await.unwrap();

    let rows = sink.read().await.unwrap();
    assert_eq!(rows.len(), 4, "seed + run + feedback + run");
    // The tool row is physically row 4. The ledger must say so —
    // a local counter would have said 3 (the feedback row's
    // ordinal) and made any checkpoint citing it unresolvable.
    let resolved = ledger
        .resolve(&[Some("tool:c9".to_string())])
        .expect("the tool row resolves");
    assert_eq!(
        resolved.source_event_seqs,
        vec![4],
        "the ledger records the counter's ordinal, not previous + 1"
    );
    try_fold_log_surface(&rows).expect("a fully stamped log folds");
}
