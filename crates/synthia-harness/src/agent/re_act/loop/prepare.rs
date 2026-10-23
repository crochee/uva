//! Prepare-phase seams: session start, hooks, durable resume,
//! run-inbox draining, and the context-manager pass (hint
//! injection, runtime-context snapshot).

use std::sync::Arc;

use synthia_context::AgentState;
use synthia_core::Clock;
use synthia_provider::{Content, Message, Role};
use synthia_steering::{HookDecision, HookEvent, InjectionPoint};
use tracing::debug;

use super::{
    super::{
        projection::{ToolProjectionCache, tool_definitions, tool_names_hash},
        state::{RunState, RuntimeSnapshotCache, Transcript},
    },
    ReActLoop,
    sample,
};
use crate::{
    events::SessionEndReason,
    input::AgentInput,
    prompt::RuntimeContext,
};

impl ReActLoop {
    pub(super) async fn drive_setup(
        &mut self,
        input: &AgentInput,
    ) -> Result<RunState, SessionEndReason> {
        if !self.sink.begin(&self.run.cancel) {
            return Err(SessionEndReason::Cancelled);
        }
        // The one interception point that can end the run before
        // the first sample: a `before_run` `Terminate`.
        let decision =
            super::hooks::dispatch_hook(&self.caps.control.steering, || {
                HookEvent::BeforeRun {
                    agent_name: self.caps.model.descriptor.name.clone(),
                    run_idx: 0,
                }
            })
            .await;
        if let HookDecision::Terminate { reason } = decision {
            return Err(SessionEndReason::Error(reason));
        }
        let mut messages = sample::prepare(self, input);

        let resume_key = self.load_durable_resume(input, &mut messages);
        let mut session = RunState::seed(
            Arc::clone(&self.run.cancel),
            self.run.subagent_depth,
            messages.clone(),
            AgentState::from_config(&self.cfg),
        )
        .with_model_hint(self.caps.model.descriptor.model_hint.clone())
        .with_resume_key(resume_key.clone());
        // After the resume seam: a snapshot may have replaced
        // `messages`, and the promoted set must describe the
        // transcript the model will actually see.
        let initial_messages = session.messages.clone();
        session.seed_called_tools(&initial_messages);
        // The checkpoint owns the context-manager pass (the only
        // path that reads `messages` and writes the pruned one
        // back). Running it here as well as at iteration 0 means
        // iteration 0 sees exactly the seams every later
        // iteration does.
        checkpoint(self, &mut session, 0).await?;
        Ok(session)
    }

    /// A checkpoint miss is the normal first-run case.
    fn load_durable_resume(
        &self,
        input: &AgentInput,
        messages: &mut Transcript,
    ) -> Option<String> {
        let checkpointer = self.caps.env.checkpointer.as_ref()?;
        let thread_id = input.thread_id.as_ref()?;
        let key = format!("{}:{}", thread_id, self.caps.model.descriptor.name);
        match checkpointer.load(&key) {
            Ok(Some(snapshot)) => {
                tracing::info!(
                    key = %key,
                    resumed_messages = snapshot.messages.len(),
                    "durable resume: loaded snapshot, replacing prepared history"
                );
                *messages = snapshot.messages;
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(
                    key = %key,
                    error = %err,
                    "checkpoint load failed; starting fresh"
                );
            }
        }
        Some(key)
    }

    pub(super) fn stamp_request_header(&self) {
        // The projection cache is the only thing `tool_definitions`
        // needs; a throwaway `RunState` here would allocate an empty
        // transcript, an `AgentState` and a `model_hint` clone to
        // reach it.
        let mut projection = ToolProjectionCache::new();
        let tools = tool_definitions(
            self.caps.as_ref(),
            &self.synthetic_defs,
            self.synthetic_fingerprint,
            &mut projection,
        );
        self.emit_typed(synthia_session::request_header(
            self.caps.model.provider.name(),
            &self.cfg.name,
            &format!("{:016x}", tool_names_hash(&tools)),
            "initial",
        ));
    }
}

// ===== context manager ===============================================

/// A panicking manager rewrites `messages` **in place**, so a
/// half-truncated transcript must end the run rather than be sent on.
pub(super) async fn apply_context(
    this: &ReActLoop,
    session: &mut RunState,
) -> Result<(), SessionEndReason> {
    // Hand the manager the file activity observed in the
    // iteration that just closed, then clear it. The manager is
    // consumer-supplied (`with_context_manager`), so a panic in
    // it would otherwise end the run unreported.
    let details_to_set = {
        if session.touched.read_files.is_empty()
            && session.touched.modified_files.is_empty()
        {
            None
        } else {
            Some(std::mem::take(&mut session.touched))
        }
    };
    // `prepare_arc` keeps the transcript shared: the manager
    // returns the same `Arc` when nothing was pruned and a
    // fresh one when it rewrites history.
    let mut state = std::mem::replace(
        &mut session.agent_state,
        synthia_context::AgentState::with_window(0),
    );
    let prepared = crate::agent::re_act::panic::catching_panics(async {
        if let Some(details) = details_to_set {
            this.caps
                .control
                .context_manager
                .set_compaction_details(details);
        }
        this.caps
            .control
            .context_manager
            .prepare_arc(Arc::clone(&session.messages), &mut state)
            .await
    })
    .await;
    let prepared = match prepared {
        Ok(managed) => managed,
        Err(message) => {
            return Err(SessionEndReason::Error(format!(
                "context manager panicked: {message}"
            )));
        }
    };
    let pruned = state.last_truncated;
    session.messages = prepared;
    session.agent_state = state;
    if session.agent_state.last_truncated {
        debug!(
            estimated_tokens = session.agent_state.estimated_tokens,
            context_window = session.agent_state.context_window,
            "context manager pruned history"
        );
        this.sink.warn(
            crate::events::WarningKind::ContextCompaction,
            None,
            format!(
                "context window pruned: ~{} of {} tokens in use",
                session.agent_state.estimated_tokens,
                session.agent_state.context_window
            ),
        );
    }
    if pruned {
        // Decision ignored: `OnCompaction` is observational.
        let _ =
            super::hooks::dispatch_hook(&this.caps.control.steering, || {
                HookEvent::OnCompaction {
                    start: 0,
                    end: session.agent_state.iteration_count,
                    summary: String::new(),
                }
            })
            .await;
    }
    Ok(())
}

/// One checkpoint between two turns: prune, refresh the runtime-context
/// snapshot, inject hints — in that order, once.
pub(super) async fn checkpoint(
    this: &ReActLoop,
    session: &mut RunState,
    iteration: usize,
) -> Result<(), SessionEndReason> {
    apply_context(this, session).await?;
    snapshot_runtime_context(this, session);
    inject_hints(this, session, iteration).await;
    Ok(())
}

pub(super) async fn inject_hints(
    this: &ReActLoop,
    session: &mut RunState,
    iteration: usize,
) {
    if this.caps.control.steering.hints.is_empty() {
        return;
    }
    let mut reminders: Vec<String> = Vec::new();
    for hint in &this.caps.control.steering.hints {
        // `AppendToToolResult` applies at commit time
        // (`append_tool_result_hints`); evaluating its trigger here
        // would run a consumer's `generate` only to discard it.
        if matches!(
            hint.injection_point(),
            InjectionPoint::AppendToToolResult { .. }
        ) {
            continue;
        }
        // Consumer-supplied like every steering surface: a panicking
        // hint is skipped, not fatal.
        let message = match crate::agent::re_act::panic::ask_hint(
            hint,
            &session.agent_state,
        ) {
            Ok(Some(message)) => message,
            Ok(None) => continue,
            Err(panic) => {
                tracing::warn!(
                    hint = hint.name(),
                    panic = %panic,
                    "hint panicked; skipping it"
                );
                continue;
            }
        };
        if message.priority == synthia_steering::HintPriority::Low
            && session.agent_state.context_utilization() >= 0.8
        {
            continue;
        }
        match hint.injection_point() {
            InjectionPoint::SystemPrompt => {
                // Session start only: the system message is the
                // provider's cached prefix. Keyed by name so one
                // firing hint cannot silence the rest.
                if iteration == 0
                    && session.system_hinted.insert(hint.name().to_string())
                {
                    let mut messages =
                        (**Arc::make_mut(&mut session.messages)).to_vec();
                    append_to_system_prompt(&mut messages, &message.content);
                    session.messages = Arc::new(messages);
                }
            }
            InjectionPoint::BeforeNextLlmCall | InjectionPoint::RecencyZone => {
                reminders.push(message.content);
            }
            InjectionPoint::AppendToToolResult { .. } => {
                // Unreachable: filtered above.
            }
        }
    }
    if reminders.is_empty() {
        return;
    }
    debug!(
        hint_count = reminders.len(),
        estimated_tokens = session.agent_state.estimated_tokens,
        "injecting steering hints before next LLM call"
    );
    let live = Arc::make_mut(&mut session.messages);
    for content in reminders {
        live.push(Message::user(format!("[reminder] {content}")));
    }
}

/// Appended only when the rendered text differs from the last-appended
/// one: a re-append would invalidate the provider's KV prefix cache.
pub(super) fn snapshot_runtime_context(
    this: &ReActLoop,
    session: &mut RunState,
) {
    let now = this.caps.env.clock.now();
    let today = now.date_naive();
    // Re-render the snapshot only when the date rolled over, so
    // no platform/snapshot allocations per iteration.
    if session
        .runtime_snapshot
        .as_ref()
        .is_none_or(|cache| cache.today != today)
    {
        let runtime = RuntimeContext::from_runtime(
            &this.caps.env.workspace_root.to_string_lossy(),
            &format!("{}/{}", this.caps.model.provider.name(), this.cfg.name),
            now,
        );
        session.runtime_snapshot = Some(RuntimeSnapshotCache {
            today,
            rendered: runtime.render_snapshot(),
        });
    }
    let rendered = session
        .runtime_snapshot
        .as_ref()
        .map(|cache| cache.rendered.as_str())
        .unwrap_or_default();
    if session.last_snapshot.as_deref() == Some(rendered) {
        debug!(
            history_len = session.messages.len(),
            "runtime-context snapshot unchanged; skipping append to preserve prompt cache"
        );
        return;
    }
    let text = rendered.to_string();
    let user_message = Message::new(Role::User, Content::text(text.clone()));
    Arc::make_mut(&mut session.messages).push(user_message);
    session.last_snapshot = Some(text);
    debug!(
        history_len = session.messages.len(),
        "appended runtime-context snapshot before next LLM sample"
    );
}

fn append_to_system_prompt(messages: &mut Vec<Message>, addition: &str) {
    if let Some(message) = messages.first_mut()
        && matches!(message.role, Role::System)
    {
        let mut text = message.content.extract_text().unwrap_or_default();
        text.push_str("\n\n");
        text.push_str(addition);
        message.content = Content::text(text);
        return;
    }
    messages.insert(0, Message::system(addition.to_string()));
}
