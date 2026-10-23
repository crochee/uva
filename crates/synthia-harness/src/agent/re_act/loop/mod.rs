//! The ReAct loop's run state machine: the loop struct and the per-iteration order (checkpoint → sample → commit → dispatch).
mod finalize;
mod guard;
mod hooks;
mod prepare;

pub(super) mod sample;
pub(super) mod tools;

use std::sync::Arc;

use synthia_provider::ToolUse;
use synthia_steering::{HookDecision, HookEvent};
use tracing::{info, warn};

use super::{
    panic::isolate,
    state::{RunState, SampleOutcome, Step},
};
use crate::{
    AgentCaps,
    agent::{caps::AgentRuntime, sink::EventSink},
    events::{SessionEndReason, SystemEvent, WarningKind},
};

pub(crate) struct ReActLoop {
    /// An `Arc` because a [`crate::ReasoningStrategy`] may move the
    /// runtime into a spawned task; sharing the handle avoids an
    /// [`AgentCaps::clone`] per run.
    pub(super) caps: Arc<AgentCaps>,
    /// Read once when the loop was built so a whole run costs exactly one
    /// provider read.
    pub(super) cfg: synthia_provider::ModelConfig,
    pub(super) run: RunState,
    /// Every synthetic plugin's advertised definitions, collected
    /// once per run: the fingerprint and the projection's compose
    /// step share this one build.
    pub(super) synthetic_defs: Arc<Vec<synthia_provider::ToolDefinition>>,
    pub(super) synthetic_fingerprint: u64,
    /// Built once so a tool call does not pay a `PathBuf` clone per
    /// dispatch.
    pub(super) ctx: synthia_tool::Context,
    pub(super) sink: EventSink,
}

impl ReActLoop {
    pub(crate) fn from_caps(
        caps: Arc<AgentCaps>,
        cfg: synthia_provider::ModelConfig,
        run: RunState,
        sink: EventSink,
    ) -> Self {
        let synthetic_defs: Vec<_> = caps
            .tools
            .synthetic
            .iter()
            .flat_map(|t| t.definitions())
            .collect();
        let synthetic_fingerprint =
            super::projection::tool_names_hash(&synthetic_defs);
        let ctx = synthia_tool::Context::new(
            String::new(),
            caps.env.workspace_root.clone(),
        );
        Self {
            caps,
            cfg,
            run,
            synthetic_defs: Arc::new(synthetic_defs),
            synthetic_fingerprint,
            ctx,
            sink,
        }
    }

    pub(crate) fn emit_typed(&self, event: synthia_session::SessionEvent) {
        self.sink.emit_typed(event);
    }

    pub(crate) fn emit_step_start(&self, step_idx: u32) {
        self.emit_typed(synthia_session::step_start(TURN_ORDINAL, step_idx));
    }

    pub(crate) fn emit_step_end_action(
        &self,
        step_idx: u32,
        action: &'static str,
    ) {
        self.emit_typed(synthia_session::step_end(
            TURN_ORDINAL,
            step_idx,
            action,
        ));
    }

    pub(crate) fn cancelled(&self, reason: &str) -> bool {
        hooks::cancelled(&*self.run.cancel, &self.sink, reason)
    }

    /// Write the buffered rows. The only moment the run waits for
    /// the disk: the step boundary, where the next model call is
    /// about to happen anyway.
    ///
    /// A failure is fatal — the run's fail-fast rule: a row the
    /// reader cannot see is a hole in the log, and a hole makes
    /// every later compaction unresolvable.
    pub(crate) async fn flush_log(&self) -> Result<(), SessionEndReason> {
        let Some(writer) = self.caps.env.log_writer.as_ref() else {
            return Ok(());
        };
        writer.flush().await.map_err(|e| {
            SessionEndReason::Error(format!("log flush failed: {e}"))
        })
    }
}

/// One `agent.run()` call is one turn, so the turn ordinal is always 0.
pub(super) const TURN_ORDINAL: u32 = 0;

pub(crate) async fn drive_from_runtime(
    runtime: AgentRuntime,
    sink: &EventSink,
    input: crate::input::AgentInput,
) {
    let cfg = runtime.caps.model.provider.model_config();
    let model_hint = runtime.caps.model.descriptor.model_hint.clone();
    let cancel = Arc::clone(&runtime.cancel);
    let subagent_depth = runtime.subagent_depth;
    let agent_state = synthia_context::AgentState::from_config(&cfg);
    let run_state = RunState::seed(
        cancel,
        subagent_depth,
        Arc::new(Vec::new()),
        agent_state,
    )
    .with_model_hint(model_hint);
    let loop_ =
        ReActLoop::from_caps(runtime.caps, cfg, run_state, sink.clone());
    drive(loop_, input).await;
}

// ===== drive ==========================================================

#[tracing::instrument(
    name = "react_loop",
    level = "info",
    skip_all,
    fields(
        agent = %this.caps.model.descriptor.name,
        max_iterations = this.caps.control.max_iterations,
        synthia.span.kind = %synthia_telemetry::SpanKind::Agent.as_str(),
        otel.kind = %synthia_telemetry::SpanKind::Agent.otel_kind(),
        agent.id = tracing::field::Empty,
        session.id = tracing::field::Empty,
    ),
)]
pub(super) async fn drive(
    mut this: ReActLoop,
    input: crate::input::AgentInput,
) {
    // The instrument fields above are declared `Empty`; record the
    // in-scope identities now. The agent's registry key is its
    // name; the session id is the caller's `thread_id` when the run
    // is part of a durable thread.
    let span = tracing::Span::current();
    span.record("agent.id", this.caps.model.descriptor.name.as_str());
    if let Some(thread_id) = input.thread_id.as_deref() {
        span.record("session.id", thread_id);
    }
    info!(
        history_len = input.history.len(),
        "session start: preparing messages and dispatching LLM loop"
    );
    // 1. Setup.
    let mut session = match this.drive_setup(&input).await {
        Ok(session) => session,
        Err(reason) => {
            let cancel = Arc::clone(&this.run.cancel);
            let depth = this.run.subagent_depth;
            return this
                .drive_finalize(RunState::failed(cancel, depth, reason), false)
                .await;
        }
    };

    // 2. Stamp the request header once per run.
    this.stamp_request_header();

    // 3. Loop.
    let mut exhausted = true;
    for iteration in 0..this.caps.control.max_iterations {
        let step =
            drive_one_iteration(&mut this, &mut session, iteration).await;
        match step {
            Step::Continue(_) => {
                // `after_step` (pi `finishTurn` parity): the step's
                // results are committed and the loop would sample
                // another iteration — the one interception point
                // that can stop it. `Block` is meaningless here and
                // falls through like `Allow`.
                if let HookDecision::Terminate { reason } =
                    hooks::dispatch_hook(&this.caps.control.steering, || {
                        HookEvent::AfterStep {
                            agent_name: this.caps.model.descriptor.name.clone(),
                            run_idx: 0,
                            iteration: iteration as u32,
                        }
                    })
                    .await
                {
                    exhausted = false;
                    session.end_reason = SessionEndReason::Error(reason);
                    break;
                }
            }
            Step::Completed => {
                exhausted = false;
                session.end_reason = SessionEndReason::Completed;
                break;
            }
            Step::Failed(reason) => {
                exhausted = false;
                session.end_reason = reason;
                break;
            }
        }
    }

    // 4. Finalize.
    this.drive_finalize(session, exhausted).await
}

async fn drive_one_iteration(
    this: &mut ReActLoop,
    session: &mut RunState,
    iteration: usize,
) -> Step {
    let step = iteration_body(this, session, iteration).await;
    this.emit_step_end_action(iteration as u32, step.action());
    // The step boundary is where the run waits for the disk: every
    // row this step produced is durable before the next model call
    // goes out, so a resume can say "it ran, the result is here".
    if let Err(reason) = this.flush_log().await {
        return Step::Failed(reason);
    }
    step
}

async fn iteration_body(
    this: &mut ReActLoop,
    session: &mut RunState,
    iteration: usize,
) -> Step {
    // 1. Prepare: counter bump, `iteration_start`, cancel check,
    // then the checkpoint.
    if let Some(step) = iteration_start_setup(this, session, iteration).await {
        return step;
    }
    // Iteration 0's checkpoint ran at the end of `drive_setup` —
    // one checkpoint per *boundary*, and setup is the boundary
    // before the first sample.
    if iteration > 0
        && let Err(reason) = prepare::checkpoint(this, session, iteration).await
    {
        return Step::Failed(reason);
    }
    this.sink.system(SystemEvent::Progress {
        message: format!("LLM pass {iteration}"),
        step: iteration,
        total: this.caps.control.max_iterations,
    });
    // 2. Sample.
    let sample = match sample_once_or_fail(this, session, iteration).await {
        Ok(sample) => sample,
        Err(reason) => return Step::Failed(reason),
    };
    // 3. Commit the assistant turn.
    let SampleOutcome { result, parts } = sample;
    sample::commit_assistant(session, parts, &result);
    // 4. Dispatch: the tool batch, or the final-answer path.
    if !result.tool_calls.is_empty() {
        handle_tool_calls(this, session, &result, iteration).await
    } else {
        handle_final_answer(&result, iteration).await
    }
}

async fn iteration_start_setup(
    this: &mut ReActLoop,
    session: &mut RunState,
    iteration: usize,
) -> Option<Step> {
    session.agent_state.iteration_count = iteration + 1;
    isolate(
        &this.sink,
        WarningKind::Hook,
        None,
        "tracker.on_iteration",
        || {
            this.caps
                .control
                .steering
                .tracker
                .on_iteration(&session.agent_state);
        },
    );
    // Emit before the cancel check so a step that never samples
    // still has its typed boundary.
    this.emit_step_start(iteration as u32);
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
        messages,
        agent_state,
        projection,
        model_hint,
        ..
    } = &mut *session;
    let outcome = sample::sample_once(
        this,
        messages,
        agent_state,
        projection,
        model_hint.as_ref(),
        iteration,
    )
    .await;
    let elapsed = started.elapsed().as_secs_f64();
    synthia_telemetry::record_llm_latency(provider_id, model, elapsed);
    match outcome {
        Ok(out) => {
            synthia_telemetry::record_llm_call(provider_id, model, "ok");
            Ok(out)
        }
        Err(reason) => {
            synthia_telemetry::record_llm_call(provider_id, model, "error");
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

pub(super) async fn handle_final_answer(
    result: &synthia_provider::SamplingResult,
    iteration: usize,
) -> Step {
    info!(
        iteration,
        assistant_text_len = result.text.len(),
        "no tool calls returned by model; ending session with Completed"
    );
    Step::Completed
}

pub(super) async fn handle_tool_calls(
    this: &mut ReActLoop,
    session: &mut RunState,
    result: &synthia_provider::SamplingResult,
    iteration: usize,
) -> Step {
    // A length stop means the assistant turn was cut off, so its
    // tool-call arguments may be silently truncated.
    if synthia_provider::is_length_stop_reason(&result.stop_reason) {
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

pub(super) async fn dispatch_truncated_batch(
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
    sample::fail_truncated_tool_batch(this, session, tool_uses);
    Step::Continue("tools_truncated")
}

pub(super) async fn dispatch_execute_tools(
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
    tools::execute_tools(this, session, tool_uses).await;
    Step::Continue("tools_complete")
}
