//! The controller loop: receiving operations, dispatching each to
//! its handler, and the idle-timeout lifecycle around them.

use std::{
    future::pending,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};
use uva::{core::CancelToken, provider::traits::ModelProvider};

use super::{
    inner::{ControllerInner, ParkedPrompt},
    ops::{SessionOp, SessionState},
    pin::{OpAck, SubmittedOp},
    run_log::{attachment_refs_of_parts, prompt_text_of_parts, truncate},
};

/// What the loop owes when it goes away (review fixes): the normal
/// exit path settles both duties itself — cancel-and-join the
/// in-flight run, then store `alive = false` — and this guard is
/// what still does them when the loop unwinds instead of
/// returning.
///
/// - **Stop the run.** The run is a detached task whose join handle
///   is a local of this loop, so an unwind detaches it. Without the
///   cancel, an orphan would keep appending to the session log
///   while the replacement controller the next request rebuilds
///   recovers the same parked turn — two runs on one log. The token
///   cancel is synchronous (it is the same signal a `Cancel` op
///   sends); the orphan's own cancellation handling does the rest.
/// - **Flip the flag.** A corpse left `alive == true` would be
///   handed back by the cache hit and never swept by the reaper.
///
/// The guard runs on **every** drop — return, panic-unwind, or task
/// abort (a *process* abort runs no destructors, and nothing
/// survives it anyway) — because both duties mean "the loop is
/// gone". The exit notification stays on the normal path alone: a
/// panicked loop removes no cache entry itself; the reaper and the
/// next request's rebuild cover it.
struct LoopExitGuard(Arc<ControllerInner>);

impl Drop for LoopExitGuard {
    fn drop(&mut self) {
        // No `expect` in a `Drop`: a panic here would abort the
        // process during unwind, and a poisoned lock only skips a
        // cancel the orphan's own limits already bound.
        if let Some(token) =
            self.0.run_cancel.lock().ok().and_then(|slot| slot.clone())
        {
            token.cancel();
        }
        self.0.alive.store(false, Ordering::SeqCst);
    }
}

pub(super) async fn run_controller_loop(
    inner: Arc<ControllerInner>,
    mut op_rx: mpsc::Receiver<SubmittedOp>,
) {
    // First local, so it drops last: whether the loop returns,
    // panics, or is aborted, its run is cancelled and `alive` ends
    // up `false`.
    let _exit = LoopExitGuard(Arc::clone(&inner));
    let mut last_activity = tokio::time::Instant::now();
    let mut run_handle: Option<tokio::task::JoinHandle<()>> = None;
    // R29-Phase-I: the reason recorded on the terminal
    // `LifecycleShutdown` event. Each exit path overrides it;
    // the default (Completed) covers the channel-closed path.
    let mut exit_reason = uva::session::SessionEndReason::Completed;
    // Cluster ownership keepalive: renew at a third of the TTL so a
    // single missed tick does not lose the lease. An empty lease (a
    // controller built outside `AppState`) renews nothing.
    //
    // The renewal deadline is an *instant*, not an `Interval`: the
    // loop's own work (an op handler, a run completion) can hold it
    // for a whole batch of fsyncs, and a deadline the loop can
    // *read* is what lets it notice that the renewal is due before it
    // starts the next batch. An `Interval` only tells the loop that a
    // tick is pending once the select is polled — too late when the
    // select is not reached.
    let lease_ttl = inner.cluster.lease_ttl;
    let mut renew_at = tokio::time::Instant::now() + lease_ttl / 3;
    let lease_held = !inner.lease.is_empty();

    loop {
        // The lease renewal comes FIRST, and the check below runs
        // before the select: a slow disk must not be able to push the
        // renewal out by a whole batch of fsyncs — that is a real
        // distance from `lease_ttl / 3`, and a lease that expires
        // mid-turn hands the session to another node.
        if lease_held
            && !renew_lease_if_due(&inner, &mut renew_at, lease_ttl).await
        {
            break;
        }
        // The lease deadline is checked before the select, so the
        // renewal is at most one round late however long the select
        // takes.
        if lease_held
            && !renew_lease_if_due(&inner, &mut renew_at, lease_ttl).await
        {
            break;
        }

        let idle_deadline =
            idle_deadline(&inner, &last_activity, run_handle.is_some());

        tokio::select! {
            biased;

            Some(submitted) = op_rx.recv() => {
                last_activity = tokio::time::Instant::now();
                if handle_received_op(&inner, submitted, &mut run_handle).await {
                    exit_reason =
                        uva::session::SessionEndReason::Interrupted;
                    break;
                }
            }

            result = wait_for_run(&mut run_handle), if run_handle.is_some() => {
                on_run_completed(&inner, result, &mut run_handle).await;
                last_activity = tokio::time::Instant::now();
            }

            _ = tokio::time::sleep_until(renew_at), if lease_held => {
                if !renew_lease_if_due(&inner, &mut renew_at, lease_ttl).await {
                    break;
                }
            }

            _ = wait_idle_timeout(idle_deadline), if idle_deadline.is_some() => {
                handle_idle_timeout(&inner, &mut run_handle).await;
                break;
            }

            else => {
                join_run(run_handle.take()).await;
                break;
            }
        }
    }

    // R29-Phase-I: every exit path lands here, so the session
    // log always ends with a `lifecycle_shutdown` marker and the
    // sink is closed exactly once.
    inner.finalise_shutdown(exit_reason).await;
    if lease_held {
        // Best effort: an expired/reassigned lease failing to delete
        // is harmless (the TTL covers it).
        let _ = inner
            .cluster
            .bus
            .release(&inner.lease, &inner.cluster.node_id)
            .await;
    }
    inner.alive.store(false, Ordering::SeqCst);
    // After `alive` went false, so the notification and a cache
    // hit's `is_alive` guard read the same fact (the notification
    // drops the controller's own cache entry; the reaper on
    // `AppState` is the backstop).
    inner.run_on_exit();
}

/// Renew the session lease when its deadline has passed.
///
/// `false` means the lease is gone — the caller must stop the
/// controller, because another node may already own the session. The
/// deadline lives with the caller (the loop), so the loop can read it
/// *before* it starts a batch of slow work: a renewal the loop only
/// notices when it reaches its `select!` is a renewal a flooding
/// plugin can push out by a whole batch of fsyncs.
///
/// A missed deadline is not a lost lease: the renewal extends the TTL
/// from *now*, exactly as the `Interval`'s delay behaviour did.
async fn renew_lease_if_due(
    inner: &ControllerInner,
    renew_at: &mut tokio::time::Instant,
    lease_ttl: Duration,
) -> bool {
    if tokio::time::Instant::now() < *renew_at {
        return true;
    }
    if let Err(e) = inner
        .cluster
        .bus
        .renew(&inner.lease, &inner.cluster.node_id, lease_ttl)
        .await
    {
        tracing::warn!(
            session_id = %inner.session_id,
            error = %e,
            "cluster: lost the session lease; stopping the controller"
        );
        return false;
    }
    *renew_at = tokio::time::Instant::now() + lease_ttl / 3;
    true
}

/// One received operation, dispatched to its handler. The
/// `Shutdown` variant returns `true` — the loop breaks with
/// `Interrupted`; every other op returns `false` and the
/// loop continues.
///
/// A submitted operation may carry a per-turn provider selection.
/// It is parked on the inner state by the variants that lead to a
/// run, and consumed by the run they start; the variants that only
/// touch an existing run (`Cancel`, `Feedback`, `Shutdown`) leave it
/// alone, so a selection can never attach itself to an unrelated
/// later turn.
///
/// An operation may also carry an **ack**: the submitter waits for
/// it when the operation's durable effect has to be on disk before
/// the caller answers. The ack is answered by the handler that owns
/// the row — right after the park's flush, before the run starts —
/// so a submitter that was promised "the turn is on disk" is never
/// told so before it is.
async fn handle_received_op(
    inner: &Arc<ControllerInner>,
    submitted: SubmittedOp,
    run_handle: &mut Option<tokio::task::JoinHandle<()>>,
) -> bool {
    let (op, provider, ack) = submitted.into_parts();
    let mut ack = ack;
    let stop = match op {
        SessionOp::Prompt { content, priority } => {
            park_provider(inner, provider);
            let failure = handle_prompt_op(inner, content, priority).await;
            ack_durability(&mut ack, failure);
            start_run_if_idle(inner, run_handle).await;
            false
        }
        SessionOp::PromptMulti {
            parts,
            agent_name,
            priority,
        } => {
            park_provider(inner, provider);
            let failure =
                handle_prompt_multi_op(inner, parts, agent_name, priority)
                    .await;
            ack_durability(&mut ack, failure);
            start_run_if_idle(inner, run_handle).await;
            false
        }
        SessionOp::Rerun {
            parts,
            agent_name,
            priority,
        } => {
            park_provider(inner, provider);
            let failure =
                handle_rerun_op(inner, parts, agent_name, priority).await;
            ack_durability(&mut ack, failure);
            start_run_if_idle(inner, run_handle).await;
            false
        }
        SessionOp::Feedback {
            message_id,
            thumbs_up,
        } => {
            let failure =
                handle_feedback_op(inner, message_id, thumbs_up).await;
            ack_durability(&mut ack, failure);
            false
        }
        SessionOp::Steer { content, priority } => {
            park_provider(inner, provider);
            let failure = handle_steer_op(inner, content, priority).await;
            ack_durability(&mut ack, failure);
            start_run_if_idle(inner, run_handle).await;
            false
        }
        SessionOp::Cancel { reason } => {
            let failure = handle_cancel_op(inner, reason).await;
            ack_durability(&mut ack, failure);
            false
        }
        SessionOp::Resume {
            token,
            decisions,
            allow_drift,
        } => {
            // A resume is answered with the session's default
            // provider: the suspended run already sampled under it,
            // and swapping it here would be drift, not a selection.
            handle_resume_op(inner, token, decisions, allow_drift).await;
            start_run_if_idle(inner, run_handle).await;
            false
        }
        SessionOp::StartPending => {
            // Nothing to append and nothing to park: a parked turn
            // is already queued (a recovered park, a turn parked
            // while the loop was coming up). This op only gives the
            // loop its start.
            start_run_if_idle(inner, run_handle).await;
            false
        }
        SessionOp::Shutdown => {
            handle_shutdown_op(inner, run_handle).await;
            true
        }
    };
    // The operation is handled: anything that writes a row has
    // written it (or failed, and said so above). The ack is answered
    // once — a handler that already answered it took it.
    ack_durability(&mut ack, None);
    stop
}

/// Report an operation's durable effect back to its submitter.
///
/// `failure` names what stopped the row from reaching the disk: a
/// submitter that promised durability turns it into an error answer
/// rather than a promise the log cannot keep. A submitter that is
/// gone (its request was cancelled) simply misses the answer.
fn ack_durability(
    ack: &mut Option<oneshot::Sender<OpAck>>,
    failure: Option<String>,
) {
    let Some(tx) = ack.take() else {
        return;
    };
    let outcome = match failure {
        Some(message) => OpAck::Failed(message),
        None => OpAck::Durable,
    };
    let _ = tx.send(outcome);
}

/// Park the turn's provider selection for the next run to consume.
///
/// The selection belongs to the turn, not to the session, so it
/// lives on the inner state only until
/// [`ControllerInner::maybe_start_run`] takes it. The slot is one
/// deep: when several turns queue behind a running one, the last
/// selection parks last and wins — a selection can never silently
/// outlive the turn that made it.
fn park_provider(
    inner: &ControllerInner,
    provider: Option<super::PinnedProvider>,
) {
    if let Some(provider) = provider {
        tracing::debug!(
            target: "uva.session",
            session_id = %inner.session_id,
            provider = provider.id().unwrap_or("<unresolved>"),
            model = provider.model_name(),
            "op_rx: turn carries an explicit provider selection"
        );
        *inner.pending_provider.lock() = Some(provider);
    }
}

// --- `run_controller_loop` op handlers -----------------------------------
//
// One handler per `SessionOp` variant, plus the shared text-turn
// park (`Prompt` and `Steer` differ only in their log line) and
// the uniform post-op starter. The loop itself
// keeps only the `select!` shape and the run-handle /
// exit-reason bookkeeping.

/// `Prompt`: log, then park the turn durably and queue it for the
/// next run.
///
/// The park row is log-only: the surface fold folds message rows —
/// `user_message` / `assistant_message` / `tool_result` and the
/// legacy `Message` / `UserInput` / `Model` envelopes — and a
/// `parked_prompt` row is not one, so it never enters the model
/// history. The run that starts from this park reads its prompt
/// from the park itself and writes the turn's `UserInput` row —
/// which *does* fold, which is how the next run remembers this
/// turn — only after it has read the sink, so this turn's prompt
/// reaches the *next* run's history, never its own twice.
async fn handle_prompt_op(
    inner: &ControllerInner,
    content: String,
    priority: u8,
) -> Option<String> {
    tracing::info!(
        target: "uva.session",
        session_id = %inner.session_id,
        op = "Prompt",
        priority,
        preview = truncate(&content, 40),
        "op_rx: received Prompt"
    );
    park_text_turn(inner, content).await
}

/// Park one plain-text turn — the single-`Text`-part shape of
/// `Prompt` and `Steer` — durably, and queue it for the next run.
///
/// A thin wrapper over [`park_durably`]: the `parked_prompt` row
/// carries the text and no attachments, and is appended and flushed
/// before this returns, so the turn survives a crash. The in-memory
/// park rides with the id the row was written under, so the run
/// that starts from it spends exactly that row.
///
/// Returns the failure that stopped the row from reaching the disk,
/// if there was one: the turn still runs on this node (the in-memory
/// park stands), but a submitter that was promised durability has to
/// hear that the promise was not kept.
async fn park_text_turn(
    inner: &ControllerInner,
    content: String,
) -> Option<String> {
    let parts = vec![uva::provider::ContentPart::Text(
        uva::provider::TextContent {
            text: content,
            cache_control: None,
        },
    )];
    let (park_id, failure) = park_durably(inner, &parts, &None, false).await;
    inner.pending_turns.lock().push_back(ParkedPrompt {
        park_id,
        parts,
        agent_name: None,
        rerun: false,
    });
    failure
}

/// `PromptMulti`: log, then park the multimodal payload on the
/// controller. `maybe_start_run` takes one park per run — the
/// multimodal prompt IS this turn, not an addition to a queue.
///
/// The park is durable: a `parked_prompt` row is appended and flushed
/// before the op returns, so the turn survives a crash. The row carries
/// the prompt's text and *re-readable* attachment refs (the payload
/// bytes are stored in the attachment store first); a node that takes
/// the session over re-parks from it. A failed append is logged and the
/// in-memory park stands — the turn still runs on this node, and the id
/// it was to be written under rides with it, so the run spends the row
/// whether or not the append reported success. The failure is still
/// reported to the submitter: a caller that was told "the turn is on
/// disk" must not be told so when it is not.
async fn handle_prompt_multi_op(
    inner: &ControllerInner,
    parts: Vec<uva::provider::ContentPart>,
    agent_name: Option<String>,
    priority: u8,
) -> Option<String> {
    tracing::info!(
        target: "uva.session",
        session_id = %inner.session_id,
        op = "PromptMulti",
        priority,
        part_count = parts.len(),
        agent_name = agent_name.as_deref().unwrap_or("-"),
        "op_rx: received PromptMulti"
    );
    let (park_id, failure) =
        park_durably(inner, &parts, &agent_name, false).await;
    inner.pending_turns.lock().push_back(ParkedPrompt {
        park_id,
        parts,
        agent_name,
        rerun: false,
    });
    failure
}

/// Append (and flush) the durable `parked_prompt` row for a turn about
/// to be parked in memory, returning the id the row was written under
/// and the failure that stopped it from reaching the disk.
///
/// The bytes stay out of the log: [`attachment_refs_of_parts`] stores
/// each payload in the attachment store and returns refs, so the row
/// holds a handle a reader can resolve. A failure is logged and
/// reported, never fatal — the caller is mid-op and the turn still
/// has to run; what the failure buys the caller is the *truth*: a
/// submitter that was promised a durable turn is told when the row
/// did not land.
///
/// The id is minted here, **before** the append, and the caller keeps
/// it whatever the append reports: a row that landed despite the error
/// is then still spent by the run started from this park (the run
/// quotes the same id), so the failure can neither duplicate the turn
/// on restart nor lose it.
async fn park_durably(
    inner: &ControllerInner,
    parts: &[uva::provider::ContentPart],
    agent_name: &Option<String>,
    rerun: bool,
) -> (String, Option<String>) {
    // The turn's provider selection is a fact about the turn, so it
    // goes into the row: a takeover resolves it by name instead of
    // silently running the deployment default.
    let (provider, model, store) = {
        let deps = inner.deps.lock();
        let selection = inner.pending_provider.lock();
        let (provider, model) = match selection.as_ref() {
            Some(pin) => (
                pin.id()
                    .map(str::to_string)
                    .unwrap_or_else(|| deps.default_provider_name.clone()),
                pin.model_name(),
            ),
            None => (
                deps.default_provider_name.clone(),
                deps.provider.model_config().name,
            ),
        };
        (provider, model, Arc::clone(&deps.attachment_store))
    };
    let park_id = uuid::Uuid::new_v4().to_string();
    let event = uva::session::SessionEvent::ParkedPrompt {
        seq: 0,
        ts: String::new(),
        park_id: park_id.clone(),
        text: prompt_text_of_parts(parts),
        attachments: attachment_refs_of_parts(parts, store.as_ref()),
        agent_name: agent_name.clone(),
        provider: Some(provider),
        model: Some(model),
        rerun,
    };
    let row = match serde_json::to_value(&event) {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!(
                target: "uva.session",
                session_id = %inner.session_id,
                error = %e,
                "Failed to serialise the parked_prompt row"
            );
            return (
                park_id,
                Some(format!(
                    "the parked_prompt row could not be serialised: {e}"
                )),
            );
        }
    };
    // The flush lands the row before the op returns: the park is the
    // promise, and a queued-but-unflushed row does not survive a crash.
    if let Err(e) = inner.append_durable_row(row).await {
        tracing::warn!(
            target: "uva.session",
            session_id = %inner.session_id,
            error = %e,
            "Failed to persist the parked_prompt row"
        );
        return (
            park_id,
            Some(format!(
                "the parked_prompt row could not be made durable: {e}"
            )),
        );
    }
    (park_id, None)
}

/// `Rerun`: log, cancel any in-flight run, then park the
/// recovered parts with `rerun: true` (what the run task
/// reads to give the persisted prompt row replace
/// semantics — see `rerun_replace_op`). Cancelling first
/// guarantees the new turn starts against a clean state —
/// otherwise the rerun would queue behind the
/// still-running original turn.
async fn handle_rerun_op(
    inner: &ControllerInner,
    parts: Vec<uva::provider::ContentPart>,
    agent_name: Option<String>,
    priority: u8,
) -> Option<String> {
    tracing::info!(
        target: "uva.session",
        session_id = %inner.session_id,
        op = "Rerun",
        priority,
        part_count = parts.len(),
        agent_name = agent_name.as_deref().unwrap_or("-"),
        "op_rx: received Rerun"
    );
    cancel_inflight_run(inner);
    let (park_id, failure) =
        park_durably(inner, &parts, &agent_name, true).await;
    inner.pending_turns.lock().push_back(ParkedPrompt {
        park_id,
        parts,
        agent_name,
        rerun: true,
    });
    failure
}

/// `Feedback`: log, then append the feedback row as a JSONL
/// event so a future analytics endpoint can aggregate.
/// Failures are logged but do not abort the controller —
/// feedback is observational and never critical to the next
/// run.
async fn handle_feedback_op(
    inner: &ControllerInner,
    message_id: String,
    thumbs_up: bool,
) -> Option<String> {
    tracing::info!(
        target: "uva.session",
        session_id = %inner.session_id,
        op = "Feedback",
        message_id,
        thumbs_up,
        "op_rx: received Feedback; persisting to sink"
    );
    // The writer stamps the row's `seq` and `ts`.
    let payload = serde_json::json!({
        "kind": "feedback",
        "message_id": message_id,
        "thumbs_up": thumbs_up,
    });
    // The feedback row goes through the session's one writer, so
    // it carries the writer's ordinal — and the flush lands it
    // before the op returns: feedback has no broadcast of its own
    // to lean on for the flush.
    if let Err(e) = inner.append_durable_row(payload).await {
        tracing::warn!(
            target: "uva.session",
            session_id = %inner.session_id,
            error = %e,
            "Failed to persist feedback event"
        );
        return Some(format!(
            "the feedback row could not be made durable: {e}"
        ));
    }
    None
}

/// `Steer`: log, then park the turn like a `Prompt`. A steer's
/// queued semantics are "the next turn's prompt" — the queue is
/// consumed only when a run starts — so the behaviour is the one
/// the queue always had; the park only makes the turn recoverable.
async fn handle_steer_op(
    inner: &ControllerInner,
    content: String,
    priority: u8,
) -> Option<String> {
    tracing::info!(
        target: "uva.session",
        session_id = %inner.session_id,
        op = "Steer",
        priority,
        preview = truncate(&content, 40),
        "op_rx: received Steer"
    );
    park_text_turn(inner, content).await
}

/// Fire the in-flight run's cancel token (without taking
/// it — the run task owns the slot until it exits). Shared
/// by `Rerun` (clean state before the new turn) and
/// `Cancel`.
fn cancel_inflight_run(inner: &ControllerInner) {
    if let Some(token) = inner
        .run_cancel
        .lock()
        .expect("run_cancel mutex poisoned")
        .as_ref()
    {
        token.cancel();
    }
}

/// Take every parked turn off the queue, synchronously, so the take
/// can happen inside the cancel's short gate section.
fn take_pending_parks(inner: &ControllerInner) -> Vec<ParkedPrompt> {
    // Collected in one lock hold: the lock is never held across an
    // await.
    inner.pending_turns.lock().drain(..).collect()
}

/// Spend every park the cancel dropped, so the controller does not
/// immediately restart the runs — and a node that takes the session
/// over does not replay turns the user just cancelled.
///
/// One `park_consumed` row per dropped park, through the session's
/// one writer, appended and flushed **before** the cancelled state
/// flips: the flip is what a taker (or a crash) sees first, so a
/// `Cancelled` session must never carry an unspent park. The rows
/// are written **outside** the steer gate — one fsync each, and the
/// gate must not serialise every steer and every run start behind
/// the disk. The ordering the takeover depends on is durability
/// before the flip, not "no steer may interleave": the parks were
/// taken under the gate, so a steer that slips into the gap parks a
/// *new* turn these rows cannot shadow. An append failure is logged
/// and dropped — the controller keeps serving, and the worst case is
/// a replayed turn, never a lost one.
async fn spend_parked_turns(
    inner: &ControllerInner,
    dropped: Vec<ParkedPrompt>,
) -> Option<String> {
    let mut failure = None;
    for parked in dropped {
        let event = uva::session::SessionEvent::ParkConsumed {
            seq: 0,
            ts: String::new(),
            park_id: parked.park_id,
        };
        let row = match serde_json::to_value(&event) {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(
                    target: "uva.session",
                    session_id = %inner.session_id,
                    error = %e,
                    "Failed to serialise the park_consumed row"
                );
                failure.get_or_insert_with(|| {
                    format!("a park_consumed row could not be serialised: {e}")
                });
                continue;
            }
        };
        if let Err(e) = inner.append_durable_row(row).await {
            tracing::warn!(
                target: "uva.session",
                session_id = %inner.session_id,
                error = %e,
                "Failed to spend a dropped park on cancel"
            );
            failure.get_or_insert_with(|| {
                format!("a park_consumed row could not be made durable: {e}")
            });
        }
    }
    failure
}

/// `Cancel`: fire the run's cancel token, spend every parked turn
/// (so the controller does not immediately restart the runs, and a
/// node that takes over does not replay them), flip the state, and
/// publish the Cancelled snapshot (R11).
///
/// The state transition runs under the same `steer_gate` the steer
/// takes before it reads the state, so a steer and a cancel cannot
/// interleave. A steer that wins the gate appends its row while the
/// run is still `Running`; the cancel then stops the run and the row
/// stays in the log — the repair path closes the tool call it
/// interrupted, and the message becomes the next turn's first user
/// message. A cancel that wins flips the state first, so the steer
/// behind it reads the cancelled state and takes the queued path.
/// One message, one row, either way.
///
/// The gate covers the *transition*, not the disk: the parks are
/// taken under it and released, the consumption rows are written
/// (one fsync each) with the gate free, and the gate is taken again
/// to flip the state. The order a takeover depends on — the
/// `park_consumed` rows durable **before** the `Cancelled` state —
/// survives the split, because the flip is the second half of it. A
/// steer that slips into the gap parks a *new* turn, which the
/// consumption rows cannot shadow. Only the approval settle runs
/// after the gate; it appends no park state.
async fn handle_cancel_op(
    inner: &ControllerInner,
    reason: Option<String>,
) -> Option<String> {
    tracing::info!(
        target: "uva.session",
        session_id = %inner.session_id,
        op = "Cancel",
        reason = reason.as_deref().unwrap_or("-"),
        "op_rx: received Cancel; firing cancellation token"
    );
    let dropped = {
        let _cancelled = inner.steer_gate.lock().await;
        cancel_inflight_run(inner);
        let dropped = take_pending_parks(inner);
        // A cancelled session must not later resume with a token
        // parked before the cancel.
        *inner.pending_resume.lock() = None;
        dropped
    };
    // The consumption rows are durable BEFORE the state flips, and
    // they are written with the gate free: one fsync per dropped
    // park must not serialise every steer and every run start behind
    // the disk.
    let failure = spend_parked_turns(inner, dropped).await;
    {
        let _cancelled = inner.steer_gate.lock().await;
        publish_cancelled_snapshot(inner, &reason);
    }
    // Cancelling answers every open interrupt: a settled ask is what
    // keeps the next `maybe_start_run` from refusing forever.
    inner.settle_open_approvals("cancelled").await;
    if let Some(reason) = reason {
        tracing::info!(
            target: "uva.session",
            session_id = %inner.session_id,
            reason,
            "Session run cancelled"
        );
    }
    failure
}

/// `Resume`: park the validated token + decisions for the next
/// `maybe_start_run` and leave the paused state.
///
/// The parked resume is what makes a start legal while the log still
/// holds the asks being answered: `maybe_start_run` consumes it and
/// the run's input carries it to the harness, which validates it
/// again before touching a tool.
///
/// A resubmission is a no-op. The token is derived from the log — the
/// last durable seq plus the call ids still undecided — so a resume the
/// log has already applied cannot match it any more: every call the
/// token names has an `approval_decided` row. That is what makes the
/// client's retry safe after a takeover, where the server deliberately
/// remembers no decision (§5.11): the second submission has nothing to
/// execute.
async fn handle_resume_op(
    inner: &ControllerInner,
    token: uva::harness::ResumeToken,
    decisions: Vec<uva::harness::ResumeDecision>,
    allow_drift: bool,
) {
    tracing::info!(
        target: "uva.session",
        session_id = %inner.session_id,
        op = "Resume",
        interrupts = decisions.len(),
        allow_drift,
        "op_rx: received Resume; parking it for the next run"
    );
    let live = *inner.state.lock().expect("state mutex poisoned");
    if live == SessionState::Running {
        // The route refuses this; a wire-forwarded op could still
        // reach an owner mid-run. Parking here would attach the token
        // to a later, unrelated run.
        tracing::warn!(
            target: "uva.session",
            session_id = %inner.session_id,
            "Resume ignored: a run is live",
        );
        return;
    }
    if is_spent_resume(inner, &token).await {
        tracing::debug!(
            target: "uva.session",
            session_id = %inner.session_id,
            pending = ?token.pending,
            "Resume ignored: the log already decided every call the token names",
        );
        return;
    }
    if inner
        .pending_resume
        .lock()
        .replace(super::inner::PendingResume {
            token,
            decisions,
            allow_drift,
        })
        .is_some()
    {
        tracing::warn!(
            target: "uva.session",
            session_id = %inner.session_id,
            "Resume replaced an unconsumed token; last writer wins",
        );
    }
    let mut state = inner.state.lock().expect("state mutex poisoned");
    if *state == SessionState::AwaitingApproval {
        *state = SessionState::Idle;
    }
}

/// `true` when the log has already decided every call `token` names.
///
/// The log is the authority: the open set is folded from it, so a
/// token whose calls are all decided describes a resume that already
/// happened. A token that names no call (a `Paused` suspension) cannot
/// be judged this way, so it is left to the harness's own validation.
///
/// Only a **successful** empty read means "spent". An unreadable log is
/// not a decision — treating it as one would drop a legitimate resume
/// the route has already answered `202` for, and the client would wait
/// for a run that never starts. The error direction is the safe one: an
/// unreadable log parks the resume, and the harness validates the token
/// itself.
async fn is_spent_resume(
    inner: &ControllerInner,
    token: &uva::harness::ResumeToken,
) -> bool {
    if token.pending.is_empty() {
        return false;
    }
    let open = match inner.try_open_interrupts().await {
        Ok(open) => open,
        Err(error) => {
            tracing::warn!(
                target: "uva.session",
                session_id = %inner.session_id,
                %error,
                "Resume: the log could not be read; parking the resume \
                 instead of treating the token as spent"
            );
            return false;
        }
    };
    !open.iter().any(|request| {
        token
            .pending
            .iter()
            .any(|call| *call == request.call_id || *call == request.id)
    })
}

/// Flip the controller state to `Cancelled` and publish the
/// R11 `Cancelled` operation snapshot on the snapshot bus.
/// Shared shape with the run task's terminal snapshots.
fn publish_cancelled_snapshot(
    inner: &ControllerInner,
    reason: &Option<String>,
) {
    let mut state = inner.state.lock().expect("state mutex poisoned");
    *state = SessionState::Cancelled;
    let _seq =
        inner
            .snapshot_bus
            .publish(uva::session::OperationSnapshot::new(
                inner.session_id.clone(),
                "default".to_string(),
                uva::session::OperationState::Cancelled {
                    reason: reason
                        .clone()
                        .unwrap_or_else(|| "cancelled".into()),
                },
                0,
                inner.deps.lock().default_max_iterations,
                uva::provider::TokenUsage::default(),
            ));
}

/// `Shutdown`: take + fire the cancel token, then bound the
/// in-flight run's join with a 3 s deadline (R29-Phase-I) so
/// a hung run task cannot keep the shutdown — and the
/// process — alive. The caller breaks out of the loop after
/// this returns.
async fn handle_shutdown_op(
    inner: &ControllerInner,
    run_handle: &mut Option<tokio::task::JoinHandle<()>>,
) {
    tracing::info!(
        target: "uva.session",
        session_id = %inner.session_id,
        op = "Shutdown",
        "op_rx: received Shutdown; breaking controller loop"
    );
    if let Some(token) = inner
        .run_cancel
        .lock()
        .expect("run_cancel mutex poisoned")
        .take()
    {
        token.cancel();
    }
    if let Some(h) = run_handle.take()
        && tokio::time::timeout(Duration::from_secs(3), h)
            .await
            .is_err()
    {
        tracing::warn!(
            target: "uva.session",
            session_id = %inner.session_id,
            "shutdown: in-flight run did not stop within 3s; abandoning it"
        );
    }
}

/// The uniform post-op starter: when no run is in flight, hand
/// `maybe_start_run` the chance to consume a parked turn.
async fn start_run_if_idle(
    inner: &Arc<ControllerInner>,
    run_handle: &mut Option<tokio::task::JoinHandle<()>>,
) {
    if run_handle.is_none()
        && let Some(h) = inner.maybe_start_run().await
    {
        *run_handle = Some(h);
    }
}

/// The `select!` run-completion arm body: clear the handle,
/// surface a panicked run, and — if more turns arrived
/// while the run was active — start the next run
/// immediately. The gate must be `maybe_start_run` itself,
/// which takes one parked turn and returns `None` when the
/// queue holds nothing.
async fn on_run_completed(
    inner: &Arc<ControllerInner>,
    result: Result<(), tokio::task::JoinError>,
    run_handle: &mut Option<tokio::task::JoinHandle<()>>,
) {
    *run_handle = None;
    if let Err(e) = result {
        tracing::error!(
            session_id = %inner.session_id,
            error = %e,
            "Session run task panicked"
        );
    }
    if let Some(h) = inner.maybe_start_run().await {
        *run_handle = Some(h);
    }
}

// --- `run_controller_loop` select! guard futures + exit arms -------------

/// The `select!` guard future for the run-completion arm:
/// await the in-flight run's join handle, or park forever
/// when none exists (the arm's precondition keeps that
/// branch unpollled anyway; the never-resolving future is
/// just the type-level filler `select!` needs).
async fn wait_for_run(
    run_handle: &mut Option<tokio::task::JoinHandle<()>>,
) -> Result<(), tokio::task::JoinError> {
    match run_handle {
        Some(h) => h.await,
        None => pending::<Result<(), tokio::task::JoinError>>().await,
    }
}

/// The `select!` guard future for the idle-timeout arm.
async fn wait_idle_timeout(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => pending::<()>().await,
    }
}

/// Await a taken run handle to completion, ignoring its
/// outcome — used on the exit paths where the controller is
/// shutting down either way.
async fn join_run(handle: Option<tokio::task::JoinHandle<()>>) {
    if let Some(h) = handle {
        let _ = h.await;
    }
}

/// The idle-timeout arm body: log, cancel any in-flight run,
/// and await it. The caller breaks out of the loop after
/// this returns.
async fn handle_idle_timeout(
    inner: &Arc<ControllerInner>,
    run_handle: &mut Option<tokio::task::JoinHandle<()>>,
) {
    tracing::info!(
        session_id = %inner.session_id,
        "Session controller idle timeout reached; shutting down"
    );
    if let Some(token) = inner
        .run_cancel
        .lock()
        .expect("run_cancel mutex poisoned")
        .take()
    {
        token.cancel();
    }
    join_run(run_handle.take()).await;
}

/// The deadline the idle-timeout arm sleeps to, or `None` while
/// anything keeps the controller alive: a live run, a subscriber,
/// or a parked turn waiting for its run.
fn idle_deadline(
    inner: &ControllerInner,
    last_activity: &tokio::time::Instant,
    run_active: bool,
) -> Option<tokio::time::Instant> {
    if run_active
        || inner.broadcaster.subscriber_count() > 0
        || !inner.pending_turns.lock().is_empty()
    {
        None
    } else {
        Some(*last_activity + inner.idle_timeout)
    }
}
