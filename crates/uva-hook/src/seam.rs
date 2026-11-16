//! The seven mount points and the payload a chain pass carries.
//!
//! The keys are the wire names the loop dispatches its kernel
//! waterfalls under. A registration on one of these keys observes,
//! rewrites, or short-circuits the pass — the same three powers the
//! typed registrations have always had:
//!
//! | Key constant | Mount point | Event that dispatches there |
//! |---|---|---|
//! | [`STEP_PRE`] | `agent/step-pre` | `HookEvent::BeforeStep` |
//! | [`STEP_POST`] | `agent/step-post` | `HookEvent::AfterStep` |
//! | [`TOOL_PRE`] | `tool/pre-execute` | `HookEvent::BeforeTool` |
//! | [`TOOL_POST`] | `tool/post-execute` | `HookEvent::AfterTool` |
//! | [`LLM_REQUEST`] | `llm/request` | `HookEvent::BeforeRequest` |
//! | [`LLM_RESULT`] | `llm/result` | `HookEvent::AfterProviderCall` |
//! | [`PROMPT_SECTIONS`] | `prompt/sections` | one section per middleware |

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use uva_context::AgentState;
use uva_core::CancelToken;
use uva_kernel::{EventValue, WaterfallCtx};
use uva_steering::HookEvent;

use crate::context::HookContext;

/// Before a loop step samples the model.
///
/// A middleware may observe the step, terminate the run, or
/// short-circuit it — a short-circuit skips the model call and its
/// value is the step's result.
pub const STEP_PRE: &str = "agent/step-pre";

/// After a loop step committed its tool results.
///
/// The pass observes a step that already ran; a middleware returns
/// `Continue` to leave the result unchanged.
pub const STEP_POST: &str = "agent/step-post";

/// Before a tool call executes.
///
/// A middleware may observe the call, rewrite its arguments, or
/// short-circuit it with a rejection. A rejection skips the tool and
/// the loop commits a matching `tool_result`.
pub const TOOL_PRE: &str = "tool/pre-execute";

/// After a tool call returned, before the loop commits its result.
///
/// A middleware may observe the result or rewrite it; the rewritten
/// value is what the model reads in the next request.
pub const TOOL_POST: &str = "tool/post-execute";

/// Before a model request goes on the wire.
///
/// Carries the request parameters (`RequestParams`): the model, the
/// temperature, the max tokens, the tool choice and the tool schemas.
/// The transcript and the system prompt are not part of the payload.
pub const LLM_REQUEST: &str = "llm/request";

/// After a model request returned.
///
/// Carries the provider's answer. A middleware may observe it or
/// rewrite it before the loop commits the step.
pub const LLM_RESULT: &str = "llm/result";

/// A collection event: one system-prompt section per middleware.
///
/// Sections render in registration order after the base manifest; a
/// run takes its section snapshot once, so a registration made
/// mid-run lands on the next run.
pub const PROMPT_SECTIONS: &str = "prompt/sections";

/// The payload every seam chain carries for one pass.
///
/// A middleware downcasts the chain context's payload to this type
/// to read the event, record ask reasons, or rebuild the context
/// with a rewritten event (see [`HookPayload::rewrite`]). It also
/// carries the run's [`HookContext`] data — the state snapshot and
/// cancellation token the typed registrations read — so a middleware
/// can expose the same context a typed registration sees.
pub struct HookPayload {
    event: HookEvent,
    // The owned pieces of a HookContext: a snapshot of the run state
    // plus the shareable cancel handle. Cheap to clone per active
    // pass; the loop's own state stays untouched.
    state: AgentState,
    cancel: Arc<dyn CancelToken>,
    // Shared across every pass of one chain run: a rewrite builds a
    // new payload, but the ask reasons a middleware recorded before
    // the rewrite must survive to the fold.
    asks: Arc<Mutex<Vec<String>>>,
}

impl HookPayload {
    /// Build the pass payload for `event` from the run's context.
    pub fn new(event: HookEvent, ctx: &HookContext<'_>) -> Self {
        Self {
            event,
            state: ctx.state.clone(),
            cancel: Arc::clone(ctx.cancel),
            asks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[must_use]
    /// The registration context this pass carries: the state
    /// snapshot and cancellation token the typed registrations read.
    pub fn hook_context(&self) -> HookContext<'_> {
        HookContext {
            state: &self.state,
            cancel: &self.cancel,
        }
    }

    #[must_use]
    /// The typed event this pass carries.
    pub fn event(&self) -> &HookEvent {
        &self.event
    }

    /// Record an ask reason; the pass continues.
    pub fn ask(&self, reason: impl Into<String>) {
        lock(&self.asks).push(reason.into());
    }

    #[must_use]
    /// The pass payload a chain context carries, if it carries one.
    pub fn payload(ctx: &WaterfallCtx) -> Option<Arc<Self>> {
        ctx.payload().downcast::<Self>().ok()
    }

    #[must_use]
    /// Continue the chain with a rewritten event.
    ///
    /// The rewritten context keeps the chain's event name, services
    /// and pass context; only the event is replaced. A context that
    /// carries no pass payload is returned unchanged.
    pub fn rewrite(
        ctx: &Arc<WaterfallCtx>,
        event: HookEvent,
    ) -> Arc<WaterfallCtx> {
        let Some(pass) = Self::payload(ctx) else {
            return Arc::clone(ctx);
        };
        let rewritten = Arc::new(Self {
            event,
            state: pass.state.clone(),
            cancel: Arc::clone(&pass.cancel),
            asks: Arc::clone(&pass.asks),
        });
        Arc::new(WaterfallCtx::new(
            ctx.event().to_owned(),
            EventValue::from_arc(rewritten),
            ctx.services().clone(),
        ))
    }

    /// Drain the ask reasons recorded on this pass.
    pub fn take_asks(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.asks))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_point_keys_are_the_wire_names() {
        assert_eq!(STEP_PRE, "agent/step-pre");
        assert_eq!(STEP_POST, "agent/step-post");
        assert_eq!(TOOL_PRE, "tool/pre-execute");
        assert_eq!(TOOL_POST, "tool/post-execute");
        assert_eq!(LLM_REQUEST, "llm/request");
        assert_eq!(LLM_RESULT, "llm/result");
        assert_eq!(PROMPT_SECTIONS, "prompt/sections");
    }
}
