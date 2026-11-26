//! The delegated-children route: the roster, the hard stop, the
//! steer.
//!
//! The control surface is the task crate's handle table; the route
//! only opens the door. These tests drive the door over a table with
//! one registered child.

use std::sync::Arc;

use futures::channel::mpsc::unbounded;
use uva::tool_task::{ChildHandle, ChildState};

use crate::{
    routes::subagents::{
        SubagentCancelParams,
        SubagentListParams,
        SubagentSteerParams,
        cancel_subagent,
        list_subagents,
        steer_subagent,
    },
    state::AppState,
};

/// The state with the native plugin set mounted — the `tool-task`
/// row included, so the control surface is published.
async fn state_with_control_surface() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let registry = uva::session::manager::SessionRegistry::new(
        dir.path().join("sessions"),
    );
    let state = AppState::for_test(registry, dir.path().to_path_buf()).await;
    (Arc::new(state), dir)
}

/// Register a running child under `parent`.
///
/// The returned receiver is the child's inbox: the test keeps it
/// alive so a steer has somewhere to land.
fn register_child(
    state: &AppState,
    parent: &str,
    child: &str,
) -> futures::channel::mpsc::UnboundedReceiver<uva::provider::Message> {
    let surface = state
        .control_surface()
        .expect("the tool-task row publishes the control surface");
    let (steer, _receiver) = unbounded();
    surface.registry().register(
        parent,
        ChildHandle::new(
            ChildState::spawn(child, "worker"),
            Arc::new(uva::core::AtomicCancelToken::new()),
        )
        .with_steer(steer.clone()),
    );
    _receiver
}

/// The roster names the children of the session.
#[tokio::test]
async fn the_roster_names_the_children() {
    let (state, _dir) = state_with_control_surface().await;
    register_child(&state, "s1", "c1");

    let response = list_subagents(
        Arc::clone(&state),
        SubagentListParams {
            session_id: "s1".to_string(),
        },
    )
    .await
    .expect("the roster is readable");

    assert_eq!(response.session_id, "s1");
    assert_eq!(response.subagents.len(), 1);
    assert_eq!(response.subagents[0].id, "c1");
    assert_eq!(response.subagents[0].name, "worker");
}

/// Stopping a child is terminal, and stopping it twice is not an
/// error: the second call answers `cancelled: false`.
#[tokio::test]
async fn the_stop_is_terminal_and_idempotent() {
    let (state, _dir) = state_with_control_surface().await;
    register_child(&state, "s1", "c1");
    let params = SubagentCancelParams {
        session_id: "s1".to_string(),
        child_id: "c1".to_string(),
    };

    let first = cancel_subagent(Arc::clone(&state), params.clone())
        .await
        .unwrap();
    assert!(first.cancelled, "the first stop stops the child");

    let second = cancel_subagent(Arc::clone(&state), params).await.unwrap();
    assert!(
        !second.cancelled,
        "a child that is already stopped is not stopped twice"
    );
}

/// A steer reaches a running child's round.
#[tokio::test]
async fn a_steer_reaches_the_running_child() {
    let (state, _dir) = state_with_control_surface().await;
    let mut inbox = register_child(&state, "s1", "c1");

    let response = steer_subagent(
        Arc::clone(&state),
        SubagentSteerParams {
            session_id: "s1".to_string(),
            child_id: "c1".to_string(),
            message: "stop and report".to_string(),
        },
    )
    .await
    .expect("the steer is delivered");

    assert!(response.delivered);
    let message = inbox.try_recv().expect("the message landed");
    let text = serde_json::to_string(&message).unwrap_or_default();
    assert!(
        text.contains("stop and report"),
        "the child reads the host's message: {text}"
    );
}

/// A steer for a child that does not exist is refused with the
/// reason, not with a silent no-op.
#[tokio::test]
async fn a_steer_for_an_unknown_child_is_refused() {
    let (state, _dir) = state_with_control_surface().await;

    let error = steer_subagent(
        Arc::clone(&state),
        SubagentSteerParams {
            session_id: "s1".to_string(),
            child_id: "nope".to_string(),
            message: "stop and report".to_string(),
        },
    )
    .await
    .expect_err("an unknown child cannot be steered");

    assert_eq!(error.code, "subagent_unknown");
}
