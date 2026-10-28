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

use axum::{Json, extract::State, http::StatusCode};
use parking_lot::Mutex;
use serde_json::{Value, json};
use elysper::{
    harness::AgentEvent,
    provider::{
        Content,
        ContentPart,
        StreamChunk,
        TextContent,
        ToolResult,
        traits::ModelProvider,
    },
    session::{
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
    api::{AppJson, AppPath},
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
        .append(json!({
            "type": "compaction",
            "surface_op": {
                "start": resolved.start,
                "end": resolved.end,
                "source_event_seqs": resolved.source_event_seqs,
            },
            "data": {"summary": "the tool turn, compacted"},
        }))
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
        _config: elysper::provider::ProviderConfig,
    ) -> Result<(), elysper::core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "tool-then-park"
    }

    fn model_config(&self) -> elysper::provider::types::ModelConfig {
        elysper::provider::types::ModelConfig {
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
        _request: elysper::provider::CompletionRequest,
    ) -> Result<elysper::provider::CompletionResponse, elysper::core::Error>
    {
        unreachable!("streaming path should not call complete()")
    }

    async fn complete_with_stream(
        &self,
        _request: elysper::provider::CompletionRequest,
        _cancel_token: Option<Arc<dyn elysper::core::CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<elysper::provider::CompletionResponse, elysper::core::Error>
    {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            on_delta(StreamChunk::ToolCallStart {
                id: "c1".to_string(),
                name: "echo".to_string(),
                arguments: json!({"text": "proof"}),
            });
            on_delta(StreamChunk::ToolCallEnd {
                id: "c1".to_string(),
            });
            on_delta(StreamChunk::IsDone {
                result: Box::default(),
            });
            return Ok(elysper::provider::CompletionResponse {
                id: "resp-1".to_string(),
                model: "fake".to_string(),
                content: Content::Single(ContentPart::Text(TextContent {
                    text: String::new(),
                    cache_control: None,
                })),
                usage: elysper::provider::TokenUsage::default(),
                cached: false,
                replay_state: None,
                stop_reason: None,
            });
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

    let echo: Arc<dyn elysper::tool::Tool> =
        Arc::new(FakeTool::new("echo", "echoed proof"));
    let registry = {
        let registry = elysper::tool::ToolRegistry::new();
        registry
            .register_entry(elysper::tool::ToolEntry::new(Arc::clone(&echo)));
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
            let event = rx.recv().await.expect("the broadcast is live");
            let AgentEvent::Model(ContentPart::ToolResult(result)) = &event
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
        _config: elysper::provider::ProviderConfig,
    ) -> Result<(), elysper::core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "park"
    }

    fn model_config(&self) -> elysper::provider::types::ModelConfig {
        elysper::provider::types::ModelConfig {
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
        _request: elysper::provider::CompletionRequest,
    ) -> Result<elysper::provider::CompletionResponse, elysper::core::Error>
    {
        unreachable!("streaming path should not call complete()")
    }

    async fn complete_with_stream(
        &self,
        _request: elysper::provider::CompletionRequest,
        _cancel_token: Option<Arc<dyn elysper::core::CancelToken>>,
        _on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<elysper::provider::CompletionResponse, elysper::core::Error>
    {
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
            row["type"] == "UserInput"
                && row.to_string().contains("the question")
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

    let (status, Json(forked)) = fork_session(
        State(Arc::clone(&state)),
        AppPath("origin".to_string()),
        AppJson(ForkSessionRequest {
            from_stream_index: None,
        }),
    )
    .await
    .expect("fork");
    assert_eq!(status, StatusCode::CREATED);

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
        .append(json!({"type": "UserInput", "data": {"text": "after"}}))
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

    let (_status, Json(forked)) = fork_session(
        State(Arc::clone(&state)),
        AppPath("origin".to_string()),
        AppJson(ForkSessionRequest {
            from_stream_index: None,
        }),
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
