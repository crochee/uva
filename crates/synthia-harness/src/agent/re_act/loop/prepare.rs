//! Prepare-phase seams: session start, hooks, durable resume,
//! run-inbox draining, and the context-manager pass (hint
//! injection, runtime-context snapshot).

use std::sync::Arc;

use sha2::{Digest, Sha256};
use synthia_context::AgentState;
use synthia_core::Clock;
use synthia_provider::{Content, Message, Role};
use synthia_session::LogWriter;
use synthia_steering::{HookDecision, HookEvent, InjectionPoint};
use tracing::debug;

use super::{
    super::state::{RunState, RuntimeSnapshotCache, Transcript},
    ReActLoop,
    sample,
};
use crate::{
    agent::history::HistoryProjection,
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
        let arbitration =
            super::hooks::dispatch_hook(&self.caps.control, || {
                HookEvent::BeforeRun {
                    agent_name: self.caps.model.descriptor.name.clone(),
                    run_idx: 0,
                }
            })
            .await;
        if let HookDecision::Terminate { reason } = arbitration.decision {
            return Err(SessionEndReason::Error(reason));
        }
        // A writer means the log owns the transcript: the host has
        // already persisted this turn's user message, so the loop
        // derives the history instead of assembling one from
        // `input.history` + `input.content`. The log cannot hold the
        // turn's bytes, so its non-text parts still ride the request.
        let (mut messages, history, attachments, rows) = match self
            .caps
            .env
            .log_writer
            .as_ref()
        {
            Some(writer) => {
                let (projection, rows) = history_from_log(writer).await?;
                check_input_history(input, &projection)?;
                let attachments = sample::attachment_parts(input);
                if !attachments.is_empty() {
                    // The request carries bytes the log will not:
                    // say so instead of losing them silently on a
                    // later resume.
                    self.sink.warn(
                            crate::events::WarningKind::Attachments,
                            None,
                            format!(
                                "{} attachment part(s) are sent with this run but are \
                                 not persisted in the session log; a resume will \
                                 not replay them",
                                attachments.len()
                            ),
                        );
                }
                let messages = sample::prepare_from_log(
                    self,
                    input,
                    projection.messages(),
                    &attachments,
                );
                (messages, Some(projection), attachments, rows)
            }
            None => {
                (sample::prepare(self, input), None, Vec::new(), Vec::new())
            }
        };

        let resume_key =
            self.load_durable_resume(input, &mut messages, history.is_some());
        let mut session = RunState::seed(
            Arc::clone(&self.run.cancel),
            self.run.subagent_depth,
            messages.clone(),
            AgentState::from_config(&self.cfg),
        )
        .with_model_hint(self.caps.model.descriptor.model_hint.clone())
        .with_resume_key(resume_key.clone())
        .with_history(history)
        .with_attachments(attachments);
        // The configured policy is the base; session-scope grants
        // recorded in this log extend it for this run. They are
        // durable facts, so a restarted process derives the same
        // effective policy without asking the user again.
        session.approval = self.caps.control.approval.clone();
        crate::agent::approval::merge_grants(&mut session.approval, &rows);
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

    // A checkpoint miss is the normal first-run case. With a writer
    // the snapshot never replaces the transcript: it carries no log
    // ordinal, so it cannot prove its tail is not behind the log,
    // and the fold always answers what the model has seen. The resume
    // key is still returned, so a non-`Completed` end keeps saving it.
    fn load_durable_resume(
        &self,
        input: &AgentInput,
        messages: &mut Transcript,
        log_owned: bool,
    ) -> Option<String> {
        let checkpointer = self.caps.env.checkpointer.as_ref()?;
        let thread_id = input.thread_id.as_ref()?;
        let key = format!("{}:{}", thread_id, self.caps.model.descriptor.name);
        if log_owned {
            debug!(
                key = %key,
                "durable resume: the log owns the transcript; keeping the fold"
            );
            return Some(key);
        }
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
}

// The request envelope this run will send, as a log row. The run
// writes it before the request; a resume fingerprints it and refuses
// to continue under a different envelope.
pub(super) fn request_snapshot_event(
    this: &ReActLoop,
    offered: &[synthia_provider::ToolDefinition],
) -> synthia_session::SessionEvent {
    let ctx = &this.prompt_context;
    let mut sections = Vec::new();
    if !this.caps.model.descriptor.instructions.trim().is_empty() {
        sections.push("instructions".to_string());
    }
    sections.push("identity".to_string());
    if ctx.has_renderable_sections() {
        sections.push("prompt_sections".to_string());
    }
    if this.caps.model.descriptor.output_schema.is_some() {
        sections.push("structured_output".to_string());
    }
    if !ctx.skills.is_empty() {
        sections.push("skills".to_string());
    }
    if !ctx.agents.is_empty() {
        sections.push("agents".to_string());
    }
    let system = ctx.assemble(&this.caps.model.descriptor);
    // The sampling envelope this run sends. Every field a resume must
    // reproduce is named once: the model descriptor's capabilities and
    // budget, plus the request-level sampling values (`None` /
    // `Auto` / no stops, the harness's deliberate defaults — see
    // `sample::completion_request`). A later change to any of them
    // changes the fingerprint.
    let params = serde_json::json!({
        "context_window": this.cfg.context_window,
        "max_output_tokens": this.cfg.max_output_tokens,
        "supports_tools": this.cfg.supports_tools,
        "supports_streaming": this.cfg.supports_streaming,
        "supports_reasoning": this.cfg.supports_reasoning,
        "temperature": serde_json::Value::Null,
        "max_tokens": serde_json::Value::Null,
        "stop_sequences": [],
        "tool_choice": "auto",
    })
    .to_string();
    let policy = serde_json::to_string(&serde_json::json!({
        "mode": this.caps.control.approval.mode,
        "ask_unknown": this.caps.control.approval.ask_unknown,
        "overrides": this.caps.control.approval.overrides,
    }))
    .unwrap_or_default();
    synthia_session::request_snapshot(
        this.caps.model.provider.name(),
        &this.cfg.name,
        &hash_text(&params),
        &tool_definitions_hash(offered),
        &hash_text(&system),
        sections,
        &hash_text(&policy),
    )
}

// The envelope fingerprint: the semantic fields only, so the row's
// own seq/ts do not make every run look like drift.
pub(super) fn request_snapshot_hash(
    event: &synthia_session::SessionEvent,
) -> Option<String> {
    crate::agent::approval::request_snapshot_hash(event)
}

// The offered set's identity: name plus schema, in name order. A
// schema edit under an unchanged name set is still drift.
fn tool_definitions_hash(
    offered: &[synthia_provider::ToolDefinition],
) -> String {
    let mut defs: Vec<&synthia_provider::ToolDefinition> =
        offered.iter().collect();
    defs.sort_by(|left, right| left.name.cmp(&right.name));
    let mut hasher = Sha256::new();
    for def in defs {
        hasher.update(def.name.as_bytes());
        hasher.update(b"\0");
        let schema =
            serde_json::to_string(&def.input_schema).unwrap_or_default();
        hasher.update(schema.as_bytes());
        hasher.update(b"\0");
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

// Read the run's log and derive its model history.
async fn history_from_log(
    writer: &LogWriter,
) -> Result<(HistoryProjection, Vec<serde_json::Value>), SessionEndReason> {
    let rows = writer.read().await.map_err(|error| {
        SessionEndReason::Error(format!("session log read failed: {error}"))
    })?;
    let projection = HistoryProjection::from_rows(&rows).map_err(|error| {
        SessionEndReason::Error(format!(
            "session log does not derive a model history: {error}"
        ))
    })?;
    Ok((projection, rows))
}

// The log is the single source of truth for a writer-backed run. A
// host that also hands in `input.history` must not contradict it: the
// host's transcript has to be empty or a prefix of the fold. The plan
// said "empty or equal"; a prefix is required because the host passes
// the prior transcript and the current turn separately, while the log
// already holds the current turn's row. Anything else would mean two
// transcripts, so the run fails closed instead of choosing one.
fn check_input_history(
    input: &AgentInput,
    projection: &HistoryProjection,
) -> Result<(), SessionEndReason> {
    let derived = projection.messages();
    if derived.starts_with(&input.history) {
        return Ok(());
    }
    Err(SessionEndReason::Error(format!(
        "input history does not match the session log ({} host messages, {} log messages); \
         a writer-backed run derives its history from the log",
        input.history.len(),
        derived.len()
    )))
}

// Re-derive the transcript from the log at a step boundary. Only a
// writer-backed run has a projection; a pure-library run owns its
// transcript and is left untouched. The log-only rows a step writes
// move the tail but not the transcript, so a refresh that changes
// nothing costs one row scan and no rebuild.
pub(super) async fn refresh_history(
    this: &ReActLoop,
    session: &mut RunState,
) -> Result<(), SessionEndReason> {
    let Some(writer) = this.caps.env.log_writer.as_ref() else {
        return Ok(());
    };
    let Some(projection) = session.history.as_mut() else {
        return Ok(());
    };
    let rows = writer.read().await.map_err(|error| {
        SessionEndReason::Error(format!("session log read failed: {error}"))
    })?;
    let changed = projection.refresh(&rows).map_err(|error| {
        SessionEndReason::Error(format!(
            "session log does not derive a model history: {error}"
        ))
    })?;
    debug!(
        agent = %this.caps.model.descriptor.name,
        watermark = projection.watermark(),
        changed,
        "step boundary: history projection refreshed from the session log"
    );
    if changed {
        rebuild_transcript(session);
    }
    Ok(())
}

// Replace the derived part of `RunState.messages` with the fresh
// projection. The system prompt is kept — a hint may have rewritten
// it — and the runtime-context snapshot is re-appended at the tail
// from its cache, so a step that refreshes history does not drop the
// volatile facts the previous request carried. The snapshot text is
// unchanged, so the checkpoint that follows sees the same rendering
// and does not append a second copy.
fn rebuild_transcript(session: &mut RunState) {
    let Some(projection) = session.history.as_ref() else {
        return;
    };
    let snapshot = session.last_snapshot.clone();
    // Projection + (attachments) + (runtime snapshot).
    let mut messages = Vec::with_capacity(projection.messages().len() + 3);
    if let Some(system) = session
        .messages
        .first()
        .filter(|message| matches!(message.role, Role::System))
    {
        messages.push(system.clone());
    }
    messages.extend_from_slice(projection.messages());
    // The log cannot reproduce the turn's bytes; keep them in every
    // request of a run that carried them.
    if !session.attachments.is_empty() {
        messages.push(Message::new(
            Role::User,
            Content::Multi(session.attachments.clone()),
        ));
    }
    if let Some(text) = snapshot {
        messages.push(Message::new(Role::User, Content::text(text)));
    }
    session.messages = Arc::new(messages);
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
    // The prune is request-local: a writer-backed run re-derives and
    // re-prunes at every step, so the same eviction would otherwise
    // warn (and run the `OnCompaction` hook) once per LLM pass.
    if pruned && !session.context_pruned {
        session.context_pruned = true;
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
        // `OnCompaction` is observational: the point accepts no
        // decision, so a handler that returns one is recorded as a
        // diagnostic and the run continues.
        super::hooks::dispatch_hook(&this.caps.control, || {
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::channel::mpsc;
    use synthia_context::AgentState;
    use synthia_core::AtomicCancelToken;

    use super::{ReActLoop, request_snapshot_event};
    use crate::{
        AgentCaps,
        agent::{
            host::{Host, PromptSection},
            re_act::state::RunState,
            sink::EventSink,
        },
    };

    // The section names the run's request snapshot records, for a host
    // holding exactly `sections`.
    fn snapshot_sections(sections: &[(&str, &str)]) -> Vec<String> {
        let provider: Arc<dyn synthia_provider::ModelProvider> =
            Arc::new(synthia_test_support::FakeProvider::new(Vec::new()));
        let mut caps = AgentCaps::for_provider(provider);
        let mut host = Host::new();
        // The registration handle withdraws on drop, so keep each one
        // alive until the snapshot has been built.
        let _registrations: Vec<_> = sections
            .iter()
            .map(|(title, body)| {
                host.prompt_section(PromptSection {
                    title: (*title).to_string(),
                    body: (*body).to_string(),
                })
            })
            .collect();
        caps.control.host = host;

        let cfg = caps.model.provider.model_config();
        let prompt_context = caps.run_prompt_context(None);
        let run = RunState::seed(
            Arc::new(AtomicCancelToken::shared()),
            0,
            Arc::new(Vec::new()),
            AgentState::from_config(&cfg),
        );
        let (tx, _rx) = mpsc::unbounded();
        let loop_ = ReActLoop::from_caps(
            Arc::new(caps),
            cfg,
            run,
            EventSink::new(Arc::new(tx), None),
            prompt_context,
        );

        match request_snapshot_event(&loop_, &[]) {
            synthia_session::SessionEvent::RequestSnapshot {
                sections, ..
            } => sections,
            other => panic!("expected a request snapshot, got {other:?}"),
        }
    }

    // The fingerprint entry follows the renderer, not the registration
    // count: a section that renders nothing must not make a resume look
    // like drift for a byte-identical prompt.
    #[test]
    fn a_blank_section_is_not_named_in_the_request_snapshot() {
        let sections = snapshot_sections(&[("", "")]);
        assert!(
            !sections.iter().any(|name| name == "prompt_sections"),
            "a blank section contributes no prompt bytes, so it must not \
             enter the envelope fingerprint; got: {sections:?}"
        );
    }

    #[test]
    fn a_renderable_section_is_named_in_the_request_snapshot() {
        let sections = snapshot_sections(&[("Audit", "Always cite it.")]);
        assert!(
            sections.iter().any(|name| name == "prompt_sections"),
            "a section that renders must be named; got: {sections:?}"
        );
    }
}
