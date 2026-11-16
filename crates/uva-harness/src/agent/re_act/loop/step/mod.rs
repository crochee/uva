//! One iteration of the ReAct loop: the seven-phase script and the
//! durable boundaries it owns.
//!
//! The interface is [`run`] — drive one iteration, return its
//! outcome. The phases it walks (`prepare`, `sample`, `tools`) are
//! this module's private implementation, and the boundaries that
//! used to be spread across siblings are written here and nowhere
//! else: the step row's one closer ([`close`], ordinary and
//! suspended alike) and the turn bookkeeping ([`TurnState`]).
//!
//! A suspension is not a crash. The approval rows and the boundary
//! rows are written and flushed before the event goes out, so the
//! operator's decision is made against a log that already says which
//! calls are waiting.

mod prepare;
mod sample;
mod tools;

// The run-level setup and the resume borrow four helpers from the
// phases: the request-envelope row `open_session` writes (and the
// resume's token check re-hashes), and the tool-result commit the
// resume's plan and a truncated batch share. These re-exports are
// that borrow, nothing more — the iteration surface stays [`run`].
pub(super) use prepare::{request_snapshot_event, request_snapshot_hash};
// The stream translator coalesces the same part kinds the commit does;
// the step module owns the one implementation.
pub(in crate::agent::re_act) use sample::coalesce_parts;
#[cfg(test)]
pub(in crate::agent::re_act) use tools::TOOL_OUTPUT_FLUSH_CHUNKS;
pub(super) use tools::{commit_tool_result, dispatch_tool_call};
use tracing::{info, warn};
use uva_approval::{ResumeToken, Suspension};
use uva_events::{SessionEndReason, SystemEvent, WarningKind};
use uva_provider::ToolUse;

use super::ReActLoop;
use crate::agent::{
    re_act::{
        panic::isolate,
        state::{RunState, SampleOutcome},
        transcript::RequestMessages,
    },
    sink::EventSink,
};

// One `agent.run()` call is one turn, so the turn ordinal is always 0.
const TURN_ORDINAL: u32 = 0;

/// One iteration body did. Each variant carries the `step_end` label
/// the `uva_session` wire records, so the event and the close
/// decision cannot drift apart: the variant, its label and the row
/// [`close`] writes for it live in this one module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Step {
    Continue(&'static str),
    Completed,
    // The batch held high-risk calls; the run ends here and a
    // resume continues it.
    Suspended,
    Failed(SessionEndReason),
}

impl Step {
    fn action(&self) -> &'static str {
        match self {
            Self::Continue(label) => label,
            Self::Completed => "completed",
            Self::Suspended => "suspended",
            Self::Failed(SessionEndReason::Cancelled) => "cancelled",
            Self::Failed(_) => "failed",
        }
    }
}

/// The run's turn bookkeeping: whether the turn this run opened is
/// still open. One `agent.run()` call is one turn — the start must
/// be in the log before the first step, so a crash mid-call leaves
/// an open turn for repair to close; a resumed run is the exception,
/// reopening the turn it continues instead of writing a second
/// start.
///
/// The flag is private to this module. The three moments that flip
/// it (a fresh run's open, a resume's reopen, a suspension's own
/// closer) and the one that reads it (finalize) all go through the
/// named operations below, so `turn_open` no longer crosses files.
#[derive(Default)]
pub(super) struct TurnState {
    // Whether finalize still owes this run's turn an ordinary close.
    open: bool,
}

impl TurnState {
    /// Write the turn's start row and mark the turn open. A fresh
    /// run opens its own turn; a resumed run reopens the suspended
    /// one it continues — the same row either way, so a crash
    /// between the resume's dispatch flush and its result leaves an
    /// open turn exactly like the ordinary mid-call crash window.
    pub(super) fn open(&mut self, sink: &EventSink) {
        sink.emit_typed(uva_session::turn_start(TURN_ORDINAL));
        self.open = true;
    }

    /// The suspension wrote this turn's closers itself; after it the
    /// ordinary close must not append a second `turn_end`.
    pub(super) fn closed_by_suspension(&mut self) {
        self.open = false;
    }

    /// Finalize's closer: write the ordinary `turn_end` when this
    /// run's own start is still open. A suspended run already wrote
    /// its `turn_end_suspended` at the suspension boundary, and a
    /// run that never opened an ordinary turn (a refused resume
    /// continues the suspended one) has nothing to close here.
    pub(super) fn close(
        &self,
        sink: &EventSink,
        end_reason: &SessionEndReason,
    ) {
        if !self.open || matches!(end_reason, SessionEndReason::Suspended) {
            return;
        }
        let mut event =
            uva_session::turn_end(TURN_ORDINAL, end_reason.as_str());
        // A failed turn's message is part of the outcome. Without
        // it the log says only "error", and every reader — the
        // page's banner, an operator reading the log — has to
        // guess why the run stopped.
        if let (
            SessionEndReason::Error(message),
            uva_session::SessionEvent::Turn { data, .. },
        ) = (end_reason, &mut event)
        {
            data["message"] = serde_json::Value::String(message.clone());
        }
        sink.emit_typed(event);
    }
}

/// Drive one iteration: the durable step boundary, the seven-phase
/// body, the step's one close, and the boundary again — so every row
/// the step produced is on disk before the next model call goes out.
pub(super) async fn run(
    this: &mut ReActLoop,
    session: &mut RunState,
    iteration: usize,
) -> Step {
    // The step boundary, before the model call: the run waits for the
    // disk here, so the facts the model is about to see are already
    // on it. The run's own prompt is one of them — the user's
    // message is durable before the model ever sees it, and a crash
    // in the window between the prompt and the answer keeps it.
    if let Err(reason) = this.flush_log().await {
        return Step::Failed(reason);
    }
    let outcome = iteration_body(this, session, iteration).await;
    let outcome = close(this, session, iteration as u32, outcome).await;
    // The step boundary is where the run waits for the disk: every
    // row this step produced is durable before the next model call
    // goes out, so a resume can say "it ran, the result is here".
    if let Err(reason) = this.flush_log().await {
        return Step::Failed(reason);
    }
    outcome
}

// The one place a finished iteration's step row is closed. Every
// outcome ends its step exactly once, here: the ordinary outcomes
// through the label `Step::action` carries, `Suspended` through the
// suspended shape — whose approval rows, turn closers and resume
// token are written in the same breath, in the order the log's
// readers already depend on (asked rows, `turn_suspended`,
// `step_end_suspended`, `turn_end_suspended`, then the flushed
// `Suspended` event).
async fn close(
    this: &mut ReActLoop,
    session: &mut RunState,
    iteration: u32,
    outcome: Step,
) -> Step {
    if !matches!(outcome, Step::Suspended) {
        this.emit_typed(uva_session::step_end(
            TURN_ORDINAL,
            iteration,
            outcome.action(),
        ));
        return outcome;
    }
    let requests = std::mem::take(&mut session.pending_approvals);
    let pending: Vec<String> = requests
        .iter()
        .map(|request| request.call_id.clone())
        .collect();
    for request in &requests {
        this.sink.append_typed(uva_session::approval_asked(
            &request.id,
            &request.call_id,
            &request.tool,
            &request.arguments_hash,
            request.risk.as_str(),
            &request.agent,
        ));
    }
    this.sink
        .append_typed(uva_session::turn_suspended(pending.clone()));
    this.sink.append_typed(uva_session::step_end_suspended());
    let last_seq = this.sink.append_typed(uva_session::turn_end_suspended());
    // The sync barrier: the approval rows must survive the crash the
    // suspension is meant to survive.
    if let Err(reason) = this.flush_log().await {
        return Step::Failed(reason);
    }
    // The suspended end above is this turn's one closer. Clear the
    // open-turn flag so a later flush failure (which ends the run as
    // `Error`) cannot append a second, ordinary `turn_end` after it.
    this.turn.closed_by_suspension();
    let hash = session.request_snapshot_hash.clone().unwrap_or_default();
    let suspension = Suspension::AwaitingApproval {
        interrupts: requests,
        resume: ResumeToken::new(last_seq, pending, hash),
    };
    this.sink.system(SystemEvent::Suspended(suspension));
    Step::Suspended
}

async fn iteration_body(
    this: &mut ReActLoop,
    session: &mut RunState,
    iteration: usize,
) -> Step {
    // 1. Prepare: counter bump, `iteration_start`, cancel check.
    if let Some(step) = iteration_start_setup(this, session, iteration).await {
        return step;
    }
    // 2. The run's in-memory transcript is the source of truth: the
    // log was read once at setup, and every message this loop commits
    // carries the ordinal its row was written under. A steer the host
    // made durable while the run was in flight arrives here.
    drain_inbox(this, session).await;
    // 3. Compaction sits before the request: plan over the transcript
    // the loop holds, summarise, write the replace, splice the result
    // back into the transcript. A summariser failure is not fatal —
    // the run continues uncompacted.
    if let Err(reason) =
        super::compaction::compact(this, session, iteration).await
    {
        return Step::Failed(reason);
    }
    // 4. The request: system prompt + durable transcript + this
    // turn's bytes, then the request-local prune, the runtime
    // snapshot and the hints. None of it is written back.
    let request = match this.build_request(session, iteration).await {
        Ok(request) => request,
        Err(reason) => return Step::Failed(reason),
    };
    this.sink.system(SystemEvent::Progress {
        message: format!("LLM pass {iteration}"),
        step: iteration,
        total: this.caps.control.max_iterations,
    });
    // 5. Sample.
    let sample =
        match sample_once_or_fail(this, session, &request, iteration).await {
            Ok(sample) => sample,
            Err(reason) => return Step::Failed(reason),
        };
    // 6. Commit the assistant turn: the one message the log row
    // serialized enters the transcript under the same ordinal.
    let SampleOutcome {
        result,
        message,
        assistant_seq,
    } = sample;
    sample::commit_assistant(session, message, &result, assistant_seq);
    // 7. Dispatch: the tool batch, or the final-answer path.
    if !result.tool_calls.is_empty() {
        handle_tool_calls(this, session, &result, iteration).await
    } else {
        handle_final_answer(&result, iteration).await
    }
}

// Move the messages the host logged mid-run into the transcript.
//
// A user turn may not interleave a `tool_use` and its `tool_result`,
// so a message that arrives while a batch is open waits for the
// window to close and is released when the batch commits.
async fn drain_inbox(this: &ReActLoop, session: &mut RunState) {
    let Some(inbox) = this.inbox.as_ref() else {
        return;
    };
    let drained = inbox.drain().await;
    for message in drained {
        // The host wrote the row; this loop does not know its
        // ordinal, so the message is never cited as provenance.
        session.transcript.deliver_user(message);
    }
}

async fn iteration_start_setup(
    this: &mut ReActLoop,
    session: &mut RunState,
    iteration: usize,
) -> Option<Step> {
    session.agent_state.iteration_count = iteration + 1;
    for tracker in &this.caps.control.seams.tracker_table() {
        isolate(
            &this.sink,
            WarningKind::Hook,
            None,
            "tracker.on_iteration",
            || {
                tracker.on_iteration(&session.agent_state);
            },
        );
    }
    // Emit before the cancel check so a step that never samples
    // still has its typed boundary.
    this.emit_typed(uva_session::step_start(TURN_ORDINAL, iteration as u32));
    if this.cancelled("cancelled before LLM call") {
        info!(
            iteration,
            "cancelled before LLM call; ending session with Cancelled"
        );
        return Some(Step::Failed(SessionEndReason::Cancelled));
    }
    None
}

async fn sample_once_or_fail(
    this: &ReActLoop,
    session: &mut RunState,
    request: &RequestMessages,
    iteration: usize,
) -> Result<SampleOutcome, SessionEndReason> {
    // `provider_id` is also baked into the LLM span fields;
    // the metric wrappers (`record_llm_call` /
    // `record_llm_latency`) are unconditional no-ops when the
    // consumer has not enabled OTel, so the same source feeds
    // both the span and the metric without a feature split.
    // Both labels borrow `this.cfg`, read once when the loop was
    // built — no provider call and no `ModelConfig` allocation
    // per iteration.
    let provider_id = this.cfg.provider.as_str();
    let model = this.cfg.name.as_str();
    let started = std::time::Instant::now();
    // Destructuring borrow replaces the old `mem::replace` /
    // `mem::replace` round-trip: one split of `RunState`'s
    // fields, no throwaway `AgentState` / empty `Arc<Vec<_>>`.
    let RunState {
        agent_state,
        projection,
        model_hint,
        ..
    } = &mut *session;
    let outcome = sample::sample_once(
        this,
        request,
        agent_state,
        projection,
        model_hint.as_ref(),
        iteration,
    )
    .await;
    let elapsed = started.elapsed().as_secs_f64();
    uva_telemetry::record_llm_latency(provider_id, model, elapsed);
    match outcome {
        Ok(out) => {
            uva_telemetry::record_llm_call(provider_id, model, "ok");
            Ok(out)
        }
        Err(reason) => {
            uva_telemetry::record_llm_call(provider_id, model, "error");
            info!(
                iteration,
                ?reason,
                "sample_once returned Err; ending session"
            );
            Err(reason)
        }
    }
}

// ===== iteration routing =============================================

async fn handle_final_answer(
    result: &uva_provider::SamplingResult,
    iteration: usize,
) -> Step {
    info!(
        iteration,
        assistant_text_len = result.text.len(),
        "no tool calls returned by model; ending session with Completed"
    );
    Step::Completed
}

async fn handle_tool_calls(
    this: &mut ReActLoop,
    session: &mut RunState,
    result: &uva_provider::SamplingResult,
    iteration: usize,
) -> Step {
    // A length stop means the assistant turn was cut off, so its
    // tool-call arguments may be silently truncated.
    if uva_provider::is_length_stop_reason(&result.stop_reason) {
        return dispatch_truncated_batch(
            this,
            session,
            &result.tool_calls,
            iteration,
        )
        .await;
    }
    if !result.tool_calls.is_empty()
        && this.cancelled("cancelled before tool execution")
    {
        info!(
            iteration,
            "cancelled before tool execution; ending session with Cancelled"
        );
        return Step::Failed(SessionEndReason::Cancelled);
    }
    dispatch_execute_tools(this, session, &result.tool_calls, iteration).await
}

async fn dispatch_truncated_batch(
    this: &ReActLoop,
    session: &mut RunState,
    tool_uses: &[ToolUse],
    iteration: usize,
) -> Step {
    warn!(
        iteration,
        tool_call_count = tool_uses.len(),
        "length stop with tool calls; failing the batch instead of executing"
    );
    sample::fail_truncated_tool_batch(this, session, tool_uses).await;
    Step::Continue("tools_truncated")
}

async fn dispatch_execute_tools(
    this: &mut ReActLoop,
    session: &mut RunState,
    tool_uses: &[ToolUse],
    iteration: usize,
) -> Step {
    info!(
        iteration,
        tool_call_count = tool_uses.len(),
        "step 4: executing tool calls returned by LLM"
    );
    if let Err(reason) = tools::execute_tools(this, session, tool_uses).await {
        return Step::Failed(reason);
    }
    if !session.pending_approvals.is_empty() {
        return Step::Suspended;
    }
    Step::Continue("tools_complete")
}
