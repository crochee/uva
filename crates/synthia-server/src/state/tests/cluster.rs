//! Cluster wiring: shared-bus lease ownership, forwarded ops, and the
//! remote handle's mirrored tail.
//!
//! Two `AppState`s over one workspace directory and one in-process
//! `LocalBus` stand in for two nodes: the first caller takes the
//! session lease and runs the controller; the second gets a remote
//! handle that forwards ops over the control channel and mirrors the
//! owner's frames back onto its own broadcast (what the SSE handler
//! subscribes to).

use std::{sync::Arc, time::Duration};

use synthia::{
    provider::{CompletionResponse, Content, traits::ModelProvider},
    test_support::FakeProvider,
};
use tempfile::TempDir;

use crate::{
    cluster::{ClusterRuntime, LocalBus},
    session::controller::SessionOp,
    state::AppState,
};

fn scripted_provider() -> Arc<dyn ModelProvider> {
    Arc::new(FakeProvider::new(vec![CompletionResponse {
        content: Content::text("pong"),
        ..CompletionResponse::default()
    }]))
}

async fn node(
    node_id: &str,
    root: &std::path::Path,
    bus: Arc<LocalBus>,
) -> Arc<AppState> {
    let manager =
        synthia::session::manager::SessionRegistry::new(root.to_path_buf());
    let mut state = AppState::for_test(manager, root.to_path_buf()).await;
    state.default_provider = scripted_provider();
    state.cluster = Arc::new(ClusterRuntime {
        bus,
        node_id: node_id.to_string(),
        namespace: "test".to_string(),
        // Short TTL so the renew arm is observable within a test.
        lease_ttl: Duration::from_millis(300),
        clustered: true,
    });
    let state = Arc::new(state);
    crate::state::app_state::cluster_ops::spawn_control_dispatcher(Arc::clone(
        &state,
    ));
    state
}

#[tokio::test]
async fn shared_bus_forwards_ops_and_mirrors_the_tail() {
    let dir = TempDir::new().unwrap();
    let bus = Arc::new(LocalBus::new());
    let a = node("a", dir.path(), Arc::clone(&bus)).await;
    let b = node("b", dir.path(), Arc::clone(&bus)).await;

    let owner = a.get_or_create_session_controller("u", "s").await.unwrap();
    assert!(!owner.is_remote(), "the first caller owns the session");
    let remote = b.get_or_create_session_controller("u", "s").await.unwrap();
    assert!(remote.is_remote(), "the second node forwards to the owner");

    // Subscribe through the remote handle exactly like the SSE route
    // does; the bus tail bridge feeds it.
    let mut tail = remote.subscribe();
    remote
        .submit(SessionOp::Prompt {
            content: "ping".to_string(),
            priority: 0,
        })
        .await
        .unwrap();

    let saw_text = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(event) = tail.recv().await {
            if let crate::session::controller::AgentEvent::Model(
                synthia::provider::ContentPart::Text(text),
            ) = event
                && text.text.contains("pong")
            {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(
        saw_text,
        "a forwarded op's run must reach the remote handle's tail"
    );
}

#[tokio::test]
async fn closing_the_owner_hands_the_lease_over() {
    let dir = TempDir::new().unwrap();
    let bus = Arc::new(LocalBus::new());
    let a = node("a", dir.path(), Arc::clone(&bus)).await;
    let b = node("b", dir.path(), Arc::clone(&bus)).await;

    let owner = a.get_or_create_session_controller("u", "s").await.unwrap();
    assert!(!owner.is_remote());
    let remote = b.get_or_create_session_controller("u", "s").await.unwrap();
    assert!(remote.is_remote());

    // The owner goes away (graceful close releases its lease; a crash
    // would let the TTL do it — asserted in the LocalBus unit tests).
    owner.close().await.unwrap();
    a.active_sessions
        .remove(&("u".to_string(), "s".to_string()));

    // B's cached remote handle must re-check ownership on the next
    // request and upgrade itself into a real controller.
    let adopted = b.get_or_create_session_controller("u", "s").await.unwrap();
    assert!(
        !adopted.is_remote(),
        "a cached remote handle must take ownership once the lease is free"
    );

    // And the adopted controller runs a turn (proving it is not just a
    // second forwarding handle).
    let mut tail = adopted.subscribe();
    adopted
        .submit(SessionOp::Prompt {
            content: "after takeover".to_string(),
            priority: 0,
        })
        .await
        .unwrap();
    let ran = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(event) = tail.recv().await {
            if let crate::session::controller::AgentEvent::Model(
                synthia::provider::ContentPart::Text(text),
            ) = event
                && text.text.contains("pong")
            {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(ran, "the adopted controller must drive the run itself");
}

#[tokio::test]
async fn local_mode_never_publishes_or_forwards() {
    // The default deployment (local bus, `clustered = false`) keeps
    // today's behaviour: one node, a real controller, no bus traffic.
    let dir = TempDir::new().unwrap();
    let manager = synthia::session::manager::SessionRegistry::new(
        dir.path().to_path_buf(),
    );
    let state = AppState::for_test(manager, dir.path().to_path_buf()).await;
    let state = Arc::new(state);

    let controller = state
        .get_or_create_session_controller("u", "s")
        .await
        .unwrap();
    assert!(!controller.is_remote());
}
