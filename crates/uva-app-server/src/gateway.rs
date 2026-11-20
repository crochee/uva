//! The App Server: a translation layer and state hub in front of the
//! headless core.
//!
//! The core speaks JSON-RPC 2.0 on stdio and knows nothing about the
//! Web. This process spawns **one** core subprocess and owns the four
//! responsibilities of an app server:
//!
//! 1. **Protocol conversion** — one browser WebSocket text message is
//!    one JSON-RPC object; on the core's stdio the same object travels
//!    as one `Content-Length` frame ([`crate::rpc::framing`]). The
//!    gateway adds and strips the header and never rewrites the JSON.
//! 2. **Thread management** — each connection keeps the set of
//!    sessions it follows. A reconnecting client re-issues
//!    `sessions/follow` for each of them and the core answers with
//!    the baseline, so the timeline rebuilds itself.
//! 3. **Event distribution** — the core's `session/event` and
//!    `anthropic/messages/event` notifications are routed by
//!    `session_id` to the connections that follow that session, not
//!    broadcast. Responses are routed by `id` to the connection that
//!    asked.
//! 4. **Approval interrupts** — the core projects a durable
//!    suspension as an `approval/request` **notification**
//!    (`params.approval_id` + `params.session_id`), routed to that
//!    session's followers exactly like a `session/event`. The client
//!    answers with `approval/respond`, an ordinary request the
//!    gateway forwards upstream; the core maps it to its durable
//!    `sessions/resume` + `ResumeToken` path. `approval_id` is the
//!    correlation id a UI shows and the log keeps — it is not the
//!    JSON-RPC `id`, and the gateway never interprets it.
//!
//! # What it does not do
//!
//! It does not aggregate, rewrite or cache event payloads, and it
//! holds no session content — the durable truth is the core's session
//! log. The only fields it reads out of a frame are `id`,
//! `params.session_id` and the `method` names that manage a
//! subscription. Everything else is opaque.
//!
//! # The HTTP surface
//!
//! - `GET /rpc` — the WebSocket the JSON-RPC frames travel on.
//! - `GET /livez` / `GET /readyz` — the probes (`readyz` also needs
//!   the core's `ready` notification).
//! - `GET /version` — the build identity.
//! - `GET /metrics` — the Prometheus text exposition.
//! - everything else — the web front-end's static files.
//!
//! # Supervision
//!
//! The core is restarted when it dies: the first retry waits a
//! second, later retries back off to five. The core's stdout is read
//! frame by frame; end of stream (or a framing error) ends that
//! generation, and the next spawn starts a fresh one. `ready` is the
//! parsed **notification method**, not a substring of the payload. A
//! client that was connected across the restart sees its socket stay
//! open; its subscriptions are dropped with the old core, and its own
//! re-follow calls rebuild them.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    extract::{State, WebSocketUpgrade, ws::Message},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use dashmap::DashMap;
use futures::{SinkExt, StreamExt, future::BoxFuture};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader},
    process::Command,
    sync::mpsc,
};
use tower::Layer as _;
use uva::core::CancelToken as _;

use crate::rpc::framing::{DEFAULT_MAX_FRAME_SIZE, read_frame, write_frame};

/// How long the first restart waits before the core is spawned again.
const RESTART_BACKOFF_MIN: Duration = Duration::from_secs(1);

/// The longest a restart waits.
const RESTART_BACKOFF_MAX: Duration = Duration::from_secs(5);

/// The prefix of an id the gateway mints for its own requests to the
/// core.
///
/// A browser's id is whatever the client sent, so the prefix is what
/// keeps the two apart: a response to a gateway-minted id is the
/// gateway's, never a client's.
const INTERNAL_ID_PREFIX: &str = "gateway:internal:";

/// The shared App Server state.
pub struct AppServer {
    /// The core supervisor's handle.
    core: CoreHandle,
    /// Every live connection, keyed by its id.
    connections: DashMap<u64, ConnectionHandle>,
    /// The next connection id.
    next_connection: AtomicU64,
    /// Whether the core has announced itself ready.
    ready: Arc<AtomicBool>,
    /// The gateway's own requests to the core, keyed by the id it
    /// minted for them. A response to one of these is the gateway's,
    /// not a browser's.
    internal_pending: DashMap<String, tokio::sync::oneshot::Sender<String>>,
    /// The counter behind the ids the gateway mints.
    next_internal_id: AtomicU64,
}

/// The handle the supervisor holds for one connected client.
struct ConnectionHandle {
    /// The connection's outbound channel.
    tx: mpsc::UnboundedSender<String>,
    /// The request ids this connection has in flight, keyed by the
    /// id's string form and holding the original value (string or
    /// number). The App Server owns no requests of its own, so every
    /// response frame belongs to whichever connection minted the id —
    /// and when the core dies, the original value is what a failure
    /// must echo for the client to correlate it.
    pending: DashMap<String, Value>,
    /// The sessions this connection follows.
    subscriptions: DashMap<String, ()>,
}

impl ConnectionHandle {
    /// Send one frame; a closed channel drops it (the socket is gone).
    fn send(&self, frame: &str) {
        let _ = self.tx.send(frame.to_string());
    }
}

impl AppServer {
    /// Whether the core is ready to answer.
    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    /// The core's Prometheus exposition, or `None` when it cannot
    /// answer.
    ///
    /// The core runs the agent loop, so the tool counters live in
    /// the core's process; the gateway holds the HTTP counters. The
    /// `/metrics` endpoint is the two together, and this is the half
    /// the gateway cannot see on its own.
    async fn core_exposition(&self) -> Option<String> {
        if !self.is_ready() {
            return None;
        }
        let id = format!(
            "{INTERNAL_ID_PREFIX}{}",
            self.next_internal_id.fetch_add(1, Ordering::Relaxed)
        );
        let (waiter, answer) = tokio::sync::oneshot::channel();
        self.internal_pending.insert(id.clone(), waiter);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "metrics/gather",
            "params": {},
        });
        if self.core.send(&request.to_string()).await.is_err() {
            self.internal_pending.remove(&id);
            return None;
        }
        let frame = tokio::time::timeout(Duration::from_secs(2), answer)
            .await
            .ok()?
            .ok()?;
        let parsed: Value = serde_json::from_str(&frame).ok()?;
        parsed
            .get("result")?
            .get("text")?
            .as_str()
            .map(str::to_string)
    }

    /// Hand one upstream frame to the core, and remember who asked.
    ///
    /// `approval/respond` is not a special case here: it is an
    /// ordinary client request, forwarded verbatim. The core maps its
    /// `approval_id` + `decision` onto the durable
    /// `sessions/resume` + `ResumeToken` path, so the gateway neither
    /// reads the decision nor keeps one.
    async fn forward_upstream(
        &self,
        connection: u64,
        frame: &str,
    ) -> Result<(), String> {
        // The only fields the App Server reads: the id, and the
        // session a follow/unfollow is about.
        let parsed: Value = serde_json::from_str(frame)
            .map_err(|error| format!("the frame is not JSON: {error}"))?;
        let Some(handle) = self.connections.get(&connection) else {
            return Err("the connection is gone".to_string());
        };
        if let Some(id) = parsed.get("id")
            && !id.is_null()
            && let Some(key) = id_key(id)
        {
            handle.pending.insert(key, id.clone());
        }
        match parsed.get("method").and_then(Value::as_str) {
            Some("sessions/follow") => {
                if let Some(session) = parsed
                    .get("params")
                    .and_then(|params| params.get("session_id"))
                    .and_then(Value::as_str)
                {
                    handle.subscriptions.insert(session.to_string(), ());
                }
            }
            Some("session/unfollow") => {
                if let Some(session) = parsed
                    .get("params")
                    .and_then(|params| params.get("session_id"))
                    .and_then(Value::as_str)
                {
                    handle.subscriptions.remove(session);
                }
            }
            _ => {}
        }
        drop(handle);
        self.core.send(frame).await
    }

    /// Route one decoded frame.
    ///
    /// `parsed` is `None` for a payload that is not JSON; the raw text
    /// is broadcast so a core bug is visible to the client. The
    /// `Content-Length` header can never reach here: the pump consumes
    /// it before this payload exists.
    fn route_frame(&self, parsed: Option<&Value>, frame: &str) {
        let Some(parsed) = parsed else {
            self.broadcast(frame);
            return;
        };
        // A response: route by id to the connection that minted it.
        if parsed.get("result").is_some() || parsed.get("error").is_some() {
            let Some(id) = parsed.get("id").and_then(id_key) else {
                return;
            };
            // The gateway's own request first: its ids carry the
            // reserved prefix, so no browser can mint one.
            if let Some((_, waiter)) = self.internal_pending.remove(&id) {
                let _ = waiter.send(frame.to_string());
                return;
            }
            for handle in self.connections.iter() {
                if handle.pending.remove(&id).is_some() {
                    handle.send(frame);
                    return;
                }
            }
            return;
        }
        // A notification: `ready` goes to everyone; the session-scoped
        // ones — including the approval projection — go to that
        // session's followers.
        match parsed.get("method").and_then(Value::as_str) {
            Some("ready") => {
                self.broadcast(frame);
                return;
            }
            Some(
                "session/event"
                | "anthropic/messages/event"
                | "approval/request",
            ) => {}
            _ => {
                self.broadcast(frame);
                return;
            }
        }
        let Some(session) = parsed
            .get("params")
            .and_then(|params| params.get("session_id"))
            .and_then(Value::as_str)
        else {
            // A session-scoped notification with no session cannot be
            // routed; broadcasting keeps it visible rather than lost.
            self.broadcast(frame);
            return;
        };
        for handle in self.connections.iter() {
            if handle.subscriptions.contains_key(session) {
                handle.send(frame);
            }
        }
    }

    /// Send one frame to every live connection.
    fn broadcast(&self, frame: &str) {
        for handle in self.connections.iter() {
            handle.send(frame);
        }
    }
}

/// The key a request id is tracked under on both sides of the pump.
///
/// A string id is its own content; a number id is its decimal form —
/// so the request registration and the response lookup derive the
/// same key. `Value::to_string()` is deliberately not used for
/// strings: it would key `"c1"` with its quotes and never match.
fn id_key(id: &Value) -> Option<String> {
    match id {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// The core's stdin as the gateway sees it.
///
/// A trait object rather than `ChildStdin`: production attaches the
/// child's pipe, and the tests attach a duplex writer so they can read
/// the exact bytes the gateway emitted. The gateway uses nothing
/// beyond the framed `write` + `flush`.
type CoreWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// The core's stdout as the supervisor sees it.
type CoreReader = Box<dyn AsyncRead + Send + Unpin>;

/// The supervisor's handle to the core subprocess.
struct CoreHandle {
    /// The core's stdin, behind a lock so concurrent upstream frames
    /// cannot interleave.
    stdin: Arc<tokio::sync::Mutex<Option<CoreWriter>>>,
}

impl CoreHandle {
    /// Hand one frame to the core as a Content-Length frame.
    ///
    /// A frame that cannot be delivered is reported to the caller,
    /// which answers the browser with a transport error; the writer is
    /// dropped so a later request reports the restart instead of
    /// writing into a dead pipe. A trailing newline is **not**
    /// appended: the core's codec reads the next header from the very
    /// next byte, and a stray `\n` would make it malformed.
    async fn send(&self, frame: &str) -> Result<(), String> {
        let mut guard = self.stdin.lock().await;
        let Some(stdin) = guard.as_mut() else {
            return Err("the core is restarting".to_string());
        };
        if let Err(error) = write_frame(stdin, frame.as_bytes()).await {
            tracing::warn!(%error, "app server: could not write to the core");
            // The core is gone; the supervisor restarts it.
            *guard = None;
            return Err("the core is gone".to_string());
        }
        Ok(())
    }
}

/// One spawned core generation, as the supervisor sees it.
///
/// Production wraps the child's pipe halves ([`spawn_process`]); a
/// test wraps a duplex pair and its own exit future, so the restart
/// loop is exercised without an OS process.
struct SpawnedCore {
    /// The write half of the core's stdin.
    stdin: Option<CoreWriter>,
    /// The read half of the core's stdout.
    stdout: CoreReader,
    /// Resolves when the process is gone.
    exited: BoxFuture<'static, ()>,
}

/// Spawn the real core subprocess and adapt its streams.
///
/// The core's stderr is the App Server's stderr, so its logs land in
/// the same place.
fn spawn_process(
    exe: &Path,
    args: &[OsString],
) -> std::io::Result<SpawnedCore> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .map(|stdin| Box::new(stdin) as Box<dyn AsyncWrite + Send + Unpin>);
    let Some(stdout) = child.stdout.take() else {
        return Err(std::io::Error::other("the core has no stdout"));
    };
    // The future owns the child so the supervisor can wait for the
    // exit after the stdout stream ends.
    let exited = Box::pin(async move {
        let _ = child.wait().await;
    });
    Ok(SpawnedCore {
        stdin,
        // The header reader consumes one byte at a time so it never
        // eats a coalesced frame's payload; the buffer keeps that from
        // costing a read syscall per header byte.
        stdout: Box::new(BufReader::new(stdout)),
        exited,
    })
}

/// What the App Server needs to serve.
pub struct ServeOptions {
    /// The address to bind.
    pub bind: String,
    /// The port to bind.
    pub port: u16,
    /// The workspace the core runs in.
    pub directory: PathBuf,
    /// The deployment configuration file, if the operator named one.
    pub config: Option<PathBuf>,
    /// A bundle patch the core should layer below the profile.
    pub bundle: Option<PathBuf>,
    /// The plugin profile the core should resolve.
    pub profile: Option<PathBuf>,
    /// A CLI patch layered over the profile.
    pub patch: Option<PathBuf>,
    /// The directory holding the web front-end's built files.
    pub web_dir: Option<PathBuf>,
}

/// Run the App Server until the process is asked to stop.
pub async fn serve(
    options: ServeOptions,
    cancel: Arc<uva::core::AtomicCancelToken>,
) -> anyhow::Result<()> {
    let app_server = Arc::new(AppServer {
        core: CoreHandle {
            stdin: Arc::new(tokio::sync::Mutex::new(None)),
        },
        connections: DashMap::new(),
        next_connection: AtomicU64::new(1),
        ready: Arc::new(AtomicBool::new(false)),
        internal_pending: DashMap::new(),
        next_internal_id: AtomicU64::new(1),
    });

    // The core subprocess: the same binary, `core` mode.
    let exe = std::env::current_exe()?;
    let mut args: Vec<OsString> = vec![OsString::from("core")];
    args.push(OsString::from("--directory"));
    args.push(options.directory.clone().into_os_string());
    if let Some(config) = &options.config {
        args.push(OsString::from("--config"));
        args.push(config.clone().into_os_string());
    }
    if let Some(bundle) = &options.bundle {
        args.push(OsString::from("--bundle"));
        args.push(bundle.clone().into_os_string());
    }
    if let Some(profile) = &options.profile {
        args.push(OsString::from("--profile"));
        args.push(profile.clone().into_os_string());
    }
    if let Some(patch) = &options.patch {
        args.push(OsString::from("--patch"));
        args.push(patch.clone().into_os_string());
    }

    let supervisor = tokio::spawn(supervise(
        Arc::clone(&app_server),
        exe,
        args,
        Arc::clone(&cancel),
    ));

    let app = router(Arc::clone(&app_server), options.web_dir.clone());
    let address = format!("{}:{}", options.bind, options.port);
    let listener = tokio::net::TcpListener::bind(&address).await?;
    tracing::info!(%address, "app server listening");
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            cancel.cancelled().await;
        })
        .await;
    supervisor.abort();
    served?;
    Ok(())
}

/// Spawn the core subprocess and keep it alive.
///
/// The core's stderr is the App Server's stderr, so its logs land in
/// the same place. Its stdout is read frame by frame and each payload
/// is routed.
async fn supervise(
    app_server: Arc<AppServer>,
    exe: PathBuf,
    args: Vec<OsString>,
    cancel: Arc<uva::core::AtomicCancelToken>,
) {
    supervise_with(app_server, move || spawn_process(&exe, &args), cancel)
        .await;
}

/// The supervisor's loop over an injectable spawn step.
///
/// `spawn` is called once per core generation: production passes
/// [`spawn_process`], and the tests pass a fake core so the restart
/// path runs without an OS process. Everything else — the backoff, the
/// ready flag, the subscription drop — is the production path.
async fn supervise_with<F>(
    app_server: Arc<AppServer>,
    mut spawn: F,
    cancel: Arc<uva::core::AtomicCancelToken>,
) where
    F: FnMut() -> std::io::Result<SpawnedCore>,
{
    let mut backoff = RESTART_BACKOFF_MIN;
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let SpawnedCore {
            stdin,
            stdout,
            exited,
        } = match spawn() {
            Ok(spawned) => spawned,
            Err(error) => {
                tracing::error!(%error, "app server: could not spawn the core");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RESTART_BACKOFF_MAX);
                continue;
            }
        };
        *app_server.core.stdin.lock().await = stdin;
        // The new generation has not announced itself yet.
        app_server.ready.store(false, Ordering::Relaxed);
        tracing::info!("app server: the core is up");

        pump_core(&app_server, stdout).await;

        // The core died: everything it owned is gone with it.
        app_server.ready.store(false, Ordering::Relaxed);
        *app_server.core.stdin.lock().await = None;
        drop_subscriptions(&app_server);
        exited.await;
        tracing::error!("app server: the core died; restarting");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RESTART_BACKOFF_MAX);
    }
}

/// Read the core's stdout as Content-Length frames and route each
/// payload.
///
/// The header block is consumed here and never reaches a browser: a
/// client sees one JSON-RPC object per WebSocket text message.
/// Readiness is the parsed `method`, so a payload that merely mentions
/// `ready` does not count. End of stream ends the generation; a
/// framing error leaves the stream unaligned, so it ends it too and
/// the supervisor restarts the core.
async fn pump_core<R: AsyncRead + Unpin>(
    app_server: &AppServer,
    mut stdout: R,
) {
    loop {
        let payload =
            match read_frame(&mut stdout, DEFAULT_MAX_FRAME_SIZE).await {
                Ok(payload) => payload,
                Err(error) if error.is_end_of_stream() => return,
                Err(error) => {
                    tracing::error!(
                        kind = error.kind(),
                        %error,
                        "app server: the core's stdout is not a frame stream"
                    );
                    return;
                }
            };
        let Ok(text) = String::from_utf8(payload) else {
            tracing::error!("app server: a core frame is not UTF-8");
            continue;
        };
        let parsed: Option<Value> = serde_json::from_str(&text).ok();
        if parsed
            .as_ref()
            .and_then(|frame| frame.get("method"))
            .and_then(Value::as_str)
            == Some("ready")
        {
            app_server.ready.store(true, Ordering::Relaxed);
        }
        app_server.route_frame(parsed.as_ref(), &text);
    }
}

/// Drop every connection's subscriptions and fail its in-flight
/// requests: the new core knows none of the old sessions' followers
/// and holds none of the old requests, so a client rebuilds its own
/// with a fresh `sessions/follow` and retries what mattered. The
/// failure echoes each request's own id — an uncorrelatable answer
/// would leave the client waiting forever.
fn drop_subscriptions(app_server: &AppServer) {
    for handle in app_server.connections.iter() {
        let pending: Vec<(String, Value)> = handle
            .pending
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        handle.subscriptions.clear();
        for (key, id) in pending {
            handle.pending.remove(&key);
            let refusal = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32000,
                    "message": "the core restarted; the request was                                 lost with it",
                    "data": {"code": "core_unavailable"},
                },
            });
            handle.send(&serde_json::to_string(&refusal).unwrap_or_default());
        }
    }
}

/// Build the App Server's router.
///
/// The static front-end is a **fallback**: every route above it wins,
/// and everything else is the web app's own territory. `--web-dir`
/// takes precedence over the assets baked in with the `embed-web`
/// feature, so a development build can point at a freshly built
/// `dist` without rebuilding the binary.
fn router(app_server: Arc<AppServer>, web_dir: Option<PathBuf>) -> Router {
    let mut app = Router::new()
        .route("/rpc", get(rpc_endpoint))
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .route("/version", get(version))
        .route("/metrics", get(metrics))
        .with_state(app_server)
        // Every request — the routes above and the front-end's
        // fallback alike — feeds the Prometheus RED counters. The
        // layer is applied after the routes so it also wraps the
        // fallback the static front-end is served from.
        .layer(axum::middleware::from_fn(metrics::track_request));
    #[cfg(feature = "embed-web")]
    {
        app = app.fallback(axum::routing::get(embedded_assets));
    }
    if let Some(directory) = web_dir {
        let index = directory.join("index.html");
        let files = tower_http::services::ServeDir::new(directory)
            .fallback(tower_http::services::ServeFile::new(index));
        // The static service sits outside the router's own layer
        // stack, so the metrics middleware is put around it by hand:
        // the front-end's requests are traffic too, and they land
        // under one label (the service matches no route, so there is
        // no template to collapse them into).
        let counted =
            axum::middleware::from_fn(metrics::track_request).layer(files);
        app = app.fallback_service(counted);
    }
    app
}

/// The WebSocket endpoint.
async fn rpc_endpoint(
    upgrade: WebSocketUpgrade,
    State(app_server): State<Arc<AppServer>>,
) -> Response {
    upgrade.on_upgrade(move |socket| serve_connection(socket, app_server))
}

/// Serve one WebSocket connection: a reader task for the browser's
/// frames and a writer task for the App Server's.
async fn serve_connection(
    socket: axum::extract::ws::WebSocket,
    app_server: Arc<AppServer>,
) {
    let (mut sink, mut stream) = socket.split();
    let connection = app_server.next_connection.fetch_add(1, Ordering::Relaxed);
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<String>();
    app_server.connections.insert(
        connection,
        ConnectionHandle {
            tx: outbound_tx,
            pending: DashMap::new(),
            subscriptions: DashMap::new(),
        },
    );

    let writer = tokio::spawn(async move {
        while let Some(frame) = outbound_rx.recv().await {
            if sink.send(Message::Text(frame.into())).await.is_err() {
                break;
            }
        }
    });

    loop {
        tokio::select! {
            frame = stream.next() => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(error) =
                            app_server.forward_upstream(connection, text.as_str())
                                .await
                        {
                            // The frame never reached the core; answer
                            // with the id it carried so the client can
                            // correlate (and retry after `ready`).
                            let id = serde_json::from_str::<Value>(
                                text.as_str(),
                            )
                            .ok()
                            .and_then(|frame| {
                                frame.get("id").cloned()
                            })
                            .unwrap_or(Value::Null);
                            let refusal = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": {
                                    "code": -32000,
                                    "message": error,
                                    "data": {"code": "core_unavailable"},
                                },
                            });
                            let handle = app_server
                                .connections
                                .get(&connection);
                            if let Some(handle) = handle {
                                let _ = handle.tx.send(
                                    serde_json::to_string(&refusal)
                                        .unwrap_or_default(),
                                );
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }

    // The connection is gone: its subscriptions and in-flight ids go
    // with it.
    app_server.connections.remove(&connection);
    writer.abort();
}

/// Liveness: the App Server process is up.
async fn livez() -> impl IntoResponse {
    axum::Json(json!({ "status": "ok" }))
}

/// Readiness: the core subprocess is alive and has announced itself.
async fn readyz(State(app_server): State<Arc<AppServer>>) -> Response {
    if app_server.is_ready() {
        return axum::Json(json!({ "status": "ok" })).into_response();
    }
    (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(json!({
            "status": "unavailable",
            "failed": ["core"],
        })),
    )
        .into_response()
}

/// The build identity.
async fn version() -> impl IntoResponse {
    axum::Json(json!({
        "version": crate::build_info::VERSION,
        "git_sha": crate::build_info::GIT_SHA,
        "short_sha": crate::build_info::short_sha(),
        "build_time": crate::build_info::BUILD_TIME,
        "target": crate::build_info::TARGET,
        "profile": crate::build_info::PROFILE,
        "long_version": crate::build_info::long_version(),
    }))
}

/// The Prometheus text exposition.
///
/// Two processes, two halves of the picture: the gateway counts the
/// HTTP requests it serves, and the core counts the tool calls the
/// agent loop makes. The core's half is fetched over the RPC
/// channel and appended — the families are disjoint (the core has no
/// HTTP surface), so the concatenation is a valid exposition.
async fn metrics(State(app_server): State<Arc<AppServer>>) -> Response {
    let mut body = uva::telemetry::gather_text();
    if let Some(core) = app_server.core_exposition().await {
        body.extend_from_slice(core.as_bytes());
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    )
        .into_response()
}

/// Serve the embedded web front-end.
///
/// The assets are baked into the binary at build time; a path that
/// names no asset falls back to `index.html`, so the single-page
/// app's own routes resolve.
#[cfg(feature = "embed-web")]
async fn embedded_assets(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let file = embedded::WebAssets::get(path)
        .or_else(|| embedded::WebAssets::get("index.html"));
    match file {
        Some(file) => {
            let mime = content_type_of(path);
            ([(header::CONTENT_TYPE, mime)], file.data.into_owned())
                .into_response()
        }
        None => "not found".into_response(),
    }
}

/// The media type of an asset path.
///
/// The web build emits a handful of extensions; a table keeps the
/// App Server free of a mime-guessing dependency.
#[cfg(feature = "embed-web")]
fn content_type_of(path: &str) -> &'static str {
    let extension = path.rsplit_once('.').map(|(_, tail)| tail);
    match extension {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The embedded asset tree.
#[cfg(feature = "embed-web")]
mod embedded {
    /// The web front-end's built files, baked into the binary.
    ///
    /// The folder is populated by `build.rs` from `uva-web/dist`
    /// when the `embed-web` feature is on. An empty tree is a valid
    /// tree: a build without the front-end compiled in serves 404s
    /// for the static paths and answers the RPC surface normally.
    #[derive(rust_embed::RustEmbed)]
    #[folder = "web-dist/"]
    pub struct WebAssets;
}

/// Whether the directory holds a web front-end to serve.
#[must_use]
pub fn has_web_dir(directory: &Path) -> bool {
    directory.join("index.html").is_file()
}

#[cfg(test)]
mod gateway_tests;

/// The Prometheus counters the HTTP surface feeds.
mod metrics;
