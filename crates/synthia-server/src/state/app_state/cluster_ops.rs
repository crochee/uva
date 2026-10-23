//! Process-wide consumer of the cluster control channel.
//!
//! One task per node drains [`ControlFrame`]s and applies them
//! locally:
//!
//! - `ForwardOp` / `CancelSession` → the local controller that owns
//!   the addressed session (a remote handle published them);
//! - `InvalidateTools` → the tool registry, so a tool registered on
//!   node A becomes callable on node B;
//! - `SetDefaultAgent` → the boot-resolved default name (no in-tree
//!   route mutates it at runtime today, but a peer's frame must still
//!   land here once one does).
//!
//! Frames this node published are skipped (`origin == node_id`): the
//! local mutation already happened, and Redis delivers to the
//! publisher too. Applying them twice would be harmless for the
//! registry mutations (idempotent insert/delete) but needlessly
//! re-forwards ops.

use std::sync::Arc;

use synthia::core::registry::Registry as _;

use crate::{
    cluster::{ControlFrame, ToolMutation},
    state::AppState,
};

/// Spawn the node's control consumer. Called once, after the state
/// handle exists (the tools and the controller cache are reached
/// through it).
pub(crate) fn spawn_control_dispatcher(state: Arc<AppState>) {
    tokio::spawn(async move {
        let bus = Arc::clone(&state.cluster.bus);
        let mut rx = match bus.subscribe_control().await {
            Ok(rx) => rx,
            Err(e) => {
                tracing::warn!(error = %e, "cluster: control subscribe failed");
                return;
            }
        };
        while let Some(frame) = rx.recv().await {
            if frame.origin() == state.cluster.node_id {
                continue;
            }
            apply(&state, frame).await;
        }
    });
}

async fn apply(state: &Arc<AppState>, frame: ControlFrame) {
    match frame {
        ControlFrame::ForwardOp { key, op, .. } => {
            forward_op(state, key.user_id, key.session_id, op.into()).await;
        }
        ControlFrame::CancelSession { key, .. } => {
            forward_op(
                state,
                key.user_id,
                key.session_id,
                crate::session::controller::SessionOp::Cancel { reason: None },
            )
            .await;
        }
        ControlFrame::InvalidateTools { mutation, .. } => {
            apply_tool_mutation(state, mutation).await;
        }
        ControlFrame::SetDefaultAgent { name, .. } => {
            *state.default_agent_name.write() = Some(name);
        }
    }
}

async fn forward_op(
    state: &Arc<AppState>,
    user_id: String,
    session_id: String,
    op: crate::session::controller::SessionOp,
) {
    let key = (user_id, session_id);
    let controller = state.active_sessions.get(&key).map(|c| c.clone());
    let Some(controller) = controller else {
        tracing::debug!(
            session = ?key,
            "cluster: forwarded op arrived but this node owns no such controller"
        );
        return;
    };
    if let Err(e) = controller.submit(op).await {
        tracing::warn!(
            session = ?key,
            error = %e,
            "cluster: forwarded op delivery failed"
        );
    }
}

async fn apply_tool_mutation(state: &Arc<AppState>, mutation: ToolMutation) {
    let registry = state.tool_registry.write().await;
    match mutation {
        ToolMutation::RegisterDynamic {
            name,
            description,
            parameters,
        } => {
            let entry = synthia::tool::ToolEntry::dynamic(
                name.clone(),
                description,
                parameters,
            );
            if let Err(e) = registry.put(entry).await {
                tracing::warn!(
                    tool = %name,
                    error = %e,
                    "cluster: failed to apply a peer's tool registration"
                );
            }
        }
        ToolMutation::Remove { name } => {
            if let Err(e) = registry.delete(&name).await {
                tracing::debug!(
                    tool = %name,
                    error = %e,
                    "cluster: peer tool removal did not apply"
                );
            }
        }
    }
}
