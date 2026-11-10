//! The transport: a domain adapter over the gateway's client.
//!
//! The TUI no longer speaks to a core. It connects to an
//! `elysper-app-server serve` gateway — the same `/rpc` WebSocket the
//! browser and the desktop shell speak — through
//! [`elysper_gateway`]. That crate owns everything process-shaped:
//! spawning `serve`, the `/readyz` probe, the 1s → 5s restart backoff
//! and the reaping. What is left here is the TUI's own domain:
//!
//! - **method mapping** — [`RequestKind`] names the wire methods the
//!   reducer's actions serialize to;
//! - **id correlation** — every [`Transport::send`] allocates an id
//!   and spawns one task that awaits the gateway client's answer and
//!   delivers it as a [`TransportEvent::Response`] carrying the
//!   request kind, so the reducer never re-derives a request from its
//!   payload;
//! - **notifications** — one catch-all subscription turns every
//!   notification into a [`TransportEvent::Notification`]; the core's
//!   `ready` broadcast becomes [`TransportEvent::Ready`] (the gateway
//!   broadcasts it to every live connection when a core generation
//!   announces itself, so a restart under a live socket is seen);
//! - **re-follow** — the session the last `sessions/follow` named is
//!   remembered and re-issued whenever the connection is new again:
//!   on a core `ready` broadcast, on a supervisor restart, and when
//!   the liveness probe first succeeds after an outage. The gateway
//!   routes `session/event` by the sessions a connection follows, so
//!   a silent WebSocket reconnect would otherwise leave the TUI deaf;
//! - **liveness** — the client reconnects its socket on its own but
//!   narrates nothing, so this module probes `health/ping` every few
//!   seconds: the first success (and the first after an outage)
//!   announces [`TransportEvent::Ready`] and re-follows, a transport
//!   failure announces [`TransportEvent::Disconnected`] once.
//!
//! # The initial `ready`
//!
//! A gateway only *broadcasts* the core's `ready`; a client that
//! connects afterwards never sees it. In launch mode the supervisor's
//! ready state already proves the core announced itself, and in
//! attach mode the liveness probe is answered by the core itself — so
//! the probe's first success is what synthesizes the initial
//! [`TransportEvent::Ready`] (with the version `health/ping` reports
//! and the workspace root the launch chose or a `ready` broadcast
//! carried).

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use elysper_gateway::{
    ClientError,
    Gateway,
    GatewayClient,
    GatewayError,
    GatewayOptions,
    GatewayState,
    NotificationHandler,
    Subscription,
};
use futures::{
    StreamExt as _,
    stream::{BoxStream, pending},
};
use serde_json::{Value, json};
use tokio::{sync::mpsc, task::JoinHandle};

/// How often the liveness probe calls `health/ping` while connected.
#[cfg(not(test))]
const PROBE_INTERVAL: Duration = Duration::from_secs(5);
/// Tests need the outage narration within the harness budget, not
/// after a production-cadence wait.
#[cfg(test)]
const PROBE_INTERVAL: Duration = Duration::from_millis(200);

/// The first retry wait when dialing an attached gateway; each
/// further failure doubles it, mirroring the client's own reconnect.
const DIAL_BACKOFF_MIN: Duration = Duration::from_millis(250);

/// The dial retry ceiling.
const DIAL_BACKOFF_MAX: Duration = Duration::from_secs(2);

/// What a request the TUI sent was for.
///
/// The kind travels beside the spawned task that awaits the answer,
/// so the delivered [`TransportEvent::Response`] names the request
/// without the reducer re-deriving it from the payload. It is also
/// what the adapter remembers for a reconnect: a `Follow` is re-issued
/// once the connection is new again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// `chat/session/create`.
    CreateSession,
    /// `sessions/follow`.
    Follow,
    /// `chat/message/send`.
    SendMessage,
    /// `approval/respond`.
    ApprovalRespond,
    /// `chat/cancel`.
    Cancel,
    /// `session/unfollow`.
    Unfollow,
    /// `sessions/events`.
    Events,
    /// `sessions/list`.
    ListSessions,
    /// `sessions/delete`.
    DeleteSession,
    /// `ui/schema`.
    UiSchema,
    /// Any directory method (`sessions/list`, `agents/get`,
    /// `evals/run`, …). The method name rides on the variant; the
    /// response is routed to
    /// [`crate::views::catalog`] **before** the reducer, so the
    /// reducer's arms for this variant never run.
    Catalog(&'static str),
}

impl RequestKind {
    /// The wire method this request kind names.
    #[must_use]
    pub fn method(self) -> &'static str {
        match self {
            Self::CreateSession => "chat/session/create",
            Self::Follow => "sessions/follow",
            Self::SendMessage => "chat/message/send",
            Self::ApprovalRespond => "approval/respond",
            Self::Cancel => "chat/cancel",
            Self::Unfollow => "session/unfollow",
            Self::Events => "sessions/events",
            Self::ListSessions => "sessions/list",
            Self::DeleteSession => "sessions/delete",
            Self::UiSchema => "ui/schema",
            Self::Catalog(method) => method,
        }
    }
}

/// One outbound JSON-RPC request.
///
/// Constructing it is the reducer's job ([`crate::app::Action`]); the
/// transport only sends it.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// What the request is (also the id-correlation key).
    pub kind: RequestKind,
    /// The JSON-RPC `params` member.
    pub params: Value,
}

impl Request {
    /// The wire method name.
    #[must_use]
    pub fn method(&self) -> &'static str {
        self.kind.method()
    }

    /// `chat/session/create` with the core's default agent.
    #[must_use]
    pub fn create_session() -> Self {
        Self {
            kind: RequestKind::CreateSession,
            params: Value::Object(serde_json::Map::new()),
        }
    }

    /// `sessions/follow` for one session.
    #[must_use]
    pub fn follow(session_id: impl Into<String>) -> Self {
        Self {
            kind: RequestKind::Follow,
            params: json!({ "session_id": session_id.into() }),
        }
    }

    /// `chat/message/send` for one session.
    #[must_use]
    pub fn send_message(
        session_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            kind: RequestKind::SendMessage,
            params: json!({
                "session_id": session_id.into(),
                "text": text.into(),
            }),
        }
    }

    /// `approval/respond` for one held call.
    ///
    /// The `approval_id` is the UI correlation id the
    /// `approval/request` notification carried — never the JSON-RPC
    /// `id`.
    #[must_use]
    pub fn respond_approval(
        session_id: impl Into<String>,
        approval_id: impl Into<String>,
        decision: &str,
    ) -> Self {
        Self {
            kind: RequestKind::ApprovalRespond,
            params: json!({
                "session_id": session_id.into(),
                "approval_id": approval_id.into(),
                "decision": decision,
            }),
        }
    }

    /// `chat/cancel` for one session.
    #[must_use]
    pub fn cancel(session_id: impl Into<String>) -> Self {
        Self {
            kind: RequestKind::Cancel,
            params: json!({ "session_id": session_id.into() }),
        }
    }

    /// `session/unfollow` for one session.
    #[must_use]
    pub fn unfollow(session_id: impl Into<String>) -> Self {
        Self {
            kind: RequestKind::Unfollow,
            params: json!({ "session_id": session_id.into() }),
        }
    }

    /// `sessions/events` for one session.
    #[must_use]
    pub fn events(session_id: impl Into<String>) -> Self {
        Self {
            kind: RequestKind::Events,
            params: json!({ "id": session_id.into() }),
        }
    }

    /// `sessions/list` — the sidebar's workspace tree, one page of
    /// summaries.
    #[must_use]
    pub fn list_sessions() -> Self {
        Self {
            kind: RequestKind::ListSessions,
            params: json!({ "limit": 50 }),
        }
    }

    /// `sessions/delete` — remove a session's durable transcript.
    #[must_use]
    pub fn delete_session(session_id: impl Into<String>) -> Self {
        Self {
            kind: RequestKind::DeleteSession,
            params: json!({ "id": session_id.into() }),
        }
    }

    /// `ui/schema` — the deployment's slot tree (no parameters).
    #[must_use]
    pub fn ui_schema() -> Self {
        Self {
            kind: RequestKind::UiSchema,
            params: Value::Object(serde_json::Map::new()),
        }
    }

    /// Any directory method with its params passed through verbatim.
    ///
    /// The method name must be one the server registered
    /// (`crates/elysper-app-server/src/rpc/methods/**`); an unknown
    /// name comes back as a JSON-RPC error, which the directory view
    /// renders as its status line.
    #[must_use]
    pub fn catalog(method: &'static str, params: Value) -> Self {
        Self {
            kind: RequestKind::Catalog(method),
            params,
        }
    }
}

/// A failure of the local send side.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// The gateway connection is not established yet; the first
    /// `ready` (or the probe that synthesizes it) has not arrived.
    /// The caller may retry once it does.
    #[error("the gateway is still connecting")]
    Connecting,
}

/// A JSON-RPC error envelope from the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcFailure {
    /// The JSON-RPC `error.code` (`-32000` for application failures).
    pub code: i64,
    /// The human-readable message.
    pub message: String,
    /// The machine code and detail, when the server attached it.
    pub data: Option<Value>,
}

impl RpcFailure {
    /// A transport-level failure: the connection died before the
    /// answer arrived. The machine code says so; the numeric code
    /// matches the application range the core uses for refusals.
    fn transport(reason: String) -> Self {
        Self {
            code: -32000,
            message: format!("transport: {reason}"),
            data: Some(json!({ "code": "transport" })),
        }
    }
}

impl From<ClientError> for RpcFailure {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::Server { code, message } => Self {
                code,
                message,
                data: None,
            },
            ClientError::Transport { reason } => Self::transport(reason),
            other => Self {
                code: -32000,
                message: other.to_string(),
                data: None,
            },
        }
    }
}

impl std::fmt::Display for RpcFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for RpcFailure {}

/// One thing the transport observed: a notification, a response, or a
/// lifecycle change of the gateway or its connection.
#[derive(Debug, Clone)]
pub enum TransportEvent {
    /// A core generation announced itself. Broadcast by the gateway
    /// to every live connection — so this arrives on a core restart
    /// under a live socket, and is synthesized by the liveness probe
    /// for a connection that missed the boot broadcast.
    Ready {
        /// The core's version string.
        version: String,
        /// The workspace root the core resolved (or the directory the
        /// launch chose, before a `ready` carries the real one).
        workspace_root: String,
    },
    /// A response to a request the TUI sent.
    Response {
        /// The id the transport allocated for the request.
        id: Value,
        /// What the request was for.
        request: Option<RequestKind>,
        /// The `result` member, or the decoded `error` member.
        result: Result<Value, RpcFailure>,
    },
    /// A notification the gateway delivered (`session/event`,
    /// `approval/request`, …).
    Notification {
        /// The notification method.
        method: String,
        /// The `params` member.
        params: Value,
    },
    /// The connection is gone: the gateway is unreachable (attach) or
    /// died and has not answered a probe since. The client keeps
    /// reconnecting on its own; the probe announces recovery as a new
    /// [`TransportEvent::Ready`].
    Disconnected {
        /// Why the connection is down.
        reason: String,
    },
    /// The gateway process could not be spawned at all. The supervisor
    /// has given up — `retry_in` is zero and nothing will retry; the
    /// operator restarts the TUI.
    SpawnFailed {
        /// The operating-system error, rendered.
        message: String,
        /// Always zero: no retry is scheduled.
        retry_in: Duration,
    },
    /// The gateway process is launching and being probed (from
    /// `GatewayState::Starting`).
    GatewayStarting {
        /// Why the gateway is (re)starting, when it said.
        reason: String,
    },
    /// The gateway process died and generation `attempt` is being
    /// relaunched after the backoff.
    GatewayRestarting {
        /// The restart attempt, 1-based.
        attempt: u32,
    },
}

/// Map one supervisor state to the event it narrates. `Ready` maps to
/// nothing — readiness is a connection fact the probe owns — and
/// `Failed` ends the driver, which the caller honors by stopping.
fn state_event(state: GatewayState) -> Option<TransportEvent> {
    match state {
        GatewayState::Starting => Some(TransportEvent::GatewayStarting {
            reason: "the gateway process is launching".to_owned(),
        }),
        GatewayState::Restarting { attempt } => {
            Some(TransportEvent::GatewayRestarting { attempt })
        }
        GatewayState::Failed { reason } => {
            Some(TransportEvent::Disconnected { reason })
        }
        GatewayState::Ready { .. } => None,
    }
}

/// Normalize an attach URL into a `ws://` one. `http(s)://` becomes
/// `ws(s)://`, and a URL without a path gains `/rpc`.
#[must_use]
pub fn normalize_attach_url(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some(("http", rest)) => ("ws", rest),
        Some(("https", rest)) => ("wss", rest),
        Some((scheme, rest)) => return format!("{scheme}://{rest}"),
        None => return url.to_owned(),
    };
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/rpc".to_owned()),
    };
    let path = if path == "/" { "/rpc".to_owned() } else { path };
    format!("{scheme}://{authority}{path}")
}

/// What the driver was asked to do.
enum Mode {
    /// Launch `elysper-app-server serve` and connect to it.
    Launch(GatewayOptions),
    /// Connect to a gateway that is already running.
    Attach(String),
}

/// The state every half of the transport shares: the event channel,
/// the client once connected, the recorded follow, and the liveness
/// bookkeeping.
struct TransportState {
    events: mpsc::UnboundedSender<TransportEvent>,
    /// The gateway client, once the WebSocket handshake is done.
    client: parking_lot::Mutex<Option<GatewayClient>>,
    /// The catch-all notification subscription, held for the
    /// driver's lifetime so its drop does not unregister the handler.
    subscription: parking_lot::Mutex<Option<Subscription>>,
    /// The session the last `sessions/follow` named, re-issued when
    /// the connection is new again.
    follow: parking_lot::Mutex<Option<String>>,
    /// The workspace root to synthesize a `Ready` with until a real
    /// `ready` broadcast carries the core's own.
    workspace_root: parking_lot::Mutex<String>,
    next_id: AtomicU64,
    /// No `Ready` has been announced for the current connection: the
    /// initial state, and whatever an outage re-established.
    down: AtomicBool,
    /// Whether a liveness probe is in flight, so failures do not pile
    /// probes up.
    probing: AtomicBool,
}

impl TransportState {
    /// Deliver one event; a dropped receiver means the driver is gone.
    fn event(&self, event: TransportEvent) {
        let _ = self.events.send(event);
    }

    /// Announce the connection is down — once per outage.
    fn announce_down(&self, reason: &str) {
        if !self.down.swap(true, Ordering::Relaxed) {
            self.event(TransportEvent::Disconnected {
                reason: reason.to_owned(),
            });
        }
    }

    fn notify(self: &Arc<Self>, method: &str, params: Value) {
        if method == "ready" {
            let version = params
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let workspace_root = params
                .get("workspace_root")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if !workspace_root.is_empty() {
                *self.workspace_root.lock() = workspace_root.clone();
            }
            self.event(TransportEvent::Ready {
                version,
                workspace_root,
            });
            self.refollow();
            return;
        }
        self.event(TransportEvent::Notification {
            method: method.to_owned(),
            params,
        });
    }

    /// Send one request and deliver its answer as a `Response` event.
    /// The id is allocated here; the spawned task knows the kind, so
    /// the event carries it whatever the server echoes.
    fn spawn_request(
        self: &Arc<Self>,
        client: GatewayClient,
        request: Request,
    ) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if request.kind == RequestKind::Follow
            && let Some(session_id) =
                request.params.get("session_id").and_then(Value::as_str)
        {
            *self.follow.lock() = Some(session_id.to_owned());
        }
        let kind = request.kind;
        let state = Arc::clone(self);
        tokio::spawn(async move {
            let result =
                match client.request(request.method(), request.params).await {
                    Ok(value) => Ok(value),
                    Err(
                        error @ (ClientError::Transport { .. }
                        | ClientError::Connect { .. }
                        | ClientError::Closed),
                    ) => {
                        // Unreachable in every guise — a dead socket,
                        // a failed redial, or the client giving up —
                        // flips the status bar and starts the probe
                        // that will announce recovery.
                        state.announce_down(&error.to_string());
                        spawn_probe(Arc::clone(&state));
                        Err(RpcFailure::from(error))
                    }
                    Err(error) => Err(RpcFailure::from(error)),
                };
            state.event(TransportEvent::Response {
                id: Value::from(id),
                request: Some(kind),
                result,
            });
        });
        id
    }

    /// Re-issue the recorded `sessions/follow`, if any, whenever the
    /// connection is new again. A failed re-follow is not fatal: the
    /// next occasion retries it, and the caller's own follow remains
    /// the authoritative path.
    fn refollow(self: &Arc<Self>) {
        let Some(session_id) = self.follow.lock().clone() else {
            return;
        };
        if let Some(client) = self.client.lock().clone() {
            self.spawn_request(client, Request::follow(session_id));
        }
    }
}

/// The write handle for one gateway connection.
///
/// A send before the connection exists fails with
/// [`SendError::Connecting`]; afterwards the answer always arrives as
/// a [`TransportEvent::Response`], even when it is a transport
/// failure.
#[derive(Clone)]
pub struct Transport {
    state: Arc<TransportState>,
}

impl Transport {
    /// Send one request and return the id it was sent under.
    ///
    /// # Errors
    ///
    /// [`SendError::Connecting`] when the WebSocket has not been
    /// established yet.
    pub async fn send(&self, request: &Request) -> Result<u64, SendError> {
        let client = self.state.client.lock().clone();
        let Some(client) = client else {
            return Err(SendError::Connecting);
        };
        Ok(self.state.spawn_request(client, request.clone()))
    }
}

/// The gateway connection plus its event stream.
///
/// Production builds one with [`Driver::launch`] (spawn
/// `elysper-app-server serve`) or [`Driver::attach`] (connect to a
/// running gateway); the tests drive [`Driver::attach`] against an
/// in-process WebSocket server.
pub struct Driver {
    state: Arc<TransportState>,
    events: mpsc::UnboundedReceiver<TransportEvent>,
    /// The supervised gateway, filled by the driver task once its
    /// first generation is ready; dropping the driver kills and reaps
    /// it.
    gateway: Arc<parking_lot::Mutex<Option<Gateway>>>,
    task: JoinHandle<()>,
}

impl Driver {
    /// Launch `elysper-app-server serve` and connect to its `/rpc`.
    ///
    /// The spawn happens inside the driver's task, so this does not
    /// block on the gateway's boot.
    #[must_use]
    pub fn launch(options: GatewayOptions) -> Self {
        Self::start(Mode::Launch(options))
    }

    /// Connect to a gateway that is already running — no process is
    /// spawned and none is supervised: if that gateway dies, the
    /// driver reports it and keeps waiting for it to come back.
    #[must_use]
    pub fn attach(url: &str) -> Self {
        Self::start(Mode::Attach(normalize_attach_url(url)))
    }

    fn start(mode: Mode) -> Self {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let state = Arc::new(TransportState {
            events: events_tx,
            client: parking_lot::Mutex::new(None),
            subscription: parking_lot::Mutex::new(None),
            follow: parking_lot::Mutex::new(None),
            workspace_root: parking_lot::Mutex::new(String::new()),
            next_id: AtomicU64::new(1),
            down: AtomicBool::new(true),
            probing: AtomicBool::new(false),
        });
        let gateway = Arc::new(parking_lot::Mutex::new(None));
        let task =
            tokio::spawn(drive(mode, Arc::clone(&state), Arc::clone(&gateway)));
        Self {
            state,
            events: events_rx,
            gateway,
            task,
        }
    }

    /// A write handle usable independently of this driver's event
    /// receiver (so `select!` can borrow both).
    #[must_use]
    pub fn sender(&self) -> Transport {
        Transport {
            state: Arc::clone(&self.state),
        }
    }

    /// The next transport event, or `None` once the driver stops.
    pub async fn recv(&mut self) -> Option<TransportEvent> {
        self.events.recv().await
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // Kill the supervised gateway (if its generation ever came
        // up), close the client, and stop the driver task. A launch
        // still waiting for its first generation needs no gateway
        // handle: the gateway crate reaps a supervision whose caller
        // vanished before the outcome.
        if let Some(gateway) = self.gateway.lock().take() {
            gateway.shutdown();
        }
        if let Some(client) = self.state.client.lock().take() {
            client.close();
        }
        self.task.abort();
    }
}

/// Launch or attach, connect, then narrate states and probe forever.
async fn drive(
    mode: Mode,
    state: Arc<TransportState>,
    gateway: Arc<parking_lot::Mutex<Option<Gateway>>>,
) {
    let mut states: BoxStream<GatewayState> = pending().boxed();
    let ws_url = match mode {
        Mode::Launch(options) => {
            let directory = options.directory.display().to_string();
            match Gateway::start(options).await {
                Ok(started) => {
                    states = started.states().boxed();
                    *state.workspace_root.lock() = directory;
                    *gateway.lock() = Some(started);
                    // The states before the first `Ready` are buffered;
                    // wait for the URL the connection is meant to dial.
                    // A `Failed` before any `Ready` ends the driver.
                    loop {
                        match states.next().await {
                            Some(GatewayState::Ready { ws_url, .. }) => {
                                break ws_url;
                            }
                            Some(GatewayState::Failed { reason }) => {
                                state.event(TransportEvent::Disconnected {
                                    reason,
                                });
                                return;
                            }
                            Some(other) => {
                                if let Some(event) = state_event(other) {
                                    state.event(event);
                                }
                            }
                            None => return,
                        }
                    }
                }
                Err(GatewayError::Spawn { program, source }) => {
                    state.event(TransportEvent::SpawnFailed {
                        message: format!("{program}: {source}"),
                        retry_in: Duration::ZERO,
                    });
                    return;
                }
                Err(error) => {
                    state.event(TransportEvent::Disconnected {
                        reason: error.to_string(),
                    });
                    return;
                }
            }
        }
        Mode::Attach(url) => url,
    };

    // Dial. An attached gateway may not be up yet — or may die and
    // come back later — so the dial retries with the client's own
    // backoff shape until it succeeds.
    let client = {
        let mut backoff = DIAL_BACKOFF_MIN;
        loop {
            match GatewayClient::connect(&ws_url).await {
                Ok(client) => break client,
                Err(error) => {
                    state.announce_down(&format!(
                        "cannot reach the gateway at {ws_url}: {error}"
                    ));
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(DIAL_BACKOFF_MAX);
                }
            }
        }
    };
    let handler: NotificationHandler = {
        let state = Arc::clone(&state);
        Arc::new(move |method, params| {
            state.notify(method, params);
        })
    };
    match client.subscribe_all(handler) {
        Ok(subscription) => {
            *state.subscription.lock() = Some(subscription);
        }
        Err(ClientError::Closed) => return, // closed before it landed
        Err(_) => {}
    }
    *state.client.lock() = Some(client);
    spawn_probe(Arc::clone(&state));

    let mut probe = tokio::time::interval(PROBE_INTERVAL);
    probe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            observed = states.next() => match observed {
                Some(GatewayState::Failed { reason }) => {
                    // The supervisor gave up; the driver stops with it.
                    state.event(TransportEvent::Disconnected { reason });
                    return;
                }
                Some(gateway_state) => {
                    if let Some(event) = state_event(gateway_state.clone()) {
                        state.event(event);
                    }
                    // A new generation means a new WebSocket and a new
                    // gateway-side follow table: re-issue ours. The
                    // request queues through the client's reconnect.
                    if matches!(
                        gateway_state,
                        GatewayState::Ready { .. }
                            | GatewayState::Restarting { .. }
                    ) {
                        state.refollow();
                    }
                }
                None => return,
            },
            _ = probe.tick() => spawn_probe(Arc::clone(&state)),
        }
    }
}
/// One `health/ping` liveness probe. The first success — and the
/// first after an outage — synthesizes the `Ready` the connection
/// missed (a gateway never re-broadcasts a boot that already
/// happened) and re-follows; a transport failure announces the
/// outage once.
fn spawn_probe(state: Arc<TransportState>) {
    if state.probing.swap(true, Ordering::Relaxed) {
        return; // one probe at a time
    }
    tokio::spawn(async move {
        let Some(client) = state.client.lock().clone() else {
            state.probing.store(false, Ordering::Relaxed);
            return;
        };
        // A parked `request` cannot see an outage — it waits for the
        // redial — so the client's own socket flag is the probe's
        // first read.
        if !client.connected() {
            state.probing.store(false, Ordering::Relaxed);
            state.announce_down("the gateway is unreachable");
            return;
        }
        let outcome = client.request("health/ping", json!({})).await;
        state.probing.store(false, Ordering::Relaxed);
        match outcome {
            Ok(value) => {
                if state.down.swap(false, Ordering::Relaxed) {
                    let version = value
                        .get("version")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let workspace_root = state.workspace_root.lock().clone();
                    state.event(TransportEvent::Ready {
                        version,
                        workspace_root,
                    });
                    state.refollow();
                }
            }
            Err(
                error @ (ClientError::Transport { .. }
                | ClientError::Connect { .. }
                | ClientError::Closed),
            ) => {
                // `Transport` is a live socket dying mid-call;
                // `Connect` is a redial against a dead gateway (the
                // idle outage: no request is in flight to notice it
                // first); `Closed` is the client done. All three mean
                // unreachable, and all three must reach the status
                // bar — only `Server` means the gateway answered.
                state.announce_down(&error.to_string());
            }
            Err(_) => {} // `Server`: the gateway answered; stay up
        }
    });
}

#[cfg(test)]
mod tests;
