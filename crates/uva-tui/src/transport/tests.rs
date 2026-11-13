//! Transport tests against an in-process WebSocket gateway.
//!
//! The fake gateway lives in the crate-root test module
//! (`crate::tests::FakeGateway`); these tests pin the transport's own
//! behaviour on top of it: URL normalization, supervisor-state
//! mapping, the passthrough of a `ready` broadcast, per-request
//! correlation, the synthesized initial `ready`, and the outage
//! narration.

use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    Driver,
    GatewayState,
    Request,
    RequestKind,
    TransportEvent,
    normalize_attach_url,
    state_event,
};
use crate::tests::{FakeGateway, Responder, next_event};

fn ping_responder() -> Responder {
    Arc::new(|frame: &Value| match frame["method"].as_str() {
        Some("health/ping") => {
            Some(Ok(json!({ "ok": true, "version": "0.1.0" })))
        }
        _ => None,
    })
}

#[test]
fn attach_urls_normalize_to_ws() {
    assert_eq!(
        normalize_attach_url("http://127.0.0.1:8080"),
        "ws://127.0.0.1:8080/rpc"
    );
    assert_eq!(
        normalize_attach_url("https://gateway.local:8443"),
        "wss://gateway.local:8443/rpc"
    );
    assert_eq!(
        normalize_attach_url("http://127.0.0.1:8080/rpc"),
        "ws://127.0.0.1:8080/rpc"
    );
    assert_eq!(
        normalize_attach_url("ws://127.0.0.1:8080/rpc"),
        "ws://127.0.0.1:8080/rpc"
    );
    assert_eq!(
        normalize_attach_url("ws://127.0.0.1:8080"),
        "ws://127.0.0.1:8080"
    );
}

#[test]
fn supervisor_states_map_to_transport_events() {
    assert!(matches!(
        state_event(GatewayState::Starting),
        Some(TransportEvent::GatewayStarting { .. })
    ));
    assert!(matches!(
        state_event(GatewayState::Restarting { attempt: 2 }),
        Some(TransportEvent::GatewayRestarting { attempt: 2 })
    ));
    assert!(matches!(
        state_event(GatewayState::Failed {
            reason: "probe budget spent".to_owned()
        }),
        Some(TransportEvent::Disconnected { reason })
        if reason == "probe budget spent"
    ));
    assert!(
        state_event(GatewayState::Ready {
            base_url: String::new(),
            ws_url: String::new()
        })
        .is_none()
    );
}

#[tokio::test]
async fn the_probe_synthesizes_the_initial_ready() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(ping_responder());
    let mut driver = Driver::attach(&gateway.url);
    match next_event(&mut driver).await {
        TransportEvent::Ready { version, .. } => {
            assert_eq!(version, "0.1.0");
        }
        other => panic!("expected a synthesized ready, got {other:?}"),
    }
}

#[tokio::test]
async fn a_ready_broadcast_passes_through_with_its_params() {
    let gateway = FakeGateway::spawn().await;
    // The broadcast's params must win over the probe's synthesis, so
    // they differ: the ping reports 9.9.9, the broadcast 0.1.0 and a
    // workspace root.
    gateway.set_responder(Arc::new(|frame: &Value| {
        match frame["method"].as_str() {
            Some("health/ping") => {
                Some(Ok(json!({ "ok": true, "version": "9.9.9" })))
            }
            _ => None,
        }
    }));
    let mut driver = Driver::attach(&gateway.url);
    // Let the probe's synthesis land first, then broadcast.
    let _ = next_event(&mut driver).await;
    gateway
        .notify(
            "ready",
            json!({ "version": "0.1.0", "workspace_root": "/work" }),
        )
        .await;
    match next_event(&mut driver).await {
        TransportEvent::Ready {
            version,
            workspace_root,
        } => {
            assert_eq!(version, "0.1.0");
            assert_eq!(workspace_root, "/work");
        }
        other => panic!("expected the broadcast ready, got {other:?}"),
    }
}

#[tokio::test]
async fn a_request_is_answered_as_a_correlated_response() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(Arc::new(|frame: &Value| {
        match frame["method"].as_str() {
            Some("health/ping") => {
                Some(Ok(json!({ "ok": true, "version": "0.1.0" })))
            }
            Some("chat/session/create") => {
                Some(Ok(json!({ "session_id": "s1", "agent_name": null })))
            }
            _ => None,
        }
    }));
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let _ = next_event(&mut driver).await; // the synthesized ready

    sender
        .send(&Request::create_session())
        .await
        .expect("the request is sent");
    let frame = gateway.wait_request("chat/session/create", 1).await;
    assert_eq!(frame["params"], json!({}));
    match next_event(&mut driver).await {
        TransportEvent::Response {
            request: Some(RequestKind::CreateSession),
            result: Ok(value),
            ..
        } => assert_eq!(value["session_id"], "s1"),
        other => panic!("expected a correlated response, got {other:?}"),
    }
}

#[tokio::test]
async fn a_dropped_connection_with_a_request_in_flight_announces_it() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(ping_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let _ = next_event(&mut driver).await; // the synthesized ready

    // A request nothing answers stays in flight across the drop.
    sender
        .send(&Request::follow("s1"))
        .await
        .expect("the request is sent");
    gateway.wait_request("sessions/follow", 1).await;
    gateway.kill();
    gateway.wait_connections(0).await;

    match next_event(&mut driver).await {
        TransportEvent::Disconnected { reason } => {
            assert!(!reason.is_empty());
        }
        other => panic!("expected a disconnect, got {other:?}"),
    }
}

#[tokio::test]
async fn an_idle_disconnect_is_announced_by_the_probe() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(ping_responder());
    let mut driver = Driver::attach(&gateway.url);
    let _ = next_event(&mut driver).await; // the synthesized ready

    // No request is in flight: the gateway dies while the TUI idles,
    // so only the periodic probe can notice. Its next `health/ping`
    // fails at the dial, which must surface as `Disconnected` — the
    // status bar may not keep saying "connected".
    gateway.kill();
    gateway.wait_connections(0).await;

    match next_event(&mut driver).await {
        TransportEvent::Disconnected { reason } => {
            assert!(!reason.is_empty());
        }
        other => panic!("expected a disconnect, got {other:?}"),
    }
}

#[tokio::test]
async fn a_catalog_request_round_trips_its_method_name() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(Arc::new(|frame: &Value| {
        match frame["method"].as_str() {
            Some("health/ping") => {
                Some(Ok(json!({ "ok": true, "version": "0.1.0" })))
            }
            Some("sessions/list") => Some(Ok(json!({ "rows": [] }))),
            _ => None,
        }
    }));
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let _ = next_event(&mut driver).await; // the synthesized ready

    sender
        .send(&Request::catalog("sessions/list", json!({ "limit": 2 })))
        .await
        .expect("the request is sent");
    let frame = gateway.wait_request("sessions/list", 1).await;
    assert_eq!(frame["method"], json!("sessions/list"));
    assert_eq!(frame["params"], json!({ "limit": 2 }));

    match next_event(&mut driver).await {
        TransportEvent::Response {
            request: Some(RequestKind::Catalog("sessions/list")),
            result: Ok(value),
            ..
        } => assert_eq!(value, json!({ "rows": [] })),
        other => panic!("expected a catalog response, got {other:?}"),
    }
}
