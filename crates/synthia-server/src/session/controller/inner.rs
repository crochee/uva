//! The controller's inner state: shared between the session handle,
//! the dispatch loop and the run task. Lifecycle shutdown, the
//! state gates, and run-config assembly live on it.

use std::{
    sync::{
        Arc,
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use synthia::{
    core::{AtomicCancelToken, Clock},
    harness::{
        AgentEvent,
        ApprovalRequest,
        ResumeDecision,
        ResumeToken,
        open_requests,
    },
    provider::traits::ModelProvider,
    session::{CompactionCheckpoint, SessionError, SessionSink},
};

use super::{
    deps::RunDependencies,
    live::LiveTurn,
    ops::SessionState,
    run_stream::RunStreamFactory,
};
use crate::{
    agent_run_config::{AgentRunConfig, CompactionEmitters, RunOverrides},
    event_stream::EventBroadcaster,
    session::input_queue::InputQueue as SessionInputQueue,
    state::UsageMetrics,
};

/// One parked turn waiting for the next run: the lossless
/// multimodal parts the text queue cannot carry, plus whether
/// the turn is a fresh prompt (`PromptMulti`) or a rerun of the
/// previous turn (`Rerun`).
///
/// A rerun's persisted prompt row must shadow the turn it
/// replaces (see `rerun_replace_op`); every other consumer of
/// the slot treats both kinds identically.
pub(super) struct ParkedPrompt {
    pub(super) parts: Vec<synthia::provider::ContentPart>,
    /// The id of the `parked_prompt` row this entry *is*.
    ///
    /// Minted when the park is made and carried through the queue so
    /// the run started from it can append `park_consumed` naming this
    /// exact row — the link is by id, not by the order the log happens
    /// to hold, because a text `Prompt` writes a `UserInput` without
    /// ever parking. It survives the crash path too: a prompt restored
    /// from the log carries the id its row does.
    pub(super) park_id: String,
    pub(super) agent_name: Option<String>,
    /// `true` when parked by `SessionOp::Rerun`.
    pub(super) rerun: bool,
}

/// The resume a pending operation parked for the next run.
pub(super) struct PendingResume {
    pub(super) token: ResumeToken,
    pub(super) decisions: Vec<ResumeDecision>,
    pub(super) allow_drift: bool,
}

pub(super) struct ControllerInner {
    pub(super) session_id: String,
    pub(super) user_id: String,
    pub(super) state: Arc<Mutex<SessionState>>,
    pub(super) queue: SessionInputQueue,
    pub(super) session_store: Arc<dyn SessionSink>,
    /// The session's one writer: the only thing that stamps a row's
    /// ordinal and the only thing that flushes it.
    pub(super) writer: Arc<synthia::session::LogWriter>,
    pub(super) broadcaster: EventBroadcaster,
    pub(super) deps: parking_lot::Mutex<RunDependencies>,
    /// Process-wide usage counters this controller records into —
    /// the shared `Arc` from its [`RunDependencies`], so the totals
    /// survive the session closing. Written only by
    /// [`ControllerInner::persist_and_broadcast`], the single funnel
    /// every agent event passes through.
    pub(super) usage: Arc<UsageMetrics>,
    /// Sub-agent session router: delegated child events are handed
    /// here (their own sessions) instead of joining this session's
    /// log and broadcast. `None` drops them.
    pub(super) subagent_router:
        Option<Arc<crate::session::subagent_router::SubagentRouter>>,
    pub(super) idle_timeout: Duration,
    pub(super) run_cancel: Mutex<Option<Arc<AtomicCancelToken>>>,
    pub(super) run_factory: Arc<dyn RunStreamFactory>,
    pub(super) alive: Arc<AtomicBool>,
    /// R11: read-only operation-snapshot bus. Publishes an
    /// [`synthia::session::OperationSnapshot`] at every run-state
    /// transition (Running / Idle / Cancelled) so HTTP `/status`,
    /// TUI, and replay consumers can observe progress without
    /// touching the run task internals.
    pub(super) snapshot_bus: synthia::session::SnapshotBus,
    /// [`SessionInputQueue`] cannot represent losslessly.
    /// Populated by the `PromptMulti` / `Rerun` op paths and
    /// consumed one entry per `maybe_start_run`.
    ///
    /// A queue, not a single slot: several turns can arrive while a
    /// run is live (a client that names its agent routes every text
    /// turn through `PromptMulti`), and overwriting the slot lost
    /// every turn but the last — the earlier message was never
    /// logged and its queued turn never started.
    pub(super) pending_multimodal:
        parking_lot::Mutex<std::collections::VecDeque<ParkedPrompt>>,
    /// The provider selection the next run samples with, parked by
    /// [`super::dispatch`] when an operation carries one (see
    /// [`PinnedProvider`](super::PinnedProvider)).
    ///
    /// `None` — the overwhelmingly common case — means the run uses
    /// the provider on [`RunDependencies`]. Like the parked
    /// multimodal payload this is per-run state: `maybe_start_run`
    /// takes it, so a selection cannot outlive the turn that made it.
    pub(super) pending_provider:
        parking_lot::Mutex<Option<Arc<dyn ModelProvider>>>,
    /// R29-Phase-I: one-shot guard for the terminal
    /// `LifecycleShutdown` emission. Every loop exit path calls
    /// `finalise_shutdown`; the first call wins so a
    /// shutdown-then-idle race cannot double-append.
    pub(super) shutdown_finalised: AtomicBool,
    /// This node's cluster bus + identity: every durable/newly-live
    /// event is fanned out here, and the session lease is renewed
    /// through it.
    pub(super) cluster: Arc<crate::cluster::ClusterRuntime>,
    /// The session lease this controller holds. Empty for a controller
    /// built outside `AppState` (tests / embedders): it then publishes
    /// no frames and renews nothing.
    pub(super) lease: String,
    /// The in-progress turn's transient projection, fed by the run's
    /// event tap. Never durable: a crash loses it by design.
    pub(super) live: parking_lot::Mutex<LiveTurn>,
    /// The resume a `SessionOp::Resume` parked. Taken by exactly one
    /// `maybe_start_run`, so a resume cannot govern two runs.
    pub(super) pending_resume: parking_lot::Mutex<Option<PendingResume>>,
    /// Set when the harness emits `Suspended`, cleared when the next run
    /// starts. The log remains the authority (`session_state()` derives
    /// from it); this only lets the list view report the pause without a
    /// second scan of a log it already read.
    pub(super) suspended: AtomicBool,
    /// Serialises the "is the run live?" decision for a steer against
    /// the run task's transition to `Idle` and against the cancel
    /// path.
    ///
    /// A steer that reads `Running` appends its row under this gate;
    /// the run task takes the gate before it publishes `Idle`, and
    /// the cancel path takes it before it fires the token and
    /// publishes `Cancelled`, so the three cannot interleave. Either
    /// the steer lands before the run settles (and the next step
    /// boundary injects it) or it reads a settled state and becomes
    /// an ordinary queued turn with no row of its own — never both,
    /// which is what keeps one message one row.
    pub(super) steer_gate: tokio::sync::Mutex<()>,
}

impl ControllerInner {
    /// Append the terminal `LifecycleShutdown` session event and
    /// close the session sink. Idempotent.
    ///
    /// Called from every exit path of the controller loop so a
    /// session log never ends without a shutdown marker — the
    /// counterpart of the `CompactionStart`/`CompactionEnd`
    /// pairing for the whole session.
    pub(super) async fn finalise_shutdown(
        &self,
        reason: synthia::session::SessionEndReason,
    ) {
        if self.shutdown_finalised.swap(true, Ordering::SeqCst) {
            return;
        }
        let event = synthia::session::SessionEvent::LifecycleShutdown {
            seq: 0,
            ts: self.deps.lock().clock.now().to_rfc3339(),
            data: serde_json::json!({ "reason": reason }),
        };
        self.append_lifecycle_event(&event).await;
        self.close_sink(reason).await;
    }

    /// Serialize + append one lifecycle terminal event,
    /// logging (not propagating) either failure — the
    /// shutdown path must run to the sink close regardless.
    async fn append_lifecycle_event(
        &self,
        event: &synthia::session::SessionEvent,
    ) {
        let value = match serde_json::to_value(event) {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!(
                    target: "synthia.session",
                    session_id = %self.session_id,
                    error = %e,
                    "Failed to serialise lifecycle_shutdown event"
                );
                return;
            }
        };
        // The marker goes through the session's one writer, and
        // the flush lands it before the sink closes — a row left
        // in the writer's queue could not survive the close.
        if let Err(e) = self.append_durable_row(value).await {
            tracing::warn!(
                target: "synthia.session",
                session_id = %self.session_id,
                error = %e,
                "Failed to append lifecycle_shutdown event"
            );
        }
    }

    /// Append one row through the session's writer and flush it.
    ///
    /// For the controller's out-of-band durable rows — feedback,
    /// the shutdown marker — which no run flushes for: the append
    /// only enqueues, so the flush is what puts the row on disk
    /// before the op returns.
    pub(super) async fn append_durable_row(
        &self,
        value: serde_json::Value,
    ) -> Result<(), SessionError> {
        self.writer.append(value).await?;
        self.writer.flush().await
    }

    /// Append `content` to the log as a steer row for the run that
    /// is live now, or report that no run is live.
    ///
    /// "Live" means the run can still sample again: the run task
    /// flips the state to `Idle` as soon as its stream ends — before
    /// its flushes — so a run that has already emitted its last
    /// request is not live, and a steer that reached it would never
    /// be carried by any request of that run.
    ///
    /// Returns `Ok(true)` once the row is durable — the caller may
    /// promise the injection — and `Ok(false)` when the session is
    /// not `Running`, in which case nothing was written: the queued
    /// turn the caller falls back to logs the message itself when it
    /// starts, and one message must stay one row.
    ///
    /// # Errors
    ///
    /// Returns the writer's error when the row cannot be made
    /// durable. The caller must not fall back to a queued turn on
    /// that error — the row may have landed, and a second delivery
    /// would duplicate the message.
    pub(super) async fn append_live_steer(
        &self,
        content: &str,
    ) -> Result<bool, SessionError> {
        // The gate orders this decision against the run task's
        // `Idle` transition and against the cancel path. The state
        // lock alone cannot: the append has to await, and holding a
        // std lock across an await would be neither correct nor
        // `Send`.
        let _live = self.steer_gate.lock().await;
        if *self.state.lock().expect("state mutex poisoned")
            != SessionState::Running
        {
            return Ok(false);
        }
        let data =
            serde_json::to_value(synthia::provider::Message::user(content))
                .map_err(|error| {
                    SessionError::AppendFailed(format!(
                        "serialize steer row: {error}"
                    ))
                })?;
        self.writer
            .append_event(&synthia::session::user_message(data, Some("steer")))
            .await?;
        // The answer is a durability promise: the row must be on disk
        // before the op returns.
        self.writer.flush().await?;
        Ok(true)
    }

    /// Close the session sink. A close failure is logged but
    /// does not abort the shutdown — the sink remains
    /// reachable on the next start.
    async fn close_sink(&self, reason: synthia::session::SessionEndReason) {
        if let Err(e) = self.session_store.close(reason).await {
            tracing::warn!(
                target: "synthia.session",
                session_id = %self.session_id,
                error = %e,
                "Failed to close session sink"
            );
        }
    }

    /// The interrupts the log still holds open, in request order.
    ///
    /// The paused state is a property of the log, so it survives a
    /// restart and cannot disagree with what a resume must answer. A
    /// read failure degrades to "no open interrupt", which is right for
    /// the status-shaped callers and **must not** be read as a decision:
    /// a caller that takes it that way asks
    /// [`Self::try_open_interrupts`] instead.
    pub(super) async fn open_interrupts(&self) -> Vec<ApprovalRequest> {
        match self.try_open_interrupts().await {
            Ok(open) => open,
            Err(error) => {
                tracing::warn!(
                    target: "synthia.session",
                    session_id = %self.session_id,
                    %error,
                    "approval scan: the log could not be read"
                );
                Vec::new()
            }
        }
    }

    /// [`Self::open_interrupts`] without the swallow.
    ///
    /// "No open interrupt" has two meanings — the asks were answered, or
    /// the log could not be read — and only the first one means a
    /// resume's token is spent. A caller that acts on the difference
    /// must see the error.
    ///
    /// # Errors
    ///
    /// The writer's read error.
    pub(super) async fn try_open_interrupts(
        &self,
    ) -> Result<Vec<ApprovalRequest>, SessionError> {
        let rows = self.writer.read().await?;
        Ok(open_requests(&rows))
    }

    /// `true` while the log holds an undecided interrupt.
    ///
    /// # Errors
    ///
    /// The writer's read error. `false` would be an *answer* — "the asks
    /// were answered" — and a caller that starts a run on it would run
    /// over a suspended turn whose log it could not read. The
    /// status-shaped callers swallow at their own boundary (they have no
    /// error channel); the run path refuses instead.
    pub(super) async fn awaiting_approval(&self) -> Result<bool, SessionError> {
        Ok(!self.try_open_interrupts().await?.is_empty())
    }

    /// Record the log-derived paused state, so `/status`, the list
    /// view and the follow baseline agree without each re-reading the
    /// log. Refuses to overwrite a live run.
    pub(super) fn mark_awaiting_approval(&self) {
        let mut state = self.state.lock().expect("state mutex poisoned");
        if *state == SessionState::Idle || *state == SessionState::Cancelled {
            *state = SessionState::AwaitingApproval;
        }
    }

    /// Append a decision for every open interrupt and flush, so a
    /// cancelled session is no longer paused.
    ///
    /// Cancelling is an answer: leaving the asks open would make the
    /// next `maybe_start_run` refuse forever.
    pub(super) async fn settle_open_approvals(&self, status: &str) {
        let requests = self.open_interrupts().await;
        if requests.is_empty() {
            return;
        }
        for request in &requests {
            self.writer.append_event_nowait(
                &synthia::session::approval_decided(
                    &request.id,
                    &request.call_id,
                    status,
                    "cancel",
                    None,
                ),
            );
        }
        if let Err(error) = self.writer.flush().await {
            tracing::warn!(
                target: "synthia.session",
                session_id = %self.session_id,
                %error,
                "failed to flush the cancelled approvals"
            );
        }
    }

    /// Start a fresh live projection for the run about to start.
    pub(super) fn reset_live(&self) {
        self.suspended.store(false, Ordering::SeqCst);
        self.live.lock().reset(uuid::Uuid::new_v4().to_string());
    }

    /// Fold one run event into the live projection.
    pub(super) fn tap_live(&self, event: &AgentEvent) {
        self.live.lock().observe(event);
    }

    /// A consistent copy of the live projection for a `follow`
    /// baseline or frame.
    pub(super) fn live_snapshot(&self) -> LiveTurn {
        self.live.lock().clone()
    }

    /// The state-gate check `maybe_start_run` runs first: the
    /// controller only starts a new run when it's Idle or
    /// Cancelled (the latter because a cancellation marks the
    /// state, then maybe_start_run wakes the controller back
    /// up — see the Cancel op handler).
    pub(super) fn is_startable(&self) -> bool {
        let state = self.state.lock().expect("state mutex poisoned");
        if *state != SessionState::Idle && *state != SessionState::Cancelled {
            tracing::trace!(
                target: "synthia.session",
                session_id = %self.session_id,
                state = ?*state,
                "maybe_start_run: skipped (not Idle/Cancelled)"
            );
            return false;
        }
        true
    }

    /// Transition the controller to `Running`, log the run's
    /// shape, publish the R11 Running snapshot (so `/status`
    /// subscribers observe the transition immediately), and
    /// install a fresh cancel token for the run about to
    /// start.
    pub(super) async fn mark_running(&self, multimodal_active: bool) {
        {
            let mut state = self.state.lock().expect("state mutex poisoned");
            *state = SessionState::Running;
        }
        let pending_count = self
            .queue
            .has_pending(&self.user_id, &self.session_id)
            .await as usize;
        tracing::info!(
            target: "synthia.session",
            session_id = %self.session_id,
            pending_count,
            multimodal = multimodal_active,
            subscribers = self.broadcaster.subscriber_count(),
        );
        let agent_name = self
            .deps
            .lock()
            .default_agent_name
            .as_ref()
            .and_then(|l| l.read().clone())
            .unwrap_or_else(|| "default".to_string());
        let max_iters = self.deps.lock().default_max_iterations;
        let _seq = self.snapshot_bus.publish(
            synthia::session::OperationSnapshot::started(
                self.session_id.clone(),
                agent_name,
                max_iters,
            ),
        );

        let cancel_token = Arc::new(AtomicCancelToken::new());
        *self.run_cancel.lock().expect("run_cancel mutex poisoned") =
            Some(cancel_token);
        self.reset_live();
    }

    /// Build an [`AgentRunConfig`] for the next run. Lets the
    /// caller pin the agent name for THIS run only; the
    /// configured default is still consulted as a fallback (and
    /// the first registered agent as a last resort) — the
    /// `explicit` argument just takes the top of the
    /// `explicit > configured default > first registered` ladder.
    ///
    /// `provider` pins the model backing for THIS run only: the
    /// turn's `model` selection, already resolved to a handle by
    /// [`AppState::resolve_provider`](crate::state::AppState::resolve_provider).
    /// `None` keeps the deployment's configured provider. The pinned
    /// handle also composes the context manager, so a summarising
    /// strategy summarises with the model that actually runs. The
    /// durable `request_header` names it too: the harness stamps it
    /// from the agent it was handed, which is built from this config.
    pub(super) fn build_run_config_with_explicit_agent(
        &self,
        explicit: Option<&str>,
        provider: Option<Arc<dyn ModelProvider>>,
        checkpoint: Option<&Arc<CompactionCheckpoint>>,
    ) -> (AgentRunConfig, RunOverrides) {
        let deps = self.deps.lock();
        let provider = provider.unwrap_or_else(|| Arc::clone(&deps.provider));
        // The node's one registry: every run advertises the live
        // surface, so a plugin tool installed at boot is visible here
        // without a per-run copy.
        let tool_registry = Arc::clone(&deps.tool_registry);

        let default_name = deps
            .default_agent_name
            .as_ref()
            .and_then(|m| m.read().clone());
        let agent_name = deps.agent_registry.as_ref().and_then(|reg| {
            crate::state::AppState::resolve_agent_name(
                reg,
                default_name.as_deref(),
                explicit,
            )
        });

        let overrides = RunOverrides {
            context_manager: deps.compose_context_manager_with_emitters(
                Arc::clone(&provider),
                checkpoint.map(|checkpoint| {
                    CompactionEmitters::new(
                        checkpoint.record_callback(),
                        checkpoint.lifecycle_callback(),
                    )
                }),
            ),
            tool_surface: deps.tool_surface.clone(),
            strategy: deps.strategy.clone(),
            tool_restriction: deps.tool_restriction.clone(),
        };

        let config = AgentRunConfig {
            provider: Arc::clone(&provider),
            tool_registry,
            workspace_root: deps.workspace_root.clone(),
            system_prompt: deps.system_prompt.clone(),
            prompt_context: deps.prompt_context.clone(),
            agent_name,
            max_iterations: Some(deps.default_max_iterations),
            steering: Arc::clone(&deps.steering),
            agent_registry: deps.agent_registry.clone(),
            log_writer: None,
            subagent_writers: None,
            host: deps.plugin_host.clone(),
        };
        (config, overrides)
    }
}
