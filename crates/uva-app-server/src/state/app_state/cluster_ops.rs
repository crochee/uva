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

use uva::core::registry::Registry as _;

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
        ControlFrame::ForwardOp { key, op, model, .. } => {
            forward_op(state, key.user_id, key.session_id, op.into(), model)
                .await;
        }
        ControlFrame::CompactSession { key, .. } => {
            compact_on_owner(state, key.user_id, key.session_id).await;
        }
        ControlFrame::CancelSession { key, .. } => {
            forward_op(
                state,
                key.user_id,
                key.session_id,
                crate::session::controller::SessionOp::Cancel { reason: None },
                None,
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

/// Run one manual compaction pass on the node that owns the session.
///
/// The route on a non-owner node publishes the frame instead of
/// answering `409`: the pass rewrites the surface, and only the owner
/// may do that.
async fn compact_on_owner(
    state: &Arc<AppState>,
    user_id: String,
    session_id: String,
) {
    let Some(controller) = state
        .active_sessions
        .get(&(user_id.clone(), session_id.clone()))
        .map(|c| c.clone())
    else {
        tracing::debug!(
            session = %session_id,
            "cluster: a compact frame arrived but this node owns no \
             such controller"
        );
        return;
    };
    if controller.is_remote() {
        return;
    }
    let Some(settings) = state.default_compaction else {
        tracing::warn!(
            session = %session_id,
            "cluster: compact requested but no compaction policy is configured"
        );
        return;
    };
    let writer = state.session_manager.writer(&user_id, &session_id);
    if let Err(error) = crate::routes::follow::run_manual_compaction_within(
        state,
        &writer,
        settings,
        crate::routes::follow::COMPACTION_TIMEOUT,
    )
    .await
    {
        tracing::warn!(
            session = %session_id,
            error = ?error,
            "cluster: the forwarded compaction pass failed"
        );
    }
}

async fn forward_op(
    state: &Arc<AppState>,
    user_id: String,
    session_id: String,
    op: crate::session::controller::SessionOp,
    model: Option<String>,
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
    let provider = match model {
        Some(model) => match state.resolve_provider(Some(&model)) {
            Ok(provider) => {
                let id = state.provider_id(Some(&model)).unwrap_or_default();
                Some(
                    crate::session::controller::PinnedProvider::new(provider)
                        .with_id(id),
                )
            }
            Err(error) => {
                tracing::warn!(
                    session = ?key,
                    model,
                    %error,
                    "cluster: model_unavailable for a forwarded op"
                );
                return;
            }
        },
        None => None,
    };
    if let Err(e) = controller.submit_with_provider(op, provider).await {
        tracing::warn!(
            session = ?key,
            error = %e,
            "cluster: forwarded op delivery failed"
        );
    }
}

async fn apply_tool_mutation(state: &Arc<AppState>, mutation: ToolMutation) {
    let registry = state.tool_registry.as_ref();
    match mutation {
        ToolMutation::RegisterDynamic {
            name,
            description,
            parameters,
        } => {
            let entry = uva::tool::ToolEntry::dynamic(
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
