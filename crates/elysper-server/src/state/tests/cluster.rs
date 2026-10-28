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

use elysper::{
    provider::{CompletionResponse, Content, traits::ModelProvider},
    test_support::FakeProvider,
};
use tempfile::TempDir;

use crate::{
    cluster::{ClusterRuntime, LocalBus},
    session::controller::SessionOp,
    state::{AppState, app_state::PARKED_PROMPT_WINDOW},
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
        elysper::session::manager::SessionRegistry::new(root.to_path_buf());
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
                elysper::provider::ContentPart::Text(text),
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
                elysper::provider::ContentPart::Text(text),
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
async fn a_takeover_rebuilds_the_state_from_the_log() {
    // Task 16 / R3: a node that acquires an expired lease inherits no
    // memory. Everything the controller reports is re-derived from the
    // log, so a session suspended on the node that died is still
    // suspended on the node that took over — and refuses to start a
    // fresh run for the same reason it did before.
    let dir = TempDir::new().unwrap();
    let bus = Arc::new(LocalBus::new());
    let a = node("a", dir.path(), Arc::clone(&bus)).await;
    let b = node("b", dir.path(), Arc::clone(&bus)).await;

    let owner = a.get_or_create_session_controller("u", "s").await.unwrap();
    assert!(!owner.is_remote());
    // A suspended turn the dead node left behind: one high-risk call
    // asked and never decided.
    let writer = a.session_manager.writer("u", "s");
    writer
        .append_event(&elysper::session::turn_start(0))
        .await
        .unwrap();
    writer
        .append_event(&elysper::session::approval_asked(
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
        .append_event(&elysper::session::turn_suspended(vec!["c1".to_string()]))
        .await
        .unwrap();
    writer
        .append_event(&elysper::session::step_end_suspended())
        .await
        .unwrap();
    writer
        .append_event(&elysper::session::turn_end_suspended())
        .await
        .unwrap();
    writer.flush().await.unwrap();

    owner.close().await.unwrap();
    a.active_sessions
        .remove(&("u".to_string(), "s".to_string()));

    let adopted = b.get_or_create_session_controller("u", "s").await.unwrap();
    assert!(!adopted.is_remote(), "node b takes the lease over");
    assert_eq!(
        adopted.state(),
        crate::session::controller::SessionState::AwaitingApproval,
        "the rebuilt controller reports the log's pause, not its own \
         fresh-process Idle"
    );
    assert_eq!(
        adopted.session_state().await,
        crate::session::controller::SessionState::AwaitingApproval
    );
}

#[tokio::test]
async fn only_one_node_holds_the_scheduler_leader_lease() {
    // The driver's gate: the node that wins the lease is the only one
    // that runs a scheduler pass, so a due job fires once across the
    // deployment rather than once per node.
    let dir = TempDir::new().unwrap();
    let bus = Arc::new(LocalBus::new());
    let a = node("a", dir.path(), Arc::clone(&bus)).await;
    let b = node("b", dir.path(), Arc::clone(&bus)).await;
    let lease = crate::cluster::scheduler_leader_lease(&a.cluster.namespace);
    let a_won = a
        .cluster
        .bus
        .try_acquire(&lease, &a.cluster.node_id, a.cluster.lease_ttl)
        .await
        .unwrap();
    let b_won = b
        .cluster
        .bus
        .try_acquire(&lease, &b.cluster.node_id, b.cluster.lease_ttl)
        .await
        .unwrap();
    assert!(a_won, "the first node takes the leader lease");
    assert!(
        !b_won,
        "the second node skips its pass while the lease is held"
    );
}

#[tokio::test]
async fn a_takeover_requeues_a_prompt_the_dead_node_parked() {
    // The durable half of `pending_turns`: the prompt is a row, so
    // the node that takes over puts it back in the queue and runs it.
    let dir = TempDir::new().unwrap();
    let bus = Arc::new(LocalBus::new());
    let a = node("a", dir.path(), Arc::clone(&bus)).await;
    let b = node("b", dir.path(), Arc::clone(&bus)).await;

    let owner = a.get_or_create_session_controller("u", "s").await.unwrap();
    assert!(!owner.is_remote());
    let writer = a.session_manager.writer("u", "s");
    writer
        .append_event(&elysper::session::SessionEvent::ParkedPrompt {
            seq: 0,
            ts: String::new(),
            park_id: "park-recovered".to_string(),
            text: "recovered turn".to_string(),
            attachments: Vec::new(),
            agent_name: None,
            provider: None,
            model: None,
            rerun: false,
        })
        .await
        .unwrap();
    writer.flush().await.unwrap();

    owner.close().await.unwrap();
    a.active_sessions
        .remove(&("u".to_string(), "s".to_string()));

    let adopted = b.get_or_create_session_controller("u", "s").await.unwrap();
    assert!(!adopted.is_remote());

    // The recovered prompt must actually run: the run records the turn
    // it was started for as a `UserInput` row.
    let rows = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = b
                .session_manager
                .sink("u", "s")
                .read_tail(50)
                .await
                .unwrap_or_default();
            let ran = rows.iter().any(|row| {
                row["type"] == "UserInput"
                    && row["data"]["text"] == "recovered turn"
            });
            if ran {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the recovered prompt runs");
    assert!(
        rows.iter().any(|row| row["type"] == "UserInput"),
        "the recovered turn is in the log"
    );
}

#[tokio::test]
async fn a_parked_turn_records_the_catalogue_provider_id() {
    // The pin resolves a handle, but an OpenAI-compatible handle
    // reports its *model* as `name()`. The park row must name the
    // catalogue entry the selection resolved from, or a takeover
    // reads the model twice and the provider is unrecoverable.
    let dir = TempDir::new().unwrap();
    let bus = Arc::new(LocalBus::new());
    let state = node("a", dir.path(), Arc::clone(&bus)).await;
    let provider_id = state.provider_id(None).expect("a default provider id");
    let handle = state.resolve_provider(None).expect("the default provider");
    let model = handle.model_config().name;
    let controller = state
        .get_or_create_session_controller("u", "s")
        .await
        .unwrap();
    controller
        .submit_durable_with_provider(
            crate::session::controller::SessionOp::Prompt {
                content: "a pinned turn".to_string(),
                priority: 1,
            },
            Some(
                crate::session::controller::PinnedProvider::new(handle)
                    .with_id(provider_id.clone()),
            ),
        )
        .await
        .expect("the turn is accepted");

    let rows = state
        .session_manager
        .sink("u", "s")
        .read_tail(50)
        .await
        .unwrap_or_default();
    let park = rows
        .iter()
        .find(|row| row["type"] == "parked_prompt")
        .expect("a parked prompt row");
    assert_eq!(park["provider"], serde_json::json!(provider_id));
    assert_eq!(park["model"], serde_json::json!(model));

    // A turn with no selection records the deployment default's
    // catalogue key — never the model spelling the handle reports.
    let controller = state
        .get_or_create_session_controller("u", "s")
        .await
        .unwrap();
    controller
        .submit_durable(crate::session::controller::SessionOp::Prompt {
            content: "an unpinned turn".to_string(),
            priority: 1,
        })
        .await
        .expect("the turn is accepted");
    let rows = state
        .session_manager
        .sink("u", "s")
        .read_tail(80)
        .await
        .unwrap_or_default();
    let unpinned = rows
        .iter()
        .filter(|row| row["type"] == "parked_prompt")
        .find(|row| row["text"] == "an unpinned turn")
        .expect("the unpinned park row");
    assert_eq!(
        unpinned["provider"],
        serde_json::json!(provider_id),
        "an unpinned turn names the default provider id"
    );
}

#[tokio::test]
async fn a_takeover_does_not_run_a_parked_turn_under_the_default_model() {
    // The park names its provider and model. A node that cannot
    // resolve that model reports `model_unavailable` and leaves the
    // turn queued: silently running the deployment default would tell
    // the user their selection took effect when it did not.
    let dir = TempDir::new().unwrap();
    let bus = Arc::new(LocalBus::new());
    let a = node("a", dir.path(), Arc::clone(&bus)).await;
    let b = node("b", dir.path(), Arc::clone(&bus)).await;

    let owner = a.get_or_create_session_controller("u", "s").await.unwrap();
    assert!(!owner.is_remote());
    let writer = a.session_manager.writer("u", "s");
    writer
        .append_event(&elysper::session::SessionEvent::ParkedPrompt {
            seq: 0,
            ts: String::new(),
            park_id: "park-unknown-model".to_string(),
            text: "a turn under an unconfigured model".to_string(),
            attachments: Vec::new(),
            agent_name: None,
            provider: Some("ghost-provider".to_string()),
            model: Some("ghost-model".to_string()),
            rerun: false,
        })
        .await
        .unwrap();
    writer.flush().await.unwrap();

    owner.close().await.unwrap();
    a.active_sessions
        .remove(&("u".to_string(), "s".to_string()));

    let adopted = b.get_or_create_session_controller("u", "s").await.unwrap();
    assert!(!adopted.is_remote());

    // Give a wrongly-started run the same window the positive test
    // uses; the assertion is that nothing shows up.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let rows = b
        .session_manager
        .sink("u", "s")
        .read_tail(50)
        .await
        .unwrap_or_default();
    assert!(
        !rows.iter().any(|row| {
            row["type"] == "UserInput"
                && row["data"]["text"] == "a turn under an unconfigured model"
        }),
        "the turn must stay queued rather than run under the default model"
    );
}

#[tokio::test]
async fn a_second_parked_prompt_survives_the_first_turns_consumption() {
    // Two multimodal prompts queued behind a live run produce two parks.
    // The first run records its turn; the node then crashes. The second
    // prompt must still be queued — clearing the queue on the first
    // `UserInput` would lose it although its row is intact.
    let dir = TempDir::new().unwrap();
    let bus = Arc::new(LocalBus::new());
    let state = node("a", dir.path(), Arc::clone(&bus)).await;
    state
        .session_manager
        .create_with_user("u".to_string(), "s".to_string())
        .await
        .unwrap();
    let writer = state.session_manager.writer("u", "s");
    for (park_id, text) in [("park-1", "first turn"), ("park-2", "second turn")]
    {
        writer
            .append_event(&elysper::session::SessionEvent::ParkedPrompt {
                seq: 0,
                ts: String::new(),
                park_id: park_id.to_string(),
                text: text.to_string(),
                attachments: Vec::new(),
                agent_name: None,
                provider: None,
                model: None,
                rerun: false,
            })
            .await
            .unwrap();
    }
    // The first run spent the first park by id and recorded its turn.
    writer
        .append_event(&elysper::session::SessionEvent::ParkConsumed {
            seq: 0,
            ts: String::new(),
            park_id: "park-1".to_string(),
        })
        .await
        .unwrap();
    writer
        .append(serde_json::json!({
            "type": "UserInput",
            "data": { "text": "first turn" },
        }))
        .await
        .unwrap();
    writer.flush().await.unwrap();

    // The crash: this process takes the session over cold.
    state
        .get_or_create_session_controller("u", "s")
        .await
        .expect("the session opens");

    let rows = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = state
                .session_manager
                .sink("u", "s")
                .read_tail(50)
                .await
                .unwrap_or_default();
            if rows.iter().any(|row| {
                row["type"] == "UserInput"
                    && row["data"]["text"] == "second turn"
            }) {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the second parked prompt is still queued and runs");
    assert_eq!(
        rows.iter()
            .filter(|row| {
                row["type"] == "UserInput"
                    && row["data"]["text"] == "first turn"
            })
            .count(),
        1,
        "the consumed prompt is not re-queued"
    );
}

#[tokio::test]
async fn a_park_behind_a_long_run_is_still_recovered() {
    // A prompt queued behind a live run sits *before* everything that
    // run goes on to write, so a fixed 200-row window can scroll past
    // it. The takeover read widens until it reaches the consumption
    // frontier; a silent drop is the one outcome this feature must never
    // produce.
    let dir = TempDir::new().unwrap();
    let bus = Arc::new(LocalBus::new());
    let state = node("a", dir.path(), Arc::clone(&bus)).await;
    state
        .session_manager
        .create_with_user("u".to_string(), "s".to_string())
        .await
        .unwrap();
    let writer = state.session_manager.writer("u", "s");
    writer
        .append_event(&elysper::session::SessionEvent::ParkedPrompt {
            seq: 0,
            ts: String::new(),
            park_id: "park-behind".to_string(),
            text: "queued behind the run".to_string(),
            attachments: Vec::new(),
            agent_name: None,
            provider: None,
            model: None,
            rerun: false,
        })
        .await
        .unwrap();
    // The live run keeps writing, well past the base window and with no
    // `UserInput` row of its own (it has not started its turn yet).
    for index in 0..(PARKED_PROMPT_WINDOW + 60) {
        writer
            .append_event(&elysper::session::usage(
                10 + index,
                1,
                11 + index,
                None,
                None,
                None,
            ))
            .await
            .unwrap();
    }
    writer.flush().await.unwrap();

    state
        .get_or_create_session_controller("u", "s")
        .await
        .expect("the session opens");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = state
                .session_manager
                .sink("u", "s")
                .read_tail(50)
                .await
                .unwrap_or_default();
            if rows.iter().any(|row| {
                row["type"] == "UserInput"
                    && row["data"]["text"] == "queued behind the run"
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the parked prompt behind the long run is recovered");
}

#[tokio::test]
async fn two_concurrent_cold_opens_start_exactly_one_run() {
    // A cold open is a read-modify-write of the controller cache that
    // also rebuilds from the log and re-queues a parked prompt. Two
    // requests racing for one session must produce one controller and
    // one recovered turn — not the duplicate the rebuild exists to
    // prevent.
    let dir = TempDir::new().unwrap();
    let bus = Arc::new(LocalBus::new());
    let state = node("a", dir.path(), Arc::clone(&bus)).await;
    state
        .session_manager
        .create_with_user("u".to_string(), "s".to_string())
        .await
        .unwrap();
    let writer = state.session_manager.writer("u", "s");
    writer
        .append_event(&elysper::session::SessionEvent::ParkedPrompt {
            seq: 0,
            ts: String::new(),
            park_id: "park-once".to_string(),
            text: "recovered once".to_string(),
            attachments: Vec::new(),
            agent_name: None,
            provider: None,
            model: None,
            rerun: false,
        })
        .await
        .unwrap();
    writer.flush().await.unwrap();

    let (first, second) = tokio::join!(
        state.get_or_create_session_controller("u", "s"),
        state.get_or_create_session_controller("u", "s"),
    );
    let (first, second) = (
        first.expect("the first open"),
        second.expect("the second open"),
    );
    assert!(
        Arc::ptr_eq(&first, &second),
        "both callers must get the session's one controller"
    );

    // Exactly one recovered turn: the run records the prompt once.
    let rows = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = state
                .session_manager
                .sink("u", "s")
                .read_tail(50)
                .await
                .unwrap_or_default();
            if rows.iter().any(|row| {
                row["type"] == "UserInput"
                    && row["data"]["text"] == "recovered once"
            }) {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the recovered prompt runs");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rows = state
        .session_manager
        .sink("u", "s")
        .read_tail(50)
        .await
        .unwrap_or(rows);
    let runs = rows
        .iter()
        .filter(|row| {
            row["type"] == "UserInput"
                && row["data"]["text"] == "recovered once"
        })
        .count();
    assert_eq!(runs, 1, "the recovered turn must start exactly once");
}

#[tokio::test]
async fn local_mode_never_publishes_or_forwards() {
    // The default deployment (local bus, `clustered = false`) keeps
    // today's behaviour: one node, a real controller, no bus traffic.
    let dir = TempDir::new().unwrap();
    let manager = elysper::session::manager::SessionRegistry::new(
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
