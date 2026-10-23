//! Tool-execution phase: bucket the batch by execution mode,
//! run it, and commit the results to the transcript.

mod commit;
mod route;

use commit::commit_all_results;
pub(crate) use commit::commit_tool_result;
pub(crate) use route::dispatch_tool_call;
use synthia_context::{AgentState, TouchedPair};
use synthia_core::sync::OwnedSemaphorePermit;
use synthia_provider::ToolUse;
use synthia_steering::tool_fingerprint;
use synthia_tool::{ExecutionMode, ToolOutput};
use tracing::{debug, info, warn};

use super::{
    super::{panic::isolate, state::RunState},
    ReActLoop,
    guard,
};
use crate::events::{SystemEvent, WarningKind};

pub(super) async fn execute_tools(
    this: &ReActLoop,
    session: &mut RunState,
    calls: &[ToolUse],
) {
    if calls.is_empty() {
        return;
    }
    debug!(
        tool_call_count = calls.len(),
        "step 4 execute_tools: bucketing by ExecutionMode (Parallel vs Sequential)"
    );

    // 0. Record every call in the run state BEFORE dispatch.
    record_tool_fingerprints(calls, &mut session.agent_state);
    let agent_state_ref: &AgentState = &session.agent_state;

    // 1. Bucket by ExecutionMode.
    let (safe_indices, unsafe_indices, concurrency) =
        bucket_by_execution_mode(this, calls, agent_state_ref);
    // 2+3. Run both buckets; slots stay `None` for calls that
    // never ran (cancelled or aborted). File activity returns
    // with each output and is committed once, below.
    let mut outputs: Vec<Option<ToolOutput>> =
        (0..calls.len()).map(|_| None).collect();
    let mut touched: Vec<TouchedPair> = Vec::new();
    run_parallel_bucket(
        this,
        calls,
        &safe_indices,
        concurrency,
        agent_state_ref,
        &mut outputs,
        &mut touched,
    )
    .await;
    run_sequential_bucket(
        this,
        calls,
        &unsafe_indices,
        agent_state_ref,
        &mut outputs,
        &mut touched,
    )
    .await;

    // 4. Commit ToolResults in LLM-call order. The function
    // re-borrows `&session.agent_state` from the `&mut session`
    // we hold; that is sound because the two access patterns
    // here are non-overlapping.
    commit_all_results(this, session, calls, outputs, touched).await;
}

/// Recorded BEFORE dispatch so loop detection sees the whole batch even
/// when an early call aborts the round.
fn record_tool_fingerprints(calls: &[ToolUse], agent_state: &mut AgentState) {
    for call in calls {
        let fingerprint = tool_fingerprint(&call.name, &call.input);
        agent_state.record_tool_call(fingerprint);
    }
}

fn bucket_by_execution_mode(
    this: &ReActLoop,
    calls: &[ToolUse],
    agent_state_ref: &AgentState,
) -> (Vec<usize>, Vec<usize>, usize) {
    let mut safe_indices: Vec<usize> = Vec::with_capacity(calls.len());
    let mut unsafe_indices: Vec<usize> = Vec::with_capacity(calls.len());
    for (idx, call) in calls.iter().enumerate() {
        let mode = this.caps.tools.registry.execution_mode(&call.name);
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
            this.caps
                .control
                .steering
                .tracker
                .recommended_concurrency(agent_state_ref)
        },
    )
    .unwrap_or(usize::MAX);
    let concurrency = recommended.min(safe_indices.len()).max(1);
    info!(
        tool_call_count = calls.len(),
        parallel_count = safe_indices.len(),
        sequential_count = unsafe_indices.len(),
        concurrency,
        "step 4 execute_tools: bucketed; running Parallel via join_all, Sequential serially"
    );
    (safe_indices, unsafe_indices, concurrency)
}

// -- Parallel bucket ------------------------------------------------------

/// Shared by the capped and uncapped paths; the permit outlives the call.
async fn run_one_parallel(
    this: &ReActLoop,
    idx: usize,
    call: &ToolUse,
    agent_state_ref: &AgentState,
    permit: Option<OwnedSemaphorePermit>,
) -> (usize, ToolOutput, Option<TouchedPair>) {
    let _permit = permit;
    let (out, touched) =
        guard::execute_tool_inner(this, call, agent_state_ref).await;
    (idx, out, touched)
}

async fn run_parallel_bucket(
    this: &ReActLoop,
    calls: &[ToolUse],
    safe_indices: &[usize],
    concurrency: usize,
    agent_state_ref: &AgentState,
    outputs: &mut [Option<ToolOutput>],
    touched: &mut Vec<TouchedPair>,
) {
    use synthia_core::sync::Semaphore;

    if safe_indices.is_empty() {
        return;
    }
    // The common path: one call's batch (or a tracker that
    // allows the whole batch at once) needs no semaphore —
    // every permit would be acquired immediately anyway.
    if concurrency >= safe_indices.len() {
        let futs = safe_indices.iter().map(|&idx| {
            run_one_parallel(this, idx, &calls[idx], agent_state_ref, None)
        });
        collect_parallel(
            futures::future::join_all(futs).await,
            outputs,
            touched,
        );
        return;
    }
    let semaphore = std::sync::Arc::new(Semaphore::new(concurrency));
    let futs = safe_indices.iter().map(|&idx| {
        let permit_source = std::sync::Arc::clone(&semaphore);
        async move {
            let permit = permit_source.acquire_owned().await;
            run_one_parallel(
                this,
                idx,
                &calls[idx],
                agent_state_ref,
                Some(permit),
            )
            .await
        }
    });
    collect_parallel(futures::future::join_all(futs).await, outputs, touched);
}

fn collect_parallel(
    results: Vec<(usize, ToolOutput, Option<TouchedPair>)>,
    outputs: &mut [Option<ToolOutput>],
    touched: &mut Vec<TouchedPair>,
) {
    for (idx, out, pair) in results {
        outputs[idx] = Some(out);
        if let Some(pair) = pair {
            touched.push(pair);
        }
    }
}

// -- Sequential bucket ----------------------------------------------------

async fn run_sequential_bucket(
    this: &ReActLoop,
    calls: &[ToolUse],
    unsafe_indices: &[usize],
    agent_state_ref: &AgentState,
    outputs: &mut [Option<ToolOutput>],
    touched: &mut Vec<TouchedPair>,
) {
    let total_sequential = unsafe_indices.len();
    let mut sequential_aborted = false;
    for (seq_pos, idx) in unsafe_indices.iter().copied().enumerate() {
        match run_one_sequential(
            this,
            calls,
            idx,
            total_sequential - seq_pos,
            agent_state_ref,
            outputs,
            touched,
        )
        .await
        {
            SequentialStep::Continue => {}
            SequentialStep::Cancelled => {
                fill_unanswered(
                    calls,
                    outputs,
                    "tool call cancelled before dispatch; safe to retry",
                );
                return;
            }
            SequentialStep::Aborted => {
                sequential_aborted = true;
                fill_unanswered(
                    calls,
                    outputs,
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
/// Abort so corrective feedback reaches the model first.
enum SequentialStep {
    Continue,
    Cancelled,
    Aborted,
}

async fn run_one_sequential(
    this: &ReActLoop,
    calls: &[ToolUse],
    idx: usize,
    remaining_sequential: usize,
    agent_state_ref: &AgentState,
    outputs: &mut [Option<ToolOutput>],
    touched: &mut Vec<TouchedPair>,
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
    let (out, pair) =
        guard::execute_tool_inner(this, call, agent_state_ref).await;
    let abort = out.is_error.unwrap_or(false);
    outputs[idx] = Some(out);
    if let Some(pair) = pair {
        touched.push(pair);
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

pub(crate) fn text_parts(parts: &[synthia_provider::ContentPart]) -> String {
    let mut out = String::new();
    for (idx, part) in parts.iter().enumerate() {
        if let synthia_provider::ContentPart::Text(text) = part {
            if idx > 0 && !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text.text);
        }
    }
    out
}
