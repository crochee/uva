//! Persist + broadcast of agent events.
//!
//! Pins the durable / ephemeral event split (after the
//! panel/session refactor, only `durable` events land in the
//! JSONL sink; system markers broadcast to subscribers but do
//! not get persisted) and the R4 Phase C.3 `request_header`
//! epoch marker (emitted once per `(provider, model,
//! tools_hash)` tuple).

use std::{sync::Arc, time::Duration};

use parking_lot::Mutex;
use synthia::{
    harness::{SessionEndReason, SystemEvent},
    provider::{ContentPart, TextContent},
};

use super::{
    super::{AgentEvent, SessionOp, SessionState},
    support::{RecordedCall, VecFactory, make_manager_and_controller},
};

/// A run leaves exactly one `request_header` row. The header is the
/// harness's epoch marker — the server no longer stamps a second one
/// before the run's prompts — so a replay reads one story about the
/// span the run produced.
#[tokio::test]
async fn a_run_leaves_exactly_one_request_header() {
    let mut deps = super::support::test_deps();
    deps.provider = Arc::new(synthia::test_support::FakeProvider::new(vec![
        synthia::provider::CompletionResponse {
            content: synthia::provider::Content::text("first answer"),
            ..Default::default()
        },
        synthia::provider::CompletionResponse {
            content: synthia::provider::Content::text("second answer"),
            ..Default::default()
        },
    ]));
    let (controller, manager, _temp) =
        super::support::make_manager_and_controller_with_deps(
            Duration::from_secs(60),
            Arc::new(super::super::AgentRunStreamFactory),
            deps,
        )
        .await;

    controller
        .submit(SessionOp::Prompt {
            content: "go".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    // The answer's row is the last thing the run writes: by the time
    // it is readable, the run stamped its header.
    super::support::wait_for_log_row(&manager, "first answer").await;

    let rows: Vec<serde_json::Value> =
        manager.sink("alice", "s1").read().await.unwrap();
    let headers: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["type"] == "request_header")
        .collect();
    assert_eq!(
        headers.len(),
        1,
        "a run leaves one request_header row, not two; got {rows:?}"
    );
    assert_eq!(
        headers[0]["data"]["reason"], "initial",
        "the session's first run is an initial epoch"
    );

    // A second run stamps its own header, and says the envelope did
    // not change.
    controller
        .submit(SessionOp::Prompt {
            content: "again".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    super::support::wait_for_log_row(&manager, "second answer").await;

    let rows: Vec<serde_json::Value> =
        manager.sink("alice", "s1").read().await.unwrap();
    let headers: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["type"] == "request_header")
        .collect();
    assert_eq!(headers.len(), 2, "each run stamps one header; got {rows:?}");
    assert_eq!(
        headers[1]["data"]["reason"], "same",
        "an unchanged request envelope says so"
    );
}

#[tokio::test]
async fn test_events_are_persisted_and_broadcast() {
    // MVP: all events are durable. Persist every event the
    // factory emits, then verify both broadcast and persistence
    // observed them.
    let events = vec![
        AgentEvent::Model(ContentPart::Text(TextContent {
            text: "hi".to_string(),
            cache_control: None,
        })),
        AgentEvent::System(SystemEvent::SessionStarted {
            session_id: "s1".to_string(),
        }),
        AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        }),
    ];
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(events, Arc::clone(&calls), None));
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // Subscribe before the run starts so events are broadcast.
    let mut rx = controller.subscribe();

    controller
        .submit(SessionOp::Prompt {
            content: "go".to_string(),
            priority: 1,
        })
        .await
        .unwrap();

    // Wait for the run to start.
    tokio::time::timeout(Duration::from_millis(500), async {
        while calls.lock().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    // Drain the queue so the controller does not immediately restart
    // the run, then wait for the run to finish.
    let _ = manager.input_queue().drain_pending("alice", "s1").await;
    tokio::time::timeout(Duration::from_millis(500), async {
        while controller.state() != SessionState::Idle {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    let persisted: Vec<serde_json::Value> =
        manager.sink("alice", "s1").read().await.unwrap();
    let type_of = |e: &serde_json::Value| -> String {
        e.get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    // Since the writer unification, `Model` events are ephemeral
    // on the wire: the model-visible facts are written to the
    // log by the run itself, through the session's writer — one
    // `assistant_message` row per turn (the fake factory mimics
    // the harness's write). The run task also appends a synthetic
    // `UserInput` envelope at run start. The `request_header` is
    // the harness's row (the fake factory runs no harness), so the
    // JSONL holds exactly two records here: this run's `UserInput`
    // and the `assistant_message` row the factory wrote.
    assert_eq!(
        persisted.len(),
        2,
        "harness-written turn + the run's prompt are persisted; got {persisted:?}"
    );
    assert!(
        persisted.iter().any(|e| type_of(e) == "UserInput"),
        "UserInput envelope from the prompt handler should be persisted"
    );
    assert!(
        persisted.iter().any(|e| type_of(e) == "assistant_message"),
        "the turn's assistant_message row must be persisted"
    );

    // The request header belongs to the harness: a factory that
    // runs no harness writes no header, so a fake run cannot claim
    // an epoch it never entered.
    assert!(
        !persisted.iter().any(|e| type_of(e) == "request_header"),
        "the request header is the harness's row; got {persisted:?}"
    );

    let received = tokio::time::timeout(Duration::from_millis(200), rx.recv())
        .await
        .unwrap()
        .unwrap();
    // The first broadcast event is the Message event (emitted first
    // by the factory).
    assert!(matches!(received, AgentEvent::Model(ContentPart::Text(_))));
}
