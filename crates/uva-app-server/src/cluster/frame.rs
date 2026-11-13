//! The cluster bus: leases + frame fan-out for a multi-node
//! deployment.
//!
//! # Delivery contract
//!
//! The bus is **best-effort** for every frame, on both backends:
//!
//! - the durable truth is the shared workspace filesystem (session
//!   JSONL, evals/, schedules/);
//! - session frames may be dropped by a slow subscriber (the local
//!   broadcast logs the gap; Redis pub/sub simply does not redeliver);
//!   an SSE client recovers with its `from` cursor, which re-reads the
//!   JSONL from disk;
//! - control frames (register-tool, set-default-agent, forward-op) are
//!   single-shot: a node that misses one keeps its previous local view
//!   until the next mutation or restart. That is the accepted trade for
//!   this round — no streams/ack machinery.
//!
//! Leases are the one **exclusive** primitive: a session's controller
//! only runs on the node holding
//! `"{namespace}:lease:session:{user}:{session}"`, renewed at TTL/3. A
//! crashed owner's lease expires and the next request on any node
//! takes over and rebuilds the session from disk (the existing
//! `get_or_create_session_controller` restore path).

use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::session::controller::SessionOp;

/// A node's stable identity (config `[cluster].node_id`, else a boot
/// ULID).
pub type NodeId = String;

/// One session's address across the cluster.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionKey {
    pub user_id: String,
    pub session_id: String,
}

impl SessionKey {
    #[must_use]
    pub fn new(
        user_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            session_id: session_id.into(),
        }
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.user_id, self.session_id)
    }
}

/// One frame on a session channel: a run event destined for SSE
/// subscribers on any node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionFrame {
    /// The frame's publisher, so a node never re-processes its own
    /// publish (Redis delivers to the publisher too).
    pub origin: NodeId,
    /// `Some(stream_index)` for a durable row; `None` for an ephemeral
    /// event (token delta, warning) that only lives on the live tail.
    /// Consumers drop `Some(cursor)` frames at or below the resume
    /// cursor they already replayed from disk.
    pub cursor: Option<u64>,
    /// The wire payload: the same `serde_json::Value` the durable log
    /// stored / the SSE frame serialises.
    pub payload: serde_json::Value,
}

/// A control-plane frame: addressed either to one session or to every
/// node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ControlFrame {
    /// Deliver `op` to whichever node owns the session.
    ForwardOp {
        origin: NodeId,
        key: SessionKey,
        op: SessionOpWire,
        /// The model the forwarded turn was submitted under, when
        /// the caller selected one. The owner resolves it against
        /// its own catalogue; a pin is a fact about the turn, not
        /// about the node that accepted it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// Abort the session's in-flight run on its owner.
    CancelSession { origin: NodeId, key: SessionKey },
    /// Run one manual compaction pass on the owner. A pass rewrites
    /// the surface a live request is built from, so only the node
    /// that owns the session may run it.
    CompactSession { origin: NodeId, key: SessionKey },
    /// A runtime registry mutation another node must apply locally.
    InvalidateTools {
        origin: NodeId,
        mutation: ToolMutation,
    },
    /// The deployment's default agent moved.
    SetDefaultAgent { origin: NodeId, name: String },
}

impl ControlFrame {
    #[must_use]
    pub fn origin(&self) -> &str {
        match self {
            Self::ForwardOp { origin, .. }
            | Self::CancelSession { origin, .. }
            | Self::CompactSession { origin, .. }
            | Self::InvalidateTools { origin, .. }
            | Self::SetDefaultAgent { origin, .. } => origin,
        }
    }
}

/// A replayable tool-registry mutation (`ToolEntry::dynamic` payload is
/// complete, so a peer can apply it verbatim).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ToolMutation {
    /// Register (or replace) a dynamic tool with its full schema.
    RegisterDynamic {
        name: String,
        description: String,
        parameters: serde_json::Value,
    },
    /// Drop a tool by name.
    Remove { name: String },
}

/// Serde mirror of [`SessionOp`].
///
/// The in-process enum is not `Serialize` (it carries runtime handles
/// on some paths in future rounds); this is the wire form the bus and
/// the remote handle exchange. `From` conversions are total in both
/// directions — a `Shutdown` forward becomes a `Cancel` of the
/// session's run, because a shutdown of the owner's whole controller is
/// a node-local lifecycle decision, never a cross-node command.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SessionOpWire {
    Prompt {
        content: String,
        priority: u8,
    },
    PromptMulti {
        parts: Vec<uva::provider::ContentPart>,
        agent_name: Option<String>,
        priority: u8,
    },
    Rerun {
        parts: Vec<uva::provider::ContentPart>,
        agent_name: Option<String>,
        priority: u8,
    },
    Feedback {
        message_id: String,
        thumbs_up: bool,
    },
    Steer {
        content: String,
        priority: u8,
    },
    /// Cancel *this* run (the only meaning a wire cancel has).
    Cancel {
        reason: Option<String>,
    },
    /// Continue a suspended run. The token, the decisions and the
    /// drift flag are the route's validated input: the owner runs
    /// the same validation against the shared log, so a stale token
    /// fails there exactly as it would locally.
    Resume {
        token: uva::harness::ResumeToken,
        decisions: Vec<uva::harness::ResumeDecision>,
        allow_drift: bool,
    },
}

/// A boxed receiver of bus frames. Both backends hand back an
/// `mpsc::Receiver`, so consumers have exactly one shape to await.
pub type FrameReceiver<T> = mpsc::Receiver<T>;

impl SessionOpWire {
    /// Encode an in-process op for the bus. `Shutdown` is a node-local
    /// lifecycle decision (the owner's whole controller stops), so it
    /// has no wire form and returns `None`.
    #[must_use]
    pub fn from_op(op: &crate::session::controller::SessionOp) -> Option<Self> {
        use crate::session::controller::SessionOp;
        match op {
            SessionOp::Prompt { content, priority } => Some(Self::Prompt {
                content: content.clone(),
                priority: *priority,
            }),
            SessionOp::PromptMulti {
                parts,
                agent_name,
                priority,
            } => Some(Self::PromptMulti {
                parts: parts.clone(),
                agent_name: agent_name.clone(),
                priority: *priority,
            }),
            SessionOp::Rerun {
                parts,
                agent_name,
                priority,
            } => Some(Self::Rerun {
                parts: parts.clone(),
                agent_name: agent_name.clone(),
                priority: *priority,
            }),
            SessionOp::Feedback {
                message_id,
                thumbs_up,
            } => Some(Self::Feedback {
                message_id: message_id.clone(),
                thumbs_up: *thumbs_up,
            }),
            SessionOp::Steer { content, priority } => Some(Self::Steer {
                content: content.clone(),
                priority: *priority,
            }),
            SessionOp::Cancel { reason } => Some(Self::Cancel {
                reason: reason.clone(),
            }),
            SessionOp::Resume {
                token,
                decisions,
                allow_drift,
            } => Some(Self::Resume {
                token: token.clone(),
                decisions: decisions.clone(),
                allow_drift: *allow_drift,
            }),
            // The turn a `StartPending` starts is a row the owner's log
            // already holds; a forwarder has no such slot to stir.
            SessionOp::StartPending => None,
            SessionOp::Shutdown => None,
        }
    }
}

impl From<SessionOpWire> for crate::session::controller::SessionOp {
    fn from(wire: SessionOpWire) -> Self {
        match wire {
            SessionOpWire::Prompt { content, priority } => {
                SessionOp::Prompt { content, priority }
            }
            SessionOpWire::PromptMulti {
                parts,
                agent_name,
                priority,
            } => SessionOp::PromptMulti {
                parts,
                agent_name,
                priority,
            },
            SessionOpWire::Rerun {
                parts,
                agent_name,
                priority,
            } => SessionOp::Rerun {
                parts,
                agent_name,
                priority,
            },
            SessionOpWire::Feedback {
                message_id,
                thumbs_up,
            } => SessionOp::Feedback {
                message_id,
                thumbs_up,
            },
            SessionOpWire::Steer { content, priority } => {
                SessionOp::Steer { content, priority }
            }
            SessionOpWire::Cancel { reason } => SessionOp::Cancel { reason },
            SessionOpWire::Resume {
                token,
                decisions,
                allow_drift,
            } => SessionOp::Resume {
                token,
                decisions,
                allow_drift,
            },
        }
    }
}

/// Errors a bus backend can return.
///
/// The server is an application crate (AGENTS.md §3.1: apps use
/// `anyhow`), so this enum carries its own `Display`/`Error` impl
/// instead of pulling in `thiserror`; it stays a typed enum because
/// callers branch on "unavailable" vs "backend failure".
#[derive(Debug)]
#[non_exhaustive]
pub enum ClusterError {
    /// The backend could not be reached or the command failed.
    Backend(String),
    /// The requested backend is not compiled in (`cluster-redis`).
    Unavailable(String),
}

impl std::fmt::Display for ClusterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(message) => write!(f, "cluster bus: {message}"),
            Self::Unavailable(message) => {
                write!(f, "cluster backend unavailable: {message}")
            }
        }
    }
}

impl std::error::Error for ClusterError {}

/// Lease + frame operations every backend implements.
#[async_trait::async_trait]
pub trait SessionBus: Send + Sync + fmt::Debug {
    /// Try to take `lease` for `node_id`. `true` = acquired (or
    /// already held by this node), `false` = held elsewhere and unexpired.
    async fn try_acquire(
        &self,
        lease: &str,
        node_id: &str,
        ttl: Duration,
    ) -> Result<bool, ClusterError>;

    /// Extend a lease this node already holds. An expired/foreign
    /// lease is an error — the caller has lost ownership and must
    /// stop.
    async fn renew(
        &self,
        lease: &str,
        node_id: &str,
        ttl: Duration,
    ) -> Result<(), ClusterError>;

    /// Release a lease this node holds (best effort).
    async fn release(
        &self,
        lease: &str,
        node_id: &str,
    ) -> Result<(), ClusterError>;

    /// Publish one session frame to every subscriber of `key`.
    async fn publish_session(
        &self,
        key: &SessionKey,
        frame: SessionFrame,
    ) -> Result<(), ClusterError>;

    /// Subscribe to a session's frames. Every node may subscribe; the
    /// owner publishes.
    async fn subscribe_session(
        &self,
        key: &SessionKey,
    ) -> Result<FrameReceiver<SessionFrame>, ClusterError>;

    /// Publish a control frame to every node (including this one —
    /// consumers skip their own `origin`).
    async fn publish_control(
        &self,
        frame: ControlFrame,
    ) -> Result<(), ClusterError>;

    /// Subscribe to the control channel.
    async fn subscribe_control(
        &self,
    ) -> Result<FrameReceiver<ControlFrame>, ClusterError>;
}

/// `"{ns}:lease:session:{user}:{session}"`.
#[must_use]
pub fn session_lease(namespace: &str, key: &SessionKey) -> String {
    format!("{namespace}:lease:session:{}", key)
}

/// `"{ns}:lease:leader:scheduler"` — the background tick's leader
/// lease. The node that holds it is the only one that fires due jobs;
/// every other node waits its turn, so a job fires once across the
/// deployment.
#[must_use]
pub fn scheduler_leader_lease(namespace: &str) -> String {
    format!("{namespace}:lease:leader:scheduler")
}
