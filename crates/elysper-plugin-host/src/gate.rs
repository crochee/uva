//! The elevated gate: one `elysper/gate` round trip per tool call.
//!
//! Only an [`Authority::Full`](crate::Authority::Full) plugin is asked.
//! The plugin answers with a [`PluginDecision`], which the host maps
//! onto the harness's [`HookDecision`]; the wire vocabulary is the
//! protocol's own, so an internal rename never breaks a plugin.
//!
//! The gate is a *tool* gate, not a policy engine: at any point other
//! than `before_tool` the plugin has no say and the answer is `Allow`.
//!
//! Every failure is fail-closed. A plugin that cannot answer — the
//! channel is gone, the deadline expired, the answer is not a decision
//! — produces `Err`, and the host treats a gate error as a block.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use elysper_harness::Gate;
use elysper_plugin_protocol::{PluginDecision, methods};
use elysper_steering::{HookDecision, HookEvent};

use crate::client::Connection;

/// The veto a `Full`-authority plugin holds over tool calls.
pub(crate) struct RemoteGate {
    name: String,
    connection: Arc<Connection>,
}

impl RemoteGate {
    pub(crate) fn new(name: String, connection: Arc<Connection>) -> Self {
        Self { name, connection }
    }
}

#[async_trait]
impl Gate for RemoteGate {
    fn name(&self) -> &str {
        &self.name
    }

    async fn check(
        &self,
        event: &HookEvent,
        _ctx: &elysper_harness::agent::host::HookContext<'_>,
    ) -> Result<HookDecision, String> {
        let HookEvent::BeforeTool {
            tool_name,
            call_id,
            arguments,
        } = event
        else {
            // A tool gate, not a general policy engine: the plugin was
            // only asked about tool calls, so it has no say elsewhere.
            return Ok(HookDecision::Allow);
        };
        let answer = self
            .connection
            .request(
                methods::GATE,
                json!({
                    "tool_name": tool_name,
                    "call_id": call_id,
                    "arguments": arguments,
                }),
            )
            .await
            // Fail-closed. The host treats a gate error as a block, and
            // a plugin that cannot answer must never get to approve.
            .map_err(|error| error.to_string())?;
        let decision: PluginDecision =
            serde_json::from_value(answer).map_err(|error| {
                format!(
                    "`{}` answered with something that is not a decision: \
                     {error}",
                    methods::GATE
                )
            })?;
        Ok(mapped(decision))
    }
}

// The harness decision a wire decision means.
fn mapped(decision: PluginDecision) -> HookDecision {
    match decision {
        PluginDecision::Allow => HookDecision::Allow,
        PluginDecision::Block { reason } => HookDecision::Block { reason },
        PluginDecision::Ask { reason } => HookDecision::Ask { reason },
        PluginDecision::Terminate { reason } => {
            HookDecision::Terminate { reason }
        }
        // `PluginDecision` is `#[non_exhaustive]`: a variant this build
        // does not know reads as `Allow`, because a decision the host
        // cannot interpret must never become a veto by accident.
        _ => HookDecision::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_wire_decision_maps_onto_its_harness_twin() {
        assert_eq!(mapped(PluginDecision::Allow), HookDecision::Allow);
        assert_eq!(
            mapped(PluginDecision::Block {
                reason: "no".into()
            }),
            HookDecision::Block {
                reason: "no".into()
            }
        );
        assert_eq!(
            mapped(PluginDecision::Ask {
                reason: "check".into()
            }),
            HookDecision::Ask {
                reason: "check".into()
            }
        );
        assert_eq!(
            mapped(PluginDecision::Terminate {
                reason: "stop".into()
            }),
            HookDecision::Terminate {
                reason: "stop".into()
            }
        );
    }
}
