//! The veto pipeline: tool restriction, hook veto, guard rewrite, observation. Each refusal converges on [`deny_with_event`] so the model always sees a clear error.

use sha2::{Digest, Sha256};
use tracing::{debug, warn};
use uva_context::AgentState;
use uva_provider::ToolUse;
use uva_steering::{Action, GuardOutcome, HookDecision};
use uva_tool::ToolOutput;

use super::{ReActLoop, tools};
use crate::{
    agent::approval::{ApprovalRequest, Risk},
    events::WarningKind,
};

/// The [`Cow`](std::borrow::Cow) arm is what lets the empty-guard path
/// skip the per-call `ToolUse` clone.
pub(super) enum Verdict<'a> {
    Allowed(std::borrow::Cow<'a, ToolUse>),
    Denied(ToolOutput),
}

/// What one tool dispatch produced.
pub(super) struct ToolOutcome {
    /// The tool's output, after the host's result transformers ran.
    pub(super) output: ToolOutput,
    /// The reason a policy ended the run after the tool returned.
    pub(super) terminate: Option<String>,
}

/// What the gate phase decided about one call, before it runs.
pub(super) enum GateDecision {
    // Every gate passed; the guarded call may execute.
    Run(ToolUse),
    // The call is refused; the output is what the model sees.
    Respond(ToolOutput),
    // The call is held for an approval.
    Hold(ApprovalRequest),
}

// What the restriction/hook seams decided before the guards run.
// `Ask` is not an approval yet: the request is built afterwards from
// the guarded call, so a rewriting guard cannot make it stale.
enum PreDecision {
    Respond(ToolOutput),
    Ask,
}

// The batch gate phase: run it on every call before any call executes,
// so a high-risk sibling cannot run ahead of the approval it needs.
pub(super) async fn gate_tool(
    this: &ReActLoop,
    call: &ToolUse,
    state: &AgentState,
    policy: &crate::agent::approval::ApprovalPolicy,
) -> GateDecision {
    let tool_name = call.name.as_str();
    // A delegated high-risk call has no parent/child approval route
    // yet, so it is refused rather than run unapproved or held for a
    // resume the parent cannot complete.
    if delegator_risk(this, call) == Some(Risk::High) {
        return GateDecision::Respond(deny_with_event(
            this,
            "delegated high-risk",
            call,
            state,
            format!(
                "delegated high-risk call `{}` blocked: child approval \
                 routing is not available",
                call.name
            ),
            format!(
                "[blocked by policy] `{}` is a high-risk delegated call \
                 and cannot be approved at this level",
                call.name
            ),
        ));
    }
    // Seam 0 + steering seam 1—the agent's tool restriction,
    // then the hooks' BeforeToolExecute veto.
    let mut gate_asks = false;
    if let Some(decision) =
        pre_execution_veto(this, call, tool_name, state).await
    {
        match decision {
            PreDecision::Respond(output) => {
                return GateDecision::Respond(output);
            }
            PreDecision::Ask => gate_asks = true,
        }
    }
    // Steering seam 2—the guard pipeline runs before an approval
    // request is built, on both the host-ask and policy paths, so
    // the request names the arguments that will execute.
    let effective = match apply_guard_pipeline(this, call, state) {
        Verdict::Allowed(rewritten) => rewritten.into_owned(),
        Verdict::Denied(denied) => return GateDecision::Respond(denied),
    };
    let risk = risk_of(this, &effective);
    let policy_asks =
        policy.requires_approval(&effective.name, risk, &this.workspace_hash);
    if gate_asks || policy_asks {
        let request = approval_request(this, &effective);
        return hold_or_refuse(this, request, &effective, state);
    }
    GateDecision::Run(effective)
}

// A held call needs a log to come back from. Without a writer the
// token would be unrecoverable, so the call is refused instead of
// suspended into a dead end.
fn hold_or_refuse(
    this: &ReActLoop,
    request: ApprovalRequest,
    call: &ToolUse,
    state: &AgentState,
) -> GateDecision {
    if this.sink.has_writer() {
        return GateDecision::Hold(request);
    }
    GateDecision::Respond(deny_with_event(
        this,
        "approval",
        call,
        state,
        format!(
            "policy asked for approval (request {}) but the run has no \
             session log to resume from",
            request.id
        ),
        format!(
            "[approval unavailable] the call was not run: the run has no \
             session log, so an approval could never be resumed \
             (request {})",
            request.id
        ),
    ))
}

/// Does NOT commit to `messages` — the caller decides commit order.
pub(super) async fn execute_gated(
    this: &ReActLoop,
    call: &ToolUse,
    state: &AgentState,
) -> ToolOutcome {
    let tool_name = call.name.as_str();
    let call_id = call.id.as_str();
    debug!(
        tool_name = %tool_name,
        tool_use_id = %call_id,
        "execute_gated: invoking tool"
    );

    // The caller already ran the guard pipeline: the call it hands
    // here is the effective one, so the approved arguments are the
    // executed arguments.
    let effective_call = call;
    let mut result = tools::dispatch_tool_call(
        this,
        effective_call,
        &effective_call.name,
        call_id,
    )
    .await;
    // The host's result transformers rewrite the output here, before
    // the loop commits it — so the `tool_result` row and the history
    // both hold the rewritten text, and the log says what the model
    // read. The audit row this branch writes is provenance, not
    // reconciliation: it names the transformers that altered the result
    // on the way to the commit. Nothing is snapshotted and no row is
    // built on a run whose host holds no result transformer: the check
    // comes before the digest.
    if this.caps.control.seams.has_tool_result_transformers() {
        let before = result_digest(&result);
        let ctx = this.hook_context(state);
        let transformers = this
            .caps
            .control
            .seams
            .tool_result_transformers(&ctx, &mut result)
            .await;
        if !transformers.is_empty() {
            // One row per rewritten result. The digests cover the whole
            // serialized output, and the host reports a change only when
            // the aggregate actually differs, so `before != after` here.
            this.sink.emit_typed(
                uva_session::SessionEvent::ToolResultTransformed {
                    seq: 0,
                    ts: String::new(),
                    transformers,
                    tool_name: tool_name.to_string(),
                    call_id: call_id.to_string(),
                    before,
                    after: result_digest(&result),
                },
            );
        }
    }
    // Steering seam 3—observation: tracker boundary and
    // after-execute hooks fire regardless of outcome shape.
    // Consumer-supplied (`Steering`); isolated like the other
    // surfaces.
    for tracker in &this.caps.control.seams.tracker_table() {
        if let Err(panic) = crate::agent::re_act::panic::caught(|| {
            tracker.on_tool_call(tool_name, &effective_call.input, state);
        }) {
            warn!(panic = %panic, "tracker.on_tool_call panicked");
        }
    }
    let terminate =
        notify_after_hooks(this, state, tool_name, call_id, &result).await;
    ToolOutcome {
        output: result,
        terminate,
    }
}

// No ToolCall-stage safety scan lives here: a consumer wires its own
// scan through a host-registered `Hook` observing the before-tool
// event, the same seam the guards below read.

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
    for tracker in &this.caps.control.seams.tracker_table() {
        if let Err(panic) = crate::agent::re_act::panic::caught(|| {
            tracker.on_tool_call(tool_name, &call.input, state);
        }) {
            warn!(panic = %panic, "tracker.on_tool_call panicked");
        }
    }
    ToolOutput::error(message)
}

async fn pre_execution_veto(
    this: &ReActLoop,
    call: &ToolUse,
    tool_name: &str,
    state: &AgentState,
) -> Option<PreDecision> {
    // Seam 0—the agent's own tool restriction.
    if let Some(restriction) = this.caps.tools.restriction.as_deref()
        && !restriction.is_relevant(tool_name, false)
    {
        return Some(PreDecision::Respond(deny_with_event(
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
        )));
    }

    // Steering seam 1 — the `tool/pre-execute` interception. `Seams`
    // builds the event lazily: it returns before cloning the arguments
    // when no hook and no gate is registered, so the zero-registration
    // hot path pays nothing for the clone.
    let arbitration = {
        let ctx = this.hook_context(state);
        this.caps
            .control
            .seams
            .tool_pre(&ctx, tool_name, call.id.as_str(), &call.input)
            .await
    };
    match arbitration.decision {
        HookDecision::Block { reason } => {
            let denied = deny_with_event(
                this,
                "hook veto",
                call,
                state,
                format!("policy blocked `{}`: {reason}", call.name),
                format!("[blocked by policy] {reason}"),
            );
            // Observers see the veto too: `tool_post` fires regardless of
            // outcome shape, denial included; the veto already happened,
            // so the decision is ignored.
            let ctx = this.hook_context(state);
            let _ = this
                .caps
                .control
                .seams
                .tool_post(&ctx, tool_name, call.id.as_str(), true)
                .await;
            return Some(PreDecision::Respond(denied));
        }
        HookDecision::Terminate { reason } => {
            return Some(PreDecision::Respond(deny_with_event(
                this,
                "hook veto",
                call,
                state,
                format!("policy ended the run: {reason}"),
                format!("[terminated by policy] {reason}"),
            )));
        }
        // An ask never reaches the decision: the arbitration
        // collects the reasons, and the gate site reports them
        // together.
        HookDecision::Allow
        | HookDecision::Ask { .. }
        | HookDecision::ReplaceParams(_) => {}
    }
    // An ask holds the call, but the request is built by `gate_tool`
    // after the guards run, so it names the effective arguments.
    if !arbitration.asks.is_empty() {
        return Some(PreDecision::Ask);
    }
    None
}

// Build the approval request the gate site holds a call with.
//
// The call's identity lives here — its id, the tool, a digest of
// the arguments and the risk the tool declared — so an approval
// decision can be checked against the call it was made for.
pub(super) fn approval_request(
    this: &ReActLoop,
    call: &ToolUse,
) -> ApprovalRequest {
    ApprovalRequest::new(
        format!("approval-{}", call.id),
        call.id.clone(),
        call.name.clone(),
        arguments_hash(&call.input),
        risk_of(this, call),
        this.caps.model.descriptor.name.clone(),
    )
}

// What the audit row's `before` / `after` are taken over: the output's
// canonical JSON (`serde_json::to_vec(&ToolOutput)`), so every field
// counts — the content parts in order, `is_error` (including `None` vs
// `Some(false)`), the metadata and the truncation marker. Rows written
// before this scheme hashed only the text projection, so `before` and
// `after` are comparable within one row and not across schemes.
const RESULT_DIGEST_PREFIX: &str = "sha256:";

// A digest of a tool result as it stands, so an audit row can name two
// distinguishable versions without copying the payload into the log.
fn result_digest(output: &ToolOutput) -> String {
    // Every field is a `String` / `Option` / map of already-serialisable
    // values, so this cannot fail; an empty preimage would still leave
    // the row's two digests equal only for an output that did not change.
    let bytes = serde_json::to_vec(output).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    format!("{RESULT_DIGEST_PREFIX}{}", hex::encode(hasher.finalize()))
}

// A stable digest of the call's arguments: the same arguments always
// hash the same way, so a decision can be matched to its call.
pub(super) fn arguments_hash(input: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(input).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

// The risk the tool declares for this call. A registry tool answers
// from its input (the shell can tell `ls` from `rm`); a delegator
// only has its annotations. A missing tool is unknown.
pub(super) fn risk_of(this: &ReActLoop, call: &ToolUse) -> Risk {
    if let Some(delegator) = this.delegators.get(&call.name) {
        return Risk::from_annotations(delegator.annotations());
    }
    let Some(entry) = this.caps.tools.registry.get(&call.name) else {
        return Risk::Unknown;
    };
    entry.tool_instance().risk(&call.input)
}

// A delegator's declared risk, `None` when no delegator owns the name.
pub(super) fn delegator_risk(this: &ReActLoop, call: &ToolUse) -> Option<Risk> {
    this.delegators
        .get(&call.name)
        .map(|tool| Risk::from_annotations(tool.annotations()))
}

// The tool's idempotence hint, used with `risk_of` to decide whether
// a dispatched call whose result was lost may be retried.
pub(super) fn idempotent_hint_of(
    this: &ReActLoop,
    call: &ToolUse,
) -> Option<bool> {
    annotations_of(this, call)
        .and_then(|annotations| annotations.idempotent_hint)
}

// A call is not proven read-only unless its risk is Low *and* the
// tool declared `readOnlyHint`. Every other call needs a dispatch
// intent row before it runs.
pub(super) fn is_proven_read_only(this: &ReActLoop, call: &ToolUse) -> bool {
    risk_of(this, call) == Risk::Low
        && annotations_of(this, call)
            .and_then(|annotations| annotations.read_only_hint)
            == Some(true)
}

fn annotations_of(
    this: &ReActLoop,
    call: &ToolUse,
) -> Option<uva_tool::ToolAnnotations> {
    if let Some(delegator) = this.delegators.get(&call.name) {
        return delegator.annotations();
    }
    this.caps
        .tools
        .registry
        .get(&call.name)
        .and_then(|entry| entry.tool_instance().annotations())
}

pub(super) fn apply_guard_pipeline<'a>(
    this: &'a ReActLoop,
    call: &'a ToolUse,
    state: &AgentState,
) -> Verdict<'a> {
    if !this.caps.control.seams.has_guards() {
        // No guards → the call passes through with zero clone.
        return Verdict::Allowed(std::borrow::Cow::Borrowed(call));
    }
    let action = Action::from_tool_use(call);
    let ctx = this.hook_context(state);
    match this.caps.control.seams.tool_guards(&action, &ctx) {
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

// The `after_tool` point: the observers see the result, and the
// point accepts a termination — a policy can end the run after a
// tool returned.
async fn notify_after_hooks(
    this: &ReActLoop,
    state: &AgentState,
    tool_name: &str,
    call_id: &str,
    result: &ToolOutput,
) -> Option<String> {
    let arbitration = {
        let ctx = this.hook_context(state);
        this.caps
            .control
            .seams
            .tool_post(
                &ctx,
                tool_name,
                call_id,
                result.is_error.unwrap_or(false),
            )
            .await
    };
    match arbitration.decision {
        HookDecision::Terminate { reason } => Some(reason),
        HookDecision::Allow
        | HookDecision::Block { .. }
        | HookDecision::Ask { .. }
        | HookDecision::ReplaceParams(_) => None,
    }
}
