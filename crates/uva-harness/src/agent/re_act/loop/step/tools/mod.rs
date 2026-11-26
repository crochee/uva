//! Tool-execution phase: schedule the batch in the model's order,
//! run it, and commit the results to the transcript.

mod commit;
mod route;
mod schedule;

use commit::commit_all_results;
pub(crate) use commit::commit_tool_result;
#[cfg(test)]
pub(crate) use route::TOOL_OUTPUT_FLUSH_CHUNKS;
pub(crate) use route::dispatch_tool_call;
use tracing::debug;
use uva_approval::ApprovalRequest;
use uva_context::AgentState;
use uva_provider::ToolUse;
use uva_steering::tool_fingerprint;
use uva_tool::ToolOutput;

use super::super::{super::state::RunState, ReActLoop, guard};

pub(super) async fn execute_tools(
    this: &ReActLoop,
    session: &mut RunState,
    calls: &[ToolUse],
) -> Result<(), uva_events::SessionEndReason> {
    if calls.is_empty() {
        return Ok(());
    }
    debug!(
        tool_call_count = calls.len(),
        "step 4 execute_tools: bucketing by ExecutionMode (Parallel vs Sequential)"
    );

    // 0. Record every call in the run state BEFORE dispatch.
    record_tool_fingerprints(calls, &mut session.agent_state);
    let agent_state_ref: &AgentState = &session.agent_state;

    // 1. The batch gate phase: every call is gated before any call
    // runs, so a high-risk sibling cannot race ahead of the approval
    // it needs. Refused calls produce their denial now; held calls
    // stay unexecuted.
    let mut batch = Batch {
        outputs: (0..calls.len()).map(|_| None).collect(),
        terminate: None,
        approvals: Vec::new(),
    };
    let mut runnable: Vec<Option<ToolUse>> =
        (0..calls.len()).map(|_| None).collect();
    for (idx, call) in calls.iter().enumerate() {
        match guard::gate_tool(this, call, agent_state_ref, &session.approval)
            .await
        {
            guard::GateDecision::Run(effective) => {
                runnable[idx] = Some(effective);
            }
            guard::GateDecision::Respond(output) => {
                batch.outputs[idx] = Some(output);
            }
            guard::GateDecision::Hold(request) => {
                batch.approvals.push(request);
            }
        }
    }

    // Every call not proven read-only gets an intent row before it
    // runs (Unknown included: it may still write). The flush is
    // fatal — a dispatch row that cannot be made durable must stop
    // the call, not run without its crash marker.
    let mut dispatched = false;
    for effective in runnable.iter().flatten() {
        if !guard::is_proven_read_only(this, effective) {
            this.sink.append_typed(uva_session::tool_dispatch(
                &effective.id,
                &effective.name,
                &guard::arguments_hash(&effective.input),
            ));
            dispatched = true;
        }
    }
    if dispatched {
        this.flush_log().await?;
    }

    // 2. Schedule the batch in the model's order. The guarded call is
    // what executes, so its (possibly rewritten) name decides the
    // wave — scheduling the raw batch would schedule a call by a
    // name that never runs. The waves skip the calls the gate did
    // not let through, so their indices are the runnable ones.
    //
    // 3. Run the waves; slots stay `None` for held calls (and for
    // calls a cancellation or abort never reached, which
    // `fill_unanswered` names). File activity returns with each
    // output and is committed once, below.
    schedule::run_batch(this, calls, &runnable, agent_state_ref, &mut batch)
        .await;

    // A held call is not an unanswered one: a sibling's cancel/abort
    // fill must not turn it into a result row, or the model (and the
    // repair pass) would believe the held call was answered.
    for request in &batch.approvals {
        if let Some(pos) =
            calls.iter().position(|call| call.id == request.call_id)
        {
            batch.outputs[pos] = None;
        }
    }
    // 4. Commit ToolResults in LLM-call order. The function
    // re-borrows `&session.agent_state` from the `&mut session`
    // we hold; that is sound because the two access patterns
    // here are non-overlapping.
    commit_all_results(this, session, calls, batch.outputs).await;
    // A policy that ended the run after a tool returned: the run
    // stops at the step boundary.
    session.terminate = batch.terminate;
    // The calls a gate held for an approval; see `Batch::approvals`.
    session.pending_approvals = batch.approvals;
    Ok(())
}

// The batch's results: one slot per call, the reason a policy ended
// the run, and the calls a gate held for an approval.
struct Batch {
    outputs: Vec<Option<ToolOutput>>,
    // A policy can end the run after a tool returned; the first
    // reason wins and the run ends at the step boundary.
    terminate: Option<String>,
    // The calls a gate held for an approval. `execute_tools` copies
    // them onto `session.pending_approvals`; `Step::Suspended` then
    // takes them from there in the step module's `close`, which
    // writes their rows before the `Suspended` event goes out.
    approvals: Vec<ApprovalRequest>,
}

// Recorded BEFORE dispatch so loop detection sees the whole batch even
// when an early call aborts the round.
fn record_tool_fingerprints(calls: &[ToolUse], agent_state: &mut AgentState) {
    for call in calls {
        let fingerprint = tool_fingerprint(&call.name, &call.input);
        agent_state.record_tool_call(fingerprint);
    }
}

/// Without the named cause the model cannot tell a safe retry from an
/// unknown-outcome skip.
pub(super) fn fill_unanswered(
    calls: &[ToolUse],
    outputs: &mut [Option<ToolOutput>],
    reason: &str,
) {
    for (call, slot) in calls.iter().zip(outputs.iter_mut()) {
        if slot.is_none() {
            *slot = Some(did_not_run(call, reason));
        }
    }
}

pub(super) fn did_not_run(call: &ToolUse, reason: &str) -> ToolOutput {
    ToolOutput::error(format!(
        "Tool call \"{}\" did not run: {reason}.",
        call.name
    ))
}
pub(crate) fn text_parts(parts: &[uva_provider::ContentPart]) -> String {
    let mut out = String::new();
    for (idx, part) in parts.iter().enumerate() {
        if let uva_provider::ContentPart::Text(text) = part {
            if idx > 0 && !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text.text);
        }
    }
    out
}
