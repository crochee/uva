//! The ReAct loop's run state machine: the loop struct and the run-level order (repair → open → iterate → finalize).
mod compaction;
mod finalize;
mod guard;
mod hooks;
mod resume;
pub(super) mod step;

use std::sync::Arc;

use tracing::info;
use uva_events::SessionEndReason;
use uva_steering::HookDecision;

use super::{state::RunState, transcript::Transcript};
use crate::{
    AgentCaps,
    agent::{caps::AgentRuntime, sink::EventSink},
    prompt::PromptContext,
};

pub(crate) struct ReActLoop {
    /// An `Arc` because a [`crate::ReasoningStrategy`] may move the
    /// runtime into a spawned task; sharing the handle avoids an
    /// [`AgentCaps::clone`] per run.
    pub(super) caps: Arc<AgentCaps>,
    /// Read once when the loop was built so a whole run costs exactly one
    /// provider read.
    pub(super) cfg: uva_provider::ModelConfig,
    /// The manifest this run renders, snapshotted once in
    /// [`ReActLoop::from_caps`]: the dispatch's context (or the agent's
    /// default) plus the host's prompt sections in registration order.
    /// A section a plugin registers during the run appears on the next
    /// run, not this one.
    pub(super) prompt_context: Arc<PromptContext>,
    pub(super) run: RunState,
    /// Built once so a tool call does not pay a `PathBuf` clone per
    /// dispatch.
    pub(super) ctx: uva_tool::Context,
    /// The workspace identity grant matching uses, hashed once per
    /// run.
    pub(super) workspace_hash: String,
    pub(super) sink: EventSink,
    /// The run's steer channel: messages the host made durable while
    /// the run was in flight, drained at the top of every iteration.
    pub(super) inbox: Option<Arc<dyn crate::input::RunInbox>>,
    /// The run's turn bookkeeping. Owned by the step module: it opens
    /// (here, for a fresh run), reopens (in the resume) and closes
    /// (in finalize) through the named operations, so the flag never
    /// crosses files.
    pub(in crate::agent::re_act::r#loop) turn: step::TurnState,
}

impl ReActLoop {
    pub(crate) fn from_caps(
        caps: Arc<AgentCaps>,
        cfg: uva_provider::ModelConfig,
        run: RunState,
        sink: EventSink,
        prompt_context: Arc<PromptContext>,
    ) -> Self {
        let ctx = uva_tool::Context::new(
            String::new(),
            caps.env.workspace_root.clone(),
        )
        .with_cancel(Arc::clone(&run.cancel));
        let workspace_hash =
            uva_approval::workspace_hash(&caps.env.workspace_root);
        Self {
            caps,
            cfg,
            prompt_context,
            run,
            ctx,
            workspace_hash,
            sink,
            inbox: None,
            turn: step::TurnState::default(),
        }
    }

    pub(crate) fn emit_typed(&self, event: uva_session::SessionEvent) {
        self.sink.emit_typed(event);
    }

    /// The execution mode **this call** declares: the registry's
    /// per-call classification over the call's own arguments.
    pub(super) fn execution_mode(
        &self,
        call: &uva_provider::ToolUse,
    ) -> uva_tool::ExecutionMode {
        self.caps
            .tools
            .registry
            .execution_mode_for(&call.name, &call.input)
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

pub(crate) async fn drive_from_runtime(
    runtime: AgentRuntime,
    sink: &EventSink,
    input: crate::input::AgentInput,
) {
    let cfg = runtime.caps.model.provider.model_config();
    let model_hint = runtime.caps.model.descriptor.model_hint.clone();
    let cancel = Arc::clone(&runtime.cancel);
    let agent_state = uva_context::AgentState::from_config(&cfg);
    let run_state = RunState::seed(cancel, Transcript::empty(), agent_state)
        .with_model_hint(model_hint);
    // The run's manifest, snapshotted here and nowhere later: this is
    // the boundary that makes a plugin's prompt section visible from
    // the next run on, and keeps the prompt byte-stable inside this one.
    let prompt_context = runtime
        .caps
        .run_prompt_context(input.prompt_context.as_ref());
    let mut loop_ = ReActLoop::from_caps(
        runtime.caps,
        cfg,
        run_state,
        sink.clone(),
        prompt_context,
    );
    // The tool context's session identity is the caller's `thread_id`
    // when the run belongs to a durable thread: tools that keep
    // per-session state (the `task` tool's child bookkeeping) or
    // attribute spilled output read it there. A session-less run
    // keeps the empty id `Context::new` starts with.
    loop_.ctx.session_id = input.thread_id.clone().unwrap_or_default();
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
        uva.span.kind = %uva_telemetry::SpanKind::Agent.as_str(),
        otel.kind = %uva_telemetry::SpanKind::Agent.otel_kind(),
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
        return this
            .drive_finalize(RunState::failed(cancel, reason), false)
            .await;
    }
    // The turn boundary. One run is one turn: the start must be in the
    // log before the first step, so a crash mid-call leaves an open
    // turn for repair to close. A resumed run is the exception — the
    // resume reopens the turn it continues and would otherwise write a
    // second start.
    if input.resume.is_none() {
        this.turn.open(&this.sink);
    }
    // The run's first boundary: the host queued this run's prompt
    // before it invoked the agent, and setup writes rows of its own.
    // Both are on disk before the first model call.
    if let Err(reason) = this.flush_log().await {
        let cancel = Arc::clone(&this.run.cancel);
        return this
            .drive_finalize(RunState::failed(cancel, reason), false)
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
            return Err(Box::new(RunState::failed(cancel, reason)));
        }
    };
    let offered = super::projection::offered_tools(&this.caps);
    let snapshot =
        step::request_snapshot_event(&*this, &offered, &session.system_prompt);
    let snapshot_hash =
        step::request_snapshot_hash(&snapshot).unwrap_or_default();
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
        let step = step::run(this, session, iteration).await;
        match step {
            step::Step::Continue(_) => {
                if let Some(reason) = session.terminate.take() {
                    session.end_reason = SessionEndReason::Error(reason);
                    return false;
                }
                // `step_post` (pi `finishTurn` parity): the one
                // interception point after the step's results are
                // committed. `Block` is meaningless here.
                let arbitration = {
                    let ctx = this.hook_context(&session.agent_state);
                    this.caps
                        .control
                        .seams
                        .step_post(
                            &ctx,
                            &this.sink,
                            &this.caps.model.descriptor.name,
                            iteration as u32,
                        )
                        .await
                };
                if let HookDecision::Terminate { reason } = arbitration.decision
                {
                    session.end_reason = SessionEndReason::Error(reason);
                    return false;
                }
            }
            step::Step::Suspended => {
                session.end_reason = SessionEndReason::Suspended;
                return false;
            }
            step::Step::Completed => {
                session.end_reason = SessionEndReason::Completed;
                return false;
            }
            step::Step::Failed(reason) => {
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
