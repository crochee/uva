//! Gateway transport tests: Content-Length in both directions,
//! response and event routing, approval forwarding, and the restart
//! loop.
//!
//! The routing seam is private (`AppServer::route_frame` and the
//! `supervise_with` spawn hook), so these live beside the module
//! rather than in `tests/`: an integration test cannot see them
//! without widening the gateway's public surface.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use dashmap::DashMap;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex},
    sync::{Notify, mpsc},
};

use super::{
    AppServer,
    ConnectionHandle,
    CoreHandle,
    SpawnedCore,
    pump_core,
    supervise_with,
};
use crate::rpc::framing::{DEFAULT_MAX_FRAME_SIZE, read_frame};

/// Encode one frame the way the core puts it on the wire.
fn frame(payload: &str) -> Vec<u8> {
    let mut bytes =
        format!("Content-Length: {}\r\n\r\n", payload.len()).into_bytes();
    bytes.extend_from_slice(payload.as_bytes());
    bytes
}

/// The `ready` notification a fake core emits.
fn ready_frame(generation: u64) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "ready",
        "params": { "version": format!("test-{generation}") },
    })
    .to_string()
}

/// An App Server with no supervisor and no connections.
fn app() -> Arc<AppServer> {
    Arc::new(AppServer {
        core: CoreHandle {
            stdin: Arc::new(tokio::sync::Mutex::new(None)),
        },
        connections: DashMap::new(),
        next_connection: AtomicU64::new(1),
        ready: Arc::new(AtomicBool::new(false)),
        internal_pending: DashMap::new(),
        next_internal_id: AtomicU64::new(1),
    })
}

/// Register a connection and hand back its id and outbound queue.
fn connect(app_server: &AppServer) -> (u64, mpsc::UnboundedReceiver<String>) {
    let connection = app_server.next_connection.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::unbounded_channel();
    app_server.connections.insert(
        connection,
        ConnectionHandle {
            tx,
            pending: DashMap::new(),
            subscriptions: DashMap::new(),
            owned: DashMap::new(),
        },
    );
    (connection, rx)
}

/// Give the gateway a writable core stdin; the returned half is the
/// wire the test reads.
async fn attach_core_stdin(app_server: &AppServer) -> DuplexStream {
    let (writer, reader) = duplex(8 * 1024);
    *app_server.core.stdin.lock().await = Some(Box::new(writer));
    reader
}

/// Close the core's stdin and read every byte the gateway wrote.
async fn take_wire(
    app_server: &AppServer,
    mut reader: DuplexStream,
) -> Vec<u8> {
    *app_server.core.stdin.lock().await = None;
    let mut wire = Vec::new();
    reader
        .read_to_end(&mut wire)
        .await
        .expect("the test reads the wire");
    wire
}

/// Feed one downstream payload the way the pump does: parse it, then
/// route.
fn route(app_server: &AppServer, payload: &str) {
    let parsed: Option<Value> = serde_json::from_str(payload).ok();
    app_server.route_frame(parsed.as_ref(), payload);
}

/// Subscribe a connection without putting a follow on the wire — for
/// tests that are not about the follow bookkeeping.
fn subscribe(app_server: &AppServer, connection: u64, session: &str) {
    app_server
        .connections
        .get(&connection)
        .expect("the connection exists")
        .subscriptions
        .insert(session.to_string(), ());
}

/// Send a `sessions/follow` so the connection subscribes to `session`.
async fn follow(app_server: &AppServer, connection: u64, session: &str) {
    let frame = json!({
        "jsonrpc": "2.0",
        "id": format!("follow-{session}"),
        "method": "sessions/follow",
        "params": { "session_id": session },
    });
    app_server
        .forward_upstream(connection, &frame.to_string())
        .await
        .expect("the follow reaches the core");
}

/// Poll `condition` until it holds or `limit` elapses.
async fn wait_until(
    mut condition: impl FnMut() -> bool,
    limit: Duration,
    what: &str,
) {
    let deadline = tokio::time::Instant::now() + limit;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The gateway writes one Content-Length frame, with no trailing
/// newline: the core's codec would read a stray `\n` as the start of
/// the next header block.
#[tokio::test]
async fn core_send_writes_one_content_length_frame_without_a_newline() {
    let app_server = app();
    let reader = attach_core_stdin(&app_server).await;
    let payload = json!({
        "jsonrpc": "2.0",
        "id": "c1",
        "method": "health/ping",
        "params": {},
    })
    .to_string();
    app_server
        .core
        .send(&payload)
        .await
        .expect("the request reaches the core");
    let wire = take_wire(&app_server, reader).await;
    assert_eq!(
        wire,
        frame(&payload),
        "the wire is exactly header + payload, no newline"
    );
    // It decodes as exactly one frame and leaves no trailing byte —
    // the newline the old LF transport appended would be left over.
    let mut bytes = wire.as_slice();
    let decoded = read_frame(&mut bytes, DEFAULT_MAX_FRAME_SIZE)
        .await
        .expect("one frame");
    assert_eq!(decoded, payload.as_bytes());
    assert!(bytes.is_empty(), "nothing follows the frame");
}

/// An answered request goes back to the connection that minted the
/// id; a second connection on the same socket sees nothing.
#[tokio::test]
async fn routes_response_to_origin_connection() {
    let app_server = app();
    let reader = attach_core_stdin(&app_server).await;
    let (first, mut first_rx) = connect(&app_server);
    let (_second, mut second_rx) = connect(&app_server);

    let request = json!({
        "jsonrpc": "2.0",
        "id": "c1",
        "method": "health/ping",
        "params": {},
    });
    app_server
        .forward_upstream(first, &request.to_string())
        .await
        .expect("the request reaches the core");
    assert_eq!(
        take_wire(&app_server, reader).await,
        frame(&request.to_string()),
        "the request crossed to the core as one frame"
    );

    let response = json!({
        "jsonrpc": "2.0",
        "id": "c1",
        "result": { "ok": true },
    });
    route(&app_server, &response.to_string());
    assert_eq!(first_rx.recv().await, Some(response.to_string()));
    assert!(
        second_rx.try_recv().is_err(),
        "the origin's response is not broadcast"
    );
}

/// A numeric id is keyed the same way on both sides: the request
/// registers the decimal form, and the response finds it.
#[tokio::test]
async fn routes_numeric_ids_to_their_origin_connection() {
    let app_server = app();
    let _reader = attach_core_stdin(&app_server).await;
    let (first, mut first_rx) = connect(&app_server);
    let (second, mut second_rx) = connect(&app_server);

    let first_request = json!({
        "jsonrpc": "2.0",
        "id": 41,
        "method": "health/ping",
        "params": {},
    });
    app_server
        .forward_upstream(first, &first_request.to_string())
        .await
        .expect("the first request reaches the core");
    let second_request = json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "health/ping",
        "params": {},
    });
    app_server
        .forward_upstream(second, &second_request.to_string())
        .await
        .expect("the second request reaches the core");

    let response = json!({
        "jsonrpc": "2.0",
        "id": 42,
        "result": { "ok": true },
    });
    route(&app_server, &response.to_string());
    assert_eq!(second_rx.recv().await, Some(response.to_string()));
    assert!(first_rx.try_recv().is_err());
}

/// `sessions/follow` and `session/unfollow` manage the subscription
/// set the router reads, and both frames still reach the core.
#[tokio::test]
async fn forward_upstream_records_follow_and_unfollow() {
    let app_server = app();
    let reader = attach_core_stdin(&app_server).await;
    let (connection, _outbound) = connect(&app_server);

    follow(&app_server, connection, "s1").await;
    assert!(
        app_server
            .connections
            .get(&connection)
            .is_some_and(|handle| handle.subscriptions.contains_key("s1"))
    );
    let unfollow = json!({
        "jsonrpc": "2.0",
        "id": "u1",
        "method": "session/unfollow",
        "params": { "session_id": "s1" },
    });
    app_server
        .forward_upstream(connection, &unfollow.to_string())
        .await
        .expect("the unfollow reaches the core");
    assert!(
        app_server
            .connections
            .get(&connection)
            .is_some_and(|handle| handle.subscriptions.is_empty())
    );

    let follow_frame = json!({
        "jsonrpc": "2.0",
        "id": "follow-s1",
        "method": "sessions/follow",
        "params": { "session_id": "s1" },
    });
    let wire = take_wire(&app_server, reader).await;
    let mut expected = frame(&follow_frame.to_string());
    expected.extend_from_slice(&frame(&unfollow.to_string()));
    assert_eq!(wire, expected, "both management frames cross framed");
}

/// Session events are routed by `params.session_id` to the followers
/// only — never broadcast to a connection that did not follow the
/// session.
#[tokio::test]
async fn routes_session_event_to_followers_only() {
    let app_server = app();
    let _reader = attach_core_stdin(&app_server).await;
    let (first, mut first_rx) = connect(&app_server);
    let (second, mut second_rx) = connect(&app_server);
    subscribe(&app_server, first, "s1");
    subscribe(&app_server, second, "s2");

    let event = json!({
        "jsonrpc": "2.0",
        "method": "session/event",
        "params": {
            "session_id": "s1",
            "kind": "row",
            "seq": 1,
            "row": { "type": "message" },
        },
    });
    route(&app_server, &event.to_string());
    assert_eq!(first_rx.recv().await, Some(event.to_string()));
    assert!(
        second_rx.try_recv().is_err(),
        "a non-follower never sees the event"
    );

    // The Anthropic compatibility stream follows the same rule.
    let anthropic = json!({
        "jsonrpc": "2.0",
        "method": "anthropic/messages/event",
        "params": { "session_id": "s2", "delta": { "text": "hi" } },
    });
    route(&app_server, &anthropic.to_string());
    assert_eq!(second_rx.recv().await, Some(anthropic.to_string()));
    assert!(first_rx.try_recv().is_err());
}

/// `approval/request` is a session-scoped notification: followers get
/// it, and `approval/respond` is an ordinary client request that the
/// gateway forwards upstream byte for byte (the core maps it to
/// `sessions/resume`; `approval_id` is the correlation id, not the
/// JSON-RPC id).
#[tokio::test]
async fn forwards_approval_request_and_response() {
    let app_server = app();
    let reader = attach_core_stdin(&app_server).await;
    let (follower, mut follower_rx) = connect(&app_server);
    let (other, mut other_rx) = connect(&app_server);
    subscribe(&app_server, follower, "s1");

    let request = json!({
        "jsonrpc": "2.0",
        "method": "approval/request",
        "params": {
            "approval_id": "appr-1",
            "session_id": "s1",
            "tool": "shell",
            "risk": "high",
            "preview": "rm -rf build",
        },
    });
    route(&app_server, &request.to_string());
    assert_eq!(
        follower_rx.recv().await,
        Some(request.to_string()),
        "the follower receives the approval request unchanged"
    );
    assert!(
        other_rx.try_recv().is_err(),
        "a connection that does not follow the session sees nothing"
    );

    let respond = json!({
        "jsonrpc": "2.0",
        "id": "c9",
        "method": "approval/respond",
        "params": {
            "approval_id": "appr-1",
            "session_id": "s1",
            "decision": "allow",
        },
    });
    app_server
        .forward_upstream(other, &respond.to_string())
        .await
        .expect("the decision reaches the core");
    assert_eq!(
        take_wire(&app_server, reader).await,
        frame(&respond.to_string()),
        "the decision is forwarded verbatim as one frame"
    );
}

/// The ACP surface's routing: the session a client opens is its own,
/// so the session's updates and the questions about it go to that
/// client — and the answer to a question finds its way back.
#[tokio::test]
async fn routes_the_acp_session_to_the_connection_that_opened_it() {
    let app_server = app();
    let _reader = attach_core_stdin(&app_server).await;
    let (owner, mut owner_rx) = connect(&app_server);
    let (_other, mut other_rx) = connect(&app_server);

    // The client prompts the session it opened: the connection owns
    // that session from now on.
    let prompt = json!({
        "jsonrpc": "2.0",
        "id": "c1",
        "method": "session/prompt",
        "params": {
            "sessionId": "s1",
            "prompt": [{"type": "text", "text": "hi"}],
        },
    });
    app_server
        .forward_upstream(owner, &prompt.to_string())
        .await
        .expect("the prompt reaches the core");

    // The session's update goes to the owner, not to everyone.
    let update = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "s1",
            "update": {"sessionUpdate": "agent_message_chunk"},
        },
    });
    route(&app_server, &update.to_string());
    assert_eq!(owner_rx.recv().await, Some(update.to_string()));
    assert!(
        other_rx.try_recv().is_err(),
        "a connection that did not open the session sees nothing"
    );

    // The core asks the client a question about that session: the
    // question reaches the owner.
    let question = json!({
        "jsonrpc": "2.0",
        "id": "uva-1",
        "method": "session/request_permission",
        "params": { "sessionId": "s1", "options": [] },
    });
    route(&app_server, &question.to_string());
    assert_eq!(owner_rx.recv().await, Some(question.to_string()));
    assert!(other_rx.try_recv().is_err());

    // The answer comes back with the question's id: the gateway
    // remembers which connection the question went to.
    let answer = json!({
        "jsonrpc": "2.0",
        "id": "uva-1",
        "result": { "outcome": { "outcome": "cancelled" } },
    });
    route(&app_server, &answer.to_string());
    assert_eq!(owner_rx.recv().await, Some(answer.to_string()));
    assert!(other_rx.try_recv().is_err());
}

/// Readiness is the parsed notification, not a substring of the
/// payload: a row that happens to contain `"method":"ready"` must not
/// make the gateway claim readiness.
#[tokio::test]
async fn ready_is_the_parsed_method_not_a_substring() {
    let app_server = app();
    let (follower, mut follower_rx) = connect(&app_server);
    app_server
        .connections
        .get(&follower)
        .expect("the connection exists")
        .subscriptions
        .insert("s1".to_string(), ());
    let (mut core_writer, core_reader) = duplex(8 * 1024);
    let pump = tokio::spawn({
        let app_server = Arc::clone(&app_server);
        async move { pump_core(&app_server, core_reader).await }
    });

    let event = json!({
        "jsonrpc": "2.0",
        "method": "session/event",
        "params": {
            "session_id": "s1",
            "kind": "row",
            "row": { "type": "tool_call", "data": { "method": "ready" } },
        },
    });
    core_writer
        .write_all(&frame(&event.to_string()))
        .await
        .expect("the fake core writes the event");
    assert_eq!(
        follower_rx.recv().await,
        Some(event.to_string()),
        "the header never reaches a client; only the JSON payload does"
    );
    assert!(
        !app_server.is_ready(),
        "a payload that mentions ready is not the ready notification"
    );

    let ready = ready_frame(1);
    core_writer
        .write_all(&frame(&ready))
        .await
        .expect("the fake core writes ready");
    assert_eq!(follower_rx.recv().await, Some(ready));
    wait_until(
        || app_server.is_ready(),
        Duration::from_secs(1),
        "the ready flag",
    )
    .await;

    drop(core_writer);
    pump.await.expect("the pump ends at end of stream");
}

/// A dead core is restarted: the connection and the App Server stay
/// up, readiness resets until the new core announces itself, and the
/// old core's subscriptions and in-flight ids are cleared.
#[tokio::test]
async fn restarts_core_and_keeps_socket_open() {
    let app_server = app();
    let (connection, mut outbound) = connect(&app_server);
    let cancel = Arc::new(uva::core::AtomicCancelToken::new());
    let spawns = Arc::new(AtomicU64::new(0));
    let close_first = Arc::new(Notify::new());
    let release_second = Arc::new(Notify::new());
    let supervisor = tokio::spawn({
        let app_server = Arc::clone(&app_server);
        let cancel = Arc::clone(&cancel);
        let spawns = Arc::clone(&spawns);
        let close_first = Arc::clone(&close_first);
        let release_second = Arc::clone(&release_second);
        async move {
            supervise_with(
                Arc::clone(&app_server),
                move || {
                    let generation = spawns.fetch_add(1, Ordering::SeqCst) + 1;
                    let (mut writer, reader) = duplex(8 * 1024);
                    if generation == 1 {
                        let close_first = Arc::clone(&close_first);
                        tokio::spawn(async move {
                            writer
                                .write_all(&frame(&ready_frame(1)))
                                .await
                                .expect("the first core announces itself");
                            close_first.notified().await;
                            drop(writer);
                        });
                        Ok(SpawnedCore {
                            stdin: Some(Box::new(tokio::io::sink())),
                            stdout: Box::new(reader),
                            exited: Box::pin(async {}),
                        })
                    } else {
                        let release_second = Arc::clone(&release_second);
                        tokio::spawn(async move {
                            release_second.notified().await;
                            writer
                                .write_all(&frame(&ready_frame(2)))
                                .await
                                .expect("the second core announces itself");
                            std::future::pending::<()>().await;
                        });
                        Ok(SpawnedCore {
                            stdin: Some(Box::new(tokio::io::sink())),
                            stdout: Box::new(reader),
                            exited: Box::pin(std::future::pending()),
                        })
                    }
                },
                cancel,
            )
            .await;
        }
    });

    // The first core is up and the connection saw its notification.
    wait_until(
        || spawns.load(Ordering::SeqCst) == 1 && app_server.is_ready(),
        Duration::from_secs(2),
        "the first ready",
    )
    .await;
    assert_eq!(outbound.recv().await, Some(ready_frame(1)));

    // Follow a session and leave a request in flight.
    subscribe(&app_server, connection, "s1");
    let request = json!({
        "jsonrpc": "2.0",
        "id": "c7",
        "method": "health/ping",
        "params": {},
    });
    app_server
        .forward_upstream(connection, &request.to_string())
        .await
        .expect("the request reaches the first core");
    assert!(
        app_server
            .connections
            .get(&connection)
            .is_some_and(|handle| handle.subscriptions.contains_key("s1"))
    );

    // The first core exits; the supervisor restarts it.
    close_first.notify_one();
    wait_until(
        || spawns.load(Ordering::SeqCst) == 2,
        Duration::from_secs(4),
        "the restart",
    )
    .await;
    assert!(
        !app_server.is_ready(),
        "readiness is reset until the new core announces itself"
    );
    assert!(
        app_server
            .connections
            .get(&connection)
            .is_some_and(|handle| handle.subscriptions.is_empty()),
        "the old core's subscriptions are dropped"
    );
    let refusal = outbound.recv().await.expect("the request is failed");
    let refusal: Value =
        serde_json::from_str(&refusal).expect("the refusal is JSON");
    assert_eq!(refusal["id"], json!("c7"));
    assert_eq!(
        refusal["error"]["data"]["code"],
        json!("core_unavailable"),
        "the in-flight request carries the restart reason"
    );

    // The second core announces itself; the socket state is intact.
    release_second.notify_one();
    wait_until(
        || app_server.is_ready(),
        Duration::from_secs(2),
        "the second ready",
    )
    .await;
    assert_eq!(outbound.recv().await, Some(ready_frame(2)));
    assert!(app_server.connections.contains_key(&connection));
    assert!(!supervisor.is_finished(), "the supervisor keeps running");
    supervisor.abort();
}

/// Read one Content-Length frame from the core's wire.
async fn read_frame_from(reader: &mut DuplexStream) -> String {
    let mut header = Vec::new();
    let mut byte = [0_u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        reader.read_exact(&mut byte).await.expect("the wire reads");
        header.push(byte[0]);
    }
    let header = String::from_utf8(header).expect("the header is UTF-8");
    let length: usize = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .expect("the frame carries a length");
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).await.expect("the body reads");
    String::from_utf8(body).expect("the body is UTF-8")
}

/// The gateway asks the core for its exposition and merges it into
/// `/metrics`: the tool counters live in the core's process, the HTTP
/// counters in the gateway's.
#[tokio::test]
async fn the_gateway_fetches_the_core_s_exposition_over_the_rpc_channel() {
    let app_server = app();
    app_server.ready.store(true, Ordering::Relaxed);
    let mut reader = attach_core_stdin(&app_server).await;

    let fetcher = tokio::spawn({
        let app_server = Arc::clone(&app_server);
        async move { app_server.core_exposition().await }
    });

    // The core sees a `metrics/gather` request whose id carries the
    // gateway's own prefix.
    let request = read_frame_from(&mut reader).await;
    let request: Value =
        serde_json::from_str(&request).expect("the request is JSON");
    assert_eq!(request["method"], json!("metrics/gather"));
    let id = request["id"].as_str().expect("the request carries an id");
    assert!(
        id.starts_with("gateway:internal:"),
        "the gateway's own id is namespaced: {id}"
    );

    // The core answers it.
    let answer = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "text": "tool_executions_total{outcome=\"ok\",tool=\"read\"} 1\n" },
    })
    .to_string();
    route(&app_server, &answer);

    let exposition = fetcher
        .await
        .expect("the fetch finished")
        .expect("the core answered");
    assert!(
        exposition.contains("tool_executions_total"),
        "the core's exposition came back whole: {exposition}"
    );
}
