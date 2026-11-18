//! `SessionOp::Cancel` — terminates the active run, is
//! idempotent under repeated calls, and is a safe no-op when
//! submitted before any run has started.

use std::{
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};

use parking_lot::Mutex;

use super::{
    super::{RunStreamFactory, SessionOp},
    support::{
        BlockingFactory,
        RecordedCall,
        VecFactory,
        make_manager_and_controller,
        wait_for_runs,
    },
};
use crate::session::status::SessionState;

#[tokio::test]
async fn test_cancel_terminates_run() {
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn RunStreamFactory> = Arc::new(BlockingFactory {
        calls: Arc::clone(&calls),
        progress_count: Arc::new(AtomicUsize::new(0)),
    });
    let (controller, _manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    controller
        .submit(SessionOp::Prompt {
            content: "start".to_string(),
            priority: 1,
        })
        .await
        .unwrap();

    // Deterministic start signal: the factory invocation,
    // not `state() == Running` (see `wait_for_runs`).
    wait_for_runs(&calls, 1).await;

    // The run holds the park it started from, and the cancel drops
    // every other queued turn, so nothing restarts the run.
    controller.cancel().await.unwrap();

    tokio::time::timeout(Duration::from_millis(500), async {
        while controller.state() != SessionState::Cancelled
            && controller.state() != SessionState::Idle
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    assert_eq!(calls.lock().len(), 1);
}

#[tokio::test]
async fn test_repeated_cancel_is_idempotent() {
    // Verify that submitting multiple `Cancel` ops in
    // quick succession does not panic, double-fire any
    // state transitions, or leave the controller in an
    // unexpected state. `AtomicCancelToken::cancel()` is
    // documented as idempotent and the handler must
    // preserve that property.
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn RunStreamFactory> = Arc::new(BlockingFactory {
        calls: Arc::clone(&calls),
        progress_count: Arc::new(AtomicUsize::new(0)),
    });
    let (controller, _manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;
    controller
        .submit(SessionOp::Prompt {
            content: "start".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    // Deterministic start signal: the factory invocation,
    // not `state() == Running` (see `wait_for_runs`).
    wait_for_runs(&calls, 1).await;

    // Three cancels back-to-back — none must error.
    controller.cancel().await.unwrap();
    controller.cancel().await.unwrap();
    controller.cancel().await.unwrap();

    // The state still converges (either `Cancelled` or
    // `Idle` after the controller reaps the run).
    tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let s = controller.state();
            if s == SessionState::Cancelled || s == SessionState::Idle {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    // Exactly one run was ever started (no spurious
    // restarts from repeated cancels re-queueing work).
    assert_eq!(calls.lock().len(), 1);
}

#[tokio::test]
async fn test_cancel_before_run_starts_is_safe_noop() {
    // Edge case: cancel arrives BEFORE any Prompt has
    // been dequeued (e.g. the user submitted then
    // immediately gave up). `run_cancel` is `None` at
    // that point, so the controller must treat the
    // cancel as a safe no-op — no panic, no
    // double-transition, no hang.
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn RunStreamFactory> =
        Arc::new(VecFactory::new(vec![], Arc::clone(&calls)));
    let (controller, _manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;
    // No Prompt submitted — controller is Idle.
    assert_eq!(controller.state(), SessionState::Idle);
    // Cancel while Idle.
    controller.cancel().await.unwrap();
    // Must remain Idle (no run to cancel).
    assert_eq!(controller.state(), SessionState::Idle);
    // No factory calls were ever made.
    assert_eq!(calls.lock().len(), 0);
}

/// A manual compaction pass must not wedge a cancel: the pass gate
/// serialises run starts only, so the cancel's state flip completes
/// while the pass still holds it. (Before the split, the pass held
/// the steer gate and the cancel queued behind the whole pass.)
#[tokio::test]
async fn a_cancel_is_not_blocked_by_a_manual_compaction_pass() {
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn RunStreamFactory> = Arc::new(BlockingFactory {
        calls: Arc::clone(&calls),
        progress_count: Arc::new(AtomicUsize::new(0)),
    });
    let (controller, _manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // The gate a `/compact` request holds for its whole pass.
    controller
        .with_compaction_gate(async {
            controller.cancel().await.unwrap();

            tokio::time::timeout(Duration::from_millis(1_000), async {
                while controller.state() != SessionState::Cancelled {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("the cancel flips while the pass holds its gate");
        })
        .await;
}
