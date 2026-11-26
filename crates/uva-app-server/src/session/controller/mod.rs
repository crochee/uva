//! Per-session controller that serializes prompt/steer/cancel operations
//! and ensures at most one `Agent::run` per session.
//!
//! # Module layout
//!
//! - `ops`: `SessionOp` / `SessionState` wire types.
//! - `run_stream`: the run-stream factory seam.
//! - `deps`: `RunDependencies` — what every run is handed.
//! - `state`: the controller-state transition table — how the
//!   lifecycle moves and what each move publishes, in one place.
//! - `SessionController` (this file): the shared handle.
//! - `inner`: the shared inner state + lifecycle gates.
//! - `run_task` / `persist`: the run task and its event sink,
//!   as method continuations of the inner state.
//! - `dispatch`: the operation loop.
//! - `run_log`: the helpers that shape a run's prompt rows.

use std::{
    future::Future,
    sync::{
        Arc,
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result};
use tokio::sync::mpsc;
use uva::session::SessionLog;

use crate::{
    cluster::{ClusterRuntime, ControlFrame, SessionKey, SessionOpWire},
    event_stream::EventBroadcaster,
    session::status::{self, SessionState},
};

mod deps;
mod dispatch;
mod inner;
mod live;
mod ops;
mod persist;
mod pin;
pub(crate) mod run_log;
mod run_stream;
mod run_task;
mod spawn_gate;
mod state;

pub use deps::{ProviderResolver, RunDependencies};
use dispatch::run_controller_loop;
use inner::ControllerInner;
pub use live::{LiveToolCall, LiveTurn};
pub use ops::{SessionOp, SteerDelivery};
pub use pin::PinnedProvider;
use pin::{OpAck, SubmittedOp};
pub use run_stream::{AgentRunStreamFactory, RunStreamFactory};
pub(crate) use spawn_gate::SpawnGate;
pub use uva::harness::{AgentEvent, AgentInput};

/// Default idle timeout before the controller shuts itself down when
/// no run is active and there are no streaming subscribers.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How long [`SessionController::close`] waits for the controller
/// loop to finish. MUST strictly exceed the loop's own worst case:
/// `handle_shutdown_op` spends up to 3 s joining an in-flight run
/// *before* the loop's exit path runs `finalise_shutdown` (append +
/// flush of the `lifecycle_shutdown` marker) and releases the lease.
/// An outer bound of 3 s would fire first and the caller would move
/// on while the loop was still mid-finalise — the runtime drop would
/// then abort it, losing the marker and the release. It must also
/// stay *below* the caller-side drain deadline
/// (`crate::shutdown::DRAIN_DEADLINE`, 8 s) so this inner warning can
/// actually fire instead of being superseded by the outer abandon.
/// 6 s keeps both: 3 s join + finalise headroom, 2 s under the drain.
pub(crate) const CLOSE_WAIT_TIMEOUT: Duration = Duration::from_secs(6);

/// How long a submitter waits for the controller loop to report an
/// operation's durable effect.
///
/// The bound is on the *wait*, not on the work: the loop parks the
/// turn and flushes the row while the submitter waits, and the wait
/// ends the moment the row is on disk. What the bound protects
/// against is a controller that is alive but wedged — the caller gets
/// an error instead of an open-ended request. A controller that died
/// needs no bound: the loop takes the operation channel with it, and
/// the wait ends there.
const DURABLE_SUBMIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared handle to a running session controller.
pub struct SessionController {
    session_id: String,
    state: Arc<state::ControllerState>,
    op_tx: mpsc::Sender<SubmittedOp>,
    broadcaster: EventBroadcaster,
    alive: Arc<AtomicBool>,
    /// R29-Phase-I: idempotency guard for
    /// [`SessionController::close`]. Set on the first call so
    /// a second call is a no-op.
    closed: AtomicBool,
    /// R11: shared with [`ControllerInner`] (same `Arc`
    /// interior) so publishes from the run loop reach
    /// subscribers of the outer handle.
    snapshot_bus: uva::session::SnapshotBus,
    /// `true` for a handle whose session is owned by another node:
    /// `submit` publishes control frames and `subscribe` is fed from
    /// the bus instead of a local run loop.
    remote: bool,
    /// The loop's shared state, for the one operation that must act
    /// on the live run without waiting behind the serialized op
    /// channel: a durable steer (see [`Self::steer`]). `None` for a
    /// remote handle — it owns no run and no log.
    inner: Option<Arc<ControllerInner>>,
    /// The signal that ends this controller's job delivery.
    ///
    /// The delivery task waits on it: a controller that is dropped
    /// (forgotten, deleted, closed) must not leave a task behind.
    job_delivery_stop: Arc<tokio::sync::Notify>,
    /// The session's cold-open gate, owned by this controller.
    ///
    /// The gate map holds a weak reference to the lock and this
    /// controller holds the strong one, so the map's entry lives
    /// exactly as long as the controller does — whichever path dropped
    /// it. `None` for a controller that was not opened through the
    /// cold-open path (a test fixture's own).
    spawn_gate: parking_lot::Mutex<Option<SpawnGate>>,
}

impl SessionController {
    /// Spawn the background controller task and return a shared handle.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        user_id: impl Into<String>,
        session_id: impl Into<String>,
        writer: Arc<dyn SessionLog>,
        deps: RunDependencies,
        idle_timeout: Duration,
        run_factory: Arc<dyn RunStreamFactory>,
    ) -> Arc<Self> {
        Self::spawn_with_lease(
            user_id,
            session_id,
            writer,
            deps,
            idle_timeout,
            run_factory,
            String::new(),
        )
    }

    /// [`Self::spawn`] with an explicit session lease. `AppState` calls
    /// this after the bus grants
    /// `"{ns}:lease:session:{user}:{session}"`; a controller with an
    /// empty lease publishes no frames and renews nothing (tests and
    /// embedders take that shape).
    ///
    /// The `writer` is the session's one writer, seen through the
    /// typed log seam: the handle that stamps the rows' ordinals,
    /// flushes them, and closes the log. The type guarantees one
    /// handle over one log — not *which* log; the caller that builds
    /// the controller pairs it with the session, and the sink living
    /// inside the handle is what makes that pairing a single value
    /// rather than two arguments that could disagree.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_lease(
        user_id: impl Into<String>,
        session_id: impl Into<String>,
        writer: Arc<dyn SessionLog>,
        deps: RunDependencies,
        idle_timeout: Duration,
        run_factory: Arc<dyn RunStreamFactory>,
        lease: String,
    ) -> Arc<Self> {
        let user_id = user_id.into();
        let session_id = session_id.into();
        let broadcaster =
            EventBroadcaster::with_label(format!("{user_id}/{session_id}"));
        let (op_tx, op_rx) = mpsc::channel(64);
        let alive = Arc::new(AtomicBool::new(true));
        // R11: one bus shared between the outer handle and the
        // inner loop (Clone shares the Arc interior).
        let snapshot_bus = uva::session::SnapshotBus::new();
        // The controller state owns the transition table; the bus it
        // publishes on is the handle's own.
        let state = state::ControllerState::new(
            session_id.clone(),
            snapshot_bus.clone(),
        );
        // Cloned out before `deps` moves into the inner mutex: the
        // counters are the controller's write handle on the
        // process-wide totals.
        let usage = Arc::clone(&deps.usage_metrics);
        let cluster = Arc::clone(&deps.cluster);
        // The delivery task's stop signal: the controller's end ends
        // it.
        let job_delivery_stop = Arc::new(tokio::sync::Notify::new());
        // Cloned out before `deps` moves into the inner mutex: the
        // delivery task needs the registry, not the whole dependency
        // set.
        let deps_jobs = deps.jobs.clone();

        let inner = Arc::new(ControllerInner {
            session_id: session_id.clone(),
            user_id: user_id.clone(),
            state: state.clone(),
            cached_history: parking_lot::Mutex::new(None),
            writer,
            broadcaster: broadcaster.clone(),
            deps: parking_lot::Mutex::new(deps),
            usage,
            idle_timeout,
            run_cancel: Mutex::new(None),
            run_factory,
            alive: alive.clone(),
            pending_turns: parking_lot::Mutex::new(
                std::collections::VecDeque::new(),
            ),
            pending_provider: parking_lot::Mutex::new(None),
            shutdown_finalised: AtomicBool::new(false),
            cluster,
            lease,
            live: parking_lot::Mutex::new(LiveTurn::default()),
            pending_resume: parking_lot::Mutex::new(None),
            steer_gate: tokio::sync::Mutex::new(()),
            steer_inbox: parking_lot::Mutex::new(None),
            compaction_pass_gate: tokio::sync::Mutex::new(()),
            on_exit: parking_lot::Mutex::new(None),
        });

        // The handle keeps the loop's shared state so a steer can act
        // on the live run directly; the op loop, the run task, and the
        // handle all share the one `Arc`.
        let controller = Arc::new(Self {
            session_id,
            state,
            op_tx,
            broadcaster,
            alive,
            closed: AtomicBool::new(false),
            snapshot_bus,
            remote: false,
            inner: Some(Arc::clone(&inner)),
            spawn_gate: parking_lot::Mutex::new(None),
            job_delivery_stop: Arc::clone(&job_delivery_stop),
        });
        tokio::spawn(run_controller_loop(inner, op_rx));

        // The background jobs this session owns: a job that finishes
        // is delivered to the session that started it, so the model
        // learns that its command is done without polling for it.
        if let Some(jobs) = deps_jobs {
            spawn_job_delivery(
                Arc::clone(&controller),
                jobs,
                Arc::clone(&job_delivery_stop),
            );
        }

        controller
    }

    /// Submit an operation to the serialized controller loop.
    ///
    /// The turn runs on the session's configured default provider.
    /// Callers that resolved a `model` selection use
    /// [`SessionController::submit_with_provider`] instead.
    pub async fn submit(&self, op: SessionOp) -> Result<()> {
        self.submit_with_provider(op, None).await
    }

    /// Build the handle for a session another node owns.
    ///
    /// Two background tasks replace the local controller loop:
    ///
    /// - **forwarder** — every `submit` becomes a control frame
    ///   (`Cancel` → `CancelSession`, anything else → `ForwardOp`);
    /// - **tail bridge** — session frames from the bus are decoded and
    ///   re-published on this handle's `EventBroadcaster`, so the SSE
    ///   handler's `subscribe()` path stays identical.
    ///
    /// Provider pins are node-local (`PinnedProvider` holds an
    /// `Arc<dyn ModelProvider>`), so a pinned submit on a remote
    /// handle is rejected rather than silently downgraded.
    ///
    /// A remote handle takes no writer: it runs no agent, writes no
    /// row, and holds no checkpoint. The node that owns the session
    /// owns its log, and a forwarder must not touch the session's
    /// files — not even to open them.
    #[must_use]
    pub fn spawn_remote(
        user_id: impl Into<String>,
        session_id: impl Into<String>,
        cluster: Arc<ClusterRuntime>,
    ) -> Arc<Self> {
        let user_id = user_id.into();
        let session_id = session_id.into();
        let key = SessionKey::new(user_id.clone(), session_id.clone());
        let broadcaster =
            EventBroadcaster::with_label(format!("{user_id}/{session_id}"));
        let (op_tx, mut op_rx) = mpsc::channel(64);
        let alive = Arc::new(AtomicBool::new(true));
        let snapshot_bus = uva::session::SnapshotBus::new();
        // A remote handle reads the state but never advances it: the
        // owning node owns the transitions.
        let state = state::ControllerState::new(
            session_id.clone(),
            snapshot_bus.clone(),
        );

        let controller = Arc::new(Self {
            session_id,
            state,
            op_tx,
            broadcaster: broadcaster.clone(),
            alive,
            closed: AtomicBool::new(false),
            snapshot_bus,
            remote: true,
            // A forwarder owns no run and no log: it can only
            // forward the op, so a steer on it becomes a queued turn
            // on the owning node.
            inner: None,
            spawn_gate: parking_lot::Mutex::new(None),
            // A remote handle owns no job delivery: the owning node
            // delivers.
            job_delivery_stop: Arc::new(tokio::sync::Notify::new()),
        });

        // Forwarder: local `submit` → bus control frame.
        {
            let cluster = Arc::clone(&cluster);
            let key = key.clone();
            let origin = cluster.node_id.clone();
            tokio::spawn(async move {
                while let Some(submitted) = op_rx.recv().await {
                    let model =
                        submitted.provider.as_ref().map(|pin| pin.model_name());
                    let turn_id = submitted.turn_id.clone();
                    let frame = match submitted.op {
                        SessionOp::Shutdown => break,
                        SessionOp::Cancel { .. } => {
                            ControlFrame::CancelSession {
                                origin: origin.clone(),
                                key: key.clone(),
                            }
                        }
                        other => match SessionOpWire::from_op(&other) {
                            Some(op) => ControlFrame::ForwardOp {
                                origin: origin.clone(),
                                key: key.clone(),
                                op,
                                model,
                                turn_id,
                            },
                            None => {
                                // Resume has no wire form: it reads
                                // the owner's log, so it is owner-node
                                // only. Say so rather than drop it
                                // silently.
                                let kind = match &other {
                                    SessionOp::Resume { .. } => "resume",
                                    SessionOp::StartPending => "start_pending",
                                    _ => "unknown",
                                };
                                tracing::warn!(
                                    session = %key,
                                    op = kind,
                                    "cluster: op has no wire form; not forwarded"
                                );
                                continue;
                            }
                        },
                    };
                    if let Err(e) = cluster.bus.publish_control(frame).await {
                        tracing::warn!(
                            session = %key,
                            error = %e,
                            "cluster: failed to forward a remote op"
                        );
                    }
                }
            });
        }

        // Tail bridge: bus session frames → this handle's broadcast.
        {
            let bus = Arc::clone(&cluster.bus);
            let broadcaster = broadcaster.clone();
            tokio::spawn(async move {
                let Ok(mut rx) = bus.subscribe_session(&key).await else {
                    return;
                };
                while let Some(frame) = rx.recv().await {
                    // The bus carries the channel's own item — a run's
                    // event or a turn's end — and the durable rows
                    // beside it. A row is not an item, so the decode
                    // refuses it and the frame is skipped.
                    if let Ok(item) = serde_json::from_value::<
                        crate::event_stream::BroadcastEvent,
                    >(frame.payload)
                    {
                        let _ = broadcaster.send(item);
                    }
                }
            });
        }

        controller
    }

    /// Submit an operation together with the provider that runs it.
    ///
    /// `provider` is `None` for the session's configured default —
    /// the same thing [`SessionController::submit`] does. The
    /// selection applies to *this* operation's turn only: it is
    /// consumed by the run the operation starts, so a later operation
    /// without one runs the default again, and two operations queued
    /// behind a running turn each keep their own provider.
    ///
    /// A **remote** handle forwards the selection's model name to the
    /// owner, which resolves it against its own catalogue: the pin is
    /// a fact about the turn, not about this node.
    ///
    /// The operation is *queued*, not *durable*: it becomes a row on
    /// disk when the controller loop handles it. A caller that must
    /// promise the row uses [`Self::submit_durable_with_provider`].
    pub async fn submit_with_provider(
        &self,
        op: SessionOp,
        provider: Option<PinnedProvider>,
    ) -> Result<()> {
        self.submit_turn(op, provider, None).await
    }

    /// Submit an operation, naming the turn it starts.
    ///
    /// The turn id is the submitter's: it becomes the turn's durable
    /// `parked_prompt` row id and the tag on every live event of the
    /// turn, so a client that asked for this turn's stream matches
    /// the stream and the transcript with one id. `None` leaves the
    /// id to the controller, which mints one.
    pub async fn submit_turn(
        &self,
        op: SessionOp,
        provider: Option<PinnedProvider>,
        turn_id: Option<String>,
    ) -> Result<()> {
        let submitted = SubmittedOp {
            op,
            provider,
            turn_id,
            ack: None,
        };
        self.op_tx
            .send(submitted)
            .await
            .context("session controller is shut down")?;
        Ok(())
    }

    /// Submit an operation and wait until its durable effect is on
    /// disk.
    ///
    /// [`Self::submit`] returns as soon as the operation is queued:
    /// the turn becomes durable later, when the controller loop
    /// handles it. This one waits for that, so an answer that says
    /// "queued" can mean "the turn is on disk" — the promise a server
    /// whose selling point is a durable session truth has to keep.
    ///
    /// The wait ends when the loop has made the operation's row
    /// land (a park's `parked_prompt` row), when the controller dies
    /// (the loop takes the operation channel with it), or when the
    /// bound expires. It cannot deadlock: the wait holds nothing the
    /// loop needs — the ack is a oneshot the loop answers — and the
    /// operation channel is bounded, so a busy controller delays the
    /// wait rather than blocking the loop.
    ///
    /// A **remote** handle (the session is owned by another node)
    /// cannot confirm the row: the operation is forwarded to the
    /// owner, which parks it there. The call returns as soon as the
    /// operation is handed to the forwarder, and the caller's promise
    /// is the forwarding.
    ///
    /// # Errors
    ///
    /// Returns the writer's error when the operation's row could not
    /// be made durable, the controller's error when it stopped before
    /// the row was durable, and a timeout error when the bound
    /// expired.
    pub async fn submit_durable(&self, op: SessionOp) -> Result<()> {
        self.submit_durable_with_provider(op, None).await
    }

    /// [`Self::submit_durable`] with the turn's provider selection.
    ///
    /// The selection applies to *this* operation's turn only, exactly
    /// as [`Self::submit_with_provider`] documents.
    pub async fn submit_durable_with_provider(
        &self,
        op: SessionOp,
        provider: Option<PinnedProvider>,
    ) -> Result<()> {
        self.submit_durable_turn(op, provider, None).await
    }

    /// [`Self::submit_durable_with_provider`] with the turn's id
    /// minted by the caller.
    ///
    /// The id names the turn the operation starts: it becomes the
    /// turn's durable `parked_prompt` row id and the tag on every
    /// live event of the turn. A client that streams one turn's
    /// answer matches the stream and the transcript with it.
    pub async fn submit_durable_turn(
        &self,
        op: SessionOp,
        provider: Option<PinnedProvider>,
        turn_id: Option<String>,
    ) -> Result<()> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        let submitted = SubmittedOp {
            op,
            provider,
            turn_id,
            // A remote handle has no log to confirm: the owner parks
            // the turn there.
            ack: (!self.remote).then_some(ack_tx),
        };
        self.op_tx
            .send(submitted)
            .await
            .context("session controller is shut down")?;
        if self.remote {
            return Ok(());
        }
        match tokio::time::timeout(DURABLE_SUBMIT_TIMEOUT, ack_rx).await {
            Ok(Ok(OpAck::Durable)) => Ok(()),
            Ok(Ok(OpAck::Failed(message))) => Err(anyhow::anyhow!(
                "the session's turn could not be made durable: {message}"
            )),
            Ok(Err(_)) => Err(anyhow::anyhow!(
                "the session controller stopped before the turn was durable"
            )),
            Err(_) => Err(anyhow::anyhow!(
                "the session controller did not make the turn durable within {}s",
                DURABLE_SUBMIT_TIMEOUT.as_secs()
            )),
        }
    }

    /// Deliver `content` into the run that is live now.
    ///
    /// A live run receives the message as a durable `user_message`
    /// row (`source: "steer"`), flushed before this returns. The
    /// loop re-derives its transcript from the log at every step
    /// boundary, so the model sees the message in its next request
    /// and never in the one already in flight; no in-memory steer
    /// queue sits in between.
    ///
    /// When no run is live — it already settled, the session is
    /// idle, or this is a remote forwarder — the message becomes an
    /// ordinary queued turn and nothing is appended here: the run
    /// that starts logs the turn itself, which is what keeps one
    /// message one row. That queued turn is parked durably before
    /// this returns, so both deliveries are on disk when the caller
    /// is answered.
    ///
    /// `provider` follows [`Self::submit_with_provider`]: it applies
    /// only when the message degrades to a queued turn, because a
    /// live run has already sampled with its own provider.
    ///
    /// # Errors
    ///
    /// Returns the writer's error when a steer row cannot be made
    /// durable, the channel's error when a queued fallback cannot
    /// be delivered, and the durability error when the queued turn's
    /// park did not reach the disk.
    pub async fn steer(
        &self,
        content: String,
        provider: Option<PinnedProvider>,
    ) -> Result<SteerDelivery> {
        if let Some(inner) = self.inner.as_ref()
            && inner.append_live_steer(&content).await?
        {
            return Ok(SteerDelivery::Steer);
        }
        self.submit_durable_with_provider(
            SessionOp::Steer {
                content,
                priority: 1,
            },
            provider,
        )
        .await?;
        Ok(SteerDelivery::Queue)
    }

    /// Convenience helper to request cancellation of the current run.
    pub async fn cancel(&self) -> Result<()> {
        self.submit(SessionOp::Cancel { reason: None }).await
    }

    /// The session this controller owns.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// `true` when the session is owned by another node and this
    /// handle only forwards ops / mirrors the bus tail.
    pub fn is_remote(&self) -> bool {
        self.remote
    }

    /// Current controller state: the in-memory lifecycle only.
    ///
    /// [`Self::session_state`] is the wire answer — it also derives
    /// `AwaitingApproval` from the log.
    pub fn state(&self) -> SessionState {
        self.state.current()
    }

    /// The state the wire reports.
    ///
    /// A live run wins outright. Otherwise the log decides, through
    /// the session-status module: an `approval_asked` row with no
    /// `approval_decided` in the last suspended segment means
    /// `AwaitingApproval`, whatever this process remembers, and a flag
    /// that still says `AwaitingApproval` while the log holds no open
    /// ask is stale — the module demotes it to `Idle`. That is what
    /// lets a restarted server report the same state.
    ///
    /// A status-shaped surface has no error channel, so an unreadable
    /// log reports the in-memory state after a warning. The run path
    /// does not make that trade — see
    /// `ControllerInner::awaiting_approval`.
    pub async fn session_state(&self) -> SessionState {
        let state = self.state();
        if state == SessionState::Running || self.remote {
            return state;
        }
        let Some(inner) = self.inner.as_ref() else {
            return state;
        };
        match inner.awaiting_approval().await {
            Ok(awaiting) => status::controller_state(status::StatusFacts {
                controller: Some(state),
                awaiting,
            }),
            Err(error) => {
                tracing::warn!(
                    target: "uva.session",
                    session_id = %inner.session_id,
                    %error,
                    "status: the log could not be read; reporting the                      in-memory state"
                );
                state
            }
        }
    }

    /// Re-queue a prompt a previous owner parked.
    ///
    /// The `parked_prompt` row is already in the log — the caller read
    /// it and rebuilt the parts — so nothing is appended here. Appending
    /// a second row per restart would grow the log by one entry per
    /// process, and the row's job (say the turn is queued) is already
    /// done. `park_id` is the recovered row's key, so the run this
    /// prompt eventually starts spends the row that already exists
    /// instead of leaving it to be recovered again. Returns `false` for
    /// a remote handle, which owns no queue.
    pub fn restore_parked_prompt(
        &self,
        parts: Vec<uva::provider::ContentPart>,
        park_id: String,
        agent_name: Option<String>,
        provider: Option<Arc<dyn uva::provider::traits::ModelProvider>>,
        rerun: bool,
    ) -> bool {
        let Some(inner) = self.inner.as_ref() else {
            return false;
        };
        if let Some(provider) = provider {
            *inner.pending_provider.lock() =
                Some(PinnedProvider::new(provider));
        }
        inner.pending_turns.lock().push_back(inner::ParkedPrompt {
            parts,
            park_id,
            agent_name,
            rerun,
        });
        true
    }

    /// Install the state the log implies.
    ///
    /// A controller spawned over an existing session starts from
    /// `Idle` — process memory has no history — so the node that takes a
    /// session over asks the log what the session is doing and records
    /// the answer here. A live run is never overwritten: the adoption is
    /// the takeover path, which runs before this controller has a run.
    pub(crate) fn adopt_state(&self, state: SessionState) {
        self.state.advance(state::Transition::Adopt { state });
    }

    /// Drop the folded history this controller cached.
    ///
    /// The cache is the log's history at one ordinal. A node that
    /// takes a session over inherits nothing from the node that lost
    /// it, so the cache goes with the takeover: the log is the truth
    /// now.
    ///
    /// The eviction itself belongs to the inner: the handle asks by
    /// name and never names the lock that holds the cache.
    pub(crate) fn clear_cached_history(&self) {
        if let Some(inner) = self.inner.as_ref() {
            inner.clear_cached_history();
        }
    }

    /// Give the controller the session's cold-open gate.
    ///
    /// The gate belongs to the controller: the map holds a weak
    /// reference to the lock and the controller holds the strong one,
    /// so the map's entry leaves with the controller — through every
    /// path that drops it, not only the one that evicts a session
    /// explicitly.
    ///
    /// The first gate wins. A controller that already owns one keeps
    /// it, so a session that goes cold again queues on the same lock
    /// as a request already waiting for it.
    pub(crate) fn attach_spawn_gate(&self, gate: SpawnGate) {
        let mut slot = self.spawn_gate.lock();
        if slot.is_none() {
            *slot = Some(gate);
        }
    }

    /// Install the callback the loop runs once, when it exits.
    ///
    /// This is the cache owner's exit notification: the controller
    /// never names the map it is cached in, and the callback is
    /// expected to hold only weak references, so no path through it
    /// can keep the state (or this controller) alive — no reference
    /// cycle exists for it to matter. First wins, like
    /// [`Self::attach_spawn_gate`].
    ///
    /// A no-op for a remote handle: it owns no loop of its own, so
    /// its cache entry is re-checked against the session lease
    /// instead (see `open_session_under_gate`).
    pub(crate) fn on_exit(&self, hook: Box<dyn FnOnce() + Send + Sync>) {
        if let Some(inner) = self.inner.as_ref() {
            inner.set_on_exit(hook);
        }
    }

    /// Run `work` while no new run may start.
    ///
    /// This is the promise a manual compaction pass needs: the pass
    /// rewrites the surface a run start reads, so a run must not
    /// begin underneath it. `maybe_start_run` takes the same gate
    /// across its transition to `Running`, so a run start that loses
    /// the race waits for the pass to finish instead. Steers and
    /// cancels do **not** queue behind a pass: they touch neither the
    /// surface nor the span the pass replaces, and the pass itself is
    /// bounded by `COMPACTION_TIMEOUT` (the follow route's compaction
    /// deadline), so there is no wedge left to protect them from.
    ///
    /// The gate's shape is the controller's business — the caller gets
    /// the promise, not a lock to hold, and the gate covers exactly
    /// the work it is given.
    ///
    /// A remote handle owns no run and no gate: there is nothing to
    /// serialise against, so the work runs as it would anywhere else.
    pub async fn with_compaction_gate<Fut, R>(&self, work: Fut) -> R
    where
        Fut: Future<Output = R>,
    {
        match self.inner.as_ref() {
            Some(inner) => inner.with_compaction_gate(work).await,
            None => work.await,
        }
    }

    /// Feed one event into the live projection, for route tests that
    /// cannot drive a real run.
    #[cfg(test)]
    pub(crate) fn tap_live_for_test(&self, event: &uva::harness::AgentEvent) {
        if let Some(inner) = self.inner.as_ref() {
            inner.tap_live(event);
        }
    }

    /// Start a fresh live projection, for route tests.
    #[cfg(test)]
    pub(crate) fn reset_live_for_test(&self) {
        if let Some(inner) = self.inner.as_ref() {
            inner.reset_live();
        }
    }

    /// The in-progress turn's transient projection.
    pub fn live_snapshot(&self) -> LiveTurn {
        match self.inner.as_ref() {
            Some(inner) => inner.live_snapshot(),
            None => LiveTurn::default(),
        }
    }

    /// Whether this handle projects the live turn itself (it owns the
    /// run) rather than receiving the owner's events from the bus.
    #[must_use]
    pub fn projects_live_locally(&self) -> bool {
        self.inner.is_some()
    }

    /// Returns `true` while the background loop is still running.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Subscribe to the session's live channel.
    ///
    /// The channel carries the run's events and the turn's end, each
    /// tagged with the turn it belongs to — see
    /// [`crate::event_stream::BroadcastEvent`].
    pub fn subscribe(
        &self,
    ) -> tokio::sync::broadcast::Receiver<crate::event_stream::BroadcastEvent>
    {
        self.broadcaster.subscribe()
    }

    /// Push one event onto this session's broadcast channel.
    ///
    /// The child-session live tail used to arrive through this
    /// method: the sub-agent router replayed a child's wrapped event
    /// onto the child session's stream. The router's routing half is
    /// gone with the delegation channel, so the method now serves the
    /// follow tests only — they inject a live event without running a
    /// session.
    #[cfg(test)]
    pub(crate) fn broadcast_event(&self, event: &uva::harness::AgentEvent) {
        let item = crate::event_stream::BroadcastEvent::Run {
            turn_id: None,
            event: Box::new(event.clone()),
        };
        if let Err(error) = self.broadcaster.send(item) {
            tracing::debug!(
                target: "uva.session",
                session_id = %self.session_id,
                error = %error,
                "child session broadcast had no subscribers"
            );
        }
    }

    /// Close the session: signal every child run, let the
    /// controller loop stop, then finalise the durable log.
    ///
    /// Idempotent — a second call returns immediately. The internal
    /// wait for the controller loop is bounded by
    /// `CLOSE_WAIT_TIMEOUT`, which strictly exceeds the loop's own
    /// 3-second run-join budget so the finalise + lease-release path
    /// always gets its turn; on expiry the loop is abandoned and the
    /// caller proceeds. The terminal `lifecycle_shutdown` event
    /// and the sink close are performed by the loop itself (see
    /// `ControllerInner::finalise_shutdown`), after all children
    /// have been signalled.
    /// # Errors
    ///
    /// Never returns `Err`: the submit is best-effort and the wait
    /// is bounded, so every path ends in `Ok(())` — callers may
    /// ignore the result (the `Result` shape is kept for future
    /// error paths only).
    pub async fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // Best-effort, deadline-free-by-construction submit: a full
        // channel or an already-exited loop logs and we still wait —
        // the loop's exit is what closes the channel, and it is the
        // bounded wait below that matters. A blocking `send` here
        // would await without any deadline.
        if let Err(e) = self.op_tx.try_send(SubmittedOp {
            op: SessionOp::Shutdown,
            provider: None,
            turn_id: None,
            ack: None,
        }) {
            tracing::debug!(
                target: "uva.session",
                session_id = %self.session_id,
                error = %e,
                "session close: shutdown op not queued (channel full or \
                 loop already exited); waiting for the loop regardless"
            );
        }
        match tokio::time::timeout(CLOSE_WAIT_TIMEOUT, self.op_tx.closed())
            .await
        {
            Ok(()) => {}
            Err(_) => {
                tracing::warn!(
                    target: "uva.session",
                    session_id = %self.session_id,
                    "session close: controller loop did not stop within \
                     {CLOSE_WAIT_TIMEOUT:?}"
                );
            }
        }
        Ok(())
    }

    /// Subscribe to the session's operation-snapshot bus (R11).
    ///
    /// Returns the receiver plus the latest snapshot published,
    /// so a late subscriber learns the current state without
    /// waiting for the next transition. HTTP `/status`, TUI,
    /// and replay consumers use this instead of polling
    /// [`SessionController::state`].
    pub fn subscribe_snapshots(
        &self,
    ) -> (
        uva::session::SnapshotReceiver,
        Option<uva::session::OperationSnapshot>,
    ) {
        self.snapshot_bus.subscribe()
    }
}

#[cfg(test)]
mod tests;

impl Drop for SessionController {
    fn drop(&mut self) {
        // The delivery task waits on this signal: a controller that is
        // gone must not leave a task behind.
        self.job_delivery_stop.notify_waiters();
    }
}

/// Deliver this session's background job completions.
///
/// A job that finishes is delivered the way a steer is: a durable row
/// plus the live run's inbox. The model learns that its command is
/// done without polling for it — and the notice is in the log, so the
/// next turn's fold sees it too.
fn spawn_job_delivery(
    controller: Arc<SessionController>,
    jobs: Arc<uva::tool_job::JobRegistry>,
    stop: Arc<tokio::sync::Notify>,
) {
    let mut completions = jobs.subscribe();
    let session_id = controller.session_id.clone();
    // The handle is weak: the task must not keep the controller alive
    // (a forgotten session's controller must be collectable).
    let weak = Arc::downgrade(&controller);
    tokio::spawn(async move {
        use futures::StreamExt;
        loop {
            let completion = tokio::select! {
                biased;
                _ = stop.notified() => return,
                completion = completions.next() => match completion {
                    Some(completion) => completion,
                    // The registry is gone: no more jobs will finish.
                    None => return,
                },
            };
            if completion.session_id != session_id {
                // Another session's job: its own controller owns it.
                continue;
            }
            let Some(controller) = weak.upgrade() else {
                // The controller is gone: nobody to deliver to.
                return;
            };
            let status = match (completion.exit_code, completion.signal) {
                (Some(code), _) => format!("exit code {code}"),
                (None, Some(signal)) => format!("killed by signal {signal}"),
                (None, None) => "no exit status".to_string(),
            };
            let message = format!(
                "[job] the background command `{}` finished ({status}). \
                 Read its output with the job tool.",
                completion.command
            );
            // A session that has closed takes no more messages; the
            // completion stays in the job's own output.
            if let Err(error) = controller.steer(message, None).await {
                tracing::debug!(
                    session_id = %session_id,
                    %error,
                    "a job completion could not be delivered"
                );
            }
        }
    });
}
