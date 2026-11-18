//! State-machine transitions: concurrent prompt dedup,
//! idle-timeout shutdown, post-shutdown error contract,
//! `OperationSnapshot` bus, idempotent `close()`, and the
//! cache's refusal to hand back a controller whose loop exited.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use uva::harness::{ResumeDecision, ResumeStatus, ResumeToken};

use super::{
    super::SessionOp,
    support::{
        BlockingFactory,
        CancelObservingFactory,
        RecordedCall,
        VecFactory,
        make_manager_and_controller,
        wait_for_runs,
    },
};
use crate::session::status::SessionState;

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
    // prompt (observable as its durable park row) — a fixed
    // sleep could read `calls` before the op was even
    // handled, which is what made this test racy.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let rows =
                manager.sink("alice", "s1").read().await.unwrap_or_default();
            if rows.iter().any(|row| {
                row["type"] == "parked_prompt" && row["text"] == "world"
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("second prompt was not parked in time");

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
        Arc::new(VecFactory::new(vec![], Arc::clone(&calls)));
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
        Arc::new(VecFactory::new(vec![], Arc::clone(&calls)));
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
    let bus = uva::session::SnapshotBus::new();
    let (_rx, latest) = bus.subscribe();
    assert!(latest.is_none());
    let _seq = bus.publish(uva::session::OperationSnapshot::started(
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
/// observe `Running` when a run starts, then `Completing` (the
/// run's outcome) and `Idle` (the session's now) when it ends —
/// without touching the run task internals. `latest` reports
/// the present, which is what `delete_session`'s busy check
/// reads.
#[tokio::test]
async fn snapshot_bus_publishes_running_completing_then_idle() {
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory = Arc::new(VecFactory::new(
        vec![super::super::AgentEvent::text_delta("done")],
        calls.clone(),
    ));
    let (controller, _manager, _temp) =
        make_manager_and_controller(Duration::from_secs(300), factory).await;

    // Subscribe BEFORE the run so we observe every transition.
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

    // Drain the receiver: Running, then Completing, then Idle.
    let first = rx.next().await.expect("Running snapshot");
    assert_eq!(
        first.state,
        uva::session::OperationState::Running { iteration: 0 }
    );
    let second = rx.next().await.expect("Completing snapshot");
    assert_eq!(second.state, uva::session::OperationState::Completing);
    let third = rx.next().await.expect("Idle snapshot");
    assert_eq!(third.state, uva::session::OperationState::Idle);
    assert_eq!(first.session_id, "s1");
    assert_eq!(second.session_id, "s1");
    assert_eq!(third.session_id, "s1");
    // `latest` reports the present — not the last run's residue.
    let (_, latest) = controller.subscribe_snapshots();
    assert_eq!(
        latest.expect("a settled bus holds a frame").state,
        uva::session::OperationState::Idle
    );
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
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(
            vec![super::super::AgentEvent::System(
                uva::harness::SystemEvent::SessionEnded {
                    reason: uva::harness::SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
        ));
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // The log a suspension leaves: one high-risk call asked, the turn
    // closed as suspended.
    let writer = manager.writer("alice", "s1");
    writer
        .append_event(&uva::session::turn_start(0))
        .await
        .unwrap();
    writer
        .append_event(&uva::session::approval_asked(
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
        .append_event(&uva::session::turn_end_suspended())
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
        .append_event(&uva::session::approval_decided(
            "a1", "c1", "resolved", "resume", None,
        ))
        .await
        .unwrap();
    writer.flush().await.unwrap();
    let rows = manager.sink("alice", "s1").read().await.unwrap();
    assert!(
        uva::harness::open_requests(&rows).is_empty(),
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
    inner: uva::session::in_memory::InMemorySessionSink,
}

#[async_trait::async_trait]
impl uva::session::SessionSink for UnreadableSink {
    fn id(&self) -> &str {
        uva::session::SessionSink::id(&self.inner)
    }

    fn alloc_seq(&self) -> u64 {
        uva::session::SessionSink::alloc_seq(&self.inner)
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
        Err(uva::session::SessionError::ReadFailed(
            "the log is unreadable".to_string(),
        ))
    }

    async fn read_from(
        &self,
        _from: u64,
    ) -> Result<Vec<serde_json::Value>, uva::session::SessionError> {
        Err(uva::session::SessionError::ReadFailed(
            "the log is unreadable".to_string(),
        ))
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
                uva::harness::SystemEvent::SessionEnded {
                    reason: uva::harness::SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
        ));
    let sink: Arc<dyn uva::session::SessionSink> = Arc::new(UnreadableSink {
        inner: uva::session::in_memory::InMemorySessionSink::new("s1"),
    });
    let writer = Arc::new(uva::session::LogWriter::new(
        Arc::clone(&sink),
        Arc::new(uva::core::SystemClock),
    ));
    let _manager = super::support::FixtureRegistry::new(
        tempfile::TempDir::new().expect("a temp dir").keep(),
    );
    let controller = super::super::SessionController::spawn(
        "alice",
        "s1",
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
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(
            vec![super::super::AgentEvent::System(
                uva::harness::SystemEvent::SessionEnded {
                    reason: uva::harness::SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
        ));
    let sink: Arc<dyn uva::session::SessionSink> = Arc::new(UnreadableSink {
        inner: uva::session::in_memory::InMemorySessionSink::new("s1"),
    });
    let writer = Arc::new(uva::session::LogWriter::new(
        Arc::clone(&sink),
        Arc::new(uva::core::SystemClock),
    ));
    let _manager = super::support::FixtureRegistry::new(
        tempfile::TempDir::new().expect("a temp dir").keep(),
    );
    let controller = super::super::SessionController::spawn(
        "alice",
        "s1",
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
/// A controller whose loop has exited must never come back out of
/// the cache: the next request for the session builds a fresh
/// controller instead of reusing the dead one, and the cache holds
/// no dead entry — not after the exit itself, and not after a hit
/// on a dead entry the exit notification could not have removed.
#[tokio::test]
async fn a_dead_controller_is_not_reused() {
    let dir = tempfile::TempDir::new().expect("a temp workspace");
    let manager =
        uva::session::manager::SessionRegistry::new(dir.path().to_path_buf());
    let state = Arc::new(
        crate::state::AppState::for_test(manager, dir.path().to_path_buf())
            .await,
    );
    let key = ("u".to_string(), "s".to_string());

    let first = state
        .get_or_create_session_controller("u", "s")
        .await
        .expect("the session opens");
    assert!(first.is_alive());

    // Explicit shutdown. `close()` returns on its own bounded budget
    // even if the loop overruns it, so the flag is polled — not
    // read once off `close()`'s return — before the cache's state
    // is asserted below.
    first.close().await.expect("the controller closes");
    tokio::time::timeout(Duration::from_secs(2), async {
        while first.is_alive() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the controller did not exit within the close budget");

    // The exit notification removed the controller's own entry...
    assert!(
        state.active_sessions.get(&key).is_none(),
        "the exit notification leaves no dead entry behind"
    );

    // The reaper is the backstop for a dead entry no notification
    // removed — swept on demand here, on its 60 s cadence in a
    // booted server.
    state
        .active_sessions
        .insert(key.clone(), Arc::clone(&first));
    state.reap_dead_sessions();
    assert!(
        state.active_sessions.get(&key).is_none(),
        "the reaper sweeps a dead entry"
    );

    // ...and a cache hit on a dead entry no notification removed
    // (a handle an embedder cached itself) refuses it: the request
    // rebuilds instead of handing the dead controller back — the
    // case the follow stream's `!is_alive` break was waiting for.
    state
        .active_sessions
        .insert(key.clone(), Arc::clone(&first));
    let second = state
        .get_or_create_session_controller("u", "s")
        .await
        .expect("the session re-opens over the dead entry");
    assert!(
        !Arc::ptr_eq(&first, &second),
        "a dead controller must never be handed back"
    );
    assert!(second.is_alive(), "the rebuilt controller runs");

    let cached = state
        .active_sessions
        .get(&key)
        .map(|entry| Arc::clone(entry.value()));
    assert!(
        cached.is_some_and(|cached| Arc::ptr_eq(&cached, &second)),
        "the cache holds the live replacement, not the dead entry"
    );
}

/// A sink whose appends panic: the park of a `Prompt` flushes its
/// row through the writer *inside the op loop*, so an appending
/// panic unwinds the loop's own task — the corpse scenario the
/// `alive` flag's drop guard exists for.
struct PanickingSink {
    inner: uva::session::in_memory::InMemorySessionSink,
}

#[async_trait::async_trait]
impl uva::session::SessionSink for PanickingSink {
    fn id(&self) -> &str {
        uva::session::SessionSink::id(&self.inner)
    }

    fn alloc_seq(&self) -> u64 {
        uva::session::SessionSink::alloc_seq(&self.inner)
    }

    async fn append(
        &self,
        _event: &serde_json::Value,
    ) -> Result<(), uva::session::SessionError> {
        panic!("the sink was asked to append");
    }

    async fn read(
        &self,
    ) -> Result<Vec<serde_json::Value>, uva::session::SessionError> {
        self.inner.read().await
    }

    async fn read_from(
        &self,
        from: u64,
    ) -> Result<Vec<serde_json::Value>, uva::session::SessionError> {
        self.inner.read_from(from).await
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

/// A panicking op loop must not stay `alive` (review fix): the
/// cache hit's `is_alive` refusal and the reaper's sweep both key
/// off that flag, so a corpse that keeps it `true` would be handed
/// back to every request for the process's life. The drop guard in
/// `run_controller_loop` flips the flag on unwind.
#[tokio::test]
async fn a_panicking_loop_is_not_reused() {
    let dir = tempfile::TempDir::new().expect("a temp workspace");
    let manager =
        uva::session::manager::SessionRegistry::new(dir.path().to_path_buf());
    let state = Arc::new(
        crate::state::AppState::for_test(manager, dir.path().to_path_buf())
            .await,
    );
    let key = ("u".to_string(), "s".to_string());

    // A controller an embedder cached itself (no exit
    // notification of its own), whose loop dies of a panic: the
    // `Prompt`'s park flushes inside the loop and the sink panics.
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let sink: Arc<dyn uva::session::SessionSink> = Arc::new(PanickingSink {
        inner: uva::session::in_memory::InMemorySessionSink::new("s1"),
    });
    let writer = Arc::new(uva::session::LogWriter::new(
        Arc::clone(&sink),
        Arc::new(uva::core::SystemClock),
    ));
    let corpse = super::super::SessionController::spawn(
        "u",
        "s",
        writer,
        super::support::test_deps(),
        Duration::from_secs(60),
        Arc::new(VecFactory::new(vec![], calls)),
    );
    state
        .active_sessions
        .insert(key.clone(), Arc::clone(&corpse));

    corpse
        .submit(SessionOp::Prompt {
            content: "boom".to_string(),
            priority: 1,
        })
        .await
        .expect("the op is queued; the loop dies handling it");

    tokio::time::timeout(Duration::from_secs(2), async {
        while corpse.is_alive() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a panicking loop must not stay alive");

    // The reaper sweeps the corpse the flag now reports...
    state.reap_dead_sessions();
    assert!(
        state.active_sessions.get(&key).is_none(),
        "the reaper sweeps a panicking corpse"
    );

    // ...and the cache hit refuses it: the next request rebuilds.
    state
        .active_sessions
        .insert(key.clone(), Arc::clone(&corpse));
    let fresh = state
        .get_or_create_session_controller("u", "s")
        .await
        .expect("the session opens over the corpse");
    assert!(
        !Arc::ptr_eq(&corpse, &fresh),
        "a panicking corpse must never be handed back"
    );
    assert!(fresh.is_alive(), "the rebuilt controller runs");
}

/// Appends succeed until the **second** `parked_prompt` row, which
/// panics. The park of a `Prompt` flushes inside the op loop, so
/// the first queued prompt parks cleanly and starts its run; the
/// second — arriving while that run is in flight — unwinds the
/// loop's own task with a live run attached.
struct PanicsOnSecondParkSink {
    inner: uva::session::in_memory::InMemorySessionSink,
    parks: AtomicUsize,
}

#[async_trait::async_trait]
impl uva::session::SessionSink for PanicsOnSecondParkSink {
    fn id(&self) -> &str {
        uva::session::SessionSink::id(&self.inner)
    }

    fn alloc_seq(&self) -> u64 {
        uva::session::SessionSink::alloc_seq(&self.inner)
    }

    async fn append(
        &self,
        event: &serde_json::Value,
    ) -> Result<(), uva::session::SessionError> {
        if event.get("type").and_then(serde_json::Value::as_str)
            == Some("parked_prompt")
            && self.parks.fetch_add(1, Ordering::SeqCst) == 1
        {
            panic!("the second parked_prompt reached the sink");
        }
        self.inner.append(event).await
    }

    async fn read(
        &self,
    ) -> Result<Vec<serde_json::Value>, uva::session::SessionError> {
        self.inner.read().await
    }

    async fn read_from(
        &self,
        from: u64,
    ) -> Result<Vec<serde_json::Value>, uva::session::SessionError> {
        self.inner.read_from(from).await
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

/// The loop's death must stop the run it owned (review P2): the
/// run is a detached task, so an unwind that only flipped the
/// `alive` flag would leave an orphan appending to the session
/// log — and because a corpse is now rebuildable, the next request
/// could start a second run for the same turn beside it. The loop
/// exit guard fires the run's cancel token, synchronously, on its
/// way out.
#[tokio::test]
async fn a_panicking_loop_cancels_its_in_flight_run() {
    let dir = tempfile::TempDir::new().expect("a temp workspace");
    let manager =
        uva::session::manager::SessionRegistry::new(dir.path().to_path_buf());
    let state = Arc::new(
        crate::state::AppState::for_test(manager, dir.path().to_path_buf())
            .await,
    );
    let key = ("u".to_string(), "s".to_string());

    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let sink: Arc<dyn uva::session::SessionSink> =
        Arc::new(PanicsOnSecondParkSink {
            inner: uva::session::in_memory::InMemorySessionSink::new("s1"),
            parks: AtomicUsize::new(0),
        });
    let writer = Arc::new(uva::session::LogWriter::new(
        Arc::clone(&sink),
        Arc::new(uva::core::SystemClock),
    ));
    let corpse = super::super::SessionController::spawn(
        "u",
        "s",
        writer,
        super::support::test_deps(),
        Duration::from_secs(60),
        Arc::new(CancelObservingFactory {
            calls: Arc::clone(&calls),
            cancelled: Arc::clone(&cancelled),
        }),
    );
    state
        .active_sessions
        .insert(key.clone(), Arc::clone(&corpse));

    // The first prompt parks cleanly and starts a run that blocks
    // until its cancel token fires.
    corpse
        .submit(SessionOp::Prompt {
            content: "one".to_string(),
            priority: 1,
        })
        .await
        .expect("the first prompt is parked");
    wait_for_runs(&calls, 1).await;

    // The second prompt's park flush panics inside the loop, with
    // the run above still in flight.
    corpse
        .submit(SessionOp::Prompt {
            content: "two".to_string(),
            priority: 1,
        })
        .await
        .expect("the op is queued; the loop dies handling it");

    tokio::time::timeout(Duration::from_secs(2), async {
        while corpse.is_alive() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a panicking loop must not stay alive");

    // The orphan observed the token the loop's death fired — it
    // was stopped, not left appending beside a replacement.
    tokio::time::timeout(Duration::from_secs(2), async {
        while !cancelled.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the loop's death cancels its in-flight run");
    assert_eq!(calls.lock().len(), 1, "the orphan ran once and never again");

    // And the corpse is refused: the next request rebuilds.
    let fresh = state
        .get_or_create_session_controller("u", "s")
        .await
        .expect("the session opens over the corpse");
    assert!(
        !Arc::ptr_eq(&corpse, &fresh),
        "a panicking corpse must never be handed back"
    );
    assert!(fresh.is_alive(), "the rebuilt controller runs");
    assert_eq!(
        calls.lock().len(),
        1,
        "the orphan's factory is not invoked again after the rebuild"
    );
}
