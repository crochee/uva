//! The approval gate the ACP face installs.
//!
//! The deployment's `approval` row publishes the policy — the stance
//! that says which calls are held — but nothing consumed it: the
//! policy was a service nobody asked, so a high-risk call was never
//! held and the operator never decided.
//!
//! The ACP face is the surface whose clients can answer a permission
//! question (the protocol's `session/request_permission` is exactly
//! that question), so the face installs the gate over the shared
//! interception table. From then on a held call suspends the turn,
//! the operator decides, and the decision lands in the durable
//! approval rows — the same rows the deployment's own surfaces read.
//!
//! The other surfaces keep their current stance: the gate is
//! installed by the face, not by the boot, so a deployment that does
//! not want calls held does not get them held.

use std::sync::Arc;

use uva::{
    harness::{ApprovalGate, KEY_APPROVAL, workspace_hash},
    hook::{HookRegistration, MountPoint, RegistrationOrigin},
    tool::Risk,
};

use crate::state::AppState;

/// Install the deployment's approval gate over the shared
/// interception table.
///
/// Returns the registration handle: a registration withdraws its
/// entry when it is dropped, so the caller keeps it for as long as
/// the gate should stand. `None` when the deployment published no
/// approval policy — a deployment without the `approval` row holds
/// nothing, and the face says so rather than inventing a stance.
pub fn install(state: &AppState) -> Option<HookRegistration> {
    let policy = state
        .assembly
        .service::<uva::approval::ApprovalPolicy>(KEY_APPROVAL)?;
    let registry = Arc::clone(&state.tool_registry);
    // The risk of a call is the tool's own answer: the tool knows
    // which of its calls are dangerous (`rm` is not `ls`), and the
    // gate asks the tool rather than a second table here.
    let risk_of: uva::approval::RiskOf = Arc::new(move |tool, arguments| {
        registry
            .get(tool)
            .map(|entry| entry.tool_instance().risk(arguments))
            .unwrap_or(Risk::Unknown)
    });
    let gate = ApprovalGate::new(
        policy,
        workspace_hash(&state.workspace_root),
        risk_of,
    );
    Some(state.plugin_host.hook_registry().register_gate_at(
        MountPoint::ToolPre,
        Arc::new(gate),
        RegistrationOrigin::InProcess,
    ))
}
