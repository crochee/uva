//! The veto pipeline: tool restriction, hook veto, guard rewrite, observation. Each refusal converges on [`deny_with_event`] so the model always sees a clear error.

use synthia_context::{AgentState, TouchedPair};
use synthia_provider::ToolUse;
use synthia_steering::{
    Action,
    GuardOutcome,
    HookDecision,
    HookEvent,
    run_guards,
};
use synthia_tool::ToolOutput;
use tracing::{debug, warn};

use super::{ReActLoop, tools};
use crate::events::WarningKind;

/// The [`Cow`](std::borrow::Cow) arm is what lets the empty-guard path
/// skip the per-call `ToolUse` clone.
enum Verdict<'a> {
    Allowed(std::borrow::Cow<'a, ToolUse>),
    Denied(ToolOutput),
}

/// Does NOT commit to `messages` — the caller decides commit order, and
/// takes the [`TouchedPair`] to commit under one scratch lock.
pub(super) async fn execute_tool_inner(
    this: &ReActLoop,
    call: &ToolUse,
    state: &AgentState,
) -> (ToolOutput, Option<TouchedPair>) {
    let tool_name = call.name.as_str();
    let call_id = call.id.as_str();
    debug!(
        tool_name = %tool_name,
        tool_use_id = %call_id,
        "execute_tool_inner: invoking tool"
    );

    // Seam 0 + steering seam 1—the agent's tool restriction,
    // then the hooks' BeforeToolExecute veto.
    if let Some(denied) = pre_execution_veto(this, call, tool_name, state).await
    {
        return (denied, None);
    }

    // Steering seam 2—guard pipeline (may rewrite the call).
    let effective_call = match apply_guard_pipeline(this, call, state) {
        Verdict::Allowed(rewritten) => rewritten,
        Verdict::Denied(denied) => return (denied, None),
    };
    let (result, touched) = tools::dispatch_tool_call(
        this,
        &effective_call,
        &effective_call.name,
        call_id,
    )
    .await;
    // Steering seam 3—observation: tracker boundary and
    // after-execute hooks fire regardless of outcome shape.
    // Consumer-supplied (`Steering`); isolated like the other
    // surfaces.
    if let Err(panic) = crate::agent::re_act::panic::caught(|| {
        this.caps.control.steering.tracker.on_tool_call(
            tool_name,
            &effective_call.input,
            state,
        );
    }) {
        warn!(panic = %panic, "tracker.on_tool_call panicked");
    }
    notify_after_hooks(this, tool_name, call_id, &result).await;
    (result, touched)
}

// No ToolCall-stage safety scan lives here: a consumer wires
// `synthia_safety::SafetyScanner` through a `before_tool`
// `HookMap` handler, the same seam the steering guards below read.

fn deny_with_event(
    this: &ReActLoop,
    source: &str,
    call: &ToolUse,
    state: &AgentState,
    event_message: String,
    tool_message: String,
) -> ToolOutput {
    warn!(
        source = %source,
        tool_name = %call.name,
        tool_use_id = %call.id,
        "execute_tool_inner: tool call denied"
    );
    this.sink.warn(
        WarningKind::Guard,
        Some(state.iteration_count),
        event_message,
    );
    denied(this, &call.name, call, state, tool_message)
}

fn denied(
    this: &ReActLoop,
    tool_name: &str,
    call: &ToolUse,
    state: &AgentState,
    message: String,
) -> ToolOutput {
    // Panic-isolated like the success path: a denial must not
    // become a run-ending error because the tracker panicked.
    if let Err(panic) = crate::agent::re_act::panic::caught(|| {
        this.caps.control.steering.tracker.on_tool_call(
            tool_name,
            &call.input,
            state,
        );
    }) {
        warn!(panic = %panic, "tracker.on_tool_call panicked");
    }
    ToolOutput::error(message)
}

async fn pre_execution_veto(
    this: &ReActLoop,
    call: &ToolUse,
    tool_name: &str,
    state: &AgentState,
) -> Option<ToolOutput> {
    // Seam 0—the agent's own tool restriction.
    if let Some(restriction) = this.caps.tools.restriction.as_deref()
        && !restriction.is_relevant(tool_name, false)
    {
        return Some(deny_with_event(
            this,
            "tool restriction",
            call,
            state,
            format!(
                "tool restriction denied `{}`: it is not \
                 available to this agent",
                call.name,
            ),
            format!(
                "[denied by tool restriction] `{}` is not \
                 available to this agent; use one of the tools you were \
                 given, or answer from what you have.",
                call.name,
            ),
        ));
    }

    // Steering seam 1 — the `before_tool` interception. The event
    // is built lazily inside `dispatch_hook`, so a zero-hook run
    // pays nothing.
    let decision =
        super::hooks::dispatch_hook(&this.caps.control.steering, || {
            HookEvent::BeforeTool {
                tool_name: tool_name.to_string(),
                call_id: call.id.clone(),
                arguments: call.input.clone(),
            }
        })
        .await;
    match decision {
        HookDecision::Block { reason } => {
            let denied = deny_with_event(
                this,
                "hook veto",
                call,
                state,
                format!("policy blocked `{}`: {reason}", call.name),
                format!("[blocked by policy] {reason}"),
            );
            // Observers see the veto too: `after_tool` fires
            // regardless of outcome shape, denial included; the
            // veto already happened, so the decision is ignored.
            let _ = super::hooks::dispatch_hook(
                &this.caps.control.steering,
                || HookEvent::AfterTool {
                    tool_name: tool_name.to_string(),
                    call_id: call.id.clone(),
                    is_error: true,
                },
            )
            .await;
            Some(denied)
        }
        HookDecision::Terminate { reason } => Some(deny_with_event(
            this,
            "hook veto",
            call,
            state,
            format!("policy ended the run: {reason}"),
            format!("[terminated by policy] {reason}"),
        )),
        HookDecision::Allow => None,
    }
}

fn apply_guard_pipeline<'a>(
    this: &'a ReActLoop,
    call: &'a ToolUse,
    state: &AgentState,
) -> Verdict<'a> {
    if this.caps.control.steering.guards.is_empty() {
        // No guards → the call passes through with zero clone.
        return Verdict::Allowed(std::borrow::Cow::Borrowed(call));
    }
    let action = Action::from_tool_use(call);
    match run_guards(&this.caps.control.steering.guards, &action, state) {
        GuardOutcome::Allowed { action } => {
            Verdict::Allowed(std::borrow::Cow::Owned(action.apply_to(call)))
        }
        GuardOutcome::Denied {
            guard,
            reason,
            severity,
        } => Verdict::Denied(deny_with_event(
            this,
            "guard",
            call,
            state,
            format!(
                "guard `{guard}` ({}) denied `{}`: {reason}",
                severity.as_str(),
                action.summary(),
            ),
            format!(
                "[blocked by guard `{guard}` ({})] {reason}",
                severity.as_str(),
            ),
        )),
    }
}

async fn notify_after_hooks(
    this: &ReActLoop,
    tool_name: &str,
    call_id: &str,
    result: &ToolOutput,
) {
    // Decision ignored: `AfterTool` is observational.
    let _ = super::hooks::dispatch_hook(&this.caps.control.steering, || {
        HookEvent::AfterTool {
            tool_name: tool_name.to_string(),
            call_id: call_id.to_string(),
            is_error: result.is_error.unwrap_or(false),
        }
    })
    .await;
}
