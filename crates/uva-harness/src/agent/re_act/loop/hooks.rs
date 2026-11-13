//! Loop-wide interception: every point goes through the agent's
//! [`Seams`](crate::seams::Seams) layer.
//!
//! This module keeps the two things the seam layer cannot know: how to
//! build a registration's context from the run's state, and the
//! cancellation reporting shared by the boundaries.

use uva_context::AgentState;

use super::ReActLoop;
use crate::{agent::host::HookContext, events::SystemEvent};

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

pub(crate) fn cancelled(
    cancel: &dyn uva_core::CancelToken,
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
