//! Tool-execution phase: bucket the batch by execution mode,
//! run it, and commit the results to the transcript.

mod commit;
mod route;

use commit::commit_all_results;
pub(crate) use commit::commit_tool_result;
use futures::StreamExt;
pub(crate) use route::dispatch_tool_call;
use tracing::{debug, info, warn};
use uva_context::AgentState;
use uva_provider::ToolUse;
use uva_steering::tool_fingerprint;
use uva_tool::{ExecutionMode, ToolOutput};

use super::{
    super::{panic::isolate, state::RunState},
    ReActLoop,
    guard::{self, ToolOutcome},
};
use crate::{
    agent::approval::ApprovalRequest,
    events::{SystemEvent, WarningKind},
};

pub(super) async fn execute_tools(
    this: &ReActLoop,
    session: &mut RunState,
    calls: &[ToolUse],
) -> Result<(), crate::events::SessionEndReason> {
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

    // 2. Bucket the runnable calls by ExecutionMode. The guarded call
    // is what executes, so its (possibly rewritten) name decides the
    // bucket — bucketing the raw batch would schedule a call by a name
    // that never runs. The buckets already skip the calls the gate
    // did not let through, so their indices are the runnable ones.
    let (safe_indices, unsafe_indices, concurrency) =
        bucket_by_execution_mode(this, &runnable, agent_state_ref);
    // 3. Run both buckets; slots stay `None` for held calls (and for
    // calls a cancellation or abort never reached, which
    // `fill_unanswered` names). File activity returns with each
    // output and is committed once, below.
    run_parallel_bucket(
        this,
        &runnable,
        &safe_indices,
        concurrency,
        agent_state_ref,
        &mut batch,
    )
    .await;
    run_sequential_bucket(
        this,
        calls,
        &runnable,
        &unsafe_indices,
        agent_state_ref,
        &mut batch,
    )
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
    // takes them from there in `suspend::boundary`, which writes their
    // rows before the `Suspended` event goes out.
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

pub(in crate::agent::re_act) fn bucket_by_execution_mode(
    this: &ReActLoop,
    guarded: &[Option<ToolUse>],
    agent_state_ref: &AgentState,
) -> (Vec<usize>, Vec<usize>, usize) {
    let mut safe_indices: Vec<usize> = Vec::with_capacity(guarded.len());
    let mut unsafe_indices: Vec<usize> = Vec::with_capacity(guarded.len());
    for (idx, call) in guarded.iter().enumerate() {
        // A held/refused call never runs; it does not occupy a bucket.
        let Some(call) = call else { continue };
        let mode = this.execution_mode(&call.name);
        if mode == ExecutionMode::Parallel {
            safe_indices.push(idx);
        } else {
            unsafe_indices.push(idx);
        }
    }
    // The fourth consumer-supplied `Tracker` method; the other three are
    // guarded. A panic here falls back to the neutral bound
    // (`usize::MAX`, i.e. the batch's own size) rather than ending a run
    // the tracker was only advising.
    let recommended = isolate(
        &this.sink,
        WarningKind::Hook,
        None,
        "tracker.recommended_concurrency",
        || {
            let mut bound = usize::MAX;
            for tracker in &this.caps.control.seams.tracker_table() {
                bound =
                    bound.min(tracker.recommended_concurrency(agent_state_ref));
            }
            bound
        },
    )
    .unwrap_or(usize::MAX);
    let concurrency = recommended.min(safe_indices.len()).max(1);
    info!(
        tool_call_count = guarded.len(),
        parallel_count = safe_indices.len(),
        sequential_count = unsafe_indices.len(),
        concurrency,
        "step 4 execute_tools: bucketed; running Parallel via join_all, Sequential serially"
    );
    (safe_indices, unsafe_indices, concurrency)
}

// -- Parallel bucket ------------------------------------------------------

// Shared by the capped and uncapped paths; the permit outlives the call.
async fn run_one_parallel(
    this: &ReActLoop,
    idx: usize,
    call: &ToolUse,
    agent_state_ref: &AgentState,
) -> (usize, ToolOutcome) {
    let outcome = guard::execute_gated(this, call, agent_state_ref).await;
    (idx, outcome)
}

async fn run_parallel_bucket(
    this: &ReActLoop,
    guarded: &[Option<ToolUse>],
    safe_indices: &[usize],
    concurrency: usize,
    agent_state_ref: &AgentState,
    batch: &mut Batch,
) {
    if safe_indices.is_empty() {
        return;
    }
    // The batch runs under a concurrency bound, so a tracker that
    // caps the parallelism is honoured by construction: at most
    // `concurrency` calls are in flight, and the next one starts as
    // soon as a slot frees. The bound is the batch's own size when
    // no tracker caps it, so the common case is unbounded — the same
    // shape the loop had before the bound existed.
    let futs: Vec<_> = safe_indices
        .iter()
        .map(|&idx| {
            run_one_parallel(
                this,
                idx,
                guarded[idx].as_ref().expect("the gate passed this call"),
                agent_state_ref,
            )
        })
        .collect();
    let results: Vec<(usize, ToolOutcome)> = futures::stream::iter(futs)
        .buffer_unordered(concurrency)
        .collect()
        .await;
    collect_parallel(results, batch);
}

fn collect_parallel(mut results: Vec<(usize, ToolOutcome)>, batch: &mut Batch) {
    // The bounded batch finishes in completion order, so the slots
    // are filled by index and the terminal outcome is taken from the
    // lowest-indexed call — the same one an unbounded batch would
    // have reported. The rows themselves are committed in the
    // assistant's source order (see `commit_all_results`).
    results.sort_by_key(|(index, _)| *index);
    for (idx, outcome) in results {
        batch.outputs[idx] = Some(outcome.output);
        if batch.terminate.is_none() {
            batch.terminate = outcome.terminate;
        }
    }
}

// -- Sequential bucket ----------------------------------------------------

async fn run_sequential_bucket(
    this: &ReActLoop,
    calls: &[ToolUse],
    guarded: &[Option<ToolUse>],
    unsafe_indices: &[usize],
    agent_state_ref: &AgentState,
    batch: &mut Batch,
) {
    let total_sequential = unsafe_indices.len();
    let mut sequential_aborted = false;
    for (seq_pos, idx) in unsafe_indices.iter().copied().enumerate() {
        match run_one_sequential(
            this,
            calls,
            guarded,
            idx,
            total_sequential - seq_pos,
            agent_state_ref,
            batch,
        )
        .await
        {
            SequentialStep::Continue => {}
            SequentialStep::Cancelled => {
                fill_unanswered(
                    calls,
                    &mut batch.outputs,
                    "tool call cancelled before dispatch; safe to retry",
                );
                return;
            }
            SequentialStep::Aborted => {
                sequential_aborted = true;
                fill_unanswered(
                    calls,
                    &mut batch.outputs,
                    "skipped after sibling sequential tool error; \
                     outcome unknown — verify external state before \
                     retrying",
                );
                break;
            }
        }
    }
    if sequential_aborted {
        debug!(
            tool_call_count = calls.len(),
            "step 4 execute_tools: Sequential round aborted after first error"
        );
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
// Abort so corrective feedback reaches the model first.
enum SequentialStep {
    Continue,
    Cancelled,
    Aborted,
}

async fn run_one_sequential(
    this: &ReActLoop,
    calls: &[ToolUse],
    guarded: &[Option<ToolUse>],
    idx: usize,
    remaining_sequential: usize,
    agent_state_ref: &AgentState,
    batch: &mut Batch,
) -> SequentialStep {
    // Silent mid-batch abort: the loop's cancel handling reports
    // the interruption once the iteration unwinds; a per-call
    // event here would duplicate it for every queued sibling.
    if this.run.cancel.is_cancelled() {
        debug!(
            tool_call_count = calls.len(),
            remaining_sequential,
            "step 4 execute_tools: cancelled mid-Sequential batch"
        );
        return SequentialStep::Cancelled;
    }
    let call = &calls[idx];
    this.sink.system(SystemEvent::Progress {
        message: format!("Executing tool {}", call.name),
        step: 0,
        total: this.caps.control.max_iterations,
    });
    let effective = guarded[idx].as_ref().expect("the gate passed this call");
    let outcome = guard::execute_gated(this, effective, agent_state_ref).await;
    let abort = outcome.output.is_error.unwrap_or(false);
    batch.outputs[idx] = Some(outcome.output);
    if batch.terminate.is_none() {
        batch.terminate = outcome.terminate;
    }
    if abort {
        warn!(
            tool_name = %call.name,
            tool_use_id = call.id,
            "step 4 execute_tools: sequential tool reported error; aborting remaining sequential calls"
        );
        return SequentialStep::Aborted;
    }
    SequentialStep::Continue
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
