//! The ReAct loop's run state machine: the loop struct and the per-iteration order (checkpoint → sample → commit → dispatch).
mod compaction;
mod finalize;
mod guard;
mod hooks;
mod prepare;
mod resume;
mod suspend;

pub(super) mod sample;
pub(super) mod tools;

use std::sync::Arc;

use synthia_provider::ToolUse;
use synthia_steering::{HookDecision, HookEvent};
use tracing::{info, warn};

use super::{
    delegation::Delegators,
    panic::isolate,
    state::{RunState, SampleOutcome, Step},
};
use crate::{
    AgentCaps,
    agent::{caps::AgentRuntime, sink::EventSink},
    events::{SessionEndReason, SystemEvent, WarningKind},
    prompt::PromptContext,
};

pub(crate) struct ReActLoop {
    /// An `Arc` because a [`crate::ReasoningStrategy`] may move the
    /// runtime into a spawned task; sharing the handle avoids an
    /// [`AgentCaps::clone`] per run.
    pub(super) caps: Arc<AgentCaps>,
    /// Read once when the loop was built so a whole run costs exactly one
    /// provider read.
    pub(super) cfg: synthia_provider::ModelConfig,
    /// The manifest this run renders, snapshotted once in
    /// [`ReActLoop::from_caps`]: the dispatch's context (or the agent's
    /// default) plus the host's prompt sections in registration order.
    /// A section a plugin registers during the run appears on the next
    /// run, not this one.
    pub(super) prompt_context: Arc<PromptContext>,
    pub(super) run: RunState,
    /// Every delegator the loop dispatches, indexed by the names it is
    /// advertised under: built once per run from the agent's own installs
    /// and its host's registrations, so dispatch is one table hit.
    pub(super) delegators: Delegators,
    /// Every delegator's advertised definitions, collected once per run:
    /// the fingerprint and the projection's compose step share this one
    /// build.
    pub(super) delegator_defs: Arc<Vec<synthia_provider::ToolDefinition>>,
    pub(super) delegator_fingerprint: u64,
    /// Built once so a tool call does not pay a `PathBuf` clone per
    /// dispatch.
    pub(super) ctx: synthia_tool::Context,
    /// The workspace identity grant matching uses, hashed once per
    /// run.
    pub(super) workspace_hash: String,
    pub(super) sink: EventSink,
    /// Whether this run opened a turn that finalize must close. A
    /// suspension writes its own `turn_end_suspended`, and a resumed
    /// run reopens the turn from the log, so the ordinary `turn_end`
    /// is written only when this run's own start is still open.
    pub(super) turn_open: bool,
}

impl ReActLoop {
    pub(crate) fn from_caps(
        caps: Arc<AgentCaps>,
        cfg: synthia_provider::ModelConfig,
        run: RunState,
        sink: EventSink,
        prompt_context: Arc<PromptContext>,
    ) -> Self {
        let mut delegators = Delegators::default();
        for tool in &caps.tools.delegators {
            delegators.merge(Arc::clone(tool));
        }
        for tool in caps.control.host.delegators() {
            delegators.merge(tool);
        }
        let delegator_defs = delegators.definitions();
        let delegator_fingerprint =
            super::projection::tool_names_hash(&delegator_defs);
        let ctx = synthia_tool::Context::new(
            String::new(),
            caps.env.workspace_root.clone(),
        );
        let workspace_hash =
            crate::agent::approval::workspace_hash(&caps.env.workspace_root);
        Self {
            caps,
            cfg,
            prompt_context,
            run,
            delegators,
            delegator_defs: Arc::new(delegator_defs),
            delegator_fingerprint,
            ctx,
            workspace_hash,
            sink,
            turn_open: false,
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

    /// The execution mode a call's name declares: a delegator's own mode,
    /// else the registry's per-name mode.
    pub(super) fn execution_mode(
        &self,
        name: &str,
    ) -> synthia_tool::ExecutionMode {
        self.delegators
            .mode(name)
            .unwrap_or_else(|| self.caps.tools.registry.execution_mode(name))
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
        self.sink.flush_log().await.map_err(|e| {
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
    // The run's manifest, snapshotted here and nowhere later: this is
    // the boundary that makes a plugin's prompt section visible from
    // the next run on, and keeps the prompt byte-stable inside this one.
    let prompt_context = runtime
        .caps
        .run_prompt_context(input.prompt_context.as_ref());
    let loop_ = ReActLoop::from_caps(
        runtime.caps,
        cfg,
        run_state,
        sink.clone(),
        prompt_context,
    );
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
    // 0. The turn boundary. One run is one turn: the start must be in
    // the log before the first step, so a crash mid-call leaves an
    // open turn for repair to close. A resumed run is the exception —
    // the resume reopens the turn it continues and would otherwise
    // write a second start.
    if input.resume.is_none() {
        this.emit_typed(synthia_session::turn_start(TURN_ORDINAL));
        this.turn_open = true;
    }

    // The run's first boundary: the host queued this run's prompt
    // before it invoked the agent, and setup writes rows of its own (a
    // compaction pass can run here). Both are on disk before the first
    // model call, and a crash between the prompt and the answer keeps
    // the prompt.
    if let Err(reason) = this.flush_log().await {
        let cancel = Arc::clone(&this.run.cancel);
        let depth = this.run.subagent_depth;
        return this
            .drive_finalize(RunState::failed(cancel, depth, reason), false)
            .await;
    }

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

    // 2. The request envelope. Its fingerprint is what a resume is
    // checked against, so it is derived before the resume runs; the
    // row itself is written only after a resume has validated the
    // token against the log's old tail.
    let offered = super::projection::offered_tools(
        &this.caps,
        &this.delegator_defs,
        this.delegator_fingerprint,
    );
    let snapshot = prepare::request_snapshot_event(&this, &offered);
    let snapshot_hash =
        prepare::request_snapshot_hash(&snapshot).unwrap_or_default();
    session.request_snapshot_hash = Some(snapshot_hash.clone());
    if let Err(reason) =
        apply_resume(&mut this, &mut session, &input, &snapshot_hash).await
    {
        return this
            .drive_finalize(session.with_end_reason(reason), false)
            .await;
    }
    // The sink owns the write, so every strategy leaves the same rows.
    this.emit_typed(snapshot);
    this.sink.stamp_request_header(&this.caps, &offered);

    // 3. Loop.
    let mut exhausted = true;
    for iteration in 0..this.caps.control.max_iterations {
        let step =
            drive_one_iteration(&mut this, &mut session, iteration).await;
        match step {
            Step::Continue(_) => {
                // A policy that ended the run after a tool
                // returned: the step's results are committed, and
                // the run stops here.
                if let Some(reason) = session.terminate.take() {
                    exhausted = false;
                    session.end_reason = SessionEndReason::Error(reason);
                    break;
                }
                // `after_step` (pi `finishTurn` parity): the step's
                // results are committed and the loop would sample
                // another iteration — the one interception point
                // that can stop it. `Block` is meaningless here and
                // falls through like `Allow`.
                let arbitration =
                    hooks::dispatch_hook(&this.caps.control, || {
                        HookEvent::AfterStep {
                            agent_name: this.caps.model.descriptor.name.clone(),
                            run_idx: 0,
                            iteration: iteration as u32,
                        }
                    })
                    .await;
                if let HookDecision::Terminate { reason } = arbitration.decision
                {
                    exhausted = false;
                    session.end_reason = SessionEndReason::Error(reason);
                    break;
                }
            }
            Step::Suspended => {
                // The suspension row and its event are already out;
                // the run ends here and a resume continues it.
                exhausted = false;
                session.end_reason = SessionEndReason::Suspended;
                break;
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

// Refuse a fresh run over pending approvals, then resume when the
// input names a token. The log's tail is still the suspension's: the
// new request snapshot has not been written yet.
async fn apply_resume(
    this: &mut ReActLoop,
    session: &mut RunState,
    input: &crate::input::AgentInput,
    snapshot_hash: &str,
) -> Result<(), SessionEndReason> {
    if input.resume.is_none()
        && let Some(writer) = this.caps.env.log_writer.as_ref()
    {
        let rows = writer.read().await.map_err(|error| {
            SessionEndReason::Error(format!("session log read failed: {error}"))
        })?;
        if resume::has_open_requests(&rows) {
            return Err(SessionEndReason::Error(
                "stale approval: the session has open approval requests; \
                 resume them or cancel them first"
                    .to_string(),
            ));
        }
    }
    if let Some(token) = input.resume.clone() {
        resume::resume_suspended(
            this,
            session,
            &token,
            &input.resume_decisions,
            input.allow_drift,
            snapshot_hash,
        )
        .await?;
    }
    Ok(())
}

async fn drive_one_iteration(
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
    let step = iteration_body(this, session, iteration).await;
    if matches!(step, Step::Suspended) {
        // The approval rows and the suspended boundary are the
        // suspension's durability: write them, then the `Suspended`
        // event (the boundary flushes first).
        if let Err(reason) = suspend::boundary(this, session).await {
            return Step::Failed(reason);
        }
    } else {
        this.emit_step_end_action(iteration as u32, step.action());
    }
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
    // The log is the transcript's source of truth: re-derive it at
    // the boundary, before the request is assembled. A log-only row
    // (the request header, the step's own structure) moves the tail
    // but not the transcript, so this costs one row scan.
    if let Err(reason) = prepare::refresh_history(this, session).await {
        return Step::Failed(reason);
    }
    // Compaction sits on the same boundary, after the projection is
    // fresh and before the request is assembled: `plan` →
    // `summarize` → durable replace → re-derive. A summariser
    // failure is not fatal — the run continues uncompacted.
    if let Err(reason) = compaction::compact(this, session, iteration).await {
        return Step::Failed(reason);
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
    sample::fail_truncated_tool_batch(this, session, tool_uses).await;
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
    if let Err(reason) = tools::execute_tools(this, session, tool_uses).await {
        return Step::Failed(reason);
    }
    if !session.pending_approvals.is_empty() {
        return Step::Suspended;
    }
    Step::Continue("tools_complete")
}
