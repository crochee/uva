//! The headless control surface for delegated children.
//!
//! A `task` call opens a child session and drives it. The model can
//! see and steer those children through the `agents_*` tools; a
//! **host** had no door at all — a child that ran away could not be
//! listed, stopped or redirected by the client that asked for the
//! work.
//!
//! These three functions are that door, and they are the same
//! operations the model has:
//!
//! | method | what it does |
//! |---|---|
//! | `subagents/list` | the roster of one session's children |
//! | `subagents/cancel` | hard-stop one child (terminal) |
//! | `subagents/steer` | deliver a message to a running child |
//!
//! The machinery is the task crate's control surface — the same
//! handle table the `task` tool fills. Nothing here keeps a second
//! truth: the roster is the table, and the table is reconciled with
//! the durable sessions on every read (a child a previous process
//! dispatched shows up as `parked` instead of vanishing).
//!
//! # Cancellation is idempotent
//!
//! Stopping a child that is already gone, unknown, or finished is
//! **not** an error: the answer says `cancelled: false` and the
//! caller moves on. A host that cancels twice (a retry, a second
//! client) must not see a failure.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uva::tool_task::{ControlError, ControlSurface};

use crate::{rpc::RpcError, state::AppState};

/// One child of a session, as the roster reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentRow {
    /// The child's session id — the handle a caller names.
    pub id: String,
    /// The peer agent the child runs as.
    pub name: String,
    /// Where the child is in its lifecycle.
    pub status: String,
    /// One line about what the child is doing now.
    pub activity: String,
}

/// The roster of one session's children.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentListResponse {
    /// The session the roster belongs to.
    pub session_id: String,
    /// The children, in the table's order.
    pub subagents: Vec<SubagentRow>,
}

/// What a cancellation did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentCancelResponse {
    /// The child the caller named.
    pub child_id: String,
    /// `true` when this call stopped a live child.
    pub cancelled: bool,
}

/// What a steer did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentSteerResponse {
    /// The child the caller named.
    pub child_id: String,
    /// `true` when the message reached the child's round.
    pub delivered: bool,
}

/// The parameters of `subagents/list`.
#[derive(Debug, Clone, Deserialize, validator::Validate)]
pub struct SubagentListParams {
    /// The session whose children are listed.
    pub session_id: String,
}

/// The parameters of `subagents/cancel`.
#[derive(Debug, Clone, Deserialize, validator::Validate)]
pub struct SubagentCancelParams {
    /// The session the child belongs to.
    pub session_id: String,
    /// The child's session id.
    pub child_id: String,
}

/// The parameters of `subagents/steer`.
#[derive(Debug, Clone, Deserialize, validator::Validate)]
pub struct SubagentSteerParams {
    /// The session the child belongs to.
    pub session_id: String,
    /// The child's session id.
    pub child_id: String,
    /// The message the child reads as its user.
    pub message: String,
}

/// The control surface, when the `tool-task` row is mounted.
///
/// A deployment without that row has no children to control, so the
/// methods answer "no control surface" rather than pretending.
fn control_surface(
    state: &Arc<AppState>,
) -> Result<Arc<ControlSurface>, RpcError> {
    state.control_surface().ok_or_else(|| {
        RpcError::application(
            "subagents_unavailable",
            "the tool-task row is not mounted; this deployment has no \
             delegated children to control",
        )
    })
}

/// `subagents/list` — the roster of one session's children.
pub async fn list_subagents(
    state: Arc<AppState>,
    params: SubagentListParams,
) -> Result<SubagentListResponse, RpcError> {
    let surface = control_surface(&state)?;
    let parent = params.session_id.as_str();
    // The lifecycle is advanced before the roster is read: a child
    // idle past the TTL is parked by now, so the roster reports the
    // state the caller would meet a moment later.
    surface.tick();
    // The table reconciles itself with the durable sessions first, so
    // a child a previous process dispatched is `parked` rather than
    // missing.
    surface.refresh_roster(parent).await;
    let rows = surface.registry().list(parent);
    let subagents = rows
        .into_iter()
        .map(|row| SubagentRow {
            id: row.id,
            name: row.name,
            status: row.status.as_str().to_string(),
            activity: row.activity,
        })
        .collect();
    Ok(SubagentListResponse {
        session_id: params.session_id,
        subagents,
    })
}

/// `subagents/cancel` — hard-stop one child.
///
/// The terminal state: the child's handles are released and it will
/// never run another round. A child that is already gone, unknown or
/// finished answers `cancelled: false` — the call is idempotent.
pub async fn cancel_subagent(
    state: Arc<AppState>,
    params: SubagentCancelParams,
) -> Result<SubagentCancelResponse, RpcError> {
    let surface = control_surface(&state)?;
    let parent = params.session_id.as_str();
    let cancelled = match surface.kill(parent, &params.child_id) {
        Ok(()) => true,
        // An unknown child, or one that is already terminal, is not a
        // failure: the caller wanted it stopped, and it is.
        Err(ControlError::UnknownChild(_)) => false,
        Err(ControlError::Rejected(
            uva::tool_task::TransitionError::Aborted { .. },
        )) => false,
        Err(error) => {
            return Err(RpcError::from(uva::core::Error::validation(format!(
                "the sub-agent could not be cancelled: {error}"
            ))));
        }
    };
    Ok(SubagentCancelResponse {
        child_id: params.child_id,
        cancelled,
    })
}

/// `subagents/steer` — deliver a message to a running child.
///
/// The message is the child's **user** turn: it is written to the
/// child's own log before it reaches the round's inbox, so the
/// transcript and the run agree on what the child was told.
pub async fn steer_subagent(
    state: Arc<AppState>,
    params: SubagentSteerParams,
) -> Result<SubagentSteerResponse, RpcError> {
    let surface = control_surface(&state)?;
    let parent = params.session_id.as_str();
    let message = params.message.trim();
    if message.is_empty() {
        return Err(RpcError::from(uva::core::Error::validation(
            "the steer message must not be empty",
        )));
    }
    // The steer branch only: a message that *starts* a round is the
    // model's business (the `agents_message` tool), and a host that
    // wants that can ask for a follow-up turn of its own.
    let cancel = uva::core::AtomicCancelToken::shared();
    let target = match surface.registry().message_target(
        parent,
        &params.child_id,
        cancel,
    ) {
        Ok(uva::tool_task::MessageTarget::Steer(target)) => target,
        Ok(uva::tool_task::MessageTarget::FollowUp(_)) => {
            return Err(RpcError::application(
                "subagent_not_running",
                format!(
                    "the sub-agent '{}' is not running a round; a steer \
                     needs a live run",
                    params.child_id
                ),
            ));
        }
        Ok(uva::tool_task::MessageTarget::ColdWake) => {
            return Err(RpcError::application(
                "subagent_not_running",
                format!(
                    "the sub-agent '{}' is parked; wake it with a follow-up \
                     turn instead",
                    params.child_id
                ),
            ));
        }
        Err(ControlError::UnknownChild(child)) => {
            return Err(RpcError::application(
                "subagent_unknown",
                format!("no sub-agent '{child}' in this session"),
            ));
        }
        Err(error) => {
            return Err(RpcError::from(uva::core::Error::validation(format!(
                "the sub-agent could not be steered: {error}"
            ))));
        }
    };
    let delivered = match uva::tool_task::deliver_steer(target, message).await {
        Ok(()) => true,
        Err(error) => {
            return Err(RpcError::from(uva::core::Error::session(format!(
                "the steer message could not be delivered: {error}"
            ))));
        }
    };
    Ok(SubagentSteerResponse {
        child_id: params.child_id,
        delivered,
    })
}
