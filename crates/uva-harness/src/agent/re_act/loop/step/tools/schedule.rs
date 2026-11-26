//! Model-order scheduling for one tool batch.
//!
//! The batch keeps the order the model wrote it in. Calls whose tool
//! declares them concurrency-safe share a wave and run together; a
//! call that must run alone is a barrier between the waves around it:
//! the wave before it has drained before it starts, and the wave after
//! it starts only once it has finished.
//!
//! Both references schedule the same way. dsh groups the plan into
//! waves and re-reads each later call's mode before starting it
//! (`packages/core/tools/src/batch.ts:78-83`), so an exclusive call
//! ends the wave that precedes it. omp chains the same barrier
//! through `lastExclusive` (`packages/agent/src/agent-loop.ts:4044`):
//! an exclusive call waits for the previous exclusive and for every
//! shared task still in flight, and the shared calls behind it wait
//! for it.

use futures::StreamExt;
use tracing::{info, warn};
use uva_context::AgentState;
use uva_events::{SystemEvent, WarningKind};
use uva_provider::ToolUse;
use uva_tool::ExecutionMode;

use super::{
    super::super::{
        super::panic::isolate,
        ReActLoop,
        guard::{self, ToolOutcome},
    },
    Batch,
    did_not_run,
    fill_unanswered,
};

// A call that never started because the turn was cancelled: the model
// may retry it, so the answer says so.
const CANCELLED_BEFORE_DISPATCH: &str =
    "tool call cancelled before dispatch; safe to retry";

// A call skipped after a sibling exclusive call failed: the failed
// call's effect on the outside world is unknown, so the model has to
// look before it acts again.
const SKIPPED_AFTER_SIBLING_ERROR: &str = "skipped after sibling sequential tool error; outcome unknown — \
     verify external state before retrying";

// Run the batch in the order the model wrote it.
//
// The calls the gate did not let through take no wave: a refused call
// is already answered, and a held one waits for an approval.
//
// An exclusive call that fails takes the exclusive calls behind it
// with it: the failed call's effect on the outside world is unknown,
// so the model has to look before it acts again. The shared waves
// behind it still run — a failed sibling does not cancel them, and
// the model asked for them.
pub(super) async fn run_batch(
    this: &ReActLoop,
    calls: &[ToolUse],
    guarded: &[Option<ToolUse>],
    agent_state_ref: &AgentState,
    batch: &mut Batch,
) {
    // The concurrency bound is the tracker's advice for this batch,
    // read once: the waves are separate, but the advice is about the
    // run, not about a wave.
    let bound = recommended_concurrency(this, agent_state_ref);
    let mut exclusive_failed = false;
    let mut idx = 0usize;
    while idx < guarded.len() {
        // A cancelled turn dispatches nothing further: the calls that
        // never started get the safe-to-retry answer.
        if this.run.cancel.is_cancelled() {
            fill_unanswered(
                calls,
                &mut batch.outputs,
                CANCELLED_BEFORE_DISPATCH,
            );
            return;
        }
        let Some(call) = guarded[idx].as_ref() else {
            // A refused or held call: it never runs, so it is no part
            // of a wave and it does not end one.
            idx += 1;
            continue;
        };
        if this.execution_mode(call) == ExecutionMode::Parallel {
            let wave = take_shared_wave(this, guarded, idx);
            idx = wave.end;
            run_shared_wave(
                this,
                guarded,
                &wave.indices,
                bound,
                agent_state_ref,
                batch,
            )
            .await;
            continue;
        }
        // An exclusive call runs alone, between the waves around it.
        if exclusive_failed {
            batch.outputs[idx] =
                Some(did_not_run(&calls[idx], SKIPPED_AFTER_SIBLING_ERROR));
            idx += 1;
            continue;
        }
        let failed = run_exclusive_wave(
            this,
            calls,
            guarded,
            idx,
            agent_state_ref,
            batch,
        )
        .await;
        exclusive_failed = failed;
        idx += 1;
    }
}

// The shared wave that starts at `start`: the run of calls whose
// tools declare them concurrency-safe.
struct SharedWave {
    // The batch positions of the wave's calls.
    indices: Vec<usize>,
    // The position after the wave.
    end: usize,
}

fn take_shared_wave(
    this: &ReActLoop,
    guarded: &[Option<ToolUse>],
    start: usize,
) -> SharedWave {
    let mut indices = Vec::new();
    let mut idx = start;
    while idx < guarded.len() {
        let Some(call) = guarded[idx].as_ref() else {
            // A refused or held call runs in no wave and does not end
            // one.
            idx += 1;
            continue;
        };
        if this.execution_mode(call) != ExecutionMode::Parallel {
            break;
        }
        indices.push(idx);
        idx += 1;
    }
    SharedWave { indices, end: idx }
}

// -- Shared wave ---------------------------------------------------------

// The calls of one shared wave, running together under the batch's
// concurrency bound.
async fn run_shared_wave(
    this: &ReActLoop,
    guarded: &[Option<ToolUse>],
    indices: &[usize],
    bound: usize,
    agent_state_ref: &AgentState,
    batch: &mut Batch,
) {
    if indices.is_empty() {
        return;
    }
    let concurrency = bound.min(indices.len()).max(1);
    let futs: Vec<_> = indices
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
    collect_shared(results, batch);
}

// Shared by the capped and uncapped paths; the permit outlives the call.
async fn run_one_parallel(
    this: &ReActLoop,
    idx: usize,
    call: &ToolUse,
    agent_state_ref: &AgentState,
) -> (usize, ToolOutcome) {
    // The cancel check is not here: it lives one layer down, in the
    // tool stream's drain (`dispatch_tool_call`), which every call
    // passes through — a call that has not started when the turn is
    // cancelled never runs, whichever wave it is in.
    let outcome = guard::execute_gated(this, call, agent_state_ref).await;
    (idx, outcome)
}

fn collect_shared(mut results: Vec<(usize, ToolOutcome)>, batch: &mut Batch) {
    // The bounded wave finishes in completion order, so the slots are
    // filled by index and the terminal outcome is taken from the
    // lowest-indexed call — the same one an unbounded wave would have
    // reported. The rows themselves are committed in the assistant's
    // source order (see `commit_all_results`).
    results.sort_by_key(|(index, _)| *index);
    for (idx, outcome) in results {
        batch.outputs[idx] = Some(outcome.output);
        if batch.terminate.is_none() {
            batch.terminate = outcome.terminate;
        }
    }
}

// -- Exclusive wave ------------------------------------------------------

// One call that runs alone: the waves around it wait for it.
//
// Returns whether the call failed — a failed exclusive call takes the
// exclusive calls behind it with it.
async fn run_exclusive_wave(
    this: &ReActLoop,
    calls: &[ToolUse],
    guarded: &[Option<ToolUse>],
    idx: usize,
    agent_state_ref: &AgentState,
    batch: &mut Batch,
) -> bool {
    let call = &calls[idx];
    this.sink.system(SystemEvent::Progress {
        message: format!("Executing tool {}", call.name),
        step: 0,
        total: this.caps.control.max_iterations,
    });
    let effective = guarded[idx].as_ref().expect("the gate passed this call");
    let outcome = guard::execute_gated(this, effective, agent_state_ref).await;
    let failed = outcome.output.is_error.unwrap_or(false);
    batch.outputs[idx] = Some(outcome.output);
    if batch.terminate.is_none() {
        batch.terminate = outcome.terminate;
    }
    if failed {
        warn!(
            tool_name = %call.name,
            tool_use_id = call.id,
            "step 4 execute_tools: exclusive tool reported error; \
             skipping the exclusive calls behind it"
        );
    }
    failed
}

// -- Concurrency bound ---------------------------------------------------

// How many calls of one wave may be in flight at once.
//
// The fourth consumer-supplied `Tracker` method; the other three are
// guarded. A panic here falls back to the neutral bound (`usize::MAX`,
// i.e. the wave's own size) rather than ending a run the tracker was
// only advising.
fn recommended_concurrency(
    this: &ReActLoop,
    agent_state_ref: &AgentState,
) -> usize {
    let bound = isolate(
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
    info!(
        concurrency = bound,
        "step 4 execute_tools: concurrency bound for this batch"
    );
    bound
}
