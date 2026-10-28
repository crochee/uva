//! Loop-wide dispatch: every interception point goes through the
//! agent's one [`Host`](crate::agent::host::Host).
//!
//! The host is the only registry. A point that has no registration
//! returns before building its event, so a run with no policy pays
//! nothing for the seams it does not use.

use elysper_context::AgentState;
use elysper_steering::HookEvent;

use super::ReActLoop;
use crate::{
    agent::host::{Arbitration, HookContext},
    events::SystemEvent,
};

impl ReActLoop {
    /// The context a registration sees at this boundary.
    pub(super) fn hook_context<'a>(
        &'a self,
        state: &'a AgentState,
    ) -> HookContext<'a> {
        HookContext {
            state,
            events: &self.sink,
            cancel: &self.run.cancel,
        }
    }
}

/// Fire one event at the host's observers.
pub(super) async fn dispatch_hook(
    this: &ReActLoop,
    state: &AgentState,
    event: impl FnOnce() -> HookEvent,
) -> Arbitration {
    let host = &this.caps.control.host;
    if !host.has_event_registrations() {
        return Arbitration::default();
    }
    let event = event();
    host.dispatch_hooks(&event, &this.hook_context(state)).await
}

/// Fire the tool gate point: the observers, then the gates.
///
/// A gate is the security capability, so it gets the last word — but
/// only when nothing before it vetoed.
pub(super) async fn dispatch_tool_gate(
    this: &ReActLoop,
    state: &AgentState,
    event: impl FnOnce() -> HookEvent,
) -> Arbitration {
    let host = &this.caps.control.host;
    if !host.has_event_registrations() {
        return Arbitration::default();
    }
    let event = event();
    let ctx = this.hook_context(state);
    let mut arbitration = host.dispatch_hooks(&event, &ctx).await;
    if !arbitration.decided() {
        arbitration.merge(host.dispatch_gates(&event, &ctx).await);
    }
    arbitration
}

pub(crate) fn cancelled(
    cancel: &dyn elysper_core::CancelToken,
    sink: &crate::agent::sink::EventSink,
    reason: &str,
) -> bool {
    if !cancel.is_cancelled() {
        return false;
    }
    tracing::info!("emitting SessionInterrupted: {}", reason);
    sink.system(SystemEvent::SessionInterrupted {
        reason: reason.to_string(),
    });
    tracing::warn!("session interrupted: {}", reason);
    true
}
