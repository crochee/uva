//! The ACP agent's place in the core's transport.
//!
//! The core's JSON-RPC channel carries two vocabularies: the core's
//! own methods and ACP's. A frame that names an ACP method goes to
//! the ACP agent; everything else goes to the core's router. The
//! agent's answers travel back the same channel, so a client that
//! speaks ACP and a client that speaks the core's JSON-RPC share one
//! pipe — which is what the gateway's WebSocket offers both.
//!
//! # The frames
//!
//! The channel carries the SDK's own transport frames, so the ACP
//! side sees the protocol's messages and never a local copy of them.
//! The core's frames are translated at the boundary: a core frame
//! becomes a transport frame on the way in, and a transport frame
//! becomes a core frame on the way out.
//!
//! # The connection
//!
//! One agent serves the channel. The protocol's connection state is
//! the handshake and the session ids, and both are per-session
//! rather than per-client: a session the agent minted belongs to
//! whoever opened it, and the gateway routes that session's frames to
//! that client.

use std::sync::Arc;

use agent_client_protocol::{
    Channel,
    RawJsonRpcError,
    RawJsonRpcId,
    RawJsonRpcMessage,
    RawJsonRpcResponse,
    TransportFrame,
};
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::Value;
use tracing::{debug, warn};

use crate::{adapters::acp::agent::AcpAgent, state::AppState};

/// The methods the ACP agent answers.
///
/// The set is the protocol's: a frame that names one of them belongs
/// to the ACP adapter, and every other frame belongs to the core's
/// router. The two vocabularies share the pipe, and the method names
/// are what tells them apart.
const ACP_METHODS: [&str; 6] = [
    "initialize",
    "authenticate",
    "session/new",
    "session/load",
    "session/prompt",
    "session/cancel",
];

/// The ACP agent's endpoint on the core's channel.
pub struct AcpEndpoint {
    /// The frames the core hands to the agent.
    to_agent: futures::channel::mpsc::UnboundedSender<TransportFrame>,
    /// The agent's half of the channel, taken when the agent starts.
    agent_side: Mutex<Option<Channel>>,
    /// The frames the agent hands back, taken when the publisher
    /// starts.
    from_agent: Mutex<
        Option<futures::channel::mpsc::UnboundedReceiver<TransportFrame>>,
    >,
}

impl Default for AcpEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpEndpoint {
    /// The endpoint's channels, before the agent runs.
    pub fn new() -> Self {
        let (core, agent) = Channel::duplex();
        let (agent_rx, agent_tx) = agent.into_parts();
        let (core_rx, core_tx) = core.into_parts();
        Self {
            to_agent: core_tx,
            agent_side: Mutex::new(Some(Channel::new(agent_rx, agent_tx))),
            from_agent: Mutex::new(Some(core_rx)),
        }
    }

    /// Start the agent over this endpoint.
    ///
    /// The agent answers the ACP frames the core's channel carries,
    /// and its own frames travel back through the core's outbound
    /// channel — one writer owns stdout, whoever produced the frame.
    pub fn start(&self, state: Arc<AppState>) {
        let Some(agent_side) = self.agent_side.lock().take() else {
            // The agent already runs: starting it twice would give
            // two agents one session table.
            return;
        };
        let agent = AcpAgent::new(Arc::clone(&state));
        tokio::spawn(async move {
            if let Err(error) = agent.connect(agent_side).await {
                warn!(%error, "the ACP agent stopped");
            }
        });
        let Some(mut frames) = self.from_agent.lock().take() else {
            return;
        };
        let outbound = state.rpc.outbound().clone();
        tokio::spawn(async move {
            while let Some(frame) = frames.next().await {
                let Some(envelope) = envelope_of(frame) else {
                    continue;
                };
                outbound.send(envelope);
            }
            debug!("the ACP agent's channel closed");
        });
    }

    /// Whether the frame is one the ACP agent answers.
    ///
    /// An ACP method is the agent's; a response — a frame with a
    /// result and no method — is the answer to a question the agent
    /// asked. The core asks no questions of its own, so every
    /// response on this channel belongs to the agent.
    #[must_use]
    pub fn answers(&self, frame: &Value) -> bool {
        match frame.get("method").and_then(Value::as_str) {
            Some(method) => ACP_METHODS.contains(&method),
            None => {
                frame.get("result").is_some() || frame.get("error").is_some()
            }
        }
    }

    /// Hand the frame to the agent.
    ///
    /// A frame the transport cannot express is dropped with a trace:
    /// the agent's protocol layer is the only one that can say what
    /// a malformed ACP frame means, and a core frame that got here
    /// is not one it can read.
    pub fn offer(&self, frame: Value) {
        let transport = match transport_frame(frame) {
            Ok(transport) => transport,
            Err(error) => {
                warn!(%error, "the ACP frame could not be carried");
                return;
            }
        };
        if self.to_agent.unbounded_send(transport).is_err() {
            warn!("the ACP agent is gone; the frame is dropped");
        }
    }
}

/// The transport frame one core frame becomes.
fn transport_frame(frame: Value) -> Result<TransportFrame, String> {
    let object = frame
        .as_object()
        .ok_or_else(|| "an ACP frame must be a JSON object".to_string())?;
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    if let Some(method) = object.get("method").and_then(Value::as_str) {
        let method = method.to_string();
        return match object.get("id") {
            Some(id) => {
                let id = raw_id(id)?;
                RawJsonRpcMessage::request(method, params, id)
                    .map(TransportFrame::Single)
                    .map_err(|error| error.to_string())
            }
            None => RawJsonRpcMessage::notification(method, params)
                .map(TransportFrame::Single)
                .map_err(|error| error.to_string()),
        };
    }
    let id = object
        .get("id")
        .ok_or_else(|| "an ACP frame needs an id".to_string())?;
    let id = raw_id(id)?;
    let response = if let Some(result) = object.get("result") {
        RawJsonRpcResponse::Result {
            id,
            result: result.clone(),
        }
    } else {
        let error = object.get("error").ok_or_else(|| {
            "an ACP frame needs a result or an error".to_string()
        })?;
        let code =
            error.get("code").and_then(Value::as_i64).unwrap_or(0) as i32;
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the client refused the request");
        let mut raw = RawJsonRpcError::new(code, message);
        if let Some(data) = error.get("data") {
            raw = raw.data(data.clone());
        }
        RawJsonRpcResponse::Error {
            id,
            error: Box::new(raw),
        }
    };
    Ok(TransportFrame::Single(RawJsonRpcMessage::Response(
        response,
    )))
}

/// The id one core frame carries, in the transport's own spelling.
fn raw_id(id: &Value) -> Result<RawJsonRpcId, String> {
    match id {
        Value::String(text) => Ok(RawJsonRpcId::Str(text.clone())),
        Value::Number(number) => number
            .as_i64()
            .map(RawJsonRpcId::Number)
            .ok_or_else(|| "a JSON-RPC id must be an integer".to_string()),
        _ => Err("a JSON-RPC id must be a string or a number".to_string()),
    }
}

/// The core frame one transport frame becomes.
fn envelope_of(frame: TransportFrame) -> Option<Value> {
    match frame {
        TransportFrame::Single(message) => serde_json::to_value(message).ok(),
        TransportFrame::Malformed { raw, error } => {
            // The peer sent something the protocol does not allow.
            // The standard parse error is the answer, and the
            // conversation continues.
            warn!(%error, raw = %raw, "the ACP frame is malformed");
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": {
                    "code": -32700,
                    "message": "the frame is not a JSON-RPC message",
                },
            }))
        }
        // A frame shape this build does not know: the protocol grows,
        // the adapter does not. The frame is dropped rather than
        // guessed at.
        _ => None,
    }
}
