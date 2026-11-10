//! End-to-end projection tests over an in-process WebSocket gateway.
//!
//! No tty, no OS process: a [`FakeGateway`] serves the same `/rpc`
//! WebSocket JSON-RPC surface `serve` does — one text message one
//! envelope — answers requests through a test-set responder, and
//! relays whatever notifications the test broadcasts. The reducer is
//! folded in beside the transport, so each test exercises the path
//! `main` runs. [`Driver::attach`] connects to it; the same harness
//! backs the transport tests in `transport/tests.rs`.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    routing::get,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::{
    app::{Action, App, ConnectionState, Decision, TurnState},
    transport::{Driver, Transport, TransportEvent},
};

/// What the fake gateway answers one request with: `None` stays
/// unanswered, `Ok` becomes a `result`, `Err` an `error` object.
pub(crate) type Responder =
    Arc<dyn Fn(&Value) -> Option<Result<Value, Value>> + Send + Sync>;

/// An in-process gateway: one `/rpc` WebSocket route whose connections
/// share a responder, a notification feed, and a request log.
pub(crate) struct FakeGateway {
    /// The `ws://127.0.0.1:<port>/rpc` URL to attach to.
    pub(crate) url: String,
    responder: Arc<Mutex<Option<Responder>>>,
    notify: broadcast::Sender<String>,
    close: broadcast::Sender<()>,
    connections: Arc<AtomicUsize>,
    received: Arc<Mutex<Vec<Value>>>,
}

impl FakeGateway {
    /// Serve on a loopback port until the test ends.
    pub async fn spawn() -> Self {
        let responder: Arc<Mutex<Option<Responder>>> =
            Arc::new(Mutex::new(None));
        let (notify_tx, _) = broadcast::channel::<String>(64);
        let (close_tx, _) = broadcast::channel::<()>(4);
        let connections = Arc::new(AtomicUsize::new(0));
        let received: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = {
            let responder = Arc::clone(&responder);
            let notify = notify_tx.clone();
            let close = close_tx.clone();
            let connections = Arc::clone(&connections);
            let received = Arc::clone(&received);
            Router::new().route(
                "/rpc",
                get(move |upgrade: WebSocketUpgrade| {
                    let responder = Arc::clone(&responder);
                    let notify = notify.clone();
                    let close = close.clone();
                    let connections = Arc::clone(&connections);
                    let received = Arc::clone(&received);
                    async move {
                        upgrade.on_upgrade(move |socket| {
                            serve_connection(
                                socket,
                                responder,
                                notify,
                                close,
                                connections,
                                received,
                            )
                        })
                    }
                }),
            )
        };
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            url: format!("ws://127.0.0.1:{port}/rpc"),
            responder,
            notify: notify_tx,
            close: close_tx,
            connections,
            received,
        }
    }

    /// Install the request responder.
    pub fn set_responder(&self, responder: Responder) {
        *self.responder.lock() = Some(responder);
    }

    /// Broadcast one notification to every live connection.
    pub async fn notify(&self, method: &str, params: Value) {
        let envelope =
            json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let _ = self.notify.send(envelope.to_string());
    }

    /// Drop every live connection — the client sees the sockets die.
    pub fn kill(&self) {
        let _ = self.close.send(());
    }

    /// How many connections are live right now.
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
    }

    /// Poll until at least `n` connections are live.
    pub async fn wait_connections(&self, n: usize) {
        for _ in 0..2_000 {
            if self.connections() >= n {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("never saw {n} connections (now {})", self.connections());
    }

    /// Every request frame the gateway has received, oldest first.
    pub fn received(&self) -> Vec<Value> {
        self.received.lock().clone()
    }

    /// Poll until at least `n` requests with this method arrived, and
    /// return the newest of them.
    pub async fn wait_request(&self, method: &str, n: usize) -> Value {
        for _ in 0..2_000 {
            let matches: Vec<Value> = self
                .received()
                .into_iter()
                .filter(|frame| frame["method"] == json!(method))
                .collect();
            if matches.len() >= n {
                return matches[matches.len() - 1].clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("never received {n} `{method}` requests");
    }
}

/// One connection: answer requests through the responder, relay the
/// notification feed, and die when the test says so.
async fn serve_connection(
    mut socket: WebSocket,
    responder: Arc<Mutex<Option<Responder>>>,
    notify: broadcast::Sender<String>,
    close: broadcast::Sender<()>,
    connections: Arc<AtomicUsize>,
    received: Arc<Mutex<Vec<Value>>>,
) {
    connections.fetch_add(1, Ordering::Relaxed);
    let mut notify_rx = notify.subscribe();
    let mut close_rx = close.subscribe();
    loop {
        tokio::select! {
            frame = socket.recv() => match frame {
                Some(Ok(Message::Text(text))) => {
                    let Ok(value) = serde_json::from_str::<Value>(&text)
                    else {
                        continue;
                    };
                    received.lock().push(value.clone());
                    let answer = responder
                        .lock()
                        .as_ref()
                        .and_then(|respond| respond(&value));
                    if let Some(answer) = answer {
                        let reply = match answer {
                            Ok(result) => json!({
                                "jsonrpc": "2.0",
                                "id": value["id"],
                                "result": result,
                            }),
                            Err(error) => json!({
                                "jsonrpc": "2.0",
                                "id": value["id"],
                                "error": error,
                            }),
                        };
                        let _ = socket
                            .send(Message::Text(reply.to_string().into()))
                            .await;
                    }
                }
                _ => break,
            },
            notified = notify_rx.recv() => match notified {
                Ok(envelope) => {
                    if socket
                        .send(Message::Text(envelope.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = close_rx.recv() => break,
        }
    }
    connections.fetch_sub(1, Ordering::Relaxed);
}

/// The next transport event, failing fast instead of hanging.
pub(crate) async fn next_event(driver: &mut Driver) -> TransportEvent {
    tokio::time::timeout(std::time::Duration::from_secs(6), driver.recv())
        .await
        .expect("a transport event arrives")
        .expect("the event channel is open")
}

fn baseline(session_id: &str, state: &str, rows: Vec<Value>) -> Value {
    json!({
        "header": {
            "session_id": session_id,
            "user_id": "u1",
            "agent_name": "default",
            "state": state,
        },
        "rows": rows,
        "cursor": 0,
        "live": {
            "turn_id": "",
            "live_seq": 0,
            "running": false,
            "text": "",
            "reasoning": "",
            "tool_calls": [],
        },
        "context": { "used": 0 },
        "pending_interrupts": [],
        "request_snapshot_hash": "",
    })
}

fn text_row(kind: &str, seq: u64, text: &str) -> Value {
    json!({
        "type": kind,
        "seq": seq,
        "data": {
            "content": { "Single": { "type": "text", "text": text } },
        },
    })
}

fn turn_end(reason: &str) -> Value {
    json!({
        "type": "turn",
        "seq": 9,
        "data": { "kind": "end", "turn": 0, "reason": reason },
    })
}

/// A responder with the conversation vocabulary: probe, create,
/// follow, send. Everything else goes unanswered.
fn conversation_responder() -> Responder {
    Arc::new(|frame: &Value| match frame["method"].as_str() {
        Some("health/ping") => {
            Some(Ok(json!({ "ok": true, "version": "0.1.0" })))
        }
        Some("chat/session/create") => {
            Some(Ok(json!({ "session_id": "s1", "agent_name": null })))
        }
        Some("sessions/follow") => Some(Ok(baseline("s1", "idle", Vec::new()))),
        Some("sessions/list") => Some(Ok(json!({
            "data": [
                { "id": "s0", "status": "completed" },
                { "id": "s1", "status": "working" },
            ],
        }))),
        Some("chat/message/send") => Some(Ok(json!({
            "message_id": "m1", "queued": true, "delivery": "queue",
        }))),
        _ => None,
    })
}

/// Send every wire action the reducer produced.
async fn send_actions(sender: &Transport, actions: &[Action]) {
    for action in actions {
        if let Some(request) = action.request() {
            sender.send(&request).await.expect("the request is sent");
        }
    }
}

/// Attach, wait for the (synthesized) `Ready`, and drive the reducer's
/// actions through the transport until the follow baseline lands.
async fn establish(
    gateway: &FakeGateway,
    driver: &mut Driver,
    sender: &Transport,
    app: &mut App,
) {
    let event = next_event(driver).await;
    assert!(matches!(event, TransportEvent::Ready { .. }), "{event:?}");
    let actions = app.apply(event);
    assert_eq!(actions, vec![Action::CreateSession, Action::FetchUiSchema]);
    send_actions(sender, &actions).await;
    gateway.wait_request("chat/session/create", 1).await;

    let event = next_event(driver).await;
    let actions = app.apply(event);
    assert_eq!(
        actions,
        vec![Action::Follow {
            session_id: "s1".to_string(),
        }]
    );
    send_actions(sender, &actions).await;
    gateway.wait_request("sessions/follow", 1).await;

    app.apply(next_event(driver).await);
    assert_eq!(app.session_id(), Some("s1"));
}

#[tokio::test]
async fn one_conversation_turn_ends_idle_with_the_assistant_text() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    app.insert_str("what is the weather?");
    let actions = app.submit();
    assert_eq!(actions.len(), 1);
    send_actions(&sender, &actions).await;
    gateway.wait_request("chat/message/send", 1).await;
    // The send's answer is its own event; consume it before the
    // notifications the turn produces.
    app.apply(next_event(&mut driver).await);
    assert_eq!(app.turn(), TurnState::Running);
    gateway
        .notify(
            "session/event",
            json!({
                "session_id": "s1",
                "kind": "row",
                "seq": 1,
                "row": text_row("user_message", 1, "what is the weather?"),
            }),
        )
        .await;
    app.apply(next_event(&mut driver).await);

    gateway
        .notify(
            "session/event",
            json!({
                "session_id": "s1",
                "kind": "live",
                "live": {
                    "turn_id": "t1",
                    "live_seq": 1,
                    "running": true,
                    "text": "Sunny with",
                    "reasoning": "",
                    "tool_calls": [],
                },
            }),
        )
        .await;
    app.apply(next_event(&mut driver).await);
    assert_eq!(app.live_text(), "Sunny with");

    gateway
        .notify(
            "session/event",
            json!({
                "session_id": "s1",
                "kind": "row",
                "seq": 2,
                "row": text_row(
                    "assistant_message",
                    2,
                    "Sunny with a light breeze.",
                ),
            }),
        )
        .await;
    app.apply(next_event(&mut driver).await);

    gateway
        .notify(
            "session/event",
            json!({
                "session_id": "s1",
                "kind": "row",
                "seq": 3,
                "row": turn_end("completed"),
            }),
        )
        .await;
    app.apply(next_event(&mut driver).await);

    assert_eq!(app.turn(), TurnState::Idle);
    assert!(app.live_text().is_empty());
    let last = app.messages().last().expect("the assistant row");
    assert_eq!(last.role, crate::app::Role::Assistant);
    assert_eq!(last.text, "Sunny with a light breeze.");
}

#[tokio::test]
async fn an_approval_answer_carries_the_exact_id_and_decision() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(Arc::new(|frame: &Value| {
        match frame["method"].as_str() {
            Some("health/ping") => {
                Some(Ok(json!({ "ok": true, "version": "0.1.0" })))
            }
            Some("chat/session/create") => {
                Some(Ok(json!({ "session_id": "s1", "agent_name": null })))
            }
            Some("sessions/follow") => {
                Some(Ok(baseline("s1", "idle", Vec::new())))
            }
            Some("approval/respond") => Some(Ok(Value::Null)),
            _ => None,
        }
    }));
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    gateway
        .notify(
            "approval/request",
            json!({
                "session_id": "s1",
                "approval_id": "a1",
                "call_id": "c1",
                "tool": "shell",
                "risk": "high",
                "arguments_hash": "sha256:args",
                "agent": "default",
            }),
        )
        .await;
    app.apply(next_event(&mut driver).await);
    assert_eq!(app.turn(), TurnState::AwaitingApproval);
    assert_eq!(app.approval().expect("the prompt").approval_id, "a1");

    let action = app
        .answer_approval(Decision::Allow)
        .expect("an answer action");
    let request = action.request().expect("the answer is a wire request");
    assert_eq!(request.method(), "approval/respond");
    assert_eq!(
        request.params,
        json!({
            "session_id": "s1",
            "approval_id": "a1",
            "decision": "allow",
        })
    );
    send_actions(&sender, std::slice::from_ref(&action)).await;
    let frame = gateway.wait_request("approval/respond", 1).await;
    assert_eq!(frame["params"]["approval_id"], "a1");
    assert_eq!(frame["params"]["decision"], "allow");
    app.apply(next_event(&mut driver).await);
    assert_eq!(app.turn(), TurnState::Running);
    assert!(app.approval().is_none());

    gateway
        .notify(
            "session/event",
            json!({
                "session_id": "s1",
                "kind": "row",
                "seq": 4,
                "row": text_row("assistant_message", 4, "done"),
            }),
        )
        .await;
    app.apply(next_event(&mut driver).await);
    gateway
        .notify(
            "session/event",
            json!({
                "session_id": "s1",
                "kind": "row",
                "seq": 5,
                "row": turn_end("completed"),
            }),
        )
        .await;
    app.apply(next_event(&mut driver).await);
    assert_eq!(app.turn(), TurnState::Idle);
    assert_eq!(app.messages().last().expect("the row").text, "done");
}

#[tokio::test]
async fn a_dropped_gateway_is_reported_and_the_reconnect_refollows() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    // A send that never gets answered keeps a request in flight, so
    // the drop is observed at once.
    gateway.set_responder(Arc::new(|frame: &Value| {
        match frame["method"].as_str() {
            Some("health/ping") => {
                Some(Ok(json!({ "ok": true, "version": "0.1.0" })))
            }
            _ => None,
        }
    }));
    app.insert_str("during the outage");
    let actions = app.submit();
    send_actions(&sender, &actions).await;
    gateway.wait_request("chat/message/send", 1).await;

    gateway.kill();
    gateway.wait_connections(0).await;
    let event = next_event(&mut driver).await;
    assert!(
        matches!(event, TransportEvent::Disconnected { .. }),
        "{event:?}"
    );
    app.apply(event);
    assert!(matches!(app.connection(), ConnectionState::Failed(_)));
    assert_eq!(app.turn(), TurnState::Idle);

    // The client reconnects on its own; the immediate probe answers
    // through the new connection, synthesizes a fresh `Ready`, and
    // re-issues the recorded follow. The responder answers it with a
    // baseline that carries the row written before the drop.
    gateway.set_responder(Arc::new(|frame: &Value| {
        match frame["method"].as_str() {
            Some("health/ping") => {
                Some(Ok(json!({ "ok": true, "version": "0.1.0" })))
            }
            Some("sessions/follow") => Some(Ok(baseline(
                "s1",
                "idle",
                vec![text_row("user_message", 1, "before the crash")],
            ))),
            _ => None,
        }
    }));
    gateway.wait_connections(1).await;
    let mut restored = false;
    for _ in 0..8 {
        let event = next_event(&mut driver).await;
        app.apply(event);
        if app
            .messages()
            .last()
            .is_some_and(|line| line.text == "before the crash")
        {
            restored = true;
            break;
        }
    }
    assert!(restored, "the follow baseline rebuilt the transcript");
    assert!(matches!(app.connection(), ConnectionState::Ready { .. }));
    assert_eq!(
        gateway.wait_request("sessions/follow", 2).await["method"],
        json!("sessions/follow")
    );
}

#[tokio::test]
async fn a_server_error_object_becomes_a_failed_response() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    gateway.set_responder(Arc::new(|frame: &Value| {
        match frame["method"].as_str() {
            Some("health/ping") => {
                Some(Ok(json!({ "ok": true, "version": "0.1.0" })))
            }
            Some("chat/message/send") => Some(Err(json!({
                "code": -32000,
                "message": "the session is closed",
                "data": { "code": "session_closed" },
            }))),
            _ => None,
        }
    }));
    app.insert_str("anyone there?");
    let actions = app.submit();
    send_actions(&sender, &actions).await;
    gateway.wait_request("chat/message/send", 1).await;

    let event = next_event(&mut driver).await;
    match event {
        TransportEvent::Response { request, .. } => {
            assert_eq!(
                request,
                Some(crate::transport::RequestKind::SendMessage)
            );
        }
        other => panic!("expected a send failure, got {other:?}"),
    }
    app.apply(event);
    assert_eq!(app.turn(), TurnState::Idle);
    assert!(app.status().contains("the session is closed"));
}

#[tokio::test]
async fn the_sidebar_populates_from_sessions_list() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    let actions = app.refresh_sidebar();
    send_actions(&sender, &actions).await;
    gateway.wait_request("sessions/list", 1).await;
    app.apply(next_event(&mut driver).await);

    let tree = app.workspaces();
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].name, "workspace");
    assert_eq!(tree[0].sessions.len(), 2);
    assert_eq!(tree[0].sessions[1].id, "s1");
}
