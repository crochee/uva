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

use elysper_provider::ToolUse;
use elysper_steering::{HookDecision, HookEvent};
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
    pub(super) cfg: elysper_provider::ModelConfig,
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
    pub(super) delegator_defs: Arc<Vec<elysper_provider::ToolDefinition>>,
    pub(super) delegator_fingerprint: u64,
    /// Built once so a tool call does not pay a `PathBuf` clone per
    /// dispatch.
    pub(super) ctx: elysper_tool::Context,
    /// The workspace identity grant matching uses, hashed once per
    /// run.
    pub(super) workspace_hash: String,
    pub(super) sink: EventSink,
    /// The run's steer channel: messages the host made durable while
    /// the run was in flight, drained at the top of every iteration.
    pub(super) inbox: Option<Arc<dyn crate::input::RunInbox>>,
    /// Whether this run opened a turn that finalize must close. A
    /// suspension writes its own `turn_end_suspended`, and a resumed
    /// run reopens the turn from the log, so the ordinary `turn_end`
    /// is written only when this run's own start is still open.
    pub(super) turn_open: bool,
}

impl ReActLoop {
    pub(crate) fn from_caps(
        caps: Arc<AgentCaps>,
        cfg: elysper_provider::ModelConfig,
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
        let ctx = elysper_tool::Context::new(
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
            inbox: None,
            turn_open: false,
        }
    }

    pub(crate) fn emit_typed(&self, event: elysper_session::SessionEvent) {
        self.sink.emit_typed(event);
    }

    pub(crate) fn emit_step_start(&self, step_idx: u32) {
        self.emit_typed(elysper_session::step_start(TURN_ORDINAL, step_idx));
    }

    pub(crate) fn emit_step_end_action(
        &self,
        step_idx: u32,
        action: &'static str,
    ) {
        self.emit_typed(elysper_session::step_end(
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
    ) -> elysper_tool::ExecutionMode {
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
    let agent_state = elysper_context::AgentState::from_config(&cfg);
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
        elysper.span.kind = %elysper_telemetry::SpanKind::Agent.as_str(),
        otel.kind = %elysper_telemetry::SpanKind::Agent.otel_kind(),
        agent.id = tracing::field::Empty,
        session.id = tracing::field::Empty,
    ),
)]
pub(super) async fn drive(
    mut this: ReActLoop,
    input: crate::input::AgentInput,
) {
    this.inbox = input.inbox.clone();
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
    // 0. The log's crash tail is repaired before this run opens its
    // own turn, so the repair cannot close a turn this run wrote.
    if let Err(reason) = repair_tail(&this).await {
        let cancel = Arc::clone(&this.run.cancel);
        let depth = this.run.subagent_depth;
        return this
            .drive_finalize(RunState::failed(cancel, depth, reason), false)
            .await;
    }
    // The turn boundary. One run is one turn: the start must be in the
    // log before the first step, so a crash mid-call leaves an open
    // turn for repair to close. A resumed run is the exception — the
    // resume reopens the turn it continues and would otherwise write a
    // second start.
    if input.resume.is_none() {
        this.emit_typed(elysper_session::turn_start(TURN_ORDINAL));
        this.turn_open = true;
    }
    // The run's first boundary: the host queued this run's prompt
    // before it invoked the agent, and setup writes rows of its own.
    // Both are on disk before the first model call.
    if let Err(reason) = this.flush_log().await {
        let cancel = Arc::clone(&this.run.cancel);
        let depth = this.run.subagent_depth;
        return this
            .drive_finalize(RunState::failed(cancel, depth, reason), false)
            .await;
    }
    // 1. Setup, the request envelope and the resume check.
    let mut session = match open_session(&mut this, &input).await {
        Ok(session) => session,
        Err(session) => return this.drive_finalize(*session, false).await,
    };
    // 2. The loop.
    let exhausted = run_iterations(&mut this, &mut session).await;
    // 4. Finalize.
    this.drive_finalize(session, exhausted).await
}

// Repair the log's crash tail before the run opens its own turn.
//
// Repair reads the log; a turn start written first would be the open
// tail the repair then closes as interrupted — every run would record
// a bogus `turn_end(interrupted)` ahead of its real one.
async fn repair_tail(this: &ReActLoop) -> Result<(), SessionEndReason> {
    let Some(log) = this.caps.env.log_writer.clone() else {
        return Ok(());
    };
    match log.repair().await {
        Ok(report) => {
            info!(
                closers = report.closers,
                last_seq = report.last_seq,
                "session setup: crash repair closed the tail"
            );
            Ok(())
        }
        Err(error) => Err(SessionEndReason::Error(format!(
            "session log repair failed: {error}"
        ))),
    }
}

// Setup, the request envelope and the resume check.
//
// Returns the run state on success, or the state a finalize call
// needs (its `end_reason` already names the failure). The envelope's fingerprint is derived
// before the resume runs; the row itself is written only after a
// resume has validated the token against the log's old tail.
async fn open_session(
    this: &mut ReActLoop,
    input: &crate::input::AgentInput,
) -> Result<RunState, Box<RunState>> {
    let mut session = match this.drive_setup(input).await {
        Ok(session) => session,
        Err(reason) => {
            let cancel = Arc::clone(&this.run.cancel);
            let depth = this.run.subagent_depth;
            return Err(Box::new(RunState::failed(cancel, depth, reason)));
        }
    };
    let offered = super::projection::offered_tools(
        &this.caps,
        &this.delegator_defs,
        this.delegator_fingerprint,
    );
    let snapshot = prepare::request_snapshot_event(
        &*this,
        &offered,
        &session.system_prompt,
    );
    let snapshot_hash =
        prepare::request_snapshot_hash(&snapshot).unwrap_or_default();
    session.request_snapshot_hash = Some(snapshot_hash.clone());
    if let Err(reason) =
        apply_resume(this, &mut session, input, &snapshot_hash).await
    {
        return Err(Box::new(session.with_end_reason(reason)));
    }
    // The sink owns the write, so every strategy leaves the same rows.
    this.emit_typed(snapshot);
    this.sink.stamp_request_header(&this.caps, &offered);
    Ok(session)
}

// Drive iterations until one ends the run. Returns whether the
// iteration budget was exhausted (the only "not our decision" end).
async fn run_iterations(this: &mut ReActLoop, session: &mut RunState) -> bool {
    for iteration in 0..this.caps.control.max_iterations {
        let step = drive_one_iteration(this, session, iteration).await;
        match step {
            Step::Continue(_) => {
                if let Some(reason) = session.terminate.take() {
                    session.end_reason = SessionEndReason::Error(reason);
                    return false;
                }
                // `after_step` (pi `finishTurn` parity): the one
                // interception point after the step's results are
                // committed. `Block` is meaningless here.
                let arbitration =
                    hooks::dispatch_hook(&*this, &session.agent_state, || {
                        HookEvent::AfterStep {
                            agent_name: this.caps.model.descriptor.name.clone(),
                            run_idx: 0,
                            iteration: iteration as u32,
                        }
                    })
                    .await;
                if let HookDecision::Terminate { reason } = arbitration.decision
                {
                    session.end_reason = SessionEndReason::Error(reason);
                    return false;
                }
            }
            Step::Suspended => {
                session.end_reason = SessionEndReason::Suspended;
                return false;
            }
            Step::Completed => {
                session.end_reason = SessionEndReason::Completed;
                return false;
            }
            Step::Failed(reason) => {
                session.end_reason = reason;
                return false;
            }
        }
    }
    true
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
    if input.resume.is_none() && resume::has_open_requests(&session.log_rows) {
        return Err(SessionEndReason::Error(
            "stale approval: the session has open approval requests; \
             resume them or cancel them first"
                .to_string(),
        ));
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
    if let Err(reason) = compaction::compact(this, session, iteration).await {
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
    // 6. Commit the assistant turn.
    let SampleOutcome {
        result,
        parts,
        assistant_seq,
    } = sample;
    sample::commit_assistant(session, parts, &result, assistant_seq);
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
// so a message that arrives while a batch is open waits in
// `deferred_user` and is released when the batch commits.
async fn drain_inbox(this: &ReActLoop, session: &mut RunState) {
    let Some(inbox) = this.inbox.as_ref() else {
        return;
    };
    let drained = inbox.drain().await;
    for message in drained {
        if session.pending_tool_calls.is_empty() {
            Arc::make_mut(&mut session.messages).push(message);
            // The host wrote the row; this loop does not know its
            // ordinal, so the message is never cited as provenance.
            session.surface_seqs.push(0);
        } else {
            session.deferred_user.push(message);
        }
    }
}

async fn iteration_start_setup(
    this: &mut ReActLoop,
    session: &mut RunState,
    iteration: usize,
) -> Option<Step> {
    session.agent_state.iteration_count = iteration + 1;
    for tracker in &this.caps.control.host.tracker_table().trackers {
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

/// The provider id a durable row records.
///
/// [`elysper_provider::ModelConfig::provider`] is the id the
/// deployment configured; [`elysper_provider::ModelProvider::name`]
/// is the *model* for an OpenAI-compatible implementation, so a row
/// built from the handle names the model twice and the provider is
/// unrecoverable from the log. A provider that leaves the id empty
/// falls back to its own name, so the row is never blank.
pub(super) fn provider_id<'a>(
    cfg: &'a elysper_provider::ModelConfig,
    provider: &'a dyn elysper_provider::ModelProvider,
) -> &'a str {
    if cfg.provider.is_empty() {
        provider.name()
    } else {
        cfg.provider.as_str()
    }
}

async fn sample_once_or_fail(
    this: &ReActLoop,
    session: &mut RunState,
    request: &super::state::Transcript,
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
    elysper_telemetry::record_llm_latency(provider_id, model, elapsed);
    match outcome {
        Ok(out) => {
            elysper_telemetry::record_llm_call(provider_id, model, "ok");
            Ok(out)
        }
        Err(reason) => {
            elysper_telemetry::record_llm_call(provider_id, model, "error");
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
    result: &elysper_provider::SamplingResult,
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
    result: &elysper_provider::SamplingResult,
    iteration: usize,
) -> Step {
    // A length stop means the assistant turn was cut off, so its
    // tool-call arguments may be silently truncated.
    if elysper_provider::is_length_stop_reason(&result.stop_reason) {
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
