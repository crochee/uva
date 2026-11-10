//! The gateway's WebSocket JSON-RPC 2.0 client.
//!
//! One WebSocket text message is one JSON-RPC object — the same
//! contract the browser speaks at `/rpc`; the `Content-Length`
//! framing lives only between the gateway and the core. The client
//! owns the single socket: it correlates responses to requests by
//! `id`, fanning notifications out to the subscribers registered with
//! [`GatewayClient::subscribe`] (one method) or
//! [`GatewayClient::subscribe_all`] (every method), and after a
//! reconnect replays every live method subscription so a gateway-side
//! core restart (which drops them) cannot leave a host deaf.

//! # Liveness
//!
//! The pump lives as long as a client handle does: dropping every
//! [`GatewayClient`] ends the task within one reconnect attempt, and
//! a [`Subscription`] never keeps the socket alive on its own (its
//! drop only removes the handler). [`GatewayClient::close`] stops
//! the pump deterministically.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use futures::{
    SinkExt as _,
    StreamExt as _,
    channel::{mpsc, oneshot},
    future::BoxFuture,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

/// The first reconnect wait; each further failure doubles it.
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(250);

/// The reconnect wait ceiling.
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(2);

/// One notification handler: the notification's method name and its
/// `params` member. Shared by [`GatewayClient::subscribe`] and
/// [`GatewayClient::subscribe_all`].
pub type NotificationHandler = Arc<dyn Fn(&str, Value) + Send + Sync>;

/// Why a client call failed.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The WebSocket handshake or the TCP dial failed.
    #[error("failed to connect to {url}: {reason}")]
    Connect {
        /// The `ws://` URL that was dialed.
        url: String,
        /// What the dial or handshake saw.
        reason: String,
    },
    /// The client was closed, so nothing can be sent.
    #[error("the client is closed")]
    Closed,
    /// The connection died before the call was answered.
    #[error("transport error: {reason}")]
    Transport {
        /// What the transport saw.
        reason: String,
    },
    /// The server answered with a JSON-RPC error object.
    #[error("server error {code}: {message}")]
    Server {
        /// The JSON-RPC error code.
        code: i64,
        /// The JSON-RPC error message.
        message: String,
    },
}

/// What travels from the public methods to the pump.
enum Outbound {
    /// One serialized JSON-RPC envelope.
    Text(String),
    /// [`GatewayClient::close`] was called: send a WebSocket close
    /// frame and end the task.
    Close,
}

/// The shared state behind a [`GatewayClient`].
struct ClientInner {
    next_id: AtomicU64,
    /// Requests awaiting their response, keyed by the normalized id.
    pending:
        Mutex<HashMap<String, oneshot::Sender<Result<Value, ClientError>>>>,
    /// The live subscriptions, keyed by allocation order. The map is
    /// the single dispatch source, which is what keeps a replay from
    /// double-mounting a handler.
    subscriptions: Mutex<HashMap<u64, SubscriptionEntry>>,
    outbound: mpsc::UnboundedSender<Outbound>,
    /// Whether a socket is currently live. The reconnect loop owns
    /// it; hosts read it to narrate outages — a `request` sent while
    /// disconnected parks until a redial succeeds, so it cannot be
    /// the liveness probe.
    connected: AtomicBool,
    closed: AtomicBool,
}

/// One registered subscription: `None` is the catch-all a host uses
/// to observe every notification (the TUI), `Some(method)` matches
/// exactly that method (the desktop shell).
type SubscriptionEntry =
    (Option<String>, Arc<dyn Fn(&str, Value) + Send + Sync>);

impl ClientInner {
    /// Allocate the next request id, `c1`, `c2`, … — the browser
    /// client's convention.
    fn next_id(&self) -> String {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        format!("c{n}")
    }

    /// Queue one envelope for the pump. Returns `Err(Closed)` when
    /// nothing is left to send it.
    fn send_text(&self, envelope: String) -> Result<(), ClientError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ClientError::Closed);
        }
        self.outbound
            .unbounded_send(Outbound::Text(envelope))
            .map_err(|_| ClientError::Closed)
    }

    /// Answer every in-flight request with a transport error — the
    /// connection it was sent on is gone, and a caller must not wait
    /// for an answer that will never come.
    fn fail_pending(&self, reason: &str) {
        for (_, sender) in self.pending.lock().drain() {
            let _ = sender.send(Err(ClientError::Transport {
                reason: reason.to_owned(),
            }));
        }
    }
}

/// The key a response id is looked up under: a string id is its own
/// content, a number id is its decimal form, so whatever the server
/// echoes resolves to the key the request was registered under.
fn id_key(id: &Value) -> Option<String> {
    match id {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

/// Route one received envelope: a response to its requester, a
/// notification to every handler of its method.
fn handle_text(inner: &ClientInner, text: &str) {
    let Ok(frame) = serde_json::from_str::<Value>(text) else {
        return; // not our protocol — ignore, keep pumping
    };
    if let Some(key) = frame.get("id").and_then(id_key) {
        let Some(sender) = inner.pending.lock().remove(&key) else {
            return; // an answer to a subscription registration
        };
        let answer = match frame.get("error") {
            Some(error) => Err(ClientError::Server {
                code: error
                    .get("code")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_owned(),
            }),
            None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = sender.send(answer);
    } else if let Some(method) = frame.get("method").and_then(Value::as_str) {
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        let handlers: Vec<_> = inner
            .subscriptions
            .lock()
            .values()
            // A catch-all entry (`None`) matches every method; a
            // method entry only its own.
            .filter(|(subscribed, _)| {
                subscribed.as_deref().is_none_or(|s| s == method)
            })
            .map(|(_, handler)| handler.clone())
            .collect();
        for handler in handlers {
            handler(method, params.clone());
        }
    }
}

/// The serialized registration requests for every distinct method
/// with a live subscription — what a fresh connection replays.
fn build_replay(inner: &ClientInner) -> Vec<String> {
    let mut methods: Vec<String> = inner
        .subscriptions
        .lock()
        .values()
        // A catch-all has no wire registration to replay: the
        // gateway routes notifications by what the host requests
        // (e.g. `sessions/follow`), not by any registration frame.
        .filter_map(|(subscribed, _)| subscribed.clone())
        .collect();
    methods.sort();
    methods.dedup();
    methods
        .into_iter()
        .map(|method| {
            json!({
                "jsonrpc": "2.0",
                "id": inner.next_id(),
                "method": method,
                "params": Value::Null,
            })
            .to_string()
        })
        .collect()
}

/// One dial: a plain TCP connection plus the WebSocket handshake.
/// Only `ws://` is supported — the gateway is a local sidecar.
type Socket = WebSocketStream<TcpStream>;

async fn dial(url: &url::Url) -> Result<Socket, String> {
    let host = url.host_str().ok_or_else(|| "no host".to_owned())?;
    let port = url.port_or_known_default().unwrap_or(80);
    let stream = TcpStream::connect((host, port))
        .await
        .map_err(|error| format!("tcp dial failed: {error}"))?;
    let (socket, _) = tokio_tungstenite::client_async(url.as_str(), stream)
        .await
        .map_err(|error| format!("handshake failed: {error}"))?;
    Ok(socket)
}

/// Why the pump stopped.
enum PumpEnd {
    /// The socket died (error or close from either side).
    SocketDead,
    /// Every client handle is gone: the channel has no senders left.
    NoSenders,
    /// `close()` was called and the close frame was sent.
    Closed,
}

/// Serve one connection: replay the subscriptions, then shuttle
/// envelopes both ways until something ends.
async fn pump(
    inner: Arc<ClientInner>,
    socket: Socket,
    outbound: &mut mpsc::UnboundedReceiver<Outbound>,
) -> PumpEnd {
    let (mut sink, mut stream) = socket.split();
    for envelope in build_replay(&inner) {
        if sink.send(Message::text(envelope)).await.is_err() {
            return PumpEnd::SocketDead;
        }
    }
    loop {
        tokio::select! {
            frame = stream.next() => match frame {
                None | Some(Err(_)) => return PumpEnd::SocketDead,
                Some(Ok(message)) => {
                    if let Message::Text(text) = message {
                        handle_text(&inner, text.as_str());
                    }
                }
            },
            outbound_item = outbound.next() => match outbound_item {
                None => return PumpEnd::NoSenders,
                Some(Outbound::Close) => {
                    let _ = sink.send(Message::Close(None)).await;
                    return PumpEnd::Closed;
                }
                Some(Outbound::Text(text)) => {
                    if sink.send(Message::text(text)).await.is_err() {
                        return PumpEnd::SocketDead;
                    }
                }
            },
        }
    }
}

/// The connection task: pump, reconnect with a 250ms → 2s backoff,
/// replay, and stop once closed or abandoned.
async fn run(
    url: url::Url,
    inner: Arc<ClientInner>,
    mut outbound: mpsc::UnboundedReceiver<Outbound>,
    first: Socket,
) {
    let mut socket = Some(first);
    let mut backoff = RECONNECT_BACKOFF_MIN;
    loop {
        let current = match socket.take() {
            Some(socket) => socket,
            None => {
                let wait = backoff;
                if let Ok(redialed) = dial(&url).await {
                    backoff = RECONNECT_BACKOFF_MIN;
                    redialed
                } else {
                    if inner.closed.load(Ordering::SeqCst) {
                        return;
                    }
                    tokio::time::sleep(wait).await;
                    backoff = backoff.mul_f64(2.0).min(RECONNECT_BACKOFF_MAX);
                    continue;
                }
            }
        };
        inner.connected.store(true, Ordering::SeqCst);
        match pump(inner.clone(), current, &mut outbound).await {
            PumpEnd::SocketDead => {
                inner.connected.store(false, Ordering::SeqCst);
                inner.fail_pending("the connection was lost");
                if inner.closed.load(Ordering::SeqCst) {
                    return;
                }
                let wait = backoff;
                tokio::time::sleep(wait).await;
                backoff = backoff.mul_f64(2.0).min(RECONNECT_BACKOFF_MAX);
            }
            PumpEnd::NoSenders | PumpEnd::Closed => {
                inner.connected.store(false, Ordering::SeqCst);
                inner.fail_pending("the client went away");
                return;
            }
        }
    }
}

/// A connection to a gateway's `/rpc` WebSocket.
///
/// Cheap to clone: every clone shares the one socket and its
/// subscription registry.
#[derive(Clone)]
pub struct GatewayClient {
    inner: Arc<ClientInner>,
}

impl std::fmt::Debug for GatewayClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayClient")
            .field("closed", &self.inner.closed.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl GatewayClient {
    /// Dial `url` (`ws://host:port/rpc`) and resolve once the
    /// WebSocket handshake is done. The connection then lives on its
    /// own task, reconnecting with a 250ms → 2s backoff whenever it
    /// drops, and replaying every live subscription after each
    /// reconnect.
    pub fn connect(
        url: &str,
    ) -> BoxFuture<'static, Result<GatewayClient, ClientError>> {
        let url = url.to_owned();
        Box::pin(async move {
            let parsed = url::Url::parse(&url).map_err(|error| {
                ClientError::Connect {
                    url: url.clone(),
                    reason: error.to_string(),
                }
            })?;
            if parsed.scheme() != "ws" {
                return Err(ClientError::Connect {
                    url,
                    reason: "only ws:// URLs are supported".to_owned(),
                });
            }
            let socket =
                dial(&parsed).await.map_err(|reason| ClientError::Connect {
                    url: url.clone(),
                    reason,
                })?;
            let (outbound_tx, outbound_rx) = mpsc::unbounded();
            let inner = Arc::new(ClientInner {
                next_id: AtomicU64::new(0),
                pending: Mutex::new(HashMap::new()),
                subscriptions: Mutex::new(HashMap::new()),
                outbound: outbound_tx,
                connected: AtomicBool::new(true),
                closed: AtomicBool::new(false),
            });
            tokio::spawn(run(parsed, inner.clone(), outbound_rx, socket));
            Ok(GatewayClient { inner })
        })
    }

    /// Send one JSON-RPC request and await its response, correlated
    /// by the allocated id. A dropped connection fails the call with
    /// [`ClientError::Transport`]; the caller decides whether to
    /// retry.
    pub fn request(
        &self,
        method: &str,
        params: Value,
    ) -> BoxFuture<'static, Result<Value, ClientError>> {
        let inner = self.inner.clone();
        let method = method.to_owned();
        Box::pin(async move {
            let id = inner.next_id();
            let envelope = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            });
            let (sender, receiver) = oneshot::channel();
            inner.pending.lock().insert(id.clone(), sender);
            if inner.send_text(envelope.to_string()).is_err() {
                inner.pending.lock().remove(&id);
                return Err(ClientError::Closed);
            }
            receiver.await.map_err(|_| ClientError::Closed)?
        })
    }

    /// Whether a socket is live right now. `false` means the
    /// reconnect loop is redialing: a [`request`](Self::request)
    /// sent now parks until a redial succeeds, so a host that must
    /// narrate outages (the TUI's status bar) polls this instead of
    /// probing over a request.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.inner.connected.load(Ordering::SeqCst)
    }

    /// Send one JSON-RPC notification (no id, no response).
    pub fn notify(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(), ClientError> {
        let envelope = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.inner.send_text(envelope.to_string())
    }

    /// Register a handler for one notification method and tell the
    /// gateway about it. The registration is replayed automatically
    /// after every reconnect, and the handler is never mounted
    /// twice — dispatch reads the registry, so a replay re-sends the
    /// registration without duplicating local handlers.
    ///
    /// Dropping the returned [`Subscription`] removes the handler.
    /// There is no wire-level unsubscribe: the gateway drops a
    /// connection's subscriptions when it dies, and a host that
    /// wants an explicit one sends it as its own request.
    pub fn subscribe(
        &self,
        method: &str,
        handler: NotificationHandler,
    ) -> Result<Subscription, ClientError> {
        self.register(Some(method.to_owned()), handler)
    }

    /// Register one handler for **every** notification, whatever the
    /// method — the catch-all a host that projects the whole wire
    /// (the TUI) needs, so a newly added notification reaches it
    /// without a per-method registration.
    ///
    /// A catch-all is purely local: nothing is registered on the
    /// wire and nothing is replayed after a reconnect, because the
    /// gateway routes notifications by what the host requests (e.g.
    /// `sessions/follow`), not by any registration frame. Dropping
    /// the returned [`Subscription`] removes the handler.
    pub fn subscribe_all(
        &self,
        handler: NotificationHandler,
    ) -> Result<Subscription, ClientError> {
        self.register(None, handler)
    }

    /// The one registration path both subscription flavours share.
    fn register(
        &self,
        method: Option<String>,
        handler: NotificationHandler,
    ) -> Result<Subscription, ClientError> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(ClientError::Closed);
        }
        let slot = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        // Only a method subscription has a wire registration to
        // send; a catch-all is local dispatch alone.
        let result = match &method {
            Some(method) => {
                let envelope = json!({
                    "jsonrpc": "2.0",
                    "id": format!("c{slot}"),
                    "method": method,
                    "params": Value::Null,
                });
                self.inner.send_text(envelope.to_string())
            }
            None => Ok(()),
        };
        // Register even when the send fails against a momentarily
        // dead socket: the queued-or-lost registration is replayed
        // by the next reconnect either way, and the handler belongs
        // to the subscription's lifetime, not the wire state.
        self.inner
            .subscriptions
            .lock()
            .insert(slot, (method, handler));
        result.map(|_| Subscription {
            inner: Arc::downgrade(&self.inner),
            slot,
        })
    }

    /// Stop the pump: close the WebSocket and fail in-flight requests.
    /// Idempotent.
    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        let _ = self.inner.outbound.unbounded_send(Outbound::Close);
    }
}

/// A subscription handle: drop to remove the handler, mirroring the
/// kernel Effect's "registration returns a handle" semantics.
pub struct Subscription {
    inner: Weak<ClientInner>,
    slot: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.subscriptions.lock().remove(&self.slot);
        }
    }
}
