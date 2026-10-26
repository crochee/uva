//! State-machine transitions: concurrent prompt dedup,
//! idle-timeout shutdown, post-shutdown error contract,
//! `OperationSnapshot` bus, idempotent `close()`.

use std::{
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};

use parking_lot::Mutex;

use super::{
    super::{SessionOp, SessionState},
    support::{
        BlockingFactory,
        RecordedCall,
        VecFactory,
        make_manager_and_controller,
        wait_for_runs,
    },
};

#[tokio::test]
async fn test_two_concurrent_prompts_spawn_one_run() {
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(BlockingFactory {
            calls: Arc::clone(&calls),
            progress_count: Arc::new(AtomicUsize::new(0)),
        });
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    controller
        .submit(SessionOp::Prompt {
            content: "hello".to_string(),
            priority: 1,
        })
        .await
        .unwrap();

    // Wait until the run is definitely active before
    // submitting the second prompt — synchronized on the
    // factory invocation (see `wait_for_runs`).
    wait_for_runs(&calls, 1).await;

    controller
        .submit(SessionOp::Prompt {
            content: "world".to_string(),
            priority: 1,
        })
        .await
        .unwrap();

    // Wait until the controller has processed the second
    // prompt (observable as a queued input) — a fixed
    // sleep could read `calls` before the op was even
    // handled, which is what made this test racy.
    tokio::time::timeout(Duration::from_secs(2), async {
        while !manager.input_queue().has_pending("alice", "s1").await {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("second prompt was not queued in time");

    assert_eq!(calls.lock().len(), 1);

    controller.cancel().await.unwrap();
    tokio::time::timeout(Duration::from_millis(500), async {
        while controller.state() != SessionState::Idle
            && controller.state() != SessionState::Cancelled
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_shutdown_after_idle_timeout() {
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(vec![], Arc::clone(&calls), None));
    let (controller, _manager, _temp) =
        make_manager_and_controller(Duration::from_millis(50), factory).await;

    assert!(controller.is_alive());
    // Poll `is_alive()` rather than sleep a fixed duration:
    // under heavy CI load (495 parallel tests) the tokio
    // scheduler can take longer than 120 ms to fire the
    // idle timer — the fixed sleep then races the loop's
    // break-and-store-false, and the assertion sees `true`.
    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.is_alive() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("controller did not shut down after idle timeout");
    assert!(!controller.is_alive());
}

/// After `idle_timeout` fires and the controller shuts
/// down, a new `submit()` call MUST return
/// `Err` with the documented "session controller is
/// shut down" context — NOT silently drop the op, NOT
/// panic. This is the contract the controller
/// depends on: when `get_or_create_session_controller`
/// fails (because the prior controller just shut down
/// between the call and the `submit`), the error
/// path bubbles up and the chat client sees a 5xx
/// instead of an empty 200 OK.
///
/// Without this contract, a previously-shut-down
/// controller could be silently reused as if it were
/// alive, leaking the queued op into a stale run.
#[tokio::test]
async fn test_submit_after_shutdown_returns_error() {
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(vec![], Arc::clone(&calls), None));
    let (controller, _manager, _temp) =
        make_manager_and_controller(Duration::from_millis(50), factory).await;

    // Wait for idle_timeout to fire and the controller
    // to drop its op_tx receiver. Poll `is_alive()` — a
    // fixed sleep races against the tokio scheduler under
    // CI load (see `test_shutdown_after_idle_timeout`).
    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.is_alive() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("controller did not shut down after idle timeout");
    assert!(!controller.is_alive());

    // Now submit MUST fail.
    let result = controller
        .submit(SessionOp::Prompt {
            content: "after-shutdown".to_string(),
            priority: 1,
        })
        .await;
    assert!(
        result.is_err(),
        "submit after shutdown must return Err, got Ok"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("shut down"),
        "error must include the documented context; got: {err_msg}"
    );
    // No factory calls were made — the post-shutdown
    // op never reached the run loop.
    assert_eq!(calls.lock().len(), 0);
}

/// R29-Phase-I: `close()` stops the loop, emits the terminal
/// `lifecycle_shutdown` event, and is idempotent.
#[tokio::test]
async fn close_emits_lifecycle_shutdown_and_is_idempotent() {
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    // A blocking run so a live run is genuinely in flight
    // when `close()` lands — the shutdown path must cancel
    // it, not just stop an idle loop.
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(BlockingFactory {
            calls: Arc::clone(&calls),
            progress_count: Arc::new(AtomicUsize::new(0)),
        });
    let (controller, manager, temp) =
        make_manager_and_controller(Duration::from_secs(300), factory).await;

    controller
        .submit(SessionOp::Prompt {
            content: "long run".to_string(),
            priority: 0,
        })
        .await
        .unwrap();
    wait_for_runs(&calls, 1).await;

    // First close: stops the loop and finalises the log.
    controller.close().await.unwrap();
    // Second close: a no-op that still resolves.
    controller.close().await.unwrap();

    assert!(
        !controller.is_alive(),
        "the controller loop must have exited after close()"
    );

    // The durable log must carry exactly one terminal
    // `lifecycle_shutdown` event.
    let events_path = super::support::find_events_jsonl(temp.path())
        .expect("events.jsonl must exist after close");
    let body = std::fs::read_to_string(&events_path).unwrap();
    let shutdowns = body
        .lines()
        .filter(|l| l.contains("\"type\":\"session_shutdown\""))
        .count();
    assert_eq!(
        shutdowns, 1,
        "exactly one lifecycle_shutdown event must be appended; log:\n{body}"
    );
    // The sink is closed — a further append must fail.
    let sink = manager.sink("alice", "s1");
    assert!(
        sink.append(&serde_json::json!({"x": 1})).await.is_err(),
        "append after close must fail"
    );
}

/// R12 closure: `subscribe_snapshots()` returns the latest
/// snapshot so a late subscriber learns the current state
/// without waiting for the next transition — the property
/// `GET /sessions/{id}/status` depends on.
#[test]
fn subscribe_snapshots_latest_is_initially_none() {
    let bus = synthia::session::SnapshotBus::new();
    let (_rx, latest) = bus.subscribe();
    assert!(latest.is_none());
    let _seq = bus.publish(synthia::session::OperationSnapshot::started(
        "s1", "default", 25,
    ));
    let (_rx2, latest2) = bus.subscribe();
    let snap = latest2.expect("latest after publish");
    assert_eq!(snap.session_id, "s1");
    assert_eq!(snap.agent_name, "default");
    assert_eq!(snap.max_iterations, 25);
}

/// R11: the controller publishes `OperationSnapshot`s on the
/// shared bus at run-state transitions. A subscriber must
/// observe `Running` when a run starts and `Completing` when
/// the factory stream ends — without touching the run task
/// internals.
#[tokio::test]
async fn snapshot_bus_publishes_running_then_completing() {
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory = Arc::new(VecFactory::new(
        vec![super::super::AgentEvent::text_delta("done")],
        calls.clone(),
        None,
    ));
    let (controller, _manager, _temp) =
        make_manager_and_controller(Duration::from_secs(300), factory).await;

    // Subscribe BEFORE the run so we observe both transitions.
    let (mut rx, latest) = controller.subscribe_snapshots();
    assert!(latest.is_none(), "fresh bus has no snapshot yet");

    controller
        .submit(SessionOp::Prompt {
            content: "hello".to_string(),
            priority: 0,
        })
        .await
        .unwrap();

    // Wait for the factory to fire (run started) then for the
    // snapshots to land on the bus.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if controller.subscribe_snapshots().1.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("snapshot bus never received the Running publish");

    // Drain the receiver: Running first, then Completing.
    let first = rx.next().await.expect("Running snapshot");
    assert_eq!(
        first.state,
        synthia::session::OperationState::Running { iteration: 0 }
    );
    let second = rx.next().await.expect("Completing snapshot");
    assert_eq!(second.state, synthia::session::OperationState::Completing);
    assert_eq!(first.session_id, "s1");
    assert_eq!(second.session_id, "s1");
}

/// A resume resubmitted after the log applied it is a no-op.
///
/// The token is derived from the log — the last durable seq plus the
/// call ids still undecided — so a second submission of the same token
/// cannot match the log once the decisions are rows. That is what makes
/// the client's retry safe after a takeover, where the server
/// deliberately remembers no decision (§5.11): the retry must not run
/// the turn a second time.
#[tokio::test]
async fn a_resubmitted_resume_is_a_no_op() {
    use synthia::harness::{ResumeDecision, ResumeStatus, ResumeToken};

    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(
            vec![super::super::AgentEvent::System(
                synthia::harness::SystemEvent::SessionEnded {
                    reason: synthia::harness::SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
            None,
        ));
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // The log a suspension leaves: one high-risk call asked, the turn
    // closed as suspended.
    let writer = manager.writer("alice", "s1");
    writer
        .append_event(&synthia::session::turn_start(0))
        .await
        .unwrap();
    writer
        .append_event(&synthia::session::approval_asked(
            "a1",
            "c1",
            "shell",
            "sha256:args",
            "high",
            "default",
        ))
        .await
        .unwrap();
    writer
        .append_event(&synthia::session::turn_end_suspended())
        .await
        .unwrap();
    writer.flush().await.unwrap();

    let token = ResumeToken::new(3, vec!["c1".to_string()], "sha256:env");
    let decisions = vec![ResumeDecision {
        id: "a1".to_string(),
        status: ResumeStatus::Resolved,
        payload: None,
    }];

    controller
        .submit(SessionOp::Resume {
            token: token.clone(),
            decisions: decisions.clone(),
            allow_drift: true,
        })
        .await
        .unwrap();
    wait_for_runs(&calls, 1).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.state() == SessionState::Running {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the resumed run settles");

    // The decisions the resumed run writes — what makes the token spent.
    writer
        .append_event(&synthia::session::approval_decided(
            "a1", "c1", "resolved", "resume", None,
        ))
        .await
        .unwrap();
    writer.flush().await.unwrap();
    let rows = manager.sink("alice", "s1").read().await.unwrap();
    assert!(
        synthia::harness::open_requests(&rows).is_empty(),
        "the log's open set is empty again"
    );

    // The client re-sends; nothing may run again.
    controller
        .submit(SessionOp::Resume {
            token,
            decisions,
            allow_drift: true,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        calls.lock().len(),
        1,
        "a resubmitted resume must not run the turn a second time"
    );
    assert_ne!(
        controller.state(),
        SessionState::Running,
        "and must not park a resume that starts a later run"
    );
}

/// A sink whose log cannot be read: appends land in memory, reads fail.
struct UnreadableSink {
    inner: synthia::session::in_memory::InMemorySessionSink,
}

#[async_trait::async_trait]
impl synthia::session::SessionSink for UnreadableSink {
    fn id(&self) -> &str {
        synthia::session::SessionSink::id(&self.inner)
    }

    fn alloc_seq(&self) -> u64 {
        synthia::session::SessionSink::alloc_seq(&self.inner)
    }

    async fn append(
        &self,
        event: &serde_json::Value,
    ) -> Result<(), synthia::session::SessionError> {
        self.inner.append(event).await
    }

    async fn read(
        &self,
    ) -> Result<Vec<serde_json::Value>, synthia::session::SessionError> {
        Err(synthia::session::SessionError::ReadFailed(
            "the log is unreadable".to_string(),
        ))
    }

    async fn read_from(
        &self,
        _from: u64,
    ) -> Result<Vec<serde_json::Value>, synthia::session::SessionError> {
        Err(synthia::session::SessionError::ReadFailed(
            "the log is unreadable".to_string(),
        ))
    }

    async fn snapshot(
        &self,
    ) -> Result<synthia::session::SessionSnapshot, synthia::session::SessionError>
    {
        self.inner.snapshot().await
    }

    async fn close(
        &self,
        reason: synthia::session::SessionEndReason,
    ) -> Result<(), synthia::session::SessionError> {
        self.inner.close(reason).await
    }
}

/// An unreadable log is not an *unpaused* one either.
///
/// `maybe_start_run` refuses a fresh run while the log holds an open
/// interrupt. A failed read used to look like "nothing is open", so a
/// prompt arriving after a crash could start a run straight over a turn
/// that was waiting for approval — the same swallow as the resume path,
/// one caller over. The run is refused and the prompt stays queued.
#[tokio::test]
async fn an_unreadable_log_refuses_a_fresh_run() {
    use std::sync::Arc;

    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(
            vec![super::super::AgentEvent::System(
                synthia::harness::SystemEvent::SessionEnded {
                    reason: synthia::harness::SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
            None,
        ));
    let sink: Arc<dyn synthia::session::SessionSink> =
        Arc::new(UnreadableSink {
            inner: synthia::session::in_memory::InMemorySessionSink::new("s1"),
        });
    let writer = Arc::new(synthia::session::LogWriter::new(
        Arc::clone(&sink),
        Arc::new(synthia::core::SystemClock),
    ));
    let manager = super::support::FixtureRegistry::new(
        tempfile::TempDir::new().expect("a temp dir").keep(),
    );
    let controller = super::super::SessionController::spawn(
        "alice",
        "s1",
        manager.input_queue(),
        sink,
        writer,
        super::support::test_deps(),
        Duration::from_secs(60),
        factory,
    );

    controller
        .submit(SessionOp::Prompt {
            content: "run over the pause".to_string(),
            priority: 0,
        })
        .await
        .expect("the prompt is delivered");
    // The op loop is asynchronous: give the start decision time to make
    // its own call.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        calls.lock().len(),
        0,
        "the run must not start on an approval state it could not read"
    );
    assert_eq!(
        controller.state(),
        SessionState::Idle,
        "and the controller does not record a pause it cannot prove"
    );
}

/// An unreadable log is not a decided one.
///
/// `is_spent_resume` reads "no open interrupt names the token's calls"
/// as *already answered*. If a read failure produced the same empty set,
/// a legitimate resume would be dropped after the route had already
/// answered `202` — the client waits for a run that never starts.
#[tokio::test]
async fn an_unreadable_log_is_not_a_spent_resume() {
    use std::sync::Arc;

    use synthia::harness::{ResumeDecision, ResumeStatus, ResumeToken};

    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(
            vec![super::super::AgentEvent::System(
                synthia::harness::SystemEvent::SessionEnded {
                    reason: synthia::harness::SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
            None,
        ));
    let sink: Arc<dyn synthia::session::SessionSink> =
        Arc::new(UnreadableSink {
            inner: synthia::session::in_memory::InMemorySessionSink::new("s1"),
        });
    let writer = Arc::new(synthia::session::LogWriter::new(
        Arc::clone(&sink),
        Arc::new(synthia::core::SystemClock),
    ));
    let manager = super::support::FixtureRegistry::new(
        tempfile::TempDir::new().expect("a temp dir").keep(),
    );
    let controller = super::super::SessionController::spawn(
        "alice",
        "s1",
        manager.input_queue(),
        sink,
        writer,
        super::support::test_deps(),
        Duration::from_secs(60),
        factory,
    );

    controller
        .submit(SessionOp::Resume {
            token: ResumeToken::new(3, vec!["c1".to_string()], "sha256:env"),
            decisions: vec![ResumeDecision {
                id: "a1".to_string(),
                status: ResumeStatus::Resolved,
                payload: None,
            }],
            allow_drift: true,
        })
        .await
        .expect("the resume is delivered");

    wait_for_runs(&calls, 1).await;
    assert_eq!(
        calls.lock().len(),
        1,
        "the resume must run rather than be mistaken for a spent token"
    );
}
