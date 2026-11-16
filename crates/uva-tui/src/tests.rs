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

/// The transcript's accounting of the wire vocabulary: every tag
/// `uva-session` knows is either applied or ignored-by-design, and
/// never both. A tag added to `KNOWN_SESSION_EVENT_TYPES` without a
/// classification fails here — the drop can no longer be accidental.
#[test]
fn every_known_row_kind_is_applied_or_ignored_by_design() {
    for tag in uva_session::KNOWN_SESSION_EVENT_TYPES {
        let applied = crate::app::APPLIED_ROW_TYPES.contains(tag);
        let ignored = crate::app::IGNORED_ROW_TYPES.contains(tag);
        assert!(
            applied || ignored,
            "the transcript has no accounting for `{tag}`: classify it \
             in APPLIED_ROW_TYPES or IGNORED_ROW_TYPES"
        );
        assert!(!applied || !ignored, "`{tag}` is both applied and ignored");
    }
}

/// The `apply_row` match must have an arm for every tag the
/// accounting declares applied: the accounting test above only sorts
/// tags into lists, so a tag added to `APPLIED_ROW_TYPES` without an
/// arm would otherwise compile, pass every test, and panic on the
/// first production row. One minimal row of each applied tag through
/// the real match makes that drift fail here instead.
#[test]
fn every_applied_row_kind_survives_apply_row() {
    for tag in crate::app::APPLIED_ROW_TYPES {
        let mut app = App::new();
        app.apply_row(&json!({ "type": tag, "seq": 1, "data": {} }));
    }
}

/// Mirrors the web projection: a warning row without a `message`
/// still renders, labeled by its `kind` slug.
#[test]
fn a_warning_row_without_a_message_falls_back_to_its_kind() {
    let mut app = App::new();
    app.apply_row(&json!({
        "type": "warning",
        "seq": 1,
        "data": { "kind": "config_drift" },
    }));
    let line = app.messages().last().expect("the warning notice");
    assert_eq!(line.role, crate::app::Role::System);
    assert_eq!(line.text, "warning: config_drift");
}

#[tokio::test]
async fn a_warning_row_becomes_a_one_line_transcript_notice() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    gateway
        .notify(
            "session/event",
            json!({
                "session_id": "s1",
                "kind": "row",
                "seq": 4,
                "row": {
                    "type": "warning",
                    "seq": 4,
                    "data": {
                        "kind": "strict_fold_refused",
                        "message": "the session log does not fold strictly",
                    },
                },
            }),
        )
        .await;
    app.apply(next_event(&mut driver).await);

    let line = app.messages().last().expect("the warning notice");
    assert_eq!(line.role, crate::app::Role::System);
    assert_eq!(line.text, "warning: the session log does not fold strictly");
}

// ---- the tool activity the operator must see ----------------------

/// The assistant row the model wrote when it called a tool: the
/// turn's text and its `tool_use` part travel in one row, the way
/// the harness's `write_assistant_turn` persists them.
fn tool_call_row(seq: u64, call_id: &str, name: &str) -> Value {
    json!({
        "type": "assistant_message",
        "seq": seq,
        "data": {
            "role": "assistant",
            "content": { "Multi": [
                { "type": "text", "text": "let me look" },
                {
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": { "command": "ls" },
                },
            ]},
        },
    })
}

/// A tool argument payload longer than one transcript line carries.
const LONG_ARGUMENT: &str = "tr '\n' ' ' < /dev/urandom | head -c 4096";

/// A tool output longer than one transcript line carries.
const LONG_OUTPUT: &str =
    "one two three four five six seven eight nine ten eleven twelve";

/// The same call row with an argument payload longer than one
/// transcript line carries, for the truncation rule to cut.
fn long_tool_call_row(seq: u64, call_id: &str, name: &str) -> Value {
    json!({
        "type": "assistant_message",
        "seq": seq,
        "data": {
            "role": "assistant",
            "content": { "Multi": [
                { "type": "text", "text": "let me look" },
                {
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": { "command": LONG_ARGUMENT },
                },
            ]},
        },
    })
}

/// A multi-byte argument payload: each character is three bytes, so
/// a byte-based cut would keep a third of the characters the
/// character-based rule keeps.
const MULTIBYTE_ARGUMENT: &str = "中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中";

/// The same call row with a multi-byte argument payload, so the
/// truncation rule proves it counts characters, not bytes.
fn multibyte_tool_call_row(seq: u64, call_id: &str, name: &str) -> Value {
    json!({
        "type": "assistant_message",
        "seq": seq,
        "data": {
            "role": "assistant",
            "content": { "Multi": [
                { "type": "text", "text": "let me look" },
                {
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": { "command": MULTIBYTE_ARGUMENT },
                },
            ]},
        },
    })
}

/// The row the crash repair writes for a call that never finished:
/// no output, an `error` recovery code, and the interrupted marker.
fn repair_closer_row(seq: u64, call_id: &str, code: &str) -> Value {
    json!({
        "type": "tool_result",
        "seq": seq,
        "is_error": true,
        "data": {
            "call_id": call_id,
            "tool_name": "shell",
            "error": code,
            "interrupted": true,
        },
    })
}

/// A tool result whose output is longer than one transcript line
/// carries, so the truncation rule has something to cut.
fn long_tool_result_row(seq: u64, call_id: &str) -> Value {
    json!({
        "type": "tool_result",
        "seq": seq,
        "data": {
            "role": "tool",
            "content": { "Single": { "type": "text", "text": LONG_OUTPUT } },
            "tool_call_id": call_id,
        },
        "tool_call_id": call_id,
    })
}

/// The row the loop wrote when the tool answered: the tool message
/// with the output, the row's own failure flag, and the id of the
/// call the result answers.
fn tool_result_row(seq: u64, call_id: &str, is_error: bool) -> Value {
    json!({
        "type": "tool_result",
        "seq": seq,
        "data": {
            "role": "tool",
            "content": { "Single": { "type": "text", "text": "a.txt\nb.txt" } },
            "tool_call_id": call_id,
        },
        "is_error": is_error,
        "tool_call_id": call_id,
    })
}

/// A tool result whose output is several text parts: the pieces are
/// one result, so the line keeps a space between them.
fn multipart_tool_result_row(seq: u64, call_id: &str) -> Value {
    json!({
        "type": "tool_result",
        "seq": seq,
        "data": {
            "role": "tool",
            "content": { "Multi": [
                { "type": "text", "text": "part one" },
                { "type": "text", "text": "part two" },
            ]},
            "tool_call_id": call_id,
        },
        "tool_call_id": call_id,
    })
}

/// Broadcast one durable row and fold it into the projection.
async fn notify_row(
    gateway: &FakeGateway,
    driver: &mut Driver,
    app: &mut App,
    seq: u64,
    row: Value,
) {
    gateway
        .notify(
            "session/event",
            json!({
                "session_id": "s1",
                "kind": "row",
                "seq": seq,
                "row": row,
            }),
        )
        .await;
    app.apply(next_event(driver).await);
}

/// The operator must see the tool activity: the call the model made
/// and the outcome the tool produced. The call belongs to the
/// assistant's turn (the model asked for it); the outcome belongs to
/// the tool.
#[tokio::test]
async fn a_tool_call_and_its_result_become_two_transcript_lines() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        2,
        tool_call_row(2, "call_1", "shell"),
    )
    .await;
    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        3,
        tool_result_row(3, "call_1", false),
    )
    .await;

    let lines = app.messages();
    let call = lines
        .iter()
        .find(|line| line.text == "tool call: shell {\"command\":\"ls\"}")
        .expect("the tool call line");
    assert_eq!(call.role, crate::app::Role::Assistant);

    let result = lines.last().expect("the tool result line");
    assert_eq!(result.role, crate::app::Role::Tool);
    assert_eq!(result.text, "shell: ok a.txt b.txt");
}

/// A failed tool says so: the outcome line carries the failure, not
/// a silent success.
#[tokio::test]
async fn a_failed_tool_result_says_it_failed() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        2,
        tool_call_row(2, "call_1", "shell"),
    )
    .await;
    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        3,
        tool_result_row(3, "call_1", true),
    )
    .await;

    // A failure that produced output says both: the outcome and what
    // the tool said before it failed.
    let result = app.messages().last().expect("the tool result line");
    assert_eq!(result.role, crate::app::Role::Tool);
    assert_eq!(result.text, "shell: failed a.txt b.txt");
}

/// A result whose call the TUI never saw — a resumed session's first
/// row, or a window that starts after the call — still renders: the
/// name is unknown, the outcome is not.
#[tokio::test]
async fn a_tool_result_without_its_call_still_renders() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        3,
        tool_result_row(3, "call_1", false),
    )
    .await;

    let result = app.messages().last().expect("the tool result line");
    assert_eq!(result.role, crate::app::Role::Tool);
    assert_eq!(result.text, "unknown tool: ok a.txt b.txt");
}

/// Two calls to the same tool are two calls, not one line repeated:
/// the transcript must not collapse them.
#[tokio::test]
async fn two_calls_to_the_same_tool_are_two_lines() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        2,
        tool_call_row(2, "call_1", "shell"),
    )
    .await;
    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        3,
        tool_call_row(3, "call_2", "shell"),
    )
    .await;

    let calls = app
        .messages()
        .iter()
        .filter(|line| line.text == "tool call: shell {\"command\":\"ls\"}")
        .count();
    assert_eq!(calls, 2, "both calls are on the transcript");
}

/// A result row whose payload is a `tool_result` part names its tool
/// in that part: the row the app-server writes for a seeded history
/// carries the part, not the run's own message shape.
#[tokio::test]
async fn a_tool_result_part_names_the_tool_it_answers() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        3,
        json!({
            "type": "tool_result",
            "seq": 3,
            "data": {
                "role": "tool",
                "content": { "Single": {
                    "type": "tool_result",
                    "tool_use_id": "call_1",
                    "tool_name": "shell",
                    "content": [],
                    "is_error": true,
                }},
            },
        }),
    )
    .await;

    // The part's content list is empty, so there is no output to
    // carry — the line says the outcome and nothing else.
    let result = app.messages().last().expect("the tool result line");
    assert_eq!(result.role, crate::app::Role::Tool);
    assert_eq!(result.text, "shell: failed");
}

/// A hook veto is a durable fact about the session: it renders as a
/// one-line notice, like the warning row. The row's fixture is the
/// wire shape the one writer of the row produces (`EventSink::
/// append_hook_block`, pinned in `docs/log-format.md`): `data.hook`
/// names the policy, `data.reason` says why, `data.tool` the call it
/// refused — so the line names all three, the way the web's `⛔`
/// notice does.
#[tokio::test]
async fn a_hook_block_row_names_the_policy_and_the_call_it_refused() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        4,
        json!({
            "type": "hook_block",
            "seq": 4,
            "data": {
                "hook": "shell-deny",
                "reason": "rm -rf is not allowed",
                "tool": "shell",
                "call_id": "call_1",
            },
        }),
    )
    .await;

    let line = app.messages().last().expect("the hook notice");
    assert_eq!(line.role, crate::app::Role::System);
    assert_eq!(
        line.text,
        "hook: shell-deny blocked shell — rm -rf is not allowed"
    );
}

/// A row that names neither the policy nor the call still renders:
/// the reason alone says what happened, and a row with nothing at
/// all still says it was blocked.
#[test]
fn a_hook_block_row_renders_whatever_fields_it_has() {
    let mut app = App::new();
    app.apply_row(&json!({
        "type": "hook_block",
        "seq": 4,
        "data": { "reason": "the command is not allowed" },
    }));
    assert_eq!(
        app.messages().last().expect("the reason-only notice").text,
        "hook: the command is not allowed"
    );

    app.apply_row(&json!({
        "type": "hook_block",
        "seq": 5,
        "data": { "hook": "shell-deny", "tool": "shell" },
    }));
    assert_eq!(
        app.messages().last().expect("the policy-only notice").text,
        "hook: shell-deny blocked shell"
    );

    app.apply_row(&json!({
        "type": "hook_block",
        "seq": 6,
        "data": {},
    }));
    assert_eq!(
        app.messages().last().expect("the bare notice").text,
        "hook: blocked"
    );
}

/// The call the crash-repair closer writes — a call that never
/// finished — says why it failed: the recovery code rides the line
/// instead of leaving the operator to guess at a bare "failed".
#[tokio::test]
async fn an_interrupted_call_names_the_recovery_code() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        2,
        tool_call_row(2, "call_1", "shell"),
    )
    .await;
    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        3,
        repair_closer_row(3, "call_1", "TOOL_NOT_STARTED"),
    )
    .await;
    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        5,
        tool_call_row(5, "call_2", "shell"),
    )
    .await;
    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        6,
        repair_closer_row(6, "call_2", "TOOL_OUTCOME_UNKNOWN"),
    )
    .await;

    let lines = app.messages();
    let first = lines
        .iter()
        .find(|line| line.text.starts_with("shell: failed"))
        .expect("the first closer's line");
    assert_eq!(first.text, "shell: failed (TOOL_NOT_STARTED)");
    let second = lines
        .iter()
        .filter(|line| line.text.starts_with("shell: failed"))
        .nth(1)
        .expect("the second closer's line");
    assert_eq!(second.text, "shell: failed (TOOL_OUTCOME_UNKNOWN)");
}

/// The arguments and the output are the two halves the transcript
/// was missing: the call line carries what the model asked for, the
/// result line what the tool said — each held to one line by the
/// projection's truncation rule.
#[tokio::test]
async fn the_call_line_carries_the_arguments_and_the_result_the_output() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        2,
        long_tool_call_row(2, "call_1", "shell"),
    )
    .await;
    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        3,
        long_tool_result_row(3, "call_1"),
    )
    .await;

    let lines = app.messages();
    let call = lines
        .iter()
        .find(|line| line.text.starts_with("tool call: shell"))
        .expect("the call line");
    // The 48-character preview plus the `…` marker: the payload is
    // longer than one line carries, and the cut is visible. The rule
    // counts characters, so the assertion does too — and exactly one
    // `…` is the marker, so a payload's own trailing `…` survives.
    assert!(call.text.ends_with('…'), "the cut is marked: {call:?}");
    let kept = call.text.strip_suffix('…').expect("one marker");
    assert_eq!(
        kept.chars().count(),
        "tool call: shell ".chars().count() + 48,
        "the arguments fill the line to the limit: {call:?}"
    );
    assert!(
        call.text.contains("tr '\\n'"),
        "the payload is real: {call:?}"
    );

    let result = lines.last().expect("the result line");
    assert_eq!(
        result.text,
        "shell: ok one two three four five six seven eight nine ten…"
    );
    assert!(result.text.ends_with('…'), "the cut is marked: {result:?}");
}

/// The one-line rule is a **character** count: a multi-byte payload
/// fills the line to the same character count an ASCII one does, and
/// the cut lands between characters. A byte-based rule would keep a
/// third of the characters and split a code point.
#[tokio::test]
async fn a_multibyte_payload_fills_the_line_by_characters() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        2,
        multibyte_tool_call_row(2, "call_1", "shell"),
    )
    .await;

    let call = app
        .messages()
        .iter()
        .find(|line| line.text.starts_with("tool call: shell"))
        .expect("the call line");
    let kept = call.text.strip_suffix('…').expect("the cut is marked");
    // The flattened arguments are `{"command":"` (12 ASCII characters)
    // followed by the payload, so 48 characters keep 36 of them.
    assert_eq!(
        kept.chars().count(),
        "tool call: shell ".chars().count() + 48,
        "a multi-byte payload fills the line to the character limit: {call:?}"
    );
    let han = kept
        .chars()
        .filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
        .count();
    assert_eq!(han, 36, "the cut kept whole characters: {call:?}");
    assert!(
        kept.len() > kept.chars().count(),
        "the payload is multi-byte, so bytes outnumber characters: {call:?}"
    );
}

/// The tool rows are applied, not ignored: the accounting lists them
/// on the applied side, so the transcript renders them.
#[test]
fn the_tool_rows_are_applied_not_ignored() {
    for tag in ["tool_result", "hook_block"] {
        assert!(
            crate::app::APPLIED_ROW_TYPES.contains(&tag),
            "`{tag}` is applied: the transcript renders it"
        );
        assert!(
            !crate::app::IGNORED_ROW_TYPES.contains(&tag),
            "`{tag}` is no longer ignored by design"
        );
    }
}

/// A tool output of several text parts is one result in several
/// pieces: the line keeps a space between them rather than gluing
/// the words together.
#[tokio::test]
async fn a_multi_part_tool_output_keeps_a_space_between_the_parts() {
    let gateway = FakeGateway::spawn().await;
    gateway.set_responder(conversation_responder());
    let mut driver = Driver::attach(&gateway.url);
    let sender = driver.sender();
    let mut app = App::new();
    establish(&gateway, &mut driver, &sender, &mut app).await;

    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        2,
        tool_call_row(2, "call_1", "shell"),
    )
    .await;
    notify_row(
        &gateway,
        &mut driver,
        &mut app,
        3,
        multipart_tool_result_row(3, "call_1"),
    )
    .await;

    let result = app.messages().last().expect("the result line");
    assert_eq!(result.text, "shell: ok part one part two");
}
