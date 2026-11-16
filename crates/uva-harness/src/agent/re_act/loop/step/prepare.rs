//! Prepare-phase seams: session start, the request assembly (system
//! prompt, request-local pruning, runtime snapshot, hints), and the
//! durable-resume check.
//!
//! The run owns one transcript: [`RunState::transcript`] is the durable
//! surface, and it is the only thing a step reads. Everything a single
//! request needs on top of it — the system prompt, the turn's non-text
//! parts, the context manager's prune, the runtime snapshot, the
//! hints — is composed here into a fresh request list and dropped
//! afterwards. That split is what keeps a compaction's `Replace` range
//! a position in the durable surface rather than in a list that
//! happens to hold a system prompt today.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracing::debug;
use uva_core::Clock;
use uva_events::SessionEndReason;
use uva_provider::{Content, ContentPart, Message, Role};
use uva_session::{ProvenancedHistory, SessionLog};
use uva_steering::{HookDecision, InjectionPoint};

use super::super::{
    super::{
        state::{RunState, RuntimeSnapshotCache},
        transcript::{RequestMessages, Transcript},
    },
    ReActLoop,
};
use crate::{input::AgentInput, prompt::RuntimeContext};

impl ReActLoop {
    pub(in crate::agent::re_act::r#loop) async fn drive_setup(
        &mut self,
        input: &AgentInput,
    ) -> Result<RunState, SessionEndReason> {
        if !self.sink.begin(&self.run.cancel) {
            return Err(SessionEndReason::Cancelled);
        }
        // The one interception point that can end the run before
        // the first sample: `run_started`.
        let arbitration = {
            let ctx = self.hook_context(&self.run.agent_state);
            self.caps
                .control
                .seams
                .run_started(
                    &ctx,
                    &self.sink,
                    &self.caps.model.descriptor.name,
                    0,
                )
                .await
        };
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
        //
        // One projection yields both halves: the messages the model
        // sees and the ordinal of the row each was written under, so
        // the transcript cannot pair a message with another row's
        // ordinal.
        let (transcript, attachments, rows) =
            match self.caps.env.log_writer.as_ref() {
                Some(log) => {
                    // The crash tail was repaired before this run
                    // opened its turn (see `drive`): the fold here
                    // reads a balanced log, and an interrupted turn
                    // does not arrive at the model as an unanswered
                    // tool call.
                    let rows = read_log(log.as_ref()).await?;
                    let history = ProvenancedHistory::from_log(&rows).map_err(
                        |error| {
                            SessionEndReason::Error(format!(
                                "session log does not derive a model \
                                 history: {error}"
                            ))
                        },
                    )?;
                    check_input_history(input, history.messages())?;
                    let attachments = super::sample::attachment_parts(input);
                    warn_attachments(self, &attachments);
                    (Transcript::from_projection(history), attachments, rows)
                }
                None => (
                    Transcript::from_messages(initial_transcript(input)),
                    Vec::new(),
                    Vec::new(),
                ),
            };

        let mut session = RunState::seed(
            Arc::clone(&self.run.cancel),
            transcript,
            uva_context::AgentState::from_config(&self.cfg),
        )
        .with_model_hint(self.caps.model.descriptor.model_hint.clone())
        .with_system_prompt(system_prompt)
        .with_attachments(attachments)
        .with_log_rows(rows);
        // The configured policy is the base; session-scope grants
        // recorded in this log extend it for this run. They are
        // durable facts, so a restarted process derives the same
        // effective policy without asking the user again.
        session.approval = self.caps.control.approval.clone();
        uva_approval::merge_grants(&mut session.approval, &session.log_rows);
        session.seed_called_tools();
        Ok(session)
    }

    /// Compose the list one request sends: the system prompt, the
    /// durable transcript, the turn's non-text parts, the context
    /// manager's request-local prune, the runtime snapshot and the
    /// hints — in that order.
    ///
    /// Nothing here writes to `session.transcript`: the request list
    /// is a per-step projection, and the durable transcript stays
    /// index-parallel with its provenance.
    pub(in crate::agent::re_act::r#loop) async fn build_request(
        &self,
        session: &mut RunState,
        iteration: usize,
    ) -> Result<RequestMessages, SessionEndReason> {
        let system = Message::system(session.system_prompt.clone());
        // The context manager can say which messages of the
        // transcript survive the window. When it can, the request
        // copies only those: copying the whole transcript and then
        // letting the manager prune the copy does the same work
        // twice — once for every message the window is about to
        // drop.
        let projected = self.project_context(session, &system).await?;
        let surface = session.transcript.messages();
        let mut messages = Vec::with_capacity(surface.len() + 4);
        messages.push(system);
        match &projected {
            Some(indices) => {
                for index in indices {
                    messages.push(surface[*index].clone());
                }
            }
            None => {
                messages.extend(surface.iter().cloned());
            }
        }
        if !session.attachments.is_empty() {
            messages.push(Message::new(
                Role::User,
                Content::Multi(session.attachments.clone()),
            ));
        }
        let mut request = match projected {
            // The manager pruned by projecting, so the list already
            // holds exactly what `prepare` would have left behind.
            Some(_) => Arc::new(messages),
            None => apply_context(self, session, messages).await?,
        };
        snapshot_runtime_context(self, session, &mut request);
        inject_hints(self, session, iteration, &mut request).await;
        Ok(request)
    }

    // Ask the context manager which messages of the transcript
    // survive the window.
    //
    // `None` means "no projection": the caller copies the whole
    // transcript and lets the manager prune the copy, exactly as
    // before. The manager that projects saves the caller a copy of
    // every message the window is about to drop — and the manager's
    // own prune, which has to remove the dropped messages from a
    // list it owns, becomes a decision instead of a rewrite.
    async fn project_context(
        &self,
        session: &mut RunState,
        system: &Message,
    ) -> Result<Option<Vec<usize>>, SessionEndReason> {
        // The attachments ride the request but not the transcript,
        // and the manager's budget covers the whole request. A
        // projection that ignored them would decide on a different
        // list than the one the request carries, so a turn with
        // attachments keeps the old shape.
        if !session.attachments.is_empty() {
            return Ok(None);
        }
        let mut state = std::mem::replace(
            &mut session.agent_state,
            uva_context::AgentState::with_window(0),
        );
        let projected = crate::agent::re_act::panic::catching_panics(async {
            self.caps
                .control
                .context_manager
                .kept_indices(
                    std::slice::from_ref(system),
                    session.transcript.messages(),
                    &mut state,
                )
                .await
        })
        .await;
        let projected = match projected {
            Ok(projection) => projection,
            Err(message) => {
                session.agent_state = state;
                return Err(SessionEndReason::Error(format!(
                    "context manager panicked: {message}"
                )));
            }
        };
        let pruned = state.last_truncated;
        session.agent_state = state;
        note_pruned(self, session, pruned).await;
        Ok(projected)
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

/// The provider id a durable row records.
///
/// [`uva_provider::ModelConfig::provider`] is the id the
/// deployment configured; [`uva_provider::ModelProvider::name`]
/// is the *model* for an OpenAI-compatible implementation, so a row
/// built from the handle names the model twice and the provider is
/// unrecoverable from the log. A provider that leaves the id empty
/// falls back to its own name, so the row is never blank.
pub(super) fn provider_id<'a>(
    cfg: &'a uva_provider::ModelConfig,
    provider: &'a dyn uva_provider::ModelProvider,
) -> &'a str {
    if cfg.provider.is_empty() {
        provider.name()
    } else {
        cfg.provider.as_str()
    }
}

// The request envelope this run will send, as a log row. The run
// writes it before the request; a resume fingerprints it and refuses
// to continue under a different envelope.
pub(in crate::agent::re_act::r#loop) fn request_snapshot_event(
    this: &ReActLoop,
    offered: &[uva_provider::ToolDefinition],
    system_prompt: &str,
) -> uva_session::SessionEvent {
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
    if !ctx.skills().is_empty() {
        sections.push("skills".to_string());
    }
    if !ctx.agents().is_empty() {
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
    uva_session::request_snapshot(
        provider_id(&this.cfg, &*this.caps.model.provider),
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
pub(in crate::agent::re_act::r#loop) fn request_snapshot_hash(
    event: &uva_session::SessionEvent,
) -> Option<String> {
    uva_approval::request_snapshot_hash(event)
}

// The offered set's identity: name plus schema, in name order. A
// schema edit under an unchanged name set is still drift.
fn tool_definitions_hash(offered: &[uva_provider::ToolDefinition]) -> String {
    let mut defs: Vec<&uva_provider::ToolDefinition> = offered.iter().collect();
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
        uva_events::WarningKind::Attachments,
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
) -> Result<RequestMessages, SessionEndReason> {
    let mut state = std::mem::replace(
        &mut session.agent_state,
        uva_context::AgentState::with_window(0),
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
    note_pruned(this, session, pruned).await;
    Ok(prepared)
}

// Report that the context window pruned the request's list.
//
// The prune is request-local, so the same eviction would warn (and
// run the `OnCompaction` hook) once per LLM pass; one warning per
// run describes the whole run. Both the pruning manager and the
// projecting one report through here, so the notice does not depend
// on which shape the manager took.
async fn note_pruned(this: &ReActLoop, session: &mut RunState, pruned: bool) {
    if !pruned || session.context_pruned {
        return;
    }
    session.context_pruned = true;
    debug!(
        estimated_tokens = session.agent_state.estimated_tokens,
        context_window = session.agent_state.context_window,
        "context manager pruned history"
    );
    this.sink.warn(
        uva_events::WarningKind::ContextCompaction,
        None,
        format!(
            "context window pruned: ~{} of {} tokens in use",
            session.agent_state.estimated_tokens,
            session.agent_state.context_window
        ),
    );
    // `compacted` is observational: the point accepts no decision, so a
    // handler that returns one is recorded as a diagnostic and the run
    // continues.
    let ctx = this.hook_context(&session.agent_state);
    this.caps
        .control
        .seams
        .compacted(&ctx, &this.sink, 0, session.agent_state.iteration_count, "")
        .await;
}

pub(super) async fn inject_hints(
    this: &ReActLoop,
    session: &mut RunState,
    iteration: usize,
    request: &mut RequestMessages,
) {
    let hints = this.caps.control.seams.hints(&session.agent_state);
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
    request: &mut RequestMessages,
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
    use uva_context::AgentState;
    use uva_core::AtomicCancelToken;

    use super::{ReActLoop, request_snapshot_event};
    use crate::{
        AgentCaps,
        agent::{
            host::{Host, PromptSection},
            re_act::{state::RunState, transcript::Transcript},
            sink::EventSink,
        },
    };

    // The section names the run's request snapshot records, for a host
    // holding exactly `sections`.
    fn snapshot_sections(sections: &[(&str, &str)]) -> Vec<String> {
        let provider: Arc<dyn uva_provider::ModelProvider> =
            Arc::new(uva_test_support::FakeProvider::new(Vec::new()));
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
        caps.control.seams = crate::seams::Seams::new(host);

        let cfg = caps.model.provider.model_config();
        let prompt_context = caps.run_prompt_context(None);
        let run = RunState::seed(
            Arc::new(AtomicCancelToken::shared()),
            Transcript::empty(),
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
            uva_session::SessionEvent::RequestSnapshot { sections, .. } => {
                sections
            }
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
