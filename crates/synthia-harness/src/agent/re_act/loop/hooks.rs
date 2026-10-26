//! Loop-wide dispatch: the steering bundle's hook map and the
//! agent's host registrations run through one pass, so a decision is
//! made once and in one place.

use std::sync::Arc;

use synthia_steering::HookEvent;
use tracing::{info, warn};

use crate::{
    agent::{caps::ControlCaps, host::Arbitration},
    events::SystemEvent,
};

/// Fire one event at the steering bundle and the host's observers.
///
/// The steering bundle comes first: it is the agent's own policy.
/// The host's observers come next. A veto short-circuits both: the
/// registrations behind it are not consulted, because the call is
/// already refused. An ask never short-circuits — the reasons are
/// collected and the pass continues.
pub(super) async fn dispatch_hook(
    caps: &ControlCaps,
    event: impl FnOnce() -> HookEvent,
) -> Arbitration {
    let host = &caps.host;
    if caps.steering.hooks.is_empty() && !host.has_event_registrations() {
        return Arbitration::default();
    }
    let event = event();
    let mut arbitration = Arbitration::default();
    arbitration.absorb(host.accept(
        &event,
        caps.steering.dispatch(&event).await,
        "steering",
    ));
    // A veto short-circuits: the registrations behind it are not
    // consulted — the call is already refused.
    if !arbitration.decided() {
        arbitration.merge(host.dispatch_observers(&event).await);
    }
    arbitration
}

/// Fire one event at the steering bundle, the host's observers and the
/// host's gates — the tool gate point.
///
/// The order is the same as everywhere else: the steering bundle, the
/// observers, then the gates. A gate is the security capability, so
/// it gets the last word — but only when nothing before it vetoed.
pub(super) async fn dispatch_tool_gate(
    caps: &ControlCaps,
    event: impl FnOnce() -> HookEvent,
) -> Arbitration {
    let host = &caps.host;
    if caps.steering.hooks.is_empty() && !host.has_event_registrations() {
        return Arbitration::default();
    }
    let event = event();
    let mut arbitration = Arbitration::default();
    arbitration.absorb(host.accept(
        &event,
        caps.steering.dispatch(&event).await,
        "steering",
    ));
    // A veto short-circuits: the registrations behind it are not
    // consulted — the call is already refused.
    if !arbitration.decided() {
        arbitration.merge(host.dispatch_observers(&event).await);
    }
    if !arbitration.decided() {
        arbitration.merge(host.dispatch_gates(&event).await);
    }
    arbitration
}

pub(crate) fn append_tool_result_hints(
    hints: &[Arc<dyn synthia_steering::Hint>],
    tool_name: &str,
    state: &synthia_context::AgentState,
    mut content: String,
) -> String {
    if hints.is_empty() {
        return content;
    }
    use synthia_steering::InjectionPoint;
    for hint in hints {
        match hint.injection_point() {
            InjectionPoint::AppendToToolResult {
                tool_name: hint_tool,
            } if hint_tool == tool_name => {}
            _ => continue,
        }
        match crate::agent::re_act::panic::ask_hint(hint, state) {
            Ok(Some(message)) => {
                content.push_str("\n\n[reminder] ");
                content.push_str(&message.content);
            }
            Ok(None) => {}
            Err(panic) => {
                tracing::warn!(
                    hint = hint.name(),
                    tool = tool_name,
                    panic = %panic,
                    "tool-result hint panicked; skipping it"
                );
            }
        }
    }
    content
}

pub(crate) fn cancelled(
    cancel: &dyn synthia_core::CancelToken,
    sink: &crate::agent::sink::EventSink,
    reason: &str,
) -> bool {
    if !cancel.is_cancelled() {
        return false;
    }
    info!("emitting SessionInterrupted: {}", reason);
    sink.system(SystemEvent::SessionInterrupted {
        reason: reason.to_string(),
    });
    warn!("session interrupted: {}", reason);
    true
}
