//! The `approval` kernel service: the plugin that publishes the
//! deployment's policy, and the risk classifier a gate over that
//! policy takes.
//!
//! # The plugin publishes policy, the host asks
//!
//! [`ApprovalPlugin`] provides the deployment's
//! [`ApprovalPolicy`](crate::ApprovalPolicy) as the `approval`
//! service. The gate that turns a held call into an `Ask` decision
//! is **not** here: it implements the harness `Gate` trait, whose
//! context still carries a harness-private sink, so it stays in
//! `uva-harness` until the interception contract moves under this
//! crate and the gate moves with it.
//!
//! # The durable half stays with the controller
//!
//! A suspension is log-derived, not memory-derived: the loop records
//! `approval_asked`, writes the turn's `turn_suspended` row and a
//! [`ResumeToken`](crate::ResumeToken), and the decision comes back
//! through `sessions/resume` as
//! [`ResumeDecision`](crate::ResumeDecision)s. This module
//! deliberately adds no second source of truth: a oneshot living
//! only in memory would not survive the restart the spec requires.
//!
//! # The service value's shape
//!
//! [`ApprovalPolicy`](crate::ApprovalPolicy) is `Sized`, so `apply`
//! calls `ctx.provide_arc(KEY_APPROVAL, Arc<ApprovalPolicy>)` and a
//! consumer writes `ctx.require::<ApprovalPolicy>(KEY_APPROVAL)`,
//! which yields an `Arc` pointing at the published policy.
//!
//! # The gate's risk input
//!
//! `HookEvent::BeforeTool` carries the tool name and the call's
//! arguments, not a risk level. The host knows how a tool classifies
//! itself (`Tool::risk`, keyed by the input), so a gate over the
//! policy takes that classifier as a function — [`RiskOf`] — instead
//! of the approval crate growing a second risk table beside the
//! registry's.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](uva_kernel::Plugin::apply), so unloading it revokes
//! the service. A host that installed a gate over the withdrawn
//! policy keeps its own clone — the gate is installed by the host,
//! and installing it is what keeps approval requests flowing. The
//! manifest injects nothing: the policy arrives with the plugin
//! value.

use std::sync::Arc;

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

use crate::approval::{ApprovalPolicy, Risk};

/// The service key the `approval` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_APPROVAL: &str = "approval";

// The plugin id the `approval` manifest declares.
const APPROVAL_ID: &str = "approval";

/// How a gate over the approval policy learns one call's risk.
///
/// The host supplies the classifier — normally the `tools` registry's
/// `Tool::risk`, which reads the call's arguments — because risk is a
/// property of the tool and its input, not of the approval policy.
/// Keeping it a parameter is what stops the approval crate from
/// growing a second risk table beside the registry's.
pub type RiskOf = Arc<dyn Fn(&str, &Value) -> Risk + Send + Sync>;

/// The `approval` kernel plugin: provides [`KEY_APPROVAL`].
///
/// Mount it with [`uva_kernel::mount_all`]. See the module docs for
/// the service's shape, the gate's risk input, and the unload
/// contract.
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
