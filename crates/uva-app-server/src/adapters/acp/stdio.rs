//! The stdio transport: the ACP conversation on the process's pipes.
//!
//! An editor (Zed, a JetBrains IDE, any ACP client) launches the
//! binary and speaks the Agent Client Protocol on its stdin and
//! stdout. The SDK's own stdio transport carries the frames — one
//! JSON-RPC message per line — so the wire is the protocol's and not
//! a copy of it.
//!
//! The core's own notification fan-out has no reader in this mode:
//! the ACP surface replaces it, so those frames are drained and
//! dropped rather than piling up.

use std::sync::Arc;

use agent_client_protocol::Stdio;
use tracing::{debug, info};
use uva::core::CancelToken;

use crate::{
    adapters::acp::{agent::AcpAgent, approval},
    state::AppState,
};

/// Run the ACP conversation on the process's stdio.
pub async fn run(
    state: Arc<AppState>,
    cancel: Arc<uva::core::AtomicCancelToken>,
) -> anyhow::Result<()> {
    // The face installs the approval gate: an ACP client can answer a
    // permission question, so a risky call is held and the operator
    // decides. The handle lives as long as the face does — a
    // registration that is dropped withdraws the gate.
    let _approval = approval::install(&state);
    drain_core_frames(state.clone()).await;
    let agent = AcpAgent::new(state);
    let conversation = agent.connect(Stdio::new());
    tokio::select! {
        outcome = conversation => {
            outcome?;
        }
        () = cancel.cancelled() => {
            info!("the ACP surface is shutting down");
        }
    }
    Ok(())
}

/// Drain the core's own notification fan-out.
///
/// In this mode the core's `session/event` projection has no reader:
/// the ACP surface carries the conversation instead. The frames are
/// dropped rather than left in the channel.
async fn drain_core_frames(state: Arc<AppState>) {
    let Some(receiver) = state.rpc.take_receiver().await else {
        return;
    };
    tokio::spawn(async move {
        let mut receiver = receiver;
        while receiver.recv().await.is_some() {}
        debug!("the ACP surface drained the core's notification fan-out");
    });
}
