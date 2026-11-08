//! Starting one agent run: admitting pending input, composing the
//! agent, and the spawned task that drives it to completion.
//! Method continuation of the inner state's `ControllerInner`.

use std::sync::Arc;

use elysper::{
    harness::AgentInput,
    provider::traits::ModelProvider,
    session::SurfaceLedger,
};
use futures::StreamExt;

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
    /// The provider the suspended turn was sampled with.
    ///
    /// The log's last request-snapshot row names the catalogue entry
    /// and the model; the resolver turns that pair back into a live
    /// handle. `None` when the log holds no snapshot or the
    /// deployment no longer offers the pair — the run then falls back
    /// to the configured default and the resume reports the drift it
    /// cannot avoid.
    async fn resume_provider(&self) -> Option<Arc<dyn ModelProvider>> {
        let resolver = self.deps.lock().provider_resolver.clone()?;
        let rows = self.writer.read().await.ok()?;
        let (provider, model) = last_request_snapshot(&rows)?;
        let resolved = resolver.resolve(&provider, &model);
        tracing::debug!(
            target: "elysper.session",
            session_id = %self.session_id,
            provider = %provider,
            model = %model,
            resolved = resolved.is_some(),
            "resume: the provider the suspension recorded"
        );
        resolved
    }
}

/// The provider and model the log's last request snapshot names.
fn last_request_snapshot(
    rows: &[serde_json::Value],
) -> Option<(String, String)> {
    rows.iter()
        .rev()
        .find(|row| {
            row.get("type").and_then(|t| t.as_str()) == Some("request_snapshot")
        })
        .and_then(|row| {
            let provider = row.get("provider")?.as_str()?.to_string();
            let model = row.get("model")?.as_str()?.to_string();
            Some((provider, model))
        })
}

impl ControllerInner {
    /// Start a new agent run if and only if the controller is idle
    /// AND a parked turn is waiting (or a resume continues a
    /// suspended one)
    pub(super) async fn maybe_start_run(
        self: &Arc<Self>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        // Run starts and manual compaction share this gate: a
        // compaction holds it while it rewrites the surface, so no run
        // can begin against the pre-compaction span. Held for the
        // whole decision — the approval read below is I/O, and it
        // must not race a pass — but it is the compaction gate, not
        // the steer gate: a steer or a cancel during this window is
        // state-transition work the pass does not touch.
        let _pass_gate = self.compaction_pass_gate.lock().await;
        if !self.is_startable() {
            return None;
        }

        // A parked resume governs exactly this run: it starts even
        // with no queued input, and it must not consume queued input
        // (that belongs to the turn after it). The log-derived paused
        // state refuses a *fresh* run, so a resume skips that gate.
        // Probed, not taken: the take belongs to the state transition
        // below, where the steer gate makes it atomic against a
        // cancel draining the queue.
        let has_resume = self.pending_resume.lock().is_some();
        if !has_resume {
            match self.awaiting_approval().await {
                // An open interrupt is a log fact: record it so
                // `/status` and the list view report the pause without a
                // second scan. The queued prompt stays queued.
                Ok(true) => {
                    self.mark_awaiting_approval();
                    tracing::info!(
                        target: "elysper.session",
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
                        target: "elysper.session",
                        session_id = %self.session_id,
                        %error,
                        "maybe_start_run: refused, the approval state could                          not be read"
                    );
                    return None;
                }
            }
        }

        // The state transition: the steer gate makes the take below
        // atomic against the cancel path (which drains the same queue
        // under the same gate) and against a live steer's
        // running-check, so a park can never be both taken and
        // drained, and a run can never be marked `Running` after a
        // steer was told the session was idle. Everything before this
        // point was read-only; the gate covers only the mutation.
        let (resume, parked_take) = {
            let _start_gate = self.steer_gate.lock().await;
            if !self.is_startable() {
                return None;
            }
            let resume = self.pending_resume.lock().take();
            // One parked turn per run, taken here: a turn queued while
            // a run was live gets its own run (and its own logged
            // prompt) instead of being folded into the running turn.
            // A resume is the exception — it continues the suspended
            // turn, and work queued during the pause belongs to the
            // turn after it.
            let parked_take = if resume.is_some() {
                None
            } else {
                self.pending_turns.lock().pop_front()
            };
            if resume.is_none() && parked_take.is_none() {
                tracing::trace!(
                    target: "elysper.session",
                    session_id = %self.session_id,
                    "maybe_start_run: skipped (no parked turns)"
                );
                return None;
            }
            self.mark_running(parked_take.is_some());
            (resume, parked_take)
        };

        // If the parked turn carried an explicit agent selection,
        // override the configured default for THIS run only. Reading
        // from the parked deps Mutex keeps the per-dispatch override
        // out of the persistent `default_agent_name` field — a
        // one-shot chat dispatch must not leak into future runs.
        let explicit_agent_name = parked_take
            .as_ref()
            .and_then(|parked| parked.agent_name.clone());
        // The turn's provider selection, taken here so it governs
        // exactly this run: `None` keeps the provider on the
        // dependencies, and a selection another op parked afterwards
        // belongs to *its* turn, not this one.
        //
        // A resume continues a turn whose request envelope is already
        // on the log, and that envelope names the provider and model
        // the turn was sampled with. The resume therefore samples
        // *that* handle, not the deployment default: a different
        // provider would change the envelope and the resume would be
        // refused as a configuration drift. A selection parked during
        // the pause belongs to the next turn, so it stays parked
        // (exactly as a parked turn does).
        let provider = if resume.is_some() {
            self.resume_provider().await
        } else {
            self.pending_provider
                .lock()
                .take()
                .map(super::PinnedProvider::take)
        };
        // R34: the run's durable surface ledger. It is filled by the
        // run task (it needs the log read, which happens there) and
        // the writer numbers its rows, so compaction provenance and
        // the fold see the same surface rows in the same order.
        let ledger = Arc::new(SurfaceLedger::new());
        // The writer feeds the run's ledger: the rows a compaction
        // writes are indexed by the run that writes them.
        self.writer.set_ledger(Arc::clone(&ledger));
        let (mut config, overrides) = self
            .build_run_config_with_explicit_agent(
                explicit_agent_name.as_deref(),
                provider,
            );
        // The agent's structural rows (the step boundary, the usage
        // frame) go into the session's one writer: the same writer a
        // compaction writes through, so the log holds one ordinal
        // sequence.
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
        let pending_turn = parked_take
            .map(|parked| (parked.park_id, parked.parts, parked.rerun));

        let span = tracing::info_span!(
            target: "elysper.session",
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
            // The park this run starts from is this run's whole
            // input: one parked turn is one turn, whether its parts
            // are a single text (a `Prompt` / `Steer`) or a
            // multimodal payload. A resume continues the suspended
            // turn and takes nothing queued — work queued during the
            // pause belongs to the turn after it.
            let (turn_parts, rerun_turn, take_park) = if resume.is_some() {
                (None, false, None)
            } else if let Some((park_id, parts, rerun)) = pending_turn {
                (Some(parts), rerun, Some(park_id))
            } else {
                (None, false, None)
            };
            // Reconstruct prior-turn assistant / tool messages from
            // the durable events the previous runs persisted to the
            // session sink. Without this, every run starts with an
            // empty history and the LLM loses all multi-turn
            // memory. The sink is the only durable source of truth
            // here — the park this run starts from is log-only and
            // never folds into history, and the turn's own
            // `UserInput` row is appended only after this read, so
            // this run sees prior turns, never its own prompt twice.
            // R34: the same read seeds the run's surface ledger
            // through the session's writer, which numbers the
            // history exactly the way the fold does — each row's
            // own `seq` in a stamped log, positions in an old one.
            // Rows this run appends go through the same writer, so
            // the ledger stays in step with the fold.
            // The log's ordinal, read in O(1): it says whether the
            // cached fold still describes the log. A turn that finds
            // the log unchanged re-uses the cached history and reads
            // nothing.
            //
            // A rerun shadows the turn it replaces, so its replace
            // semantics are computed against THIS fold — the surface
            // as it stands before the rerun's own rows land. It
            // therefore never reads the cache.
            let snapshot_seq = inner
                .session_store
                .snapshot()
                .await
                .map(|snapshot| snapshot.last_event_seq)
                .unwrap_or(0);
            let cached = if rerun_turn {
                None
            } else {
                inner.cached_history.lock().clone().and_then(|cached| {
                    (cached.last_event_seq == snapshot_seq)
                        .then_some(cached.history)
                })
            };
            let (mut log, sink_history, rerun_replace) = match cached {
                // The log has not moved since the last fold: the
                // cached history is the history. The ledger is not
                // seeded again — the writer seeded it when the fold
                // was made, and seeding it twice would number the
                // rows twice.
                Some(history) => {
                    (RunLog::new(Arc::clone(&inner.writer)), history, None)
                }
                None => match inner.session_store.read().await {
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
                        // One strict projection for every reader: the
                        // run's history is what the log derives, never a
                        // second, lenient shape. A log that does not fold
                        // is logged and the run continues from an empty
                        // history, exactly as an unreadable log does.
                        let history =
                            match elysper::session::derive_history(&events) {
                                Ok(history) => history,
                                Err(error) => {
                                    tracing::error!(
                                        target: "elysper.session",
                                        session_id = %inner.session_id,
                                        %error,
                                        "Failed to fold session log; \
                                         starting run with empty history"
                                    );
                                    Vec::new()
                                }
                            };
                        // The fold is the history: keep it so the next
                        // turn that finds the log unchanged does not
                        // read it again.
                        *inner.cached_history.lock() = Some(
                            crate::session::controller::inner::CachedHistory {
                                last_event_seq: snapshot_seq,
                                history: history.clone(),
                            },
                        );
                        (
                            RunLog::new(Arc::clone(&inner.writer)),
                            history,
                            rerun_replace,
                        )
                    }
                    Err(e) => {
                        tracing::error!(
                            target: "elysper.session",
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
                },
            };
            // The run's request header is the harness's: one row
            // per run, stamped by the sink before the first model
            // call, naming the provider that ran and the tool set the
            // model was offered. The server does not write a second
            // one — two headers per run told a replay two stories
            // about the same span of the log.
            tracing::info!(
                target: "elysper.session",
                session_id = %inner.session_id,
                sink_history_len = sink_history.len(),
                "Agent run task: composed history from the sink"
            );
            // Spend the park this run started from, by id, **before**
            // the turn's own prompt row. The pair is the durable
            // evidence that *this* run, not some other one, took the
            // park: the link is by id, not by position, because rows
            // that are no one's prompt (a steer that landed mid-run,
            // a usage frame) can sit between the park and its run —
            // a reader pairing the two by position would spend a
            // park whose turn never ran, and a crash then dropped a
            // prompt whose row was intact.
            //
            // A failed append is logged, not fatal: the run cannot be
            // stopped by a log write, and the park it could not spend
            // re-runs the turn on restart rather than losing it.
            if let Some(park_id) = take_park
                && let Err(e) = log
                    .append_event(
                        &elysper::session::SessionEvent::ParkConsumed {
                            seq: 0,
                            ts: String::new(),
                            park_id,
                        },
                    )
                    .await
            {
                tracing::error!(
                    target: "elysper.session",
                    session_id = %inner.session_id,
                    error = %e,
                    "Failed to persist the park_consumed row"
                );
            }
            // The turn's `Text` parts are its durable record: the
            // user's question lands in the transcript here — without
            // this row a replay shows the assistant's reply with no
            // question above it. A park's parts are the whole turn,
            // text and multimodal alike, so this is the one place
            // the row is written.
            let current_turn_text = turn_parts
                .as_deref()
                .map(prompt_text_of_parts)
                .unwrap_or_default();
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
            let attachment_refs = turn_parts
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
                        target: "elysper.session",
                        session_id = %inner.session_id,
                        error = %e,
                        "Failed to persist current-turn user prompt to session sink"
                    );
                }
            }
            let history = sink_history;
            // Build the AgentInput. The parked turn's parts ARE the
            // turn: they feed the agent directly via
            // `AgentInput::multi_with_history`, so a text park's
            // single `Text` part is exactly the prompt the text path
            // always sent. The turn's text also lands in the
            // transcript above, so the NEXT turn's agent — which
            // re-loads from the sink — sees what the user said;
            // this turn's in-memory bytes die with it either way.
            let input = if let Some(pending) = resume {
                let mut input = AgentInput::text("")
                    .resume_decisions(pending.token, pending.decisions);
                if pending.allow_drift {
                    input = input.allow_drift();
                }
                input
            } else if let Some(parts) = turn_parts {
                if parts.is_empty() {
                    // Defensive: an empty payload — an empty input
                    // so the agent starts at minimum.
                    AgentInput::text("")
                } else {
                    AgentInput::multi_with_history(history, parts)
                }
            } else {
                // Defensive: a run with neither a park nor a
                // resume. `maybe_start_run` refuses that shape;
                // keep the agent alive if it ever happens anyway.
                AgentInput::text("")
            };

            tracing::info!(
                target: "elysper.session",
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
                target: "elysper.session",
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
                        target: "elysper.session",
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
                target: "elysper.session",
                session_id = %inner.session_id,
                "Agent run task: transitioned to Idle"
            );
            // R34: every row this run produced is now in the log.
            // The compaction rows sit in the writer's queue: the
            // flush puts them on disk so the compaction's provenance
            // is durable before the run ends.
            if let Err(e) = inner.writer.flush().await {
                tracing::warn!(
                    target: "elysper.session",
                    session_id = %inner.session_id,
                    error = %e,
                    "Failed to flush the session writer"
                );
            }
            tracing::info!(
                target: "elysper.session",
                session_id = %inner.session_id,
                event_count,
                "Agent run task: factory stream ended"
            );
            // R11: publish the Completing snapshot.
            let _seq = inner.snapshot_bus.publish(
                elysper::session::OperationSnapshot::new(
                    inner.session_id.clone(),
                    "default".to_string(),
                    elysper::session::OperationState::Completing,
                    0,
                    inner.deps.lock().default_max_iterations,
                    elysper::provider::TokenUsage::default(),
                ),
            );
        }))
    }
}
