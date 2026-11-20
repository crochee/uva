//! The `tool/pre-execute` gate over the `approval` service.
//!
//! # The plugin publishes policy, the gate asks
//!
//! [`ApprovalPlugin`] (in `uva-approval`) provides the deployment's
//! [`ApprovalPolicy`] as the `approval` service. [`ApprovalGate`] is
//! the harness [`Gate`] over
//! that policy: at
//! [`HookEvent::BeforeTool`](uva_steering::HookEvent::BeforeTool) it returns
//! [`HookDecision::Ask`](uva_steering::HookDecision::Ask) when the policy holds the call, and
//! [`HookDecision::Allow`](uva_steering::HookDecision::Allow) otherwise. The gate does **not** decide the
//! approval, record the request, or resume the turn.
//!
//! # A thin adapter, and why it is still here
//!
//! The gate is one `requires_approval` lookup dressed in the harness
//! `Gate` trait. It stays in the harness only because `Gate`'s
//! [`HookContext`] still carries the harness-private `EventSink`;
//! when the interception contract moves under `uva-approval`, the
//! gate moves with it and this module disappears.
//!
//! # The durable half stays with the controller
//!
//! A suspension is log-derived, not memory-derived: the loop records
//! `approval_asked`, writes the turn's `turn_suspended` row and a
//! [`ResumeToken`], and the decision comes back through
//! `sessions/resume` as [`ResumeDecision`]s. That path — including a
//! rejected / timed-out interrupt that writes an `is_error` tool result
//! without dispatching the call — is pinned by
//! `crate::agent::re_act::tests::lifecycle::approval_resume::a_rejected_interrupt_writes_an_is_error_result`
//! (and its siblings). This module deliberately adds no second source
//! of truth: a oneshot living only in memory would not survive the
//! restart the spec requires.
//!
//! # The gate's risk input
//!
//! [`HookEvent::BeforeTool`](uva_steering::HookEvent::BeforeTool) carries the tool name and the call's
//! arguments, not a risk level. The host knows how a tool classifies
//! itself (`Tool::risk`, keyed by the input), so the gate takes that
//! classifier as a function — [`RiskOf`] —
//! instead of growing a second risk table:
//!
//! ```ignore
//! let policy = ctx.require::<ApprovalPolicy>(KEY_APPROVAL)?;
//! let gate = ApprovalGate::new(policy, workspace_hash, Arc::new(|tool, args| {
//!     registry.get(tool).map_or(Risk::Unknown, |entry| entry.risk(args))
//! }));
//! // The deployment's one table: the host carries it, `hooks`
//! // publishes it.
//! hooks.register_gate_at(
//!     uva_hook::MountPoint::ToolPre,
//!     Arc::new(gate),
//!     uva_hook::RegistrationOrigin::InProcess,
//! );
//! ```

use std::sync::Arc;

use async_trait::async_trait;
// The approval vocabulary's harness address: `uva-approval` owns the
// names, the crate root re-exports them through this module, so the
// frozen surface keeps its paths while the defining crate sits one
// layer below the harness.
pub use uva_approval::{
    ApprovalMode,
    ApprovalPlugin,
    ApprovalPolicy,
    ApprovalRequest,
    GrantKey,
    GrantScope,
    KEY_APPROVAL,
    ResumeCoverageError,
    ResumeDecision,
    ResumeStatus,
    ResumeToken,
    RiskOf,
    Suspension,
    align_decisions,
    check_coverage,
    latest_request_snapshot_hash,
    may_retry,
    open_requests,
    suspended_segment,
    workspace_hash,
};
use uva_steering::{HookDecision, HookEvent};

use crate::agent::host::{Gate, HookContext};

// The gate's name in the host's diagnostics.
const GATE_NAME: &str = "approval";

/// The harness gate over one
/// [`ApprovalPolicy`].
///
/// At `tool/pre-execute` it asks when the policy holds the call and
/// allows the rest. Every other event is a no-op: the approval gate
/// only governs tool dispatch.
pub struct ApprovalGate {
    policy: Arc<ApprovalPolicy>,
    workspace_hash: String,
    risk_of: RiskOf,
}

impl ApprovalGate {
    #[must_use]
    /// Build a gate over `policy` for one workspace.
    ///
    /// `workspace_hash` is the same digest the approval policy's grants
    /// are keyed by, so a grant recorded for this workspace applies.
    /// `risk_of` classifies one call; see the [module
    /// docs](crate::approval_plugin) for where a host gets it.
    pub fn new(
        policy: Arc<ApprovalPolicy>,
        workspace_hash: impl Into<String>,
        risk_of: RiskOf,
    ) -> Self {
        Self {
            policy,
            workspace_hash: workspace_hash.into(),
            risk_of,
        }
    }

    // The decision one event earns from this policy.
    fn decision(&self, event: &HookEvent) -> HookDecision {
        let HookEvent::BeforeTool {
            tool_name,
            arguments,
            ..
        } = event
        else {
            return HookDecision::Allow;
        };
        let risk = (self.risk_of)(tool_name, arguments);
        if self
            .policy
            .requires_approval(tool_name, risk, &self.workspace_hash)
        {
            HookDecision::Ask {
                reason: format!(
                    "tool `{tool_name}` requires approval (risk: {})",
                    risk.as_str()
                ),
            }
        } else {
            HookDecision::Allow
        }
    }
}

#[async_trait]
impl Gate for ApprovalGate {
    fn name(&self) -> &str {
        GATE_NAME
    }

    async fn check(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        Ok(self.decision(event))
    }
}
