//! Multi-turn memory contracts.
//!
//! The second prompt must seed its `history` from the previous
//! run's persisted turns; user-role messages must be
//! reconstructed as `Role::User`; a durable compaction
//! checkpoint written by an earlier run must fold to the
//! compacted surface (summary in place of tool results).

use std::{sync::Arc, time::Duration};

use parking_lot::Mutex;
use uva::provider::{Content, ContentPart, Role, TextContent};

use super::{
    super::{AgentEvent, AgentInput, SessionOp},
    support::{
        RecordingFactory,
        make_manager_and_controller,
        wait_for_log_row,
    },
};
use crate::session::status::SessionState;

/// Regression: the second prompt submitted to the same session
/// must seed `AgentInput::history` with the assistant turns the
/// previous run persisted to the `SessionSink`.
///
/// Without the fix, the controller never re-reads the sink, so
/// `input.history` is empty on every run and the LLM sees a
/// fresh conversation — losing multi-turn memory.
#[tokio::test]
async fn test_second_prompt_seeds_history_from_persisted_turns() {
    let calls = Arc::new(Mutex::new(Vec::<AgentInput>::new()));
    // First run emits one durable assistant text chunk; second
    // run emits another. Both must be observable to the agent.
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(RecordingFactory::new(
            Arc::clone(&calls),
            vec![AgentEvent::Model(ContentPart::Text(TextContent {
                text: "first reply".to_string(),
                cache_control: None,
            }))],
            vec![AgentEvent::Model(ContentPart::Text(TextContent {
                text: "second reply".to_string(),
                cache_control: None,
            }))],
        ));

    let (controller, _manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // Turn 1
    controller
        .submit(SessionOp::Prompt {
            content: "turn-1 prompt".to_string(),
            priority: 1,
        })
        .await
        .unwrap();

    // Wait for the run to finish and the controller to return
    // to Idle so we know the first run's events have been
    // persisted by `persist_and_broadcast`.
    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.state() != SessionState::Idle {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("first run should finish");

    // After state flips to Idle the controller still holds the
    // first run's `JoinHandle` for one more select! iteration;
    // only once `run_handle.await` returns and sets
    // `run_handle = None` does a new op_rx prompt actually
    // spawn a fresh run. Wait for that window to close before
    // submitting turn 2, otherwise the controller will see
    // `run_handle.is_some()` and drop the dispatch. Poll on
    // the captured calls vector: once it stops growing, the
    // first run's stream has been fully consumed.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if calls.lock().len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("first run input should be recorded");
    // Tiny grace period for the spawned task to exit so the
    // controller loop clears `run_handle`.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Turn 2 — submits a fresh prompt. The fix must cause
    // `input.history` of run #2 to contain run #1's assistant
    // text "first reply" so the agent has memory of the prior
    // turn.
    controller
        .submit(SessionOp::Prompt {
            content: "turn-2 prompt".to_string(),
            priority: 1,
        })
        .await
        .unwrap();

    // The controller serializes through op_rx; once the
    // second run is dispatched the factory records a second
    // input. Allow generous slack because the controller may
    // briefly sit in its idle branch before re-entering the
    // select! iteration that picks up op_rx.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if calls.lock().len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second run should start");

    let recorded = calls.lock();
    assert_eq!(recorded.len(), 2, "controller should have spawned two runs");

    // The second run's input must carry the first run's
    // persisted assistant text inside `history`. We allow
    // the current-turn prompt itself to live in `content`,
    // but the prior assistant turn must be in `history` —
    // that is the contract being violated by the bug.
    let second = &recorded[1];
    let history_text = second
        .history
        .iter()
        .filter_map(|m| match &m.content {
            Content::Single(ContentPart::Text(t)) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        history_text.contains("first reply"),
        "second run's history must include first run's assistant text; got history={:?}",
        second.history
    );

    // The current-turn prompt must live in `content`, NOT in
    // `history` — appending the prompt to the sink before the
    // run drains the queue would cause this assertion to fail
    // because the run would read its own prompt back out of
    // `sink_history` and feed it to the agent twice (once via
    // `history`, once via `content`). The fix persists the
    // drained prompts after reading the sink.
    assert!(
        !history_text.contains("turn-2 prompt"),
        "second run's history must NOT include the current-turn prompt (would cause \
         duplicate-feed); got history={:?}",
        second.history
    );

    // Also assert turn-2's prompt landed as the input content
    // so we know the history seed didn't overwrite the prompt.
    let second_prompt_text = match second.content.first() {
        Some(ContentPart::Text(t)) => t.text.clone(),
        _ => panic!("second run content should be text"),
    };
    assert_eq!(second_prompt_text, "turn-2 prompt");
}

/// Regression: user prompts must be persisted to the session sink
/// and reconstructed as `Message{Role:User}` in the next run's
/// history. Without the `UserInput` envelope append in the
/// `Prompt` handler, `events_to_history` only sees assistant
/// messages and the LLM treats every turn as a fresh conversation.
#[tokio::test]
async fn test_user_prompt_persists_and_reacts_to_role_user_in_history() {
    let calls = Arc::new(Mutex::new(Vec::<AgentInput>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(RecordingFactory::new(
            Arc::clone(&calls),
            vec![AgentEvent::Model(ContentPart::Text(TextContent {
                text: "ack".to_string(),
                cache_control: None,
            }))],
            vec![AgentEvent::Model(ContentPart::Text(TextContent {
                text: "ack".to_string(),
                cache_control: None,
            }))],
        ));

    let (controller, _manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    controller
        .submit(SessionOp::Prompt {
            content: "我的猫叫蓝杉".to_string(),
            priority: 1,
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.state() != SessionState::Idle {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("first run should finish");

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if calls.lock().len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("first run input should be recorded");
    tokio::time::sleep(Duration::from_millis(20)).await;

    controller
        .submit(SessionOp::Prompt {
            content: "它几岁？".to_string(),
            priority: 1,
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if calls.lock().len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second run should start");

    let recorded = calls.lock();
    assert_eq!(recorded.len(), 2);
    let second = &recorded[1];

    let user_messages: Vec<&str> = second
        .history
        .iter()
        .filter(|m| m.role == Role::User)
        .filter_map(|m| match &m.content {
            Content::Single(ContentPart::Text(t)) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        user_messages.iter().any(|t| t.contains("蓝杉")),
        "second run history must contain turn-1 user prompt as Role::User; got history={:?}",
        second.history
    );

    // The current-turn prompt must NOT appear in `history` —
    // duplicating it would make the LLM treat the prompt as a
    // fresh message and discard the conversation context.
    assert!(
        user_messages.iter().all(|t| !t.contains("它几岁")),
        "second run history must NOT include the current-turn prompt (would cause \
         duplicate-feed); got history={:?}",
        second.history
    );
}

/// The run path rebuilds every run's history from the durable log
/// (`derive_history` on `session_store.read()`); a compaction an
/// earlier run committed must therefore show up as the *compacted*
/// surface — the summary in place of the tool results, not the
/// pre-compaction span. That is the whole point of persisting the
/// compaction row.
#[tokio::test]
async fn resumed_history_shows_the_durable_compaction_checkpoint() {
    let tool_row = |call_id: &str| {
        serde_json::json!({
            "type": "Model",
            "data": {
                "type": "tool_result",
                "tool_use_id": call_id,
                "tool_name": "read",
                "content": [{
                    "type": "text",
                    "text": format!("raw output of {call_id}"),
                }],
            }
        })
    };

    let calls = Arc::new(Mutex::new(Vec::<AgentInput>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(RecordingFactory::new(
            Arc::clone(&calls),
            vec![AgentEvent::Model(ContentPart::Text(TextContent {
                text: "ack".to_string(),
                cache_control: None,
            }))],
            vec![],
        ));
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // Prime the log exactly as a compacted run leaves it: rows
    // 1..4 are the turn's messages, row 5 is the checkpoint the
    // run's checkpoint bridge wrote (span + cited log seqs).
    let sink = manager.sink("alice", "s1");
    sink.alloc_seq();
    sink.append(&serde_json::json!({
        "type": "UserInput",
        "data": {"text": "hello"},
    }))
    .await
    .unwrap();
    sink.alloc_seq();
    sink.append(&serde_json::json!({
        "type": "Model",
        "data": {"type": "text", "text": "working"},
    }))
    .await
    .unwrap();
    sink.alloc_seq();
    sink.append(&tool_row("c1")).await.unwrap();
    sink.alloc_seq();
    sink.append(&tool_row("c2")).await.unwrap();
    sink.alloc_seq();
    sink.append(&serde_json::json!({
        "type": "compaction",
        "seq": 5,
        "ts": "2026-09-12T00:00:00Z",
        "surface_op": {"start": 2, "end": 4, "source_event_seqs": [3, 4]},
        "data": {"source_indices": [2, 3], "summary": "compacted tail"},
    }))
    .await
    .unwrap();

    controller
        .submit(SessionOp::Prompt {
            content: "carry on".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.lock().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the run factory was not invoked in time");
    let recorded = calls.lock();
    let history = &recorded[0].history;
    let flattened = format!("{history:?}");
    assert!(
        flattened.contains("compacted tail"),
        "the checkpoint's summary must seed the resumed history; got {flattened}"
    );
    assert!(
        !flattened.contains("raw output of c1")
            && !flattened.contains("raw output of c2"),
        "the pre-compaction span must not be replayed; got {flattened}"
    );
}

/// A log the strict fold refuses must not cost the run its history.
///
/// The strict fold refuses a compaction whose citation does not name
/// the rows its span covers — a compaction written in the old
/// coordinates, say. The run used to swallow that refusal and start
/// from an **empty** history: the whole conversation was lost, which
/// is worse than the situation the refusal replaced. The run now
/// falls back to the lenient fold, which skips the refused
/// compaction and replays the span it shadowed — so the model sees
/// the history the log actually supports.
#[tokio::test]
async fn a_log_that_does_not_fold_strictly_still_seeds_the_run_with_its_history()
 {
    let calls = Arc::new(Mutex::new(Vec::<AgentInput>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(RecordingFactory::new(
            Arc::clone(&calls),
            vec![AgentEvent::Model(ContentPart::Text(TextContent {
                text: "ack".to_string(),
                cache_control: None,
            }))],
            vec![],
        ));
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // Prime the log with two turns and a compaction that cites the
    // wrong rows: the span [0, 1) covers the user turn (seq 1), but
    // the citation names seq 2 — the assistant turn. The strict fold
    // refuses it; the lenient fold skips it and replays the span.
    let sink = manager.sink("alice", "s1");
    sink.alloc_seq();
    sink.append(&serde_json::json!({
        "type": "UserInput",
        "seq": 1,
        "data": {"text": "hello"},
    }))
    .await
    .unwrap();
    sink.alloc_seq();
    sink.append(&serde_json::json!({
        "type": "Model",
        "seq": 2,
        "data": {"type": "text", "text": "working"},
    }))
    .await
    .unwrap();
    sink.alloc_seq();
    sink.append(&serde_json::json!({
        "type": "compaction",
        "seq": 3,
        "ts": "2026-09-12T00:00:00Z",
        "surface_op": {"start": 0, "end": 1, "source_event_seqs": [2]},
        "data": {"summary": "compacted turn"},
    }))
    .await
    .unwrap();

    controller
        .submit(SessionOp::Prompt {
            content: "carry on".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.lock().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the run factory was not invoked in time");
    let recorded = calls.lock();
    let history = &recorded[0].history;
    let flattened = format!("{history:?}");
    assert!(
        flattened.contains("hello"),
        "the run must still see the conversation the log holds; got {flattened}"
    );
    assert!(
        flattened.contains("working"),
        "the replayed span must reach the model; got {flattened}"
    );
}

/// A refused fold must be visible to the client — and visible once.
///
/// The session's log is the client's view of the session: a refusal
/// the client cannot see is a refusal it cannot act on, because the
/// run looks like any other. The run records the refusal as a
/// `warning` row, once per session: the refusal belongs to the
/// *log*, not to the run, so a later run that is refused the same
/// way must not write the fact again.
///
/// The row does not move the surface either. `warning` is not one of
/// the row kinds the fold projects, so the history the model sees is
/// the history the log had before the row landed.
#[tokio::test]
async fn a_refused_fold_is_recorded_once_in_the_session_log() {
    let calls = Arc::new(Mutex::new(Vec::<AgentInput>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(RecordingFactory::new(
            Arc::clone(&calls),
            vec![AgentEvent::Model(ContentPart::Text(TextContent {
                text: "ack".to_string(),
                cache_control: None,
            }))],
            vec![AgentEvent::Model(ContentPart::Text(TextContent {
                text: "ack again".to_string(),
                cache_control: None,
            }))],
        ));
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // Prime the log with two turns and a compaction that cites the
    // wrong rows: the span [0, 1) covers the user turn (seq 1), but
    // the citation names seq 2 — the assistant turn. The strict fold
    // refuses it.
    let sink = manager.sink("alice", "s1");
    sink.alloc_seq();
    sink.append(&serde_json::json!({
        "type": "UserInput",
        "seq": 1,
        "data": {"text": "hello"},
    }))
    .await
    .unwrap();
    sink.alloc_seq();
    sink.append(&serde_json::json!({
        "type": "Model",
        "seq": 2,
        "data": {"type": "text", "text": "working"},
    }))
    .await
    .unwrap();
    sink.alloc_seq();
    sink.append(&serde_json::json!({
        "type": "compaction",
        "seq": 3,
        "ts": "2026-09-12T00:00:00Z",
        "surface_op": {"start": 0, "end": 1, "source_event_seqs": [2]},
        "data": {"summary": "compacted turn"},
    }))
    .await
    .unwrap();

    controller
        .submit(SessionOp::Prompt {
            content: "carry on".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while calls.lock().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the run factory was not invoked in time");

    // The refusal is in the log, naming its kind and saying why.
    let rows = wait_for_log_row(&manager, FOLD_REFUSAL_KIND).await;
    let warnings: Vec<_> =
        rows.iter().filter(|row| is_fold_refusal(row)).collect();
    assert_eq!(
        warnings.len(),
        1,
        "the refusal is recorded once; log: {rows:?}"
    );
    let data = warnings[0]
        .get("data")
        .expect("the warning row carries its data");
    assert_eq!(
        data.get("kind").and_then(serde_json::Value::as_str),
        Some(FOLD_REFUSAL_KIND),
        "the row names the refusal's kind"
    );
    let message = data
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(!message.is_empty(), "the row says why the fold was refused");

    // The row is log-only: the surface the model sees is the surface
    // the log had without it.
    let without: Vec<serde_json::Value> = rows
        .iter()
        .filter(|row| !is_fold_refusal(row))
        .cloned()
        .collect();
    let folded_with = uva::session::fold_log_surface(&rows);
    let folded_without = uva::session::fold_log_surface(&without);
    assert_eq!(
        folded_with.messages, folded_without.messages,
        "a warning row must not change the folded surface"
    );
    assert_eq!(
        folded_with.surface_seqs, folded_without.surface_seqs,
        "a warning row must not renumber the surface"
    );

    // A second run is refused the same way — and writes no second
    // row: the refusal is a property of the log.
    controller
        .submit(SessionOp::Prompt {
            content: "carry on again".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while calls.lock().len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the second run was not invoked in time");
    // The fold happens before the factory is invoked, so the second
    // run has already been refused by the time its input is
    // recorded. Give the row a moment to land, then count.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = sink.read().await.unwrap();
    let warnings = rows.iter().filter(|row| is_fold_refusal(row)).count();
    assert_eq!(
        warnings, 1,
        "the refusal is recorded once per session, not once per run; \
         log: {rows:?}"
    );
}

/// The `kind` slug the run records a refused fold under.
///
/// Spelled out here on purpose: the slug is a contract with the
/// client, so the test pins its value instead of importing the
/// constant the run uses — a slug that changed under the test would
/// change the contract without changing the test.
const FOLD_REFUSAL_KIND: &str = "strict_fold_refused";

/// Whether the row is the warning that records a refused fold.
fn is_fold_refusal(row: &serde_json::Value) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("warning")
        && row
            .get("data")
            .and_then(|data| data.get("kind"))
            .and_then(serde_json::Value::as_str)
            == Some(FOLD_REFUSAL_KIND)
}

/// A sink that counts the whole-log reads.
struct CountingSink {
    inner: Arc<dyn uva::session::SessionSink>,
    reads: std::sync::atomic::AtomicUsize,
}

impl CountingSink {
    fn new(inner: Arc<dyn uva::session::SessionSink>) -> Self {
        Self {
            inner,
            reads: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl uva::session::SessionSink for CountingSink {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn alloc_seq(&self) -> u64 {
        self.inner.alloc_seq()
    }

    async fn append(
        &self,
        event: &serde_json::Value,
    ) -> Result<(), uva::session::SessionError> {
        self.inner.append(event).await
    }

    async fn read(
        &self,
    ) -> Result<Vec<serde_json::Value>, uva::session::SessionError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.read().await
    }

    async fn read_from(
        &self,
        from: u64,
    ) -> Result<Vec<serde_json::Value>, uva::session::SessionError> {
        self.inner.read_from(from).await
    }

    async fn read_tail(
        &self,
        window: usize,
    ) -> Result<Vec<serde_json::Value>, uva::session::SessionError> {
        self.inner.read_tail(window).await
    }

    async fn snapshot(
        &self,
    ) -> Result<uva::session::SessionSnapshot, uva::session::SessionError> {
        self.inner.snapshot().await
    }

    async fn close(
        &self,
        reason: uva::session::SessionEndReason,
    ) -> Result<(), uva::session::SessionError> {
        self.inner.close(reason).await
    }
}

/// A turn whose log has not moved reads nothing.
///
/// Every turn re-reads the session's log to rebuild the model's
/// history; that read is the whole log, so a session paid for its own
/// length on every turn. The controller now keeps the folded history
/// and re-uses it when the log's ordinal says the log is the same one
/// it folded — so a turn that arrives on an unchanged log reads
/// nothing, and the model sees the same history the last turn saw.
#[tokio::test]
async fn a_turn_on_an_unchanged_log_reads_nothing() {
    let calls = Arc::new(Mutex::new(Vec::<AgentInput>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(RecordingFactory::new(
            Arc::clone(&calls),
            vec![AgentEvent::Model(ContentPart::Text(TextContent {
                text: "first reply".to_string(),
                cache_control: None,
            }))],
            vec![AgentEvent::Model(ContentPart::Text(TextContent {
                text: "second reply".to_string(),
                cache_control: None,
            }))],
        ));
    let deps = super::support::test_deps();
    let temp = tempfile::TempDir::new().unwrap();
    let manager =
        super::support::FixtureRegistry::new(temp.path().to_path_buf());
    manager
        .create_with_user("s1".to_string(), "alice".to_string())
        .await
        .unwrap();
    let sink = Arc::new(CountingSink::new(manager.sink("alice", "s1")));
    let controller = super::super::SessionController::spawn(
        "alice",
        "s1",
        manager.writer("alice", "s1"),
        deps,
        Duration::from_secs(60),
        factory,
    );

    // Turn 1: the log is read and folded.
    controller
        .submit(SessionOp::Prompt {
            content: "turn one".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.state() != SessionState::Idle {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first run settles");
    let reads_after_turn_one = sink.reads();
    let first_history = calls.lock().last().map(|input| input.history.clone());

    // The second turn runs on the same log: the ordinal the cached
    // fold was taken at is moved to the log's current one, which is
    // what a turn on an unchanged log sees.
    use uva::session::SessionSink as _;
    let ordinal = sink.snapshot().await.expect("a snapshot").last_event_seq;
    if let Some(inner) = controller.inner.as_ref()
        && let Some(cached) = inner.cached_history.lock().as_mut()
    {
        cached.last_event_seq = ordinal;
    }

    controller
        .submit(SessionOp::Prompt {
            content: "turn two".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.state() != SessionState::Idle {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the second run settles");

    assert_eq!(
        sink.reads(),
        reads_after_turn_one,
        "a turn whose log has not moved must not read the log again"
    );
    let second_history = calls.lock().last().map(|input| input.history.clone());
    assert_eq!(
        first_history, second_history,
        "an unchanged log must yield the same history as the last fold"
    );
}
