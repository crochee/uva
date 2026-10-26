//! The cold-open gate: one entry per live controller, evicted with it,
//! and a bounded wait for the requests queued behind it.

use std::{sync::Arc, time::Duration};

use crate::state::AppState;

/// A state over a fresh workspace, with a gate budget short enough to
/// prove the bound without waiting the production half-minute.
async fn state_with_gate_budget(
    root: &std::path::Path,
    budget: Duration,
) -> Arc<AppState> {
    let manager =
        synthia::session::manager::SessionRegistry::new(root.to_path_buf());
    let mut state = AppState::for_test(manager, root.to_path_buf()).await;
    state.spawn_gate_timeout = budget;
    Arc::new(state)
}

/// One wedged cold open must not hold every request for that session
/// for the life of the process: a waiter that cannot take the gate in
/// the budget fails with a diagnostic naming the session.
#[tokio::test]
async fn a_wedged_cold_open_fails_instead_of_blocking() {
    let dir = tempfile::TempDir::new().expect("a temp workspace");
    let state =
        state_with_gate_budget(dir.path(), Duration::from_millis(50)).await;
    let key = ("u".to_string(), "s".to_string());

    // The wedged holder: an open that never finished.
    let wedged = state.hold_spawn_gate(&key).await;

    let started = tokio::time::Instant::now();
    let error = match state.get_or_create_session_controller("u", "s").await {
        Ok(_) => panic!("a gate held forever must not be waited on forever"),
        Err(error) => error,
    };
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the waiter failed inside its budget, not eventually: {:?}",
        started.elapsed()
    );
    let message = error.to_string();
    assert!(
        message.contains("u/s") && message.contains("open gate"),
        "the diagnostic names the wedged session and the gate: {message}"
    );

    // The holder releasing lets the next request through — the failure
    // is the waiter's, not the session's.
    drop(wedged);
    let controller = state
        .get_or_create_session_controller("u", "s")
        .await
        .expect("the session opens once the gate is free");
    assert!(!controller.is_remote());
}

/// The gate is keyed by `(user, session)` and lives exactly as long as
/// its controller: the controller owns the lock, so the gate leaves
/// with the controller, and a re-created session of the same name gets
/// a fresh gate and a fresh controller.
#[tokio::test]
async fn forgetting_a_session_evicts_its_controller_and_gate() {
    let dir = tempfile::TempDir::new().expect("a temp workspace");
    let state =
        state_with_gate_budget(dir.path(), Duration::from_secs(5)).await;
    let key = ("u".to_string(), "s".to_string());

    let first = state
        .get_or_create_session_controller("u", "s")
        .await
        .expect("the session opens");
    assert!(
        state.has_spawn_gate(&key),
        "opening a session creates its gate"
    );
    assert!(state.active_sessions.get(&key).is_some());

    first.close().await.expect("the controller closes");
    state.forget_session(&key);
    assert!(state.active_sessions.get(&key).is_none());

    // The gate belongs to the controller, so it leaves with the
    // controller and not with the map entry that pointed at it. This
    // handle is the last one to the controller.
    let first_handle = Arc::downgrade(&first);
    drop(first);
    assert!(
        !state.has_spawn_gate(&key),
        "the gate leaves with the controller: {:?}",
        state.has_spawn_gate(&key)
    );

    let again = state
        .get_or_create_session_controller("u", "s")
        .await
        .expect("the re-created session opens");
    assert!(
        first_handle.upgrade().is_none(),
        "the fresh open is a new controller, not the dead one"
    );
    assert!(again.is_alive(), "and the re-created session runs");
    assert!(state.has_spawn_gate(&key), "and it has its own gate");
}

/// The gate map must not grow with the number of sessions the process
/// has ever opened.
///
/// A gate belongs to its controller, so it leaves with it — through
/// `forget_session` or through any other path that drops the
/// controller. A map that only one eviction path cleans keeps one
/// entry per session this process ever opened, and a session's gate
/// outlives the controller that queued behind it.
#[tokio::test]
async fn the_spawn_gate_map_does_not_outlive_the_controllers_it_belongs_to() {
    let dir = tempfile::TempDir::new().expect("a temp workspace");
    let state =
        state_with_gate_budget(dir.path(), Duration::from_secs(5)).await;

    for index in 0..8 {
        let session = format!("s{index}");
        let key = ("u".to_string(), session.clone());
        let controller = state
            .get_or_create_session_controller("u", &session)
            .await
            .expect("the session opens");
        assert!(
            state.active_sessions.get(&key).is_some(),
            "the open registered its controller"
        );
        // The eviction path `forget_session` does not cover: the
        // controller leaves the map on its own (an ephemeral session's
        // pin guard drops it exactly this way).
        state.active_sessions.remove(&key);
        drop(controller);
    }

    assert_eq!(
        state.spawn_gate_count(),
        0,
        "the gate map kept one entry per session this process opened"
    );
}
