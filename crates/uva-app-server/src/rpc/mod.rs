//! The JSON-RPC 2.0 core.
//!
//! One method table, one error model, one transport.
//!
//! - [`RpcRouter`] — the method table. A method is a name
//!   (`domain/verb`, the slash style LSP / MCP / gRPC use) and a
//!   handler that takes the shared state plus the `params` member
//!   and answers a JSON value. Every name is spelled once, in
//!   [`methods::names`], and the router registers by those
//!   constants.
//! - [`error`] — the boundary error type. Application failures carry
//!   a machine code in `error.data.code`; protocol failures use the
//!   standard JSON-RPC codes.
//! - [`framing`] — the Content-Length frame codec: an async reader /
//!   writer pair for the gateway, and the blocking reader the stdio
//!   thread uses.
//! - [`stdio`] — the stdio pump: Content-Length frames on stdin, the
//!   same framing on stdout, one task per request so responses may
//!   arrive out of order.
//! - [`inflight`] — the per-request cancel token and deadline table.
//! - [`notify`] — the server→client notification fan-out
//!   (`session/event`, and the per-call `approval/request` projection).
//!
//! # Approvals
//!
//! A durable suspension is projected as an `approval/request`
//! notification (`params.approval_id` + `params.session_id`; the id is
//! the UI correlation id, not the JSON-RPC `id`). The client answers
//! with `approval/respond`, an ordinary request registered in
//! [`methods::sessions`]. It maps the answer onto the existing
//! `sessions/resume` + `ResumeToken` path — the log's
//! `approval_decided` rows remain the truth, so a restart loses no
//! decision and a timeout still defaults to deny.
//!
//! # Wire shape
//!
//! A frame is `Content-Length: <n>\r\n\r\n` followed by exactly `n`
//! bytes of JSON. The length is the payload's **byte** length; the
//! header name is case-insensitive and extra header lines are
//! tolerated. A request payload is:
//!
//! ```text
//! {"jsonrpc":"2.0","id":1,"method":"health/ping","params":{}}
//! ```
//!
//! `id` may be a string or a number and is echoed verbatim. A frame
//! without `id` is a notification: it is processed and answered with
//! nothing.
//!
//! A response payload is:
//!
//! ```text
//! {"jsonrpc":"2.0","id":1,"result":{"ok":true,"version":"0.1.0"}}
//! {"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"…","data":{"code":"not_found"}}}
//! ```
//!
//! A notification payload is, without `id`:
//!
//! ```text
//! {"jsonrpc":"2.0","method":"ready","params":{"version":"0.1.0","workspace_root":"/srv"}}
//! ```
//!
//! # Cancellation
//!
//! A client may cancel a request with a `$/cancelRequest` frame whose
//! `params.id` is the original id. Sent as a notification it stops
//! the work silently; sent as a request, the answer reports whether
//! the id was in flight (`result.cancelled`) or names it `not_found`.
//! A cancelled request never writes its own response.
//!
//! # Concurrency
//!
//! The pump reads a frame, spawns a task for it, and moves on. Two
//! requests may therefore be in flight at once and their responses
//! may arrive in either order — a client matches on `id`.
pub mod error;
pub mod framing;
#[cfg(test)]
mod framing_tests;
pub mod inflight;
pub mod methods;
pub mod notify;
pub mod stdio;
#[cfg(test)]
mod stdio_tests;

use std::{collections::HashMap, sync::Arc};

pub use error::{
    APPLICATION_ERROR,
    INTERNAL_ERROR,
    INVALID_PARAMS,
    INVALID_REQUEST,
    METHOD_NOT_FOUND,
    PARSE_ERROR,
    RpcError,
};
use futures::future::BoxFuture;
pub use inflight::Inflight;
pub use notify::FollowRegistry;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
pub use stdio::{StioOptions, StioPump};

use crate::state::AppState;

/// A registered method: the shared state plus the `params` member in,
/// the JSON result out.
pub type Handler = Arc<
    dyn Fn(Arc<AppState>, Value) -> BoxFuture<'static, Result<Value, RpcError>>
        + Send
        + Sync,
>;
/// The method table.
///
/// Registration is by `&'static str` so the table has no allocation
/// on lookup and the method names in the binary are the names on the
/// wire.
#[derive(Default)]
pub struct RpcRouter {
    methods: HashMap<&'static str, Handler>,
}
impl RpcRouter {
    /// An empty table.
    pub fn new() -> Self {
        Self {
            methods: HashMap::new(),
        }
    }

    /// Register a method.
    ///
    /// The handler is boxed into a [`Handler`] here, so the call site
    /// stays a plain async closure.
    pub fn register<F, Fut>(&mut self, method: &'static str, handler: F)
    where
        F: Fn(Arc<AppState>, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, RpcError>> + Send + 'static,
    {
        self.methods.insert(
            method,
            Arc::new(move |state, params| Box::pin(handler(state, params))),
        );
    }

    /// The handler for a method, if the table knows it.
    pub fn get(&self, method: &str) -> Option<&Handler> {
        self.methods.get(method)
    }

    /// Every registered method name, sorted.
    pub fn method_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> =
            self.methods.keys().copied().collect();
        names.sort_unstable();
        names
    }

    /// Run one request frame from its JSON text.
    ///
    /// Returns the response envelope, or `None` when the frame was a
    /// notification (no `id`) — a notification gets no response, not
    /// even an error one.
    pub async fn dispatch(
        &self,
        state: Arc<AppState>,
        line: &str,
    ) -> Option<Value> {
        match serde_json::from_str(line) {
            Ok(frame) => self.dispatch_value(state, frame).await,
            Err(error) => Some(error_frame(
                Value::Null,
                PARSE_ERROR,
                &format!("parse error: {error}"),
            )),
        }
    }

    /// Run one already-parsed request frame.
    ///
    /// The pump parses each inbound frame once — it needs the `id`
    /// and `method` for the in-flight table — and hands the value
    /// here. [`Self::dispatch`] is the text-taking wrapper for
    /// callers that have only the wire bytes.
    pub async fn dispatch_value(
        &self,
        state: Arc<AppState>,
        frame: Value,
    ) -> Option<Value> {
        let Some(object) = frame.as_object() else {
            return Some(error_frame(
                Value::Null,
                INVALID_REQUEST,
                "a request frame must be a JSON object",
            ));
        };
        let id = object.get("id").cloned();
        let notification = id.is_none();
        let id = id.unwrap_or(Value::Null);
        if !notification && !matches!(id, Value::String(_) | Value::Number(_)) {
            return Some(error_frame(
                Value::Null,
                INVALID_REQUEST,
                "`id` must be a string or a number",
            ));
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return if notification {
                None
            } else {
                Some(error_frame(
                    id,
                    INVALID_REQUEST,
                    "`method` must be a string",
                ))
            };
        };
        let params = object
            .get("params")
            .cloned()
            .filter(|value| !value.is_null())
            .unwrap_or_else(|| json!({}));
        let Some(handler) = self.get(method) else {
            return if notification {
                None
            } else {
                Some(error_frame(
                    id,
                    METHOD_NOT_FOUND,
                    &format!("unknown method `{method}`"),
                ))
            };
        };
        let outcome = handler(state, params).await;
        if notification {
            // A notification's failure is logged, not answered.
            if let Err(error) = outcome {
                error.log();
            }
            return None;
        }
        Some(match outcome {
            Ok(result) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            }),
            Err(error) => {
                error.log();
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": error.to_error_object(),
                })
            }
        })
    }
}

/// Build a response frame that carries a protocol-level failure.
pub fn error_frame(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}
/// Deserialize the `params` member into a typed request struct and
/// run the struct's own schema validation.
///
/// The `validation` machine code answers a schema failure — the same
/// code the HTTP boundary used for it.
pub fn params<T>(params: Value) -> Result<T, RpcError>
where
    T: DeserializeOwned + validator::Validate,
{
    let value: T = serde_json::from_value(params).map_err(|error| {
        RpcError::protocol(INVALID_PARAMS, format!("invalid params: {error}"))
    })?;
    value.validate()?;
    Ok(value)
}
/// Deserialize the `params` member into a typed request struct
/// without schema validation.
///
/// Used by the request types that carry no `#[validate(...)]`
/// attributes of their own.
pub fn params_raw<T>(params: Value) -> Result<T, RpcError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(params).map_err(|error| {
        RpcError::protocol(INVALID_PARAMS, format!("invalid params: {error}"))
    })
}
/// Serialize a handler's return value into a JSON result.
pub fn encode<T: Serialize>(value: T) -> Result<Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|error| RpcError::new(anyhow::anyhow!(error)))
}
/// The outbound half of the transport: every envelope the process
/// emits — responses and notifications alike — goes through one
/// channel, so one writer owns stdout and lines never interleave.
#[derive(Clone)]
pub struct Outbound {
    tx: tokio::sync::mpsc::UnboundedSender<Value>,
}
impl Outbound {
    /// Wrap the sending half of an envelope channel.
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<Value>) -> Self {
        Self { tx }
    }

    /// A channel with no reader — the writer is gone, so every send
    /// is dropped. Used by fixtures that never speak the protocol.
    pub fn detached() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        Self::new(tx)
    }

    /// Emit one envelope.
    pub fn send(&self, envelope: Value) {
        let _ = self.tx.send(envelope);
    }

    /// Emit one notification.
    pub fn notify(&self, method: &str, params: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }
}
/// The process-wide RPC runtime: the outbound channel and the
/// per-session follow subscribers.
///
/// It lives on [`AppState`] so every method handler reaches the
/// notification fan-out without a second argument. The runtime owns
/// both halves of the envelope channel: the sender every handler
/// writes through, and the receiver the transport drains.
pub struct RpcRuntime {
    outbound: Outbound,
    receiver:
        tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Value>>>,
    follows: Arc<FollowRegistry>,
}
impl Default for RpcRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl RpcRuntime {
    /// Build the runtime with a live envelope channel.
    ///
    /// The receiving half stays inside the runtime until the
    /// transport asks for it.
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let outbound = Outbound::new(tx);
        Self {
            follows: Arc::new(FollowRegistry::new(outbound.clone())),
            outbound,
            receiver: tokio::sync::Mutex::new(Some(rx)),
        }
    }

    /// Build the runtime and hand back the receiving half.
    ///
    /// Tests use this to observe the envelopes the runtime emits;
    /// the transport itself asks for the receiver via
    /// [`Self::take_receiver`].
    pub fn channel() -> (Self, tokio::sync::mpsc::UnboundedReceiver<Value>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let outbound = Outbound::new(tx);
        (
            Self {
                follows: Arc::new(FollowRegistry::new(outbound.clone())),
                outbound,
                receiver: tokio::sync::Mutex::new(None),
            },
            rx,
        )
    }

    /// Build the runtime with no reader: every send is dropped.
    ///
    /// Used by fixtures that never speak the protocol.
    pub fn detached() -> Self {
        let outbound = Outbound::detached();
        Self {
            follows: Arc::new(FollowRegistry::new(outbound.clone())),
            outbound,
            receiver: tokio::sync::Mutex::new(None),
        }
    }

    /// The outbound channel.
    pub fn outbound(&self) -> &Outbound {
        &self.outbound
    }

    /// Take the receiving half of the envelope channel.
    ///
    /// The transport asks for it once; a second call answers `None`
    /// (the channel is already gone).
    pub async fn take_receiver(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<Value>> {
        self.receiver.lock().await.take()
    }

    /// The per-session follow subscribers.
    pub fn follows(&self) -> &Arc<FollowRegistry> {
        &self.follows
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// A state the fixture owns for the length of the test.
    async fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("a temp workspace");
        let manager = uva::session::manager::SessionRegistry::new(
            dir.path().join("sessions"),
        );
        let state = AppState::for_test(manager, dir.path().to_path_buf()).await;
        (Arc::new(state), dir)
    }
    fn router_with_one_method() -> RpcRouter {
        let mut router = RpcRouter::new();
        router.register("health/ping", |_state, _params| async {
            Ok(json!({ "ok": true }))
        });
        router
    }
    #[tokio::test]
    async fn dispatch_answers_a_request_frame() {
        let router = router_with_one_method();
        let (state, _dir) = test_state().await;
        let response = router
            .dispatch(
                state,
                r#"{"jsonrpc":"2.0","id":1,"method":"health/ping"}"#,
            )
            .await
            .expect("a request frame is answered");
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["ok"], true);
    }
    #[tokio::test]
    async fn dispatch_echoes_a_string_id() {
        let router = router_with_one_method();
        let (state, _dir) = test_state().await;
        let response = router
            .dispatch(
                state,
                r#"{"jsonrpc":"2.0","id":"c7","method":"health/ping"}"#,
            )
            .await
            .expect("a request frame is answered");
        assert_eq!(response["id"], "c7");
    }
    #[tokio::test]
    async fn dispatch_rejects_an_unparseable_frame() {
        let router = router_with_one_method();
        let (state, _dir) = test_state().await;
        let response = router
            .dispatch(state, "{not json")
            .await
            .expect("a malformed frame is answered");
        assert_eq!(response["error"]["code"], PARSE_ERROR);
    }
    #[tokio::test]
    async fn dispatch_rejects_an_unknown_method() {
        let router = router_with_one_method();
        let (state, _dir) = test_state().await;
        let response = router
            .dispatch(state, r#"{"jsonrpc":"2.0","id":1,"method":"nope/nope"}"#)
            .await
            .expect("an unknown method is answered");
        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
    }
    #[tokio::test]
    async fn dispatch_rejects_a_non_scalar_id() {
        let router = router_with_one_method();
        let (state, _dir) = test_state().await;
        let response = router
            .dispatch(
                state,
                r#"{"jsonrpc":"2.0","id":{"a":1},"method":"health/ping"}"#,
            )
            .await
            .expect("a malformed id is answered");
        assert_eq!(response["error"]["code"], INVALID_REQUEST);
    }
    #[tokio::test]
    async fn dispatch_answers_nothing_for_a_notification() {
        let router = router_with_one_method();
        let (state, _dir) = test_state().await;
        let response = router
            .dispatch(state, r#"{"jsonrpc":"2.0","method":"health/ping"}"#)
            .await;
        assert!(response.is_none());
    }
    #[test]
    fn method_names_are_sorted() {
        let mut router = RpcRouter::new();
        router.register("b/two", |_state, _params| async { Ok(json!(null)) });
        router.register("a/one", |_state, _params| async { Ok(json!(null)) });
        assert_eq!(router.method_names(), vec!["a/one", "b/two"]);
    }
}
