//! The read-only observer: host hook events, forwarded to the plugin as
//! `elysper/event` notifications.
//!
//! Observation is the one capability every authority level grants, and it
//! grants nothing else: the plugin is told what happened and cannot
//! answer, so an unreachable plugin costs a notification, never a turn.
//! That is why [`observe`](RemoteObserver::observe) always decides
//! `Allow` — a decision here would be a veto, and a veto is the gate's
//! job.
//!
//! The wire vocabulary is narrower than the harness's: the internal
//! events a remote plugin cannot act on (`before_request`,
//! `after_provider_call`, …) are not translated at all, so the plugin's
//! authors only see events that mean something on the wire.

use std::sync::Arc;

use async_trait::async_trait;
use elysper_harness::Hook;
use elysper_plugin_protocol::{PluginEvent, methods};
use elysper_steering::{HookDecision, HookEvent};

use crate::client::Connection;

/// Forwards the observable subset of the run's events to one plugin.
pub(crate) struct RemoteObserver {
    name: String,
    connection: Arc<Connection>,
}

impl RemoteObserver {
    pub(crate) fn new(name: String, connection: Arc<Connection>) -> Self {
        Self { name, connection }
    }
}

#[async_trait]
impl Hook for RemoteObserver {
    fn name(&self) -> &str {
        &self.name
    }

    async fn observe(
        &self,
        event: &HookEvent,
        _ctx: &elysper_harness::agent::host::HookContext<'_>,
    ) -> HookDecision {
        if let Some(event) = wire_event(event)
            && let Ok(params) = serde_json::to_value(event)
        {
            // A dead plugin is not a failed turn: the observation is
            // a courtesy, so its delivery is best-effort.
            let _ = self.connection.notify(methods::EVENT, params).await;
        }
        HookDecision::Allow
    }
}

// The wire event one internal event maps to, if it is on the wire.
fn wire_event(event: &HookEvent) -> Option<PluginEvent> {
    match event {
        HookEvent::BeforeRun { agent_name, .. } => {
            Some(PluginEvent::TurnStart {
                agent_name: agent_name.clone(),
            })
        }
        HookEvent::AfterRun {
            agent_name, reason, ..
        } => Some(PluginEvent::TurnEnd {
            agent_name: agent_name.clone(),
            reason: reason.clone(),
        }),
        HookEvent::BeforeStep { iteration, .. } => {
            Some(PluginEvent::BeforeStep {
                iteration: *iteration,
            })
        }
        HookEvent::AfterStep { iteration, .. } => {
            Some(PluginEvent::AfterStep {
                iteration: *iteration,
            })
        }
        HookEvent::BeforeTool {
            tool_name,
            call_id,
            arguments,
        } => Some(PluginEvent::BeforeTool {
            tool_name: tool_name.clone(),
            call_id: call_id.clone(),
            arguments: arguments.clone(),
        }),
        HookEvent::AfterTool {
            tool_name,
            call_id,
            is_error,
        } => Some(PluginEvent::AfterTool {
            tool_name: tool_name.clone(),
            call_id: call_id.clone(),
            is_error: *is_error,
        }),
        // `before_request`, `before_provider_call`,
        // `after_provider_call`, …: internal telemetry no remote plugin
        // can act on, so they never reach the wire.
        _ => None,
    }
}
