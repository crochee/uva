//! Risk-graded admission: allow a tool call, or block it above a risk
//! threshold.
//!
//! [`TrafficLight`] is a [`Gate`]. On `tool/pre-execute` it asks the
//! tool registry for the tool's own [`uva_tool::Tool::risk`] (which may depend
//! on the call's arguments) and compares it with a [`TrafficPolicy`]:
//!
//! - a risk the policy does not block → `Allow`;
//! - a blocked risk → `Block`, which short-circuits the waterfall.
//!
//! The agent loop turns that veto into the durable `hook_block` row
//! (the gate's name, the reason, the tool and the call id), so a
//! reloaded session still says who refused what.
//!
//! The default policy blocks [`Risk::High`] and lets [`Risk::Low`] and
//! [`Risk::Unknown`] through. Blocking an unknown tool by default
//! would make every unannotated third-party tool unusable; a
//! deployment that wants fail-closed admission passes
//! `TrafficPolicy::blocking([Risk::High, Risk::Unknown])`.
//!
//! ```rust,ignore
//! use std::sync::Arc;
//!
//! use uva_hook::{HookRegistry, MountPoint, RegistrationOrigin};
//! use uva_hook_plugins::traffic_light::{TrafficLight, TrafficPolicy};
//! use uva_tool::{Risk, registry::ToolRegistry};
//!
//! let tools = Arc::new(ToolRegistry::new());
//! let gate = TrafficLight::with_policy(
//!     tools,
//!     TrafficPolicy::blocking([Risk::High, Risk::Unknown]),
//! );
//! let registry = HookRegistry::new();
//! let _registration = registry.register_gate_at(
//!     MountPoint::ToolPre,
//!     Arc::new(gate),
//!     RegistrationOrigin::InProcess,
//! );
//! ```

use std::sync::Arc;

use uva_hook::{Gate, HookContext, HookDecision, HookEvent};
use uva_tool::{Risk, registry::ToolRegistry};

#[cfg(test)]
mod tests;

/// Which risk levels the traffic light refuses.
///
/// A policy is a block list, not a threshold: [`Risk`] has no total
/// order (`Unknown` is not "between" low and high), so naming the
/// refused levels is the honest shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrafficPolicy {
    blocked: Vec<Risk>,
}

impl TrafficPolicy {
    /// Build a policy from the risk levels it blocks.
    #[must_use]
    pub fn blocking(risks: impl IntoIterator<Item = Risk>) -> Self {
        Self {
            blocked: risks.into_iter().collect(),
        }
    }

    /// Does this policy refuse `risk`?
    #[must_use]
    pub fn blocks(&self, risk: Risk) -> bool {
        self.blocked.contains(&risk)
    }

    /// The blocked levels, in the order they were configured.
    #[must_use]
    pub fn blocked(&self) -> &[Risk] {
        &self.blocked
    }
}

impl Default for TrafficPolicy {
    /// Block [`Risk::High`]; low and unknown calls pass.
    fn default() -> Self {
        Self::blocking([Risk::High])
    }
}

/// A [`Gate`] that blocks tool calls by the tool's declared risk.
///
/// The registry lookup happens per call, so a tool whose risk depends
/// on its arguments (a shell that may run `ls` or `rm`) is judged on
/// the arguments of *this* call, not on a static name table.
pub struct TrafficLight {
    registry: Arc<ToolRegistry>,
    policy: TrafficPolicy,
}

impl TrafficLight {
    /// Build a traffic light with [`TrafficPolicy::default`].
    #[must_use]
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self::with_policy(registry, TrafficPolicy::default())
    }

    /// Build a traffic light with an explicit policy.
    #[must_use]
    pub fn with_policy(
        registry: Arc<ToolRegistry>,
        policy: TrafficPolicy,
    ) -> Self {
        Self { registry, policy }
    }

    /// The policy this gate applies.
    #[must_use]
    pub fn policy(&self) -> &TrafficPolicy {
        &self.policy
    }

    /// The risk of one call: the registered tool's own answer for
    /// its arguments, or [`Risk::Unknown`] when no tool by that name
    /// is registered.
    #[must_use]
    pub fn risk_of(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Risk {
        self.registry
            .get(tool_name)
            .map(|entry| entry.tool_instance().risk(arguments))
            .unwrap_or(Risk::Unknown)
    }
}

#[async_trait::async_trait]
impl Gate for TrafficLight {
    fn name(&self) -> &str {
        "traffic-light"
    }

    async fn check(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        let HookEvent::BeforeTool {
            tool_name,
            arguments,
            ..
        } = event
        else {
            return Ok(HookDecision::Allow);
        };
        let risk = self.risk_of(tool_name, arguments);
        if self.policy.blocks(risk) {
            Ok(HookDecision::Block {
                reason: format!(
                    "traffic-light: `{tool_name}` is {} risk",
                    risk.as_str()
                ),
            })
        } else {
            Ok(HookDecision::Allow)
        }
    }
}
