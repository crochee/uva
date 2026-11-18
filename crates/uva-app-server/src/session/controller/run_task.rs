//! Starting one agent run: admitting pending input, composing the
//! agent, and the spawned task that drives it to completion.
//! Method continuation of the inner state's `ControllerInner`.

use std::sync::Arc;

use futures::StreamExt;
use uva::{
    harness::AgentInput,
    provider::traits::ModelProvider,
    session::{
        RowTag,
        Rows,
        SessionEvent,
        SessionLog,
        SurfaceLedger,
        restore_row,
    },
};

use super::{
    inner::ControllerInner,
    run_log::{
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
    /// handle. `Ok(None)` when the log holds no snapshot or the
    /// deployment no longer offers the pair — the run then falls back
    /// to the configured default and the resume reports the drift it
    /// cannot avoid.
    ///
    /// The log is read through [`Rows::read`], which **refuses** a log
    /// carrying a row this build cannot interpret. That refusal is an
    /// error, not an absent snapshot: a resume on the configured
    /// default would sample a different provider than the suspended
    /// turn used, and the request envelope would drift — the very
    /// thing the resume exists to prevent. So the error refuses the
    /// run instead of answering "no snapshot".
    async fn resume_provider(
        &self,
    ) -> anyhow::Result<Option<Arc<dyn ModelProvider>>> {
        let Some(resolver) = self.deps.lock().provider_resolver.clone() else {
            return Ok(None);
        };
        let raw = self.writer.read().await?;
        let rows = Rows::read(&raw)?;
        let Some((provider, model)) = last_request_snapshot(rows) else {
            return Ok(None);
        };
        let resolved = resolver.resolve(&provider, &model);
        tracing::debug!(
            target: "uva.session",
            session_id = %self.session_id,
            provider = %provider,
            model = %model,
            resolved = resolved.is_some(),
            "resume: the provider the suspension recorded"
        );
        Ok(resolved)
    }
}

/// The provider and model the log's last request snapshot names.
///
/// The read goes through the vocabulary, which compares the row's tag
/// before decoding it: a row of any other tag costs no decode. The
/// tail is scanned from the end, so a snapshot whose payload does not
/// decode is skipped in favour of the one before it — a broken tail
/// row is a defect, not a newer answer.
fn last_request_snapshot(rows: Rows<'_>) -> Option<(String, String)> {
    rows.iter()
        .rev()
        .filter(|row| row.tag() == Some(RowTag::RequestSnapshot))
        .find_map(|row| match row.event() {
            Some(SessionEvent::RequestSnapshot {
                provider, model, ..
            }) => Some((provider, model)),
            // Only the tag the filter named can arrive here, so no
            // other variant can; answering `None` keeps the caller's
            // fallback exact instead of panicking on a shape this
            // function cannot produce.
            _ => None,
        })
}

/// The history a run starts from when the log does not fold
/// strictly.
///
/// The strict fold refuses a replace whose provenance it cannot
/// prove — a compaction whose citation does not name the rows its
/// span covers. The lenient fold is the same fold with that one rule
/// relaxed: it skips the refused replace and replays the span it
/// shadowed. The run therefore keeps the history the log actually
/// supports, which is *more* of the conversation, not less.
///
/// An empty history — what this replaced — loses every turn the log
/// still holds, which is worse than the situation the refusal
/// replaced.
fn lenient_history(
    events: &[serde_json::Value],
) -> Vec<uva::provider::Message> {
    let surface = uva::session::fold_log_surface(events);
    uva::session::surface_to_messages(&surface.messages)
}

/// The `kind` slug of the warning row that records a refused fold.
///
/// A stable, machine-readable slug, in the shape the harness already
/// writes for an accepted resume drift (`config_drift`): a client
/// reads the row to learn that the log did not fold strictly, and
/// the slug is what it matches on.
const FOLD_REFUSAL_KIND: &str = "strict_fold_refused";

/// Whether the log already records this fold refusal.
///
/// The refusal is a property of the *log*, not of the run: every
/// later run folds the same log and is refused the same way, so the
/// row belongs to the log and is written once. The rows in hand are
/// the log's own, so the check is a scan of them.
fn fold_refusal_recorded(events: &[serde_json::Value]) -> bool {
    events.iter().any(|row| {
        restore_row(row).restores_to(RowTag::Warning)
            && row
                .get("data")
                .and_then(|data| data.get("kind"))
                .and_then(serde_json::Value::as_str)
                == Some(FOLD_REFUSAL_KIND)
    })
}

impl ControllerInner {
    /// Record a refused fold in the session's durable log.
    ///
    /// The session's log is the client's view of the session, and a
    /// refusal the client cannot see is a refusal it cannot act on:
    /// the run looks like any other. The row is a `warning` — the
    /// log-only variant the harness already writes for an accepted
    /// resume drift — and it carries the refusal's `kind` and a
    /// `message`.
    ///
    /// The row does not change the folded surface: `warning` is not
    /// one of the row kinds the fold projects, so the surface the
    /// model sees is the surface the log had before the row landed.
    ///
    /// One row per session: the refusal belongs to the log, so a
    /// later run that is refused the same way finds the row and
    /// writes none.
    async fn record_fold_refusal(
        &self,
        events: &[serde_json::Value],
        error: &uva::session::HistoryError,
    ) {
        if fold_refusal_recorded(events) {
            return;
        }
        let event = uva::session::warning(
            FOLD_REFUSAL_KIND,
            &format!(
                "the session log does not fold strictly; the run \
                 replayed the lenient fold: {error}"
            ),
        );
        // The row goes through the session's one writer, and the
        // flush lands it: the next turn reads the log, and a row
        // still queued would not be in it.
        if let Err(e) = self.append_durable_event(&event).await {
            tracing::error!(
                target: "uva.session",
                session_id = %self.session_id,
                error = %e,
                "Failed to persist the fold-refusal warning"
            );
        }
    }

    /// The history a run starts from, and the replace op a rerun's
    /// prompt row carries.
    ///
    /// One strict projection for every reader: the run's history is
    /// what the log derives, never a second, lenient shape. A log
    /// the strict fold refuses — a compaction whose citation does
    /// not name the rows it shadows — does not cost the run its
    /// conversation: the run falls back to the lenient fold, which
    /// skips the refused compaction and replays the span it
    /// shadowed. An empty history would lose turns the log still
    /// holds, which is worse than the situation the refusal
    /// replaced.
    ///
    /// The refusal is recorded in the log (see
    /// [`Self::record_fold_refusal`]), and the log is read again
    /// afterwards: the history below is then the log the client sees
    /// — the warning row and all — and the cached fold is keyed by an
    /// ordinal that fold covers.
    async fn fold_run_history(
        &self,
        events: &[serde_json::Value],
        rerun_turn: bool,
        snapshot_seq: u64,
    ) -> (Vec<uva::provider::Message>, Option<uva::session::SurfaceOp>) {
        // A rerun shadows the turn it replaces, so the prompt row it
        // is about to append carries replace semantics computed
        // against THIS fold — the surface as it stands before the
        // rerun's own rows land.
        let rerun_replace = if rerun_turn {
            rerun_replace_op(events)
        } else {
            None
        };
        match uva::session::derive_history(events) {
            Ok(history) => {
                self.cache_fold(snapshot_seq, history.clone());
                (history, rerun_replace)
            }
            Err(error) => {
                tracing::error!(
                    target: "uva.session",
                    session_id = %self.session_id,
                    %error,
                    "Session log does not fold strictly; replaying the \
                     lenient fold"
                );
                self.record_fold_refusal(events, &error).await;
                // The warning row landed, so the log moved. Read it
                // again, so the history below is the log as it stands
                // — warning row and all — and the marker names an
                // ordinal the fold covers. The marker is sampled
                // BEFORE that read: it is the log's own ordinal after
                // the warning row, and a lower bound on what the
                // fold covers, so a row that lands while the fold is
                // made invalidates the cache rather than being
                // dropped from the next run's history.
                let marker = self.writer.last_seq();
                match self.writer.read().await {
                    Ok(events) => {
                        let rerun_replace = if rerun_turn {
                            rerun_replace_op(&events)
                        } else {
                            None
                        };
                        let history = lenient_history(&events);
                        self.cache_fold(marker, history.clone());
                        (history, rerun_replace)
                    }
                    Err(e) => {
                        tracing::error!(
                            target: "uva.session",
                            session_id = %self.session_id,
                            error = %e,
                            "Failed to re-read the session log after \
                             recording the fold refusal"
                        );
                        // The log cannot be read again, so there is no
                        // fold to cache: a cache keyed by an ordinal
                        // the log may have moved past would hand the
                        // next run a history it cannot trust.
                        (lenient_history(events), rerun_replace)
                    }
                }
            }
        }
    }

    /// Keep the fold as the session's cached history.
    ///
    /// The cache is keyed by the log's ordinal: a turn that finds the
    /// log unchanged re-uses the fold instead of reading the log
    /// again. The key is the log's own ordinal, sampled before the
    /// read that produced the fold — so it names an ordinal the fold
    /// covers, and a row that lands while the fold is being made
    /// invalidates the cache instead of being dropped from the next
    /// run's history.
    fn cache_fold(&self, marker: u64, history: Vec<uva::provider::Message>) {
        *self.cached_history.lock() =
            Some(crate::session::controller::inner::CachedHistory {
                last_event_seq: marker,
                history,
            });
    }
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
        if !self.state.is_startable() {
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
                        target: "uva.session",
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
                        target: "uva.session",
                        session_id = %self.session_id,
                        %error,
                        "maybe_start_run: refused, the approval state could                          not be read"
                    );
                    return None;
                }
            }
        }

        // The provider a resume continues its suspended turn with.
        //
        // A resume continues a turn whose request envelope is already
        // on the log, and that envelope names the provider and model
        // the turn was sampled with. The resume therefore samples
        // *that* handle, not the deployment default: a different
        // provider would change the envelope and the resume would be
        // refused as a configuration drift. A selection parked during
        // the pause belongs to the next turn, so it stays parked
        // (exactly as a parked turn does).
        //
        // The log is read **before** the state transition below: a log
        // this build cannot read is a refusal, not a fallback to the
        // deployment default — a resume on the default provider would
        // change the very envelope the resume exists to reproduce. The
        // parked resume stays parked, so the next op re-reads the log
        // and says why.
        //
        // The read is I/O, so it stays outside the steer gate: the
        // gate must not be held across a disk read.
        let resume_provider = if self.pending_resume.lock().is_some() {
            match self.resume_provider().await {
                Ok(provider) => Some(provider),
                Err(error) => {
                    tracing::error!(
                        target: "uva.session",
                        session_id = %self.session_id,
                        %error,
                        "maybe_start_run: refused, the session log could not be read"
                    );
                    return None;
                }
            }
        } else {
            None
        };

        // The state transition: the steer gate makes the take below
        // atomic against the cancel path (which drains the same queue
        // under the same gate) and against a live steer's
        // running-check, so a park can never be both taken and
        // drained, and a run can never be marked `Running` after a
        // steer was told the session was idle. Everything before this
        // point was read-only; the gate covers only the mutation.
        let (resume, parked_take, provider) = {
            let _start_gate = self.steer_gate.lock().await;
            if !self.state.is_startable() {
                return None;
            }
            // The resume and the provider read for it are taken
            // together: a resume whose provider was never read cannot
            // start on this pass — the log was not read for it, so
            // the run would sample the deployment default instead of
            // the handle the suspended turn used. Such a resume stays
            // parked; the op that parked it starts the run on its own
            // pass.
            let mut parked = self.pending_resume.lock();
            let taken = parked.take();
            // The provider the resume's log named: `Some` when the
            // resume was read for one, `None` when there is no
            // resume at all.
            let (resume, provider) = match (taken, resume_provider) {
                (Some(resume), Some(provider)) => {
                    (Some(resume), Some(provider))
                }
                (Some(resume), None) => {
                    *parked = Some(resume);
                    tracing::warn!(
                        target: "uva.session",
                        session_id = %self.session_id,
                        "maybe_start_run: skipped, a resume was parked after its provider was read"
                    );
                    return None;
                }
                (None, _) => (None, None),
            };
            drop(parked);
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
                    target: "uva.session",
                    session_id = %self.session_id,
                    "maybe_start_run: skipped (no parked turns)"
                );
                return None;
            }
            // The run's provider: the one the resume's log named — or
            // the deployment default, when the log names no snapshot —
            // and the deployment's pinned selection for a fresh run.
            let provider = match provider {
                Some(provider) => provider,
                None => self
                    .pending_provider
                    .lock()
                    .take()
                    .map(super::PinnedProvider::take),
            };
            self.mark_running(parked_take.is_some());
            (resume, parked_take, provider)
        };

        // If the parked turn carried an explicit agent selection,
        // override the configured default for THIS run only. Reading
        // from the parked deps Mutex keeps the per-dispatch override
        // out of the persistent `default_agent_name` field — a
        // one-shot chat dispatch must not leak into future runs.
        let explicit_agent_name = parked_take
            .as_ref()
            .and_then(|parked| parked.agent_name.clone());
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
        let factory = Arc::clone(&self.run_factory);
        let inner = Arc::clone(self);
        // The park this run starts from, if any: its id rides along so
        // the run can spend exactly that row (see the `park_consumed`
        // append below). A resume parks nothing and takes nothing.
        let pending_turn = parked_take
            .map(|parked| (parked.park_id, parked.parts, parked.rerun));

        let span = tracing::info_span!(
            target: "uva.session",
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
            // prompt row is appended only after this read, so
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
            // The writer's *flushed* counter is the movement
            // detector: `last_seq()` reports the last ordinal the
            // writer made durable, not the rows still in its queue, so
            // a cached fold is reused exactly when the log it was made
            // from has not changed.
            let snapshot_seq = inner.writer.last_seq();
            let cached = if rerun_turn {
                None
            } else {
                inner.cached_history.lock().clone().and_then(|cached| {
                    (cached.last_event_seq == snapshot_seq)
                        .then_some(cached.history)
                })
            };
            // The run's handle on the session's one writer. The
            // controller holds the typed seam, so every row this run
            // persists — the park's consumption mark, the turn's
            // prompt, the op loop's out-of-band rows the router
            // writes — draws from one ordinal sequence.
            let log: Arc<dyn SessionLog> = Arc::clone(&inner.writer);
            let (sink_history, rerun_replace) = match cached {
                // The log has not moved since the last fold: the
                // cached history is the history. The ledger is not
                // seeded again — the writer seeded it when the fold
                // was made, and seeding it twice would number the
                // rows twice.
                Some(history) => (history, None),
                None => match inner.writer.read().await {
                    Ok(events) => {
                        inner.writer.seed_ledger(&events);
                        // The fold is the history: it is derived from
                        // the log, kept so the next turn that finds
                        // the log unchanged does not read it again.
                        let (history, rerun_replace) = inner
                            .fold_run_history(&events, rerun_turn, snapshot_seq)
                            .await;
                        (history, rerun_replace)
                    }
                    Err(e) => {
                        tracing::error!(
                            target: "uva.session",
                            session_id = %inner.session_id,
                            error = %e,
                            "Failed to read session sink; starting run with empty history"
                        );
                        // No readable log ⇒ nothing to shadow ⇒ the
                        // rerun degrades to a plain append.
                        (Vec::new(), None)
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
                target: "uva.session",
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
                    .append_event(&uva::session::park_consumed(park_id))
                    .await
            {
                tracing::error!(
                    target: "uva.session",
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
                // The row is the typed `user_message`: the writer
                // stamps its ordinal, the variant owns its shape, and
                // the payload is the provider message the model saw —
                // never a `json!` literal the call site has to keep in
                // step with the reader.
                //
                // A row that could not be encoded or appended is logged,
                // not fatal: a log write must never stop the run, and the
                // park this turn came from still re-runs on restart.
                match serde_json::to_value(uva::provider::Message::user(
                    current_turn_text.as_str(),
                )) {
                    Ok(data) => {
                        let row = uva::session::turn_prompt_row(
                            data,
                            attachment_refs,
                            rerun_replace,
                        );
                        if let Err(e) = log.append_event(&row).await {
                            tracing::error!(
                                target: "uva.session",
                                session_id = %inner.session_id,
                                error = %e,
                                "Failed to persist current-turn user prompt \
                                 to session sink"
                            );
                        }
                    }
                    Err(e) => tracing::error!(
                        target: "uva.session",
                        session_id = %inner.session_id,
                        error = %e,
                        "Failed to encode the current-turn user prompt"
                    ),
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
            // The run's tool context reads its session identity from
            // the input's `thread_id`; tools that keep per-session
            // state (the `task` tool's child bookkeeping) key on it.
            let mut input = input;
            input.thread_id = Some(inner.session_id.clone());

            tracing::info!(
                target: "uva.session",
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
                target: "uva.session",
                session_id = %inner.session_id,
                "Agent run task: factory returned; draining events"
            );
            let mut event_count = 0usize;
            while let Some(event) = stream.next().await {
                event_count += 1;
                if let Err(e) = inner.persist_and_broadcast(&event).await {
                    tracing::error!(
                        target: "uva.session",
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
                // The settle transition consumes the controller
                // state's private suspended record: a suspended run
                // leaves the session paused (the log's open
                // interrupts are the truth, and the cached state
                // mirrors them so the list view and `is_startable`
                // agree without a rescan); anything else — a cancel
                // that already decided — stays where it is.
                inner.state.settle();
            }
            tracing::info!(
                target: "uva.session",
                session_id = %inner.session_id,
                "Agent run task: transitioned to Idle"
            );
            // R34: every row this run produced is now in the log.
            // The compaction rows sit in the writer's queue: the
            // flush puts them on disk so the compaction's provenance
            // is durable before the run ends.
            if let Err(e) = inner.writer.flush().await {
                tracing::warn!(
                    target: "uva.session",
                    session_id = %inner.session_id,
                    error = %e,
                    "Failed to flush the session writer"
                );
            }
            tracing::info!(
                target: "uva.session",
                session_id = %inner.session_id,
                event_count,
                "Agent run task: factory stream ended"
            );
            // R11: publish the Completing snapshot. The transition
            // table owns the frame; the run task only picks the
            // moment — after the flush, so subscribers see durable
            // rows before the run's outcome.
            inner
                .state
                .settle_flushed(inner.deps.lock().default_max_iterations);
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One request snapshot row, shaped as the writer emits it.
    fn snapshot(seq: u64, provider: &str, model: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "request_snapshot",
            "seq": seq,
            "ts": "2026-01-01T00:00:00+00:00",
            "provider": provider,
            "model": model,
            "params_hash": "p",
            "tools_hash": "t",
            "system_hash": "s",
            "sections": [],
            "policy_hash": "h",
        })
    }

    /// A snapshot row whose required `model` never reached the log —
    /// a broken tail row.
    fn broken_snapshot(seq: u64) -> serde_json::Value {
        let mut row = snapshot(seq, "openai", "gpt");
        row.as_object_mut()
            .expect("a row is an object")
            .remove("model");
        row
    }

    /// The resume pair is the *last row that decodes*. A tail row
    /// whose payload is broken is a defect, so the earlier snapshot
    /// answers instead of the resume falling back to the default
    /// provider — the read compares tags before decoding, and a
    /// snapshot it cannot decode was never a newer answer.
    #[test]
    fn the_last_decodable_snapshot_names_the_resume_pair() {
        let rows = vec![
            snapshot(1, "anthropic", "old-model"),
            snapshot(2, "openai", "gpt"),
            broken_snapshot(3),
        ];
        let log = Rows::read(&rows).expect("the log is readable");
        assert_eq!(
            last_request_snapshot(log),
            Some(("openai".to_string(), "gpt".to_string())),
        );
    }

    /// No snapshot, no answer: the run falls back to the configured
    /// default rather than inventing a pair.
    #[test]
    fn a_log_without_a_snapshot_names_no_pair() {
        let rows = vec![serde_json::json!({
            "type": "user_message",
            "seq": 1,
            "ts": "2026-01-01T00:00:00+00:00",
            "data": {},
        })];
        let log = Rows::read(&rows).expect("the log is readable");
        assert_eq!(last_request_snapshot(log), None);
    }

    /// A log carrying a row this build does not know is **refused**,
    /// not read. The resume cannot say which provider the suspended
    /// turn was sampled with, and "no snapshot" would resume the turn
    /// on the deployment default — a different request envelope, which
    /// is the drift the resume exists to prevent. So the log does not
    /// come into existence: there is no answer to collapse.
    #[test]
    fn an_unreadable_log_is_refused_rather_than_answered() {
        let rows = vec![
            snapshot(1, "openai", "gpt"),
            serde_json::json!({"type": "row_from_the_future", "seq": 2}),
        ];
        let error = Rows::read(&rows)
            .expect_err("the log carries a row this build cannot read");
        assert_eq!(error.index, 1);
        assert_eq!(error.tag, "row_from_the_future");
    }
}
