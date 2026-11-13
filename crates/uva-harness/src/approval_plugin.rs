//! The `approval` service and the `tool/pre-execute` gate that reads it.
//!
//! # The plugin publishes policy, the gate asks
//!
//! [`ApprovalPlugin`] provides the deployment's [`ApprovalPolicy`] as
//! the `approval` service. [`ApprovalGate`] is the harness `Gate` over
//! that policy: at [`HookEvent::BeforeTool`](uva_steering::HookEvent::BeforeTool) it returns
//! [`HookDecision::Ask`](uva_steering::HookDecision::Ask) when the policy holds the call, and
//! [`HookDecision::Allow`](uva_steering::HookDecision::Allow) otherwise. The gate does **not** decide the
//! approval, record the request, or resume the turn.
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
//! # The service value's shape
//!
//! [`ApprovalPolicy`] is `Sized`, so `apply` calls
//! `ctx.provide_arc(KEY_APPROVAL, Arc<ApprovalPolicy>)` and a consumer
//! writes `ctx.require::<ApprovalPolicy>(KEY_APPROVAL)`, which yields an
//! `Arc` pointing at the published policy.
//!
//! # The gate's risk input
//!
//! [`HookEvent::BeforeTool`](uva_steering::HookEvent::BeforeTool) carries the tool name and the call's
//! arguments, not a risk level. The host knows how a tool classifies
//! itself (`Tool::risk`, keyed by the input), so the gate takes that
//! classifier as a function instead of growing a second risk table:
//!
//! ```ignore
//! let policy = ctx.require::<ApprovalPolicy>(KEY_APPROVAL)?;
//! let gate = ApprovalGate::new(policy, workspace_hash, Arc::new(|tool, args| {
//!     registry.get(tool).map_or(Risk::Unknown, |entry| entry.risk(args))
//! }));
//! host.gate(Arc::new(gate));
//! ```
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](uva_kernel::Plugin::apply), so unloading it revokes
//! the service. A host that installed a gate over the withdrawn policy
//! keeps its own clone — the gate is installed by the host, and
//! installing it is what keeps approval requests flowing. The manifest
//! injects nothing: the policy arrives with the plugin value.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use uva_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};
use uva_steering::{HookDecision, HookEvent};

use crate::agent::{
    approval::{ApprovalPolicy, Risk},
    host::{Gate, HookContext},
};

/// The service key the `approval` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_APPROVAL: &str = "approval";

// The plugin id the `approval` manifest declares.
const APPROVAL_ID: &str = "approval";

// The gate's name in the host's diagnostics.
const GATE_NAME: &str = "approval";

/// How the gate learns one call's risk.
///
/// The host supplies the classifier — normally the `tools` registry's
/// `Tool::risk`, which reads the call's arguments — because risk is a
/// property of the tool and its input, not of the approval policy.
/// Keeping it a parameter is what stops this module from growing a
/// second risk table beside the registry's.
pub type RiskOf = Arc<dyn Fn(&str, &Value) -> Risk + Send + Sync>;

/// The `approval` kernel plugin: provides [`KEY_APPROVAL`].
///
/// Mount it with [`uva_kernel::mount_all`]. See the [module
/// docs](crate::approval_plugin) for the service's shape, the gate's
/// risk input, and the unload contract.
pub struct ApprovalPlugin {
    manifest: PluginManifest,
    policy: ApprovalPolicy,
}

impl ApprovalPlugin {
    /// Publish `policy` as the `approval` service.
    ///
    /// The policy is built by the host (the deployment default plus
    /// session-scope grants); the plugin only publishes it.
    pub fn new(policy: ApprovalPolicy) -> Self {
        let mut manifest =
            PluginManifest::new(APPROVAL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_APPROVAL.to_string());
        Self { manifest, policy }
    }

    #[must_use]
    /// Build the `tool/pre-execute` gate over this plugin's policy.
    ///
    /// `risk_of` is the host's classifier — normally the `tools`
    /// registry's `Tool::risk`, so the gate and the loop agree on what
    /// a call costs. The gate is a separate install because the host
    /// owns which agent runs under it.
    pub fn gate(
        &self,
        workspace_hash: impl Into<String>,
        risk_of: RiskOf,
    ) -> ApprovalGate {
        ApprovalGate::new(
            Arc::new(self.policy.clone()),
            workspace_hash,
            risk_of,
        )
    }
}

impl KernelPlugin for ApprovalPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        Ok(vec![ctx.provide_arc(
            KEY_APPROVAL,
            Arc::new(self.policy.clone()),
        )?])
    }
}

/// The harness gate over one [`ApprovalPolicy`].
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
