//! Starting one agent run: admitting pending input, composing the
//! agent, and the spawned task that drives it to completion.
//! Method continuation of the inner state's `ControllerInner`.

use std::sync::Arc;

use futures::StreamExt;
use synthia::{
    harness::AgentInput,
    provider::{Content, ContentPart, Message, Role, TextContent},
    session::{CompactionCheckpoint, SurfaceLedger},
};

use super::{
    inner::ControllerInner,
    ops::SessionState,
    run_log::{
        RunLog,
        attachment_refs_of_parts,
        prompt_text_of_parts,
        rerun_replace_op,
    },
};

impl ControllerInner {
    /// Start a new agent run if and only if the controller is idle
    /// AND there is pending work — either a parked multimodal
    /// prompt or at least one queued text
    pub(super) async fn maybe_start_run(
        self: &Arc<Self>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        // Run starts and manual compaction share this gate: a
        // compaction holds it while it rewrites the surface, so no run
        // can begin against the pre-compaction span.
        let _start_gate = self.steer_gate.lock().await;
        if !self.is_startable() {
            return None;
        }

        // A parked resume governs exactly this run: it starts even
        // with no queued input, and it must not consume queued input
        // (that belongs to the turn after it). The log-derived paused
        // state refuses a *fresh* run, so a resume skips that gate.
        let resume = self.pending_resume.lock().take();
        if resume.is_none() {
            match self.awaiting_approval().await {
                // An open interrupt is a log fact: record it so
                // `/status` and the list view report the pause without a
                // second scan. The queued prompt stays queued.
                Ok(true) => {
                    self.mark_awaiting_approval();
                    tracing::info!(
                        target: "synthia.session",
                        session_id = %self.session_id,
                        "maybe_start_run: refused, the session is awaiting                          approval"
                    );
                    return None;
                }
                Ok(false) => {}
                // The log is unreadable, so whether a turn is suspended
                // on an approval is unknown. Starting would run straight
                // over that pause; refusing leaves the queued work in
                // place for the next op (which re-reads), and says why.
                Err(error) => {
                    tracing::error!(
                        target: "synthia.session",
                        session_id = %self.session_id,
                        %error,
                        "maybe_start_run: refused, the approval state could                          not be read"
                    );
                    return None;
                }
            }
        }

        // Multimodal prompts (with optional agent override) take
        // precedence over text-only queue entries: the parked queue is
        // the only place the lossless parts (image/audio/file bytes)
        // live. One entry per run, so two turns queued during a run
        // each get their own run (and their own logged prompt) instead
        // of overwriting each other.
        let multimodal_take = if resume.is_some() {
            None
        } else {
            self.pending_multimodal.lock().pop_front()
        };
        let has_text_pending = resume.is_none()
            && self
                .queue
                .has_pending(&self.user_id, &self.session_id)
                .await;
        if resume.is_none() && multimodal_take.is_none() && !has_text_pending {
            tracing::trace!(
                target: "synthia.session",
                session_id = %self.session_id,
                "maybe_start_run: skipped (no pending inputs)"
            );
            return None;
        }

        let multimodal_active = multimodal_take.is_some();
        self.mark_running(multimodal_active).await;

        // If the multimodal slot carried an explicit agent
        // selection, override the configured default for THIS
        // run only. Reading from the parked deps Mutex keeps
        // the per-dispatch override out of the persistent
        // `default_agent_name` field — a one-shot chat
        // dispatch must not leak into future runs.
        let explicit_agent_name = multimodal_take
            .as_ref()
            .and_then(|parked| parked.agent_name.clone());
        // The turn's provider selection, taken here so it governs
        // exactly this run: `None` keeps the provider on the
        // dependencies, and a selection another op parked afterwards
        // belongs to *its* turn, not this one.
        //
        // A resume is the exception: it continues a turn whose
        // envelope was already sampled under the session's default
        // provider. A selection parked during the pause belongs to the
        // next turn, so it stays parked (exactly as a multimodal
        // payload does) — consuming it would sample a different
        // provider and report `ConfigDrift` against the token.
        let provider = if resume.is_some() {
            None
        } else {
            self.pending_provider.lock().take()
        };
        // R34: the run's durable surface ledger and checkpoint. The
        // ledger is filled by the run task (it needs the log read,
        // which happens there); the checkpoint is installed on the
        // composed context manager *now* so the manager the factory
        // builds already carries its emitters. The checkpoint writes
        // through the session's one writer: its callbacks are
        // synchronous, so they take the writer's non-waiting path
        // and the rows wait in its spill rather than being dropped.
        let ledger = Arc::new(SurfaceLedger::new());
        // The writer feeds the run's ledger: the rows the checkpoint
        // writes are indexed by the run that writes them, so the
        // ledger and the fold see the same surface rows in the same
        // order.
        self.writer.set_ledger(Arc::clone(&ledger));
        let compaction_configured = self.deps.lock().compaction.is_some();
        let checkpoint = compaction_configured.then(|| {
            CompactionCheckpoint::new(
                Arc::clone(&self.writer),
                Arc::clone(&ledger),
            )
        });
        let (mut config, overrides) = self
            .build_run_config_with_explicit_agent(
                explicit_agent_name.as_deref(),
                provider,
                checkpoint.as_ref(),
            );
        // The agent's structural rows (the step boundary, the usage
        // frame) go into the session's one writer: the same writer
        // the compaction checkpoint writes through, so the log holds
        // one ordinal sequence.
        config.log_writer = Some(Arc::clone(&self.writer));
        // Delegated children write their own sessions: the run's
        // `task` delegator resolves each child's writer through
        // the same registry the router routes child events by.
        if let Some(router) = self.subagent_router.as_ref() {
            config.subagent_writers = Some(Arc::new(
                crate::session::subagent_router::ChildSessionWriters::new(
                    Arc::clone(router),
                    self.user_id.clone(),
                    self.session_id.clone(),
                ),
            ));
        }
        let factory = Arc::clone(&self.run_factory);
        let inner = Arc::clone(self);
        // The park this run starts from, if any: its id rides along so
        // the run can spend exactly that row (see the `park_consumed`
        // append below). A resume parks nothing and takes nothing.
        let pending_turn = multimodal_take
            .map(|parked| (parked.park_id, parked.parts, parked.rerun));

        let span = tracing::info_span!(
            target: "synthia.session",
            "agent.run",
            session.id = %self.session_id,
            user.id = %self.user_id,
            iteration.id = tracing::field::Empty,
            agent.name = tracing::field::Empty,
        );
        Some(tokio::spawn(async move {
            let _entered = span.enter();
            span.record(
                "agent.name",
                config.agent_name.as_deref().unwrap_or("<default>"),
            );
            // When a multimodal payload is parked on `inner`, we
            // skip the queue entirely: the multimodal prompt
            // IS this run's input, and merging it with whatever
            // straggler text the queue holds would conflate two
            // turns into one. The text queue's existence on the
            // controller is preserved for the plain-text path.
            let (pending_text, multimodal_parts, rerun_turn, take_park) =
                if resume.is_some() {
                    // A resume continues the suspended turn; nothing
                    // queued during the pause belongs to it.
                    (Vec::new(), None, false, None)
                } else if let Some((park_id, parts, rerun)) = pending_turn {
                    (Vec::new(), Some(parts), rerun, Some(park_id))
                } else {
                    let drained = match inner
                        .queue
                        .drain_pending(&inner.user_id, &inner.session_id)
                        .await
                    {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::error!(
                                target: "synthia.session",
                                session_id = %inner.session_id,
                                error = %e,
                                "Failed to drain session input queue"
                            );
                            Vec::new()
                        }
                    };
                    (drained, None, false, None)
                };
            tracing::debug!(
                target: "synthia.session",
                session_id = %inner.session_id,
                drained_count = pending_text.len(),
                "Agent run task: drained input queue"
            );
            let (pending_history, prompt) = pending_text.into_iter().fold(
                (Vec::new(), String::new()),
                |(mut hist, mut last), entry| {
                    if last.is_empty() {
                        last = entry.content;
                    } else {
                        hist.push(Message {
                            role: Role::User,
                            content: Content::Single(ContentPart::Text(
                                TextContent {
                                    text: last,
                                    cache_control: None,
                                },
                            )),
                            tool_call_id: None,
                            name: None,
                            ..Default::default()
                        });
                        last = entry.content;
                    }
                    (hist, last)
                },
            );
            // Reconstruct prior-turn assistant / tool messages from
            // the durable events the previous runs persisted to the
            // session sink. Without this, every run starts with an
            // empty history and the LLM loses all multi-turn
            // memory. The sink is the only durable source of truth
            // here — `InputQueue` is per-run and ephemeral.
            // R34: the same read seeds the run's surface ledger
            // through the session's writer, which numbers the
            // history exactly the way the fold does — each row's
            // own `seq` in a stamped log, positions in an old one.
            // Rows this run appends go through the same writer, so
            // the ledger stays in step with the fold.
            let (mut log, sink_history, rerun_replace) = match inner
                .session_store
                .read()
                .await
            {
                Ok(events) => {
                    inner.writer.seed_ledger(&events);
                    // A rerun shadows the turn it replaces, so
                    // the prompt row it is about to append
                    // carries replace semantics computed against
                    // THIS fold — the surface as it stands before
                    // the rerun's own rows land.
                    let rerun_replace = if rerun_turn {
                        rerun_replace_op(&events)
                    } else {
                        None
                    };
                    (
                        RunLog::new(Arc::clone(&inner.writer)),
                        synthia::context::events_to_messages(&events),
                        rerun_replace,
                    )
                }
                Err(e) => {
                    tracing::error!(
                        target: "synthia.session",
                        session_id = %inner.session_id,
                        error = %e,
                        "Failed to read session sink; starting run with empty history"
                    );
                    (
                        RunLog::new(Arc::clone(&inner.writer)),
                        Vec::new(),
                        // No readable log ⇒ nothing to shadow ⇒
                        // the rerun degrades to a plain append.
                        None,
                    )
                }
            };
            // The run's request header is the harness's: one row
            // per run, stamped by the sink before the first model
            // call, naming the provider that ran and the tool set the
            // model was offered. The server does not write a second
            // one — two headers per run told a replay two stories
            // about the same span of the log.
            tracing::info!(
                target: "synthia.session",
                session_id = %inner.session_id,
                sink_history_len = sink_history.len(),
                pending_history_len = pending_history.len(),
                "Agent run task: composed history from sink + pending"
            );
            // Persist this run's drained user prompts to the sink so
            // the NEXT run sees them in `sink_history`. We do this
            // AFTER reading the sink (so this run doesn't echo its
            // own prompt back through `sink_history`) and BEFORE
            // invoking the agent (so a fast agent doesn't emit
            // assistant events before the user prompts are
            // recorded — `events_to_history` walks the JSONL in
            // append order, so user prompts must precede the
            // assistant turns they elicited).
            //
            // Multimodal runs persist only the textual component
            // (when one exists). Raw image/audio bytes are NOT
            // serialised into the JSONL sink: the sink is for
            // `UserInput` envelopes whose `data.text` is a plain
            // string, and base64-encoding 5 MB of image data per
            // turn would bloat the per-session log without
            // anything reading it back as image bytes. The
            // durable rehydration only needs the text transcript;
            // the model already saw the bytes for THIS turn.
            for msg in &pending_history {
                let text = match &msg.content {
                    Content::Single(ContentPart::Text(t)) => &t.text,
                    _ => continue,
                };
                if let Err(e) = log
                    .append(serde_json::json!({
                        "type": "UserInput",
                        "data": { "text": text },
                    }))
                    .await
                {
                    tracing::error!(
                        target: "synthia.session",
                        session_id = %inner.session_id,
                        error = %e,
                        "Failed to persist drained user prompt to session sink"
                    );
                }
            }
            // Spend the park this run started from, by id, **before**
            // the turn's own prompt row. The pair is the durable
            // evidence that *this* run, not some other one, took the
            // park: a text `Prompt` writes a `UserInput` without ever
            // parking, so a reader pairing the two by position would
            // spend a park whose turn never ran — and a crash then
            // dropped a prompt whose row was intact.
            //
            // A failed append is logged, not fatal: the run cannot be
            // stopped by a log write, and the park it could not spend
            // re-runs the turn on restart rather than losing it.
            if let Some(park_id) = take_park
                && let Err(e) = log
                    .append_event(
                        &synthia::session::SessionEvent::ParkConsumed {
                            seq: 0,
                            ts: String::new(),
                            park_id,
                        },
                    )
                    .await
            {
                tracing::error!(
                    target: "synthia.session",
                    session_id = %inner.session_id,
                    error = %e,
                    "Failed to persist the park_consumed row"
                );
            }
            // The typed text of a multimodal turn never reaches
            // `prompt`: `PromptMulti` parks the whole payload in
            // `parts`, so `pending_history` is empty and `prompt`
            // is `""`. Persist the `Text` parts here so the user's
            // question lands in the durable transcript — without
            // this a multimodal turn writes no `UserInput` row at
            // all, and replay shows the assistant's reply with no
            // question above it.
            let current_turn_text = if prompt.is_empty() {
                multimodal_parts
                    .as_deref()
                    .map(prompt_text_of_parts)
                    .unwrap_or_default()
            } else {
                prompt.clone()
            };
            // A rerun's prompt row carries the replace op computed
            // against the pre-rerun fold, so the durable log folds
            // to the replacement story (one prompt, the new answer)
            // instead of the turn twice — see `rerun_replace_op`.
            //
            // The row also records SMALL references to the turn's
            // attachments (kind / name / mime / byte length) — never
            // their bytes, per the note above. Without a reference a
            // reloaded transcript has no way to show that the turn
            // carried anything, so a file or image turn rendered as an
            // empty bubble on the session-detail page even though the
            // live chat had shown a chip for it. A text-only turn omits
            // the field entirely, keeping the common row shape
            // unchanged.
            let attachment_refs = multimodal_parts
                .as_deref()
                .map(|parts| {
                    attachment_refs_of_parts(
                        parts,
                        inner.deps.lock().attachment_store.as_ref(),
                    )
                })
                .unwrap_or_default();
            if !current_turn_text.is_empty() || !attachment_refs.is_empty() {
                let mut row = serde_json::json!({
                    "type": "UserInput",
                    "data": { "text": current_turn_text },
                });
                if !attachment_refs.is_empty()
                    && let Some(obj) =
                        row.get_mut("data").and_then(|d| d.as_object_mut())
                    && let Ok(refs) = serde_json::to_value(&attachment_refs)
                {
                    obj.insert("attachments".to_string(), refs);
                }
                if let (Some(op), Some(obj)) =
                    (rerun_replace.as_ref(), row.as_object_mut())
                    && let Ok(op_value) = serde_json::to_value(op)
                {
                    obj.insert("surface_op".to_string(), op_value);
                }
                if let Err(e) = log.append(row).await {
                    tracing::error!(
                        target: "synthia.session",
                        session_id = %inner.session_id,
                        error = %e,
                        "Failed to persist current-turn user prompt to session sink"
                    );
                }
            }
            let mut history = sink_history;
            history.extend(pending_history);
            // Build the AgentInput. Two paths:
            //
            // 1. Multimodal path (`multimodal_parts.is_some()`):
            //    bypass the text-fold and feed the parts
            //    directly via `AgentInput::multi_with_history`. The text
            //    preview is still echoed into `history` so the
            //    NEXT turn's agent sees what the user said
            //    alongside the image (the bytes are in-memory
            //    only — the next run re-loads from the sink and
            //    therefore has no access to them).
            //
            // 2. Text path: identical to before.
            let input = if let Some(pending) = resume {
                let mut input = AgentInput::text("")
                    .resume_decisions(pending.token, pending.decisions);
                if pending.allow_drift {
                    input = input.allow_drift();
                }
                input
            } else if let Some(parts) = multimodal_parts {
                if !prompt.is_empty() {
                    let text_preview = Message {
                        role: Role::User,
                        content: Content::Single(ContentPart::Text(
                            TextContent {
                                text: prompt.clone(),
                                cache_control: None,
                            },
                        )),
                        tool_call_id: None,
                        name: None,
                        ..Default::default()
                    };
                    history.push(text_preview);
                }
                if parts.is_empty() {
                    // Defensive: empty multimodal payload — fall
                    // through to a plain-text input so the agent
                    // sees the typed prompt at minimum.
                    AgentInput::text(prompt)
                } else {
                    AgentInput::multi_with_history(
                        std::mem::take(&mut history),
                        parts,
                    )
                }
            } else if prompt.is_empty() {
                AgentInput::text("")
            } else if history.is_empty() {
                AgentInput::text(prompt)
            } else {
                AgentInput::history(history, prompt)
            };

            tracing::info!(
                target: "synthia.session",
                session_id = %inner.session_id,
                "Agent run task: invoking factory.run_stream"
            );
            let cancel = inner
                .run_cancel
                .lock()
                .expect("run_cancel mutex poisoned")
                .clone()
                .expect("run_cancel token must be set before run starts");
            let mut stream =
                factory.run_stream(config, overrides, input, cancel);
            tracing::info!(
                target: "synthia.session",
                session_id = %inner.session_id,
                "Agent run task: factory returned; draining events"
            );
            let mut event_count = 0usize;
            while let Some(event) = stream.next().await {
                event_count += 1;
                if let Err(e) =
                    inner.persist_and_broadcast(&event, &mut log).await
                {
                    tracing::error!(
                        target: "synthia.session",
                        session_id = %inner.session_id,
                        event_kind = event.kind(),
                        error = %e,
                        "Failed to persist or broadcast event"
                    );
                }
            }
            drop(stream);
            // The settle boundary. The run's stream has ended, so no
            // request of this run can carry a steer any more: "live"
            // means "can still sample again". The flip happens BEFORE
            // the flushes below, because a steer that landed during
            // the fsync window would otherwise be answered "steer"
            // while no request of this run carried it — the message
            // would survive only as a stray row in the next run's
            // transcript. A steer takes the same gate before it
            // reads this state, so the two cannot interleave: either
            // the steer appended its row while the run was still
            // `Running` (and the next step boundary would have
            // injected it) or it reads `Idle` and becomes an ordinary
            // queued turn. One decision, one row.
            {
                let _settled = inner.steer_gate.lock().await;
                let mut state =
                    inner.state.lock().expect("state mutex poisoned");
                if *state == SessionState::Running {
                    // A suspended run leaves the session paused: the
                    // log's open interrupts are the truth, and the
                    // cached state mirrors them so the list view and
                    // `is_startable` agree without a rescan.
                    *state = if inner
                        .suspended
                        .swap(false, std::sync::atomic::Ordering::SeqCst)
                    {
                        SessionState::AwaitingApproval
                    } else {
                        SessionState::Idle
                    };
                }
            }
            tracing::info!(
                target: "synthia.session",
                session_id = %inner.session_id,
                "Agent run task: transitioned to Idle"
            );
            // R34: every row this run produced is now in the log, so
            // a compaction record that could not be mapped when it
            // was emitted gets its second chance. The checkpoint's own
            // rows — the lifecycle triple and the record — sit in the
            // writer's queue: the flush puts them in the log and in
            // the ledger, so a parked record resolves against the
            // surface the earlier compactions left behind. Records
            // still without provable provenance are dropped
            // (log-only) by the checkpoint; the surface stays
            // foldable either way. The resolved record goes through
            // the same writer the checkpoint used, so it carries the
            // same ordinal source.
            if let Err(e) = inner.writer.flush().await {
                tracing::warn!(
                    target: "synthia.session",
                    session_id = %inner.session_id,
                    error = %e,
                    "Failed to flush the session writer"
                );
            }
            let resolved = match checkpoint.as_ref() {
                Some(checkpoint) => checkpoint.resolve_pending(),
                None => Vec::new(),
            };
            for event in resolved {
                if let Err(e) = inner.writer.append_event(&event).await {
                    tracing::warn!(
                        target: "synthia.session",
                        session_id = %inner.session_id,
                        error = %e,
                        "Failed to persist a resolved compaction checkpoint"
                    );
                }
            }
            // The checkpoint's own rows — the lifecycle triple and
            // the record — sit in the writer's queue: the flush puts
            // them in the log, so the compaction's provenance is on
            // disk before the run ends.
            if let Err(e) = inner.writer.flush().await {
                tracing::warn!(
                    target: "synthia.session",
                    session_id = %inner.session_id,
                    error = %e,
                    "Failed to flush the session writer"
                );
            }
            tracing::info!(
                target: "synthia.session",
                session_id = %inner.session_id,
                event_count,
                "Agent run task: factory stream ended"
            );
            // R11: publish the Completing snapshot.
            let _seq = inner.snapshot_bus.publish(
                synthia::session::OperationSnapshot::new(
                    inner.session_id.clone(),
                    "default".to_string(),
                    synthia::session::OperationState::Completing,
                    0,
                    inner.deps.lock().default_max_iterations,
                    synthia::provider::TokenUsage::default(),
                ),
            );
        }))
    }
}
