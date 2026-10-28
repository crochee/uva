//! Prepare-phase seams: session start, the request assembly (system
//! prompt, request-local pruning, runtime snapshot, hints), and the
//! durable-resume check.
//!
//! The run owns one transcript: [`RunState::messages`] is the durable
//! surface, index-parallel with [`RunState::surface_seqs`], and it is
//! the only thing a step reads. Everything a single request needs on
//! top of it — the system prompt, the turn's non-text parts, the
//! context manager's prune, the runtime snapshot, the hints — is
//! composed here into a fresh request list and dropped afterwards.
//! That split is what keeps a compaction's `Replace` range a position
//! in the durable surface rather than in a list that happens to hold a
//! system prompt today.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use elysper_core::Clock;
use elysper_provider::{Content, ContentPart, Message, Role};
use elysper_session::{SessionLog, derive_history, try_fold_log_surface};
use elysper_steering::{HookDecision, HookEvent, InjectionPoint};
use tracing::debug;

use super::{
    super::state::{RunState, RuntimeSnapshotCache, Transcript},
    ReActLoop,
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
        let arbitration =
            super::hooks::dispatch_hook(self, &self.run.agent_state, || {
                HookEvent::BeforeRun {
                    agent_name: self.caps.model.descriptor.name.clone(),
                    run_idx: 0,
                }
            })
            .await;
        if let HookDecision::Terminate { reason } = arbitration.decision {
            return Err(SessionEndReason::Error(reason));
        }
        // The run's system prompt: assembled once, here, and kept on
        // the run state. A `SystemPrompt` hint may rewrite it once
        // for the whole session.
        let system_prompt = super::sample::assemble_system_prompt(self);
        // A writer means the log owns the transcript: the host has
        // already persisted this turn's user message, so the loop
        // derives the history from the log instead of assembling one
        // from `input.history` + `input.content`. The log cannot hold
        // the turn's bytes, so its non-text parts ride the request.
        let (messages, surface_seqs, attachments, rows) =
            match self.caps.env.log_writer.as_ref() {
                Some(log) => {
                    // The crash tail was repaired before this run
                    // opened its turn (see `drive`): the fold here
                    // reads a balanced log, and an interrupted turn
                    // does not arrive at the model as an unanswered
                    // tool call.
                    let rows = read_log(log.as_ref()).await?;
                    let derived = derive_history(&rows).map_err(|error| {
                        SessionEndReason::Error(format!(
                            "session log does not derive a model history: \
                             {error}"
                        ))
                    })?;
                    let folded =
                        try_fold_log_surface(&rows).map_err(|error| {
                            SessionEndReason::Error(format!(
                                "session log does not fold into a surface: \
                                 {error}"
                            ))
                        })?;
                    check_input_history(input, &derived)?;
                    let attachments = super::sample::attachment_parts(input);
                    warn_attachments(self, &attachments);
                    (Arc::new(derived), folded.surface_seqs, attachments, rows)
                }
                None => (
                    Arc::new(initial_transcript(input)),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                ),
            };

        let mut session = RunState::seed(
            Arc::clone(&self.run.cancel),
            self.run.subagent_depth,
            messages,
            elysper_context::AgentState::from_config(&self.cfg),
        )
        .with_model_hint(self.caps.model.descriptor.model_hint.clone())
        .with_system_prompt(system_prompt)
        .with_attachments(attachments)
        .with_surface_seqs(surface_seqs)
        .with_log_rows(rows);
        // The configured policy is the base; session-scope grants
        // recorded in this log extend it for this run. They are
        // durable facts, so a restarted process derives the same
        // effective policy without asking the user again.
        session.approval = self.caps.control.approval.clone();
        crate::agent::approval::merge_grants(
            &mut session.approval,
            &session.log_rows,
        );
        let initial_messages = session.messages.clone();
        session.seed_called_tools(&initial_messages);
        Ok(session)
    }

    /// Compose the list one request sends: the system prompt, the
    /// durable transcript, the turn's non-text parts, the context
    /// manager's request-local prune, the runtime snapshot and the
    /// hints — in that order.
    ///
    /// Nothing here writes to `session.messages`: the request list is
    /// a per-step projection, and the durable transcript stays
    /// index-parallel with its provenance.
    pub(super) async fn build_request(
        &self,
        session: &mut RunState,
        iteration: usize,
    ) -> Result<Transcript, SessionEndReason> {
        let mut messages = Vec::with_capacity(session.messages.len() + 4);
        messages.push(Message::system(session.system_prompt.clone()));
        messages.extend(session.messages.iter().cloned());
        if !session.attachments.is_empty() {
            messages.push(Message::new(
                Role::User,
                Content::Multi(session.attachments.clone()),
            ));
        }
        let mut request = apply_context(self, session, messages).await?;
        snapshot_runtime_context(self, session, &mut request);
        inject_hints(self, session, iteration, &mut request).await;
        Ok(request)
    }
}

// The transcript a run with no writer owns: the caller's history
// plus this turn's message. The system prompt is not part of it —
// the request builder prepends it.
fn initial_transcript(input: &AgentInput) -> Vec<Message> {
    let mut messages = input.history.clone();
    messages.push(input.to_message());
    messages
}

// The request envelope this run will send, as a log row. The run
// writes it before the request; a resume fingerprints it and refuses
// to continue under a different envelope.
pub(super) fn request_snapshot_event(
    this: &ReActLoop,
    offered: &[elysper_provider::ToolDefinition],
    system_prompt: &str,
) -> elysper_session::SessionEvent {
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
    // The provider id the host configured; the handle's `name()` is
    // the *model* for an OpenAI-compatible implementation, which
    // would name the model twice in the row.
    elysper_session::request_snapshot(
        super::provider_id(&this.cfg, &*this.caps.model.provider),
        &this.cfg.name,
        &hash_text(&params),
        &tool_definitions_hash(offered),
        &hash_text(system_prompt),
        sections,
        &hash_text(&policy),
    )
}

// The envelope fingerprint: the semantic fields only, so the row's
// own seq/ts do not make every run look like drift.
pub(super) fn request_snapshot_hash(
    event: &elysper_session::SessionEvent,
) -> Option<String> {
    crate::agent::approval::request_snapshot_hash(event)
}

// The offered set's identity: name plus schema, in name order. A
// schema edit under an unchanged name set is still drift.
fn tool_definitions_hash(
    offered: &[elysper_provider::ToolDefinition],
) -> String {
    let mut defs: Vec<&elysper_provider::ToolDefinition> =
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

// Read the run's log. Setup reads it once and hands the rows to every
// consumer that needs them.
pub(super) async fn read_log(
    log: &dyn SessionLog,
) -> Result<Vec<serde_json::Value>, SessionEndReason> {
    log.read().await.map_err(|error| {
        SessionEndReason::Error(format!("session log read failed: {error}"))
    })
}

// The request carries bytes the log will not: say so instead of
// losing them silently on a later resume.
fn warn_attachments(this: &ReActLoop, attachments: &[ContentPart]) {
    if attachments.is_empty() {
        return;
    }
    this.sink.warn(
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

// The log is the single source of truth for a writer-backed run. A
// host that also hands in `input.history` must not contradict it: the
// host's transcript has to be empty or a prefix of the fold. The plan
// said "empty or equal"; a prefix is required because the host passes
// the prior transcript and the current turn separately, while the log
// already holds the current turn's row. Anything else would mean two
// transcripts, so the run fails closed instead of choosing one.
fn check_input_history(
    input: &AgentInput,
    derived: &[Message],
) -> Result<(), SessionEndReason> {
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

// ===== request-local passes =========================================

/// A panicking manager rewrites the request **in place**, so a
/// half-truncated transcript must end the run rather than be sent on.
///
/// The prune is request-local: the durable transcript keeps every
/// message, and only the list about to be sent is trimmed.
pub(super) async fn apply_context(
    this: &ReActLoop,
    session: &mut RunState,
    messages: Vec<Message>,
) -> Result<Transcript, SessionEndReason> {
    let mut state = std::mem::replace(
        &mut session.agent_state,
        elysper_context::AgentState::with_window(0),
    );
    let prepared = crate::agent::re_act::panic::catching_panics(async {
        this.caps
            .control
            .context_manager
            .prepare_arc(Arc::new(messages), &mut state)
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
    session.agent_state = state;
    // The prune is request-local, so the same eviction would warn
    // (and run the `OnCompaction` hook) once per LLM pass; one
    // warning per run describes the whole run.
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
        super::hooks::dispatch_hook(this, &session.agent_state, || {
            HookEvent::OnCompaction {
                start: 0,
                end: session.agent_state.iteration_count,
                summary: String::new(),
            }
        })
        .await;
    }
    Ok(prepared)
}

pub(super) async fn inject_hints(
    this: &ReActLoop,
    session: &mut RunState,
    iteration: usize,
    request: &mut Transcript,
) {
    let hints = this.caps.control.host.hints(&session.agent_state);
    if hints.is_empty() {
        return;
    }
    let mut reminders: Vec<String> = Vec::new();
    for (point, message) in hints {
        match point {
            InjectionPoint::SystemPrompt => {
                // Session start only: the system message is the
                // provider's cached prefix. Keyed by name so one
                // firing hint cannot silence the rest.
                if iteration == 0
                    && session
                        .system_hinted
                        .insert(format!("system:{message:?}"))
                {
                    append_to_system_prompt(
                        &mut session.system_prompt,
                        &message.content,
                    );
                }
            }
            InjectionPoint::BeforeNextLlmCall | InjectionPoint::RecencyZone => {
                reminders.push(message.content);
            }
            InjectionPoint::AppendToToolResult { .. } => {
                // Evaluated at commit time by the host.
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
    let live = Arc::make_mut(request);
    for content in reminders {
        live.push(Message::user(format!("[reminder] {content}")));
    }
}

/// Appended only when the rendered text differs from the last-appended
/// one: a re-append would invalidate the provider's KV prefix cache.
pub(super) fn snapshot_runtime_context(
    this: &ReActLoop,
    session: &mut RunState,
    request: &mut Transcript,
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
            request_len = request.len(),
            "runtime-context snapshot unchanged; skipping append to preserve prompt cache"
        );
        return;
    }
    let text = rendered.to_string();
    Arc::make_mut(request)
        .push(Message::new(Role::User, Content::text(text.clone())));
    session.last_snapshot = Some(text);
    debug!(
        request_len = request.len(),
        "appended runtime-context snapshot before next LLM sample"
    );
}

fn append_to_system_prompt(system_prompt: &mut String, addition: &str) {
    system_prompt.push_str("\n\n");
    system_prompt.push_str(addition);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::channel::mpsc;
    use elysper_context::AgentState;
    use elysper_core::AtomicCancelToken;

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
        let provider: Arc<dyn elysper_provider::ModelProvider> =
            Arc::new(elysper_test_support::FakeProvider::new(Vec::new()));
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

        match request_snapshot_event(&loop_, &[], "") {
            elysper_session::SessionEvent::RequestSnapshot {
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
