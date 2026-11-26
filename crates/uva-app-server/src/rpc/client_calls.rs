//! The core's own requests to the client.
//!
//! The core face is a JSON-RPC 2.0 server: the client asks, the core
//! answers. A client that owns tools needs the other direction — the
//! core asks the client to run one of them and waits for the answer
//! (omp's `host_tool_call` / `host_tool_result` shape). That is this
//! channel: a request frame carrying an id the core minted, and the
//! client's response frame correlated back to the waiter that asked.
//!
//! # The id namespace
//!
//! The ids the core mints carry a reserved prefix
//! ([`CORE_CALL_ID_PREFIX`]), so a client-minted request id can never
//! collide with them. The prefix matters twice: the gateway reserves
//! `gateway:internal:` the same way for *its* requests to the core,
//! and the core's stdio pump consults this table **before** the ACP
//! adapter — which otherwise claims every response-shaped frame — so
//! only an id this table holds can steer an answer away from ACP.
//!
//! # Timeouts
//!
//! A request that no client answers must not hang the turn that
//! asked: every caller passes a budget, and an expired call is
//! removed from the table and answered with
//! [`ClientCallError::Timeout`].

use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use serde_json::Value;
use tokio::sync::oneshot;

/// The prefix every core-minted request id carries.
///
/// The namespace is reserved in the protocol's sense: a client that
/// mints its own request ids never lands here by accident, and a
/// response frame whose id carries the prefix belongs to the core's
/// own pending call (when the table holds it).
pub const CORE_CALL_ID_PREFIX: &str = "core:internal:";

/// Why a core→client request did not come back with a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientCallError {
    /// The client did not answer within the budget.
    Timeout,
    /// The client answered with a JSON-RPC error object.
    Refused {
        /// The client's refusal message.
        message: String,
    },
    /// The transport closed before the answer arrived.
    Closed,
}

impl std::fmt::Display for ClientCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "the client did not answer in time"),
            Self::Refused { message } => write!(f, "{message}"),
            Self::Closed => {
                write!(f, "the client is gone; the call was dropped")
            }
        }
    }
}

/// The pending core→client requests, keyed by the id the core minted.
///
/// One table lives on the process-wide [`RpcRuntime`](super::RpcRuntime):
/// the code that asks holds the receiving half, and the transport that
/// reads the client's answer delivers it here by id.
pub struct ClientCalls {
    pending:
        Mutex<HashMap<String, oneshot::Sender<Result<Value, ClientCallError>>>>,
    next_id: AtomicU64,
}

impl Default for ClientCalls {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientCalls {
    /// An empty table.
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        }
    }

    /// Mint a request id and register its waiter.
    pub(crate) fn begin(
        &self,
    ) -> (String, oneshot::Receiver<Result<Value, ClientCallError>>) {
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = format!("{CORE_CALL_ID_PREFIX}{sequence}");
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .expect("the client-call table is not poisoned")
            .insert(id.clone(), sender);
        (id, receiver)
    }

    /// Forget a call without answering it — the timeout path, so a
    /// late answer finds no waiter and is dropped by the table.
    pub(crate) fn drop(&self, id: &str) {
        self.pending
            .lock()
            .expect("the client-call table is not poisoned")
            .remove(id);
    }

    /// How many calls are waiting for their answers.
    pub fn len(&self) -> usize {
        self.pending
            .lock()
            .expect("the client-call table is not poisoned")
            .len()
    }

    /// Whether no call is waiting.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Deliver a response frame to the call that asked, if it was
    /// ours.
    ///
    /// A frame is ours when it is a response (a `result` or an
    /// `error`, no `method`) whose `id` this table holds; the answer
    /// is routed to the waiter and the entry removed. Anything else —
    /// a request, a notification, a response to a request *the
    /// client* minted — leaves the table untouched and answers
    /// `false`, so the transport's other claimants (the ACP adapter,
    /// the router) see it exactly as before.
    pub fn resolve(&self, frame: &Value) -> bool {
        if frame.get("method").is_some() {
            return false;
        }
        if frame.get("result").is_none() && frame.get("error").is_none() {
            return false;
        }
        let Some(id) = frame.get("id").and_then(Value::as_str) else {
            return false;
        };
        let Some(sender) = self
            .pending
            .lock()
            .expect("the client-call table is not poisoned")
            .remove(id)
        else {
            return false;
        };
        let outcome = if let Some(result) = frame.get("result") {
            Ok(result.clone())
        } else {
            Err(ClientCallError::Refused {
                message: frame
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("the client refused the request")
                    .to_string(),
            })
        };
        sender.send(outcome).is_ok()
    }
}

/// The answer a client gives a tool execution: text content and an
/// error flag. Text is the v1 shape — a client that wants to answer
/// with parts (an image, an attachment) has no way to say so yet.
#[derive(serde::Deserialize)]
pub struct ClientToolResult {
    /// The tool's answer, as text.
    pub content: String,
    /// Whether the answer reports a failure.
    #[serde(default)]
    pub is_error: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_minted_id_carries_the_reserved_prefix() {
        let calls = ClientCalls::new();
        let (id, _receiver) = calls.begin();
        assert!(id.starts_with(CORE_CALL_ID_PREFIX), "{id}");
        let (next, _receiver) = calls.begin();
        assert_ne!(id, next, "the ids are distinct");
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn a_result_frame_reaches_the_waiter_that_asked() {
        let calls = ClientCalls::new();
        let (id, receiver) = calls.begin();
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"content": "ran on the client"},
        });
        assert!(calls.resolve(&frame), "the frame was ours");
        let answer = receiver.blocking_recv().expect("the waiter is answered");
        assert_eq!(
            answer.expect("the client answered with a result")["content"],
            "ran on the client"
        );
        assert!(calls.is_empty(), "the entry is removed");
    }

    #[test]
    fn an_error_frame_maps_to_a_refusal() {
        let calls = ClientCalls::new();
        let (id, receiver) = calls.begin();
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32000, "message": "no such tool"},
        });
        assert!(calls.resolve(&frame));
        match receiver.blocking_recv().expect("the waiter is answered") {
            Err(ClientCallError::Refused { message }) => {
                assert_eq!(message, "no such tool");
            }
            other => panic!("a refusal, not {other:?}"),
        }
    }

    #[test]
    fn a_foreign_frame_is_not_ours() {
        let calls = ClientCalls::new();
        let (id, _receiver) = calls.begin();
        // A response to a request the client minted.
        assert!(!calls.resolve(&json!({
            "jsonrpc": "2.0", "id": 7, "result": {},
        })));
        // A request, not a response.
        assert!(!calls.resolve(&json!({
            "jsonrpc": "2.0", "id": id, "method": "health/ping",
        })));
        // Our id, but no answer in it.
        assert!(!calls.resolve(&json!({
            "jsonrpc": "2.0", "id": id,
        })));
        assert_eq!(calls.len(), 1, "the waiter is still pending");
    }

    #[test]
    fn dropping_a_call_leaves_no_waiter() {
        let calls = ClientCalls::new();
        let (id, receiver) = calls.begin();
        calls.drop(&id);
        // A late answer finds no waiter and is not claimed.
        assert!(!calls.resolve(&json!({
            "jsonrpc": "2.0", "id": id, "result": {},
        })));
        assert!(
            receiver.blocking_recv().is_err(),
            "the waiter was dropped, not answered"
        );
    }
}
