//! Loop-wide hint-append and cancellation plumbing. Interception
//! itself is `Steering::hooks` (a `HookMap`) dispatched directly at
//! each call site.

use std::sync::Arc;

use synthia_steering::{HookDecision, HookEvent, Steering};
use tracing::{info, warn};

use crate::events::SystemEvent;

/// Empty-hook fast path stays lazy; `Allow` is the no-hook outcome.
pub(super) async fn dispatch_hook(
    steering: &Steering,
    event: impl FnOnce() -> HookEvent,
) -> HookDecision {
    if steering.hooks.is_empty() {
        return HookDecision::Allow;
    }
    steering.hooks.dispatch(&event()).await
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
