//! `GET /api/v1/sessions/{id}/follow` — the reconnectable session
//! stream.
//!
//! Wire shape. Every frame is an SSE `event: <name>` with a JSON
//! `data:` line:
//!
//! ```text
//! event: baseline
//! data: { header, rows, cursor, live, context,
//!         pending_interrupts, request_snapshot_hash }
//!
//! event: row
//! data: { seq: <u64>, turn_id: <string>, row: <durable row> }
//!
//! event: live
//! data: { turn_id, live_seq, running, text, reasoning, tool_calls }
//!
//! event: interrupts
//! data: { pending_interrupts: [ApprovalRequest, ...] }
//! ```
//!
//! The first frame is a **complete baseline**: the header, the durable
//! window (always the last [`FOLLOW_WINDOW`] rows), the cursor those
//! rows end at, the live projection of the in-progress turn, the
//! context meter, the open interrupts and the request-envelope
//! fingerprint. A client replaces its whole window from it and then
//! applies increments from `cursor + 1`; it has no reason to merge the
//! baseline into state it kept from an earlier connection.
//!
//! `?from=` is a staleness check only, not a window selector: the
//! baseline never shrinks or extends to suit it. A cursor past the log
//! tail is refused with `410`, because it can only come from a log the
//! client is no longer talking to. A client that needs more history than
//! [`FOLLOW_WINDOW`] rows reads `GET /api/v1/sessions/{id}`.
//!
//! `resume` and `compact` are **owner-node only**: their token check
//! reads the session's log, which only the owner holds. A handle for a
//! session another node owns answers `409 remote_session`.
//!
//! Later frames are either durable (`row`, carrying `seq`) or
//! transient (`live`, carrying `live_seq`). A `row`'s `turn_id` names
//! the turn whose projection it supersedes — the client drops a live
//! projection when a row for the same `turn_id` arrives. A `live`
//! frame whose `live_seq` is not greater than the baseline's is
//! discarded (it predates the window the client just accepted).
//!
//! A crash mid-turn loses `live` and leaves no half-written assistant
//! row — the durable log simply never got one. That is the "one row
//! per turn" trade-off, not data loss a client must repair.

use std::{
    collections::BTreeSet,
    convert::Infallible,
    sync::Arc,
    time::Duration,
};

use axum::{
    extract::State,
    http::StatusCode,
    response::{
        IntoResponse,
        Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use synthia::{
    harness::{ApprovalRequest, latest_request_snapshot_hash, open_requests},
    provider::traits::ModelProvider,
    session::{SessionEvent, TokenMeter},
};
use tokio::sync::broadcast;

use crate::{
    api::{AppError, AppJson, AppPath, AppQuery, validate_resource_name},
    routes::chat::resolve_user_id,
    session::controller::{
        LiveTurn,
        SessionController,
        SessionOp,
        SessionState,
    },
    state::AppState,
};

/// How many durable rows the baseline carries. The window is fixed:
/// `from` cannot widen it, and a client that needs older rows reads
/// the session transcript instead.
pub const FOLLOW_WINDOW: usize = 200;

/// How often the durable tail is polled for rows the live tap never
/// saw (structural rows carry no event).
const FOLLOW_POLL: Duration = Duration::from_millis(100);

/// The session-level facts a client needs before its first row.
#[derive(Debug, Serialize)]
pub struct SessionHeader {
    /// The session id.
    pub session_id: String,
    /// The session's owner.
    pub user_id: String,
    /// The agent the session runs as (delegated children name the
    /// peer that ran them).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    /// `idle`, `running`, `awaiting_approval` or `cancelled`.
    pub state: &'static str,
    /// First durable row's timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Last durable row's timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// How much of the model's window the next request would occupy.
#[derive(Debug, Serialize)]
pub struct ContextMeter {
    /// Tokens the next request is projected to carry.
    pub used: u64,
    /// The model's advertised window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
}

/// The complete first frame of a `follow` stream.
#[derive(Debug, Serialize)]
pub struct FollowSnapshot {
    /// Session-level facts.
    pub header: SessionHeader,
    /// The last [`FOLLOW_WINDOW`] durable rows.
    pub rows: Vec<Value>,
    /// The ordinal of the last durable row (`0` for an empty log).
    pub cursor: u64,
    /// The in-progress turn's transient projection.
    pub live: LiveTurn,
    /// Context occupancy for the next request.
    pub context: ContextMeter,
    /// Interrupts still waiting for a decision.
    pub pending_interrupts: Vec<ApprovalRequest>,
    /// Fingerprint of the log's newest request envelope (`""` when
    /// the log has none).
    pub request_snapshot_hash: String,
}

/// `?from=` — the cursor the caller last saw.
#[derive(Debug, Default, Deserialize, validator::Validate)]
pub struct FollowQuery {
    /// Only a staleness check: a value past the log tail is refused
    /// with `410`, and the baseline window is unaffected otherwise.
    #[serde(default)]
    pub from: Option<u64>,
}

/// GET /api/v1/sessions/{id}/follow
pub async fn follow(
    State(state): State<Arc<AppState>>,
    AppPath(session_id): AppPath<String>,
    AppQuery(query): AppQuery<FollowQuery>,
) -> Result<Response, AppError> {
    validate_resource_name(&session_id)?;
    let user_id = resolve_user_id(&state);
    // Subscribe before the baseline read: an event that lands between
    // the two is buffered, and the client's `live_seq`/`seq` filters
    // discard anything the baseline already covers.
    let controller = state
        .get_or_create_session_controller(&user_id, &session_id)
        .await?;
    let rx = controller.subscribe();
    let sink = state.session_manager.sink(&user_id, &session_id);
    let rows =
        read_tail(sink.as_ref(), FOLLOW_WINDOW)
            .await
            .map_err(|error| {
                AppError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    synthia::core::Error::session(format!(
                        "read session log: {error}"
                    )),
                )
                .with_code("session_log_read")
            })?;
    // The header carries two whole-log facts (the session's first
    // timestamp, a delegated session's agent) that a window cannot
    // answer, so the head is read on its own — bounded, like the tail.
    // A head that cannot be read is a log that cannot be read: failing
    // the request keeps `created_at` a whole-log fact. Substituting the
    // window's first timestamp would put a plausible, wrong value on
    // the wire instead.
    let head = sink.read_head().await.map_err(|error| {
        AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            synthia::core::Error::session(format!(
                "read session log head: {error}"
            )),
        )
        .with_code("session_log_read")
    })?;
    let baseline = build_baseline(
        &state,
        &controller,
        &user_id,
        &session_id,
        sink.as_ref(),
        rows,
        head,
    )
    .await;
    if let Some(from) = query.from
        && from > baseline.cursor
    {
        return Ok(truncated_response(from, baseline.cursor));
    }
    let sink = state.session_manager.sink(&user_id, &session_id);
    Ok(Sse::new(follow_stream(controller, sink, rx, baseline))
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// The durable tail a `follow` baseline is built from.
///
/// This is the whole point of [`SessionSink::read_tail`]: the baseline
/// used to call `read()` and slice the last [`FOLLOW_WINDOW`] rows in
/// memory, which made every subscription cost the session's entire
/// history — a cost that grows with the log for the one call that runs
/// on every reconnect.
pub(crate) async fn read_tail(
    sink: &dyn synthia::session::SessionSink,
    window: usize,
) -> Result<Vec<Value>, synthia::session::SessionError> {
    sink.read_tail(window).await
}

/// Build the complete baseline from the log's tail window.
///
/// `rows` is already the window ([`read_tail`]); `head` is the log's
/// first row. The window answers the row-shaped facts; the three
/// *whole-log* folds — the context meter, the model window it is priced
/// against, and the resume fingerprint — read the full log when the
/// window cannot prove it holds the last turn (see
/// [`window_holds_last_turn`]). They must: `TokenMeter` folds every
/// surface and usage row, and `context_provider` resolves the model
/// from the newest `request_header`, a row a long turn wrote before the
/// window starts. Folding the window there answers with the default
/// model's window and an empty fingerprint — and an empty fingerprint
/// is exactly what "this log has no request envelope" looks like on the
/// wire.
///
/// The window's fast path is kept for the common case (a short turn, or
/// a window that begins at the current turn), so this is a fallback,
/// not a second read on every poll.
///
/// The two folds are **not** interchangeable, and only one shape makes
/// them agree. `TokenMeter::measure` is
/// `anchor.baseline + (surface - anchor.surface)`; with a provider usage
/// anchor inside the window both folds hold the same anchor row and the
/// same rows after it, so the deltas cancel and the token totals match —
/// while `surface_tokens` itself still does not (the window's fold never
/// saw the earlier turns). With **no** usage anchor in the window — no
/// `usage` row, or one the last `request_header` reset — the window's
/// fold has no baseline and degenerates to pricing the window's own
/// surface, a different number from the whole-log surface `/usage`
/// prices. [`window_holds_last_turn`] therefore demands an anchor as
/// well as the turn, and a session without one takes the whole-log
/// path.
pub(crate) async fn build_baseline(
    state: &AppState,
    controller: &SessionController,
    user_id: &str,
    session_id: &str,
    sink: &dyn synthia::session::SessionSink,
    rows: Vec<Value>,
    head: Option<Value>,
) -> FollowSnapshot {
    let cursor = cursor_of(&rows);
    let full = if window_holds_last_turn(&rows) {
        None
    } else {
        match sink.read().await {
            Ok(full) => Some(full),
            Err(error) => {
                tracing::warn!(
                    session_id = %session_id,
                    %error,
                    "follow: the whole-log fallback failed; the baseline's \
                     context describes the window only"
                );
                None
            }
        }
    };
    let facts: &[Value] = full.as_deref().unwrap_or(&rows);
    // The interrupt fold needs the whole last suspended turn. A window
    // that does not prove it holds the segment is `None`; the full log
    // is then the answer — and it is the *log*, not the controller, so
    // a handset-owning node's follower gets it too (a remote handle has
    // no controller to ask).
    let pending_interrupts = match open_requests_in_window(facts) {
        Some(open) => open,
        None => open_requests(facts),
    };
    let request_snapshot_hash =
        latest_request_snapshot_hash(facts).unwrap_or_default();
    let context = context_meter(state, facts);
    let head = head.as_ref().or_else(|| rows.first());
    let header = SessionHeader {
        session_id: session_id.to_string(),
        user_id: user_id.to_string(),
        agent_name: agent_attribution(head),
        state: state_label(reported_state(
            controller.state(),
            &pending_interrupts,
        )),
        created_at: head.and_then(row_ts),
        updated_at: last_ts(&rows),
    };
    FollowSnapshot {
        header,
        rows,
        cursor,
        live: controller.live_snapshot(),
        context,
        pending_interrupts,
        request_snapshot_hash,
    }
}

/// Whether the window provably answers every fold `build_baseline` runs
/// on it.
///
/// Two clauses, both required:
///
/// 1. **The last turn is whole.** Rows are appended in order, so a
///    window containing the turn's own start row holds every row the
///    turn wrote after it — its `request_header`, its `request_snapshot`,
///    its usage rows. Two turn boundaries in the window prove the same
///    thing: the last turn sits between them. With neither, the window
///    may have clipped the turn's head, and a fact folded from it
///    describes an older turn.
/// 2. **A provider usage anchor survives the fold.** `measure` is
///    `anchor.baseline + (surface - anchor.surface)`. Without an anchor
///    the meter prices only the rows it folded, so a window's
///    `context.used` would be the *window's* surface tokens while
///    `GET /sessions/{id}/usage` prices the whole surface — two routes,
///    one session, two numbers. The newest `request_header` resets the
///    anchor, so the test is the meter's own end state: fold the window
///    and ask whether the newest anchor is a usage report.
fn window_holds_last_turn(rows: &[Value]) -> bool {
    let mut starts = 0;
    let mut ends = 0;
    for row in rows {
        match turn_boundary(row) {
            Some(true) => starts += 1,
            Some(false) => ends += 1,
            None => {}
        }
    }
    (starts > 0 || ends >= 2) && window_holds_usage_anchor(rows)
}

/// Whether folding `rows` leaves the meter anchored on a provider usage
/// report.
///
/// `context_pressure` answers `Some` only for a usage baseline (an
/// estimated one is still a single fold's guess), so this is the same
/// the meter asks itself when it projects pressure — asked of the window
/// instead of the whole log.
fn window_holds_usage_anchor(rows: &[Value]) -> bool {
    let events: Vec<SessionEvent> =
        rows.iter().filter_map(SessionEvent::from_value).collect();
    let mut meter = TokenMeter::new();
    if meter.observe(&events).is_err() {
        return false;
    }
    meter.context_pressure(None).is_some()
}

/// The state the wire reports, derived the way the controller derives
/// it — but from the window where the window can answer.
///
/// A live run wins outright. Otherwise the log decides: an open
/// interrupt means `AwaitingApproval`, and a cached `AwaitingApproval`
/// with no open interrupt means the asks were answered, so the flag is
/// stale. This is the controller's own rule
/// (`SessionController::session_state`), with the approval set the
/// baseline already folded instead of a second read of the log.
fn reported_state(
    in_memory: SessionState,
    pending_interrupts: &[ApprovalRequest],
) -> SessionState {
    if in_memory == SessionState::Running {
        return SessionState::Running;
    }
    if !pending_interrupts.is_empty() {
        return SessionState::AwaitingApproval;
    }
    if in_memory == SessionState::AwaitingApproval {
        return SessionState::Idle;
    }
    in_memory
}

/// Open interrupts folded from a tail window, or `None` when the
/// window cannot prove it holds the whole last suspended turn.
///
/// The fold needs the segment that starts after the **previous** turn
/// boundary, so a clipped window that holds the suspended end but not
/// the boundary before it would fold rows from an older turn — and
/// resurrect asks that were answered before it. `None` sends the
/// caller to the full log instead of guessing.
fn open_requests_in_window(rows: &[Value]) -> Option<Vec<ApprovalRequest>> {
    let ends: Vec<bool> = rows
        .iter()
        .filter(|row| turn_boundary(row) == Some(false))
        .map(ends_suspended_turn)
        .collect();
    let last_suspended = *ends.last()?;
    if !last_suspended {
        // The last turn boundary in the window is an ordinary end, so
        // nothing is suspended — unless the last boundary is older than
        // the window, which is the `None` case above.
        return Some(Vec::new());
    }
    if ends.len() < 2 {
        return None;
    }
    Some(open_requests(rows))
}

/// `Some(true)` for a turn start, `Some(false)` for a turn end, `None`
/// when the row is not a turn boundary.
fn turn_boundary(row: &Value) -> Option<bool> {
    let SessionEvent::Turn { data, .. } = SessionEvent::from_value(row)? else {
        return None;
    };
    let kind = data.get("kind").or_else(|| data.get("phase"))?.as_str()?;
    match kind {
        "start" => Some(true),
        "end" => Some(false),
        _ => None,
    }
}

/// Whether a turn-end row closes a **suspended** turn.
fn ends_suspended_turn(row: &Value) -> bool {
    let Some(SessionEvent::Turn { data, .. }) = SessionEvent::from_value(row)
    else {
        return false;
    };
    data.get("reason").and_then(Value::as_str) == Some("suspended")
        || data.get("status").and_then(Value::as_str) == Some("suspended")
}

/// The ordinal a baseline cursor points at: the last stamped `seq`,
/// or the row count for a log written before ordinals were stamped
/// (the fold numbers that log positionally, so the cursor does too).
fn cursor_of(rows: &[Value]) -> u64 {
    match rows.last() {
        None => 0,
        Some(row) => row
            .get("seq")
            .and_then(Value::as_u64)
            .unwrap_or(rows.len() as u64),
    }
}

fn state_label(state: SessionState) -> &'static str {
    match state {
        SessionState::Idle => "idle",
        SessionState::Running => "running",
        SessionState::AwaitingApproval => "awaiting_approval",
        SessionState::Cancelled => "cancelled",
    }
}

fn agent_attribution(head: Option<&Value>) -> Option<String> {
    let first = head?;
    if first.get("type").and_then(Value::as_str) != Some("subagent_enter") {
        return None;
    }
    first
        .pointer("/data/agent_name")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn row_ts(row: &Value) -> Option<String> {
    row.get("ts").and_then(Value::as_str).map(str::to_string)
}

fn last_ts(rows: &[Value]) -> Option<String> {
    rows.iter()
        .rev()
        .find_map(|row| row.get("ts").and_then(Value::as_str))
        .map(str::to_string)
}

/// The provider the session last ran with, resolved from the log's
/// newest `request_header`.
///
/// A turn may pin a provider (`model: "<provider>/<model>"`); the
/// window and the summariser must use that handle, not the deployment
/// default. A row whose provider cannot be resolved falls back to the
/// default, which is also what a session with no header uses.
pub(crate) fn context_provider(
    state: &AppState,
    rows: &[Value],
) -> Arc<dyn ModelProvider> {
    let header = rows.iter().rev().find_map(|row| {
        let SessionEvent::RequestHeader { data, .. } =
            SessionEvent::from_value(row)?
        else {
            return None;
        };
        let provider = data.get("provider")?.as_str()?;
        let model = data.get("model")?.as_str()?;
        Some(format!("{provider}/{model}"))
    });
    match header {
        Some(selection) => state
            .resolve_provider(Some(&selection))
            .unwrap_or_else(|_| Arc::clone(&state.default_provider)),
        None => Arc::clone(&state.default_provider),
    }
}

/// Price the next request against the model's window.
///
/// The meter anchors on the provider's newest usage report; before
/// one exists it falls back to the heuristic surface price. Either
/// way the number reacts to a compaction, which reports no usage of
/// its own.
pub(crate) fn context_meter(state: &AppState, rows: &[Value]) -> ContextMeter {
    let events: Vec<SessionEvent> =
        rows.iter().filter_map(SessionEvent::from_value).collect();
    let mut meter = TokenMeter::new();
    let _ = meter.observe(&events);
    let window =
        context_provider(state, rows).model_config().context_window as u64;
    let used = meter
        .context_pressure(Some(window))
        .map(|pressure| pressure.projected_tokens)
        .unwrap_or_else(|| meter.measure().total_tokens);
    ContextMeter {
        used,
        window: Some(window),
    }
}

/// `410 Gone` for a cursor past the tail, carrying the current cursor
/// so the client can restart without guessing.
fn truncated_response(from: u64, cursor: u64) -> Response {
    let body = serde_json::json!({
        "code": "follow_cursor_truncated",
        "message": format!(
            "follow cursor {from} is past the log tail ({cursor}); restart from 0"
        ),
        "cursor": cursor,
    });
    (StatusCode::GONE, axum::Json(body)).into_response()
}

/// The rows past `cursor`, read from the log's tail.
///
/// A stamped log (every row the writer wrote carries its ordinal)
/// resumes by ordinal: the window is the only part of the log the poll
/// has to read, and its first row is normally already delivered, which
/// is how the poll knows nothing was cut off. `rows_after` falls back
/// to the full read in the two cases the window cannot answer —
/// a legacy log numbered by position, and a burst large enough to fill
/// the window between two polls.
async fn rows_after(
    sink: &dyn synthia::session::SessionSink,
    cursor: u64,
) -> Result<Vec<Value>, synthia::session::SessionError> {
    let tail = sink.read_tail(FOLLOW_WINDOW).await?;
    if tail.iter().any(|row| row.get("seq").is_none()) {
        // A log written before ordinals: the cursor counts rows, so
        // only the full read can resume it.
        let rows = sink.read().await?;
        let skip = usize::try_from(cursor).unwrap_or(usize::MAX);
        return Ok(rows.into_iter().skip(skip).collect());
    }
    match tail.iter().position(|row| is_past(row, cursor)) {
        None => Ok(Vec::new()),
        Some(0) if tail.len() == FOLLOW_WINDOW => {
            // The window's first row is already past the cursor: rows
            // this poll should deliver may have been cut off, so read
            // the whole log rather than lose them.
            let rows = sink.read().await?;
            Ok(rows
                .into_iter()
                .filter(|row| is_past(row, cursor))
                .collect())
        }
        Some(index) => Ok(tail[index..].to_vec()),
    }
}

/// `true` when the row's ordinal is past `cursor`.
fn is_past(row: &Value, cursor: u64) -> bool {
    row.get("seq").and_then(Value::as_u64).unwrap_or_default() > cursor
}

fn baseline_frame(snapshot: &FollowSnapshot) -> Event {
    let data = serde_json::to_string(snapshot).unwrap_or_else(|_| "{}".into());
    Event::default().event("baseline").data(data)
}

fn row_frame(seq: u64, turn_id: Option<&str>, row: &Value) -> Event {
    let data = serde_json::json!({
        "seq": seq,
        "turn_id": turn_id,
        "row": row,
    })
    .to_string();
    Event::default().event("row").data(data)
}

fn live_frame(live: &LiveTurn) -> Event {
    let data = serde_json::to_string(live).unwrap_or_else(|_| "{}".into());
    Event::default().event("live").data(data)
}

fn interrupts_frame(pending: &[ApprovalRequest]) -> Event {
    let data = serde_json::json!({ "pending_interrupts": pending }).to_string();
    Event::default().event("interrupts").data(data)
}

/// The baseline, then the durable tail (polled) and the live tap
/// (broadcast).
///
/// The stream stays open across turns: a settled turn is not the end
/// of the session, so closing would force a reconnect between every
/// turn. The client closes it.
fn follow_stream(
    controller: Arc<SessionController>,
    sink: Arc<dyn synthia::session::SessionSink>,
    rx: broadcast::Receiver<synthia::harness::AgentEvent>,
    baseline: FollowSnapshot,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        yield Ok(baseline_frame(&baseline));
        let mut cursor = baseline.cursor;
        let mut live_seq = baseline.live.live_seq;
        let mut pending: Vec<String> = baseline
            .pending_interrupts
            .iter()
            .map(|request| request.id.clone())
            .collect();
        let mut ticker = tokio::time::interval(FOLLOW_POLL);
        ticker.set_missed_tick_behavior(
            tokio::time::MissedTickBehavior::Delay,
        );
        let mut rx = rx;
        loop {
            tokio::select! {
                received = rx.recv() => match received {
                    Ok(event) => {
                        let live = controller.live_snapshot();
                        if live.live_seq > live_seq {
                            live_seq = live.live_seq;
                            yield Ok(live_frame(&live));
                        }
                        if is_terminal(&event) {
                            tracing::debug!(
                                target: "synthia.session",
                                "follow: the run settled; the stream stays open"
                            );
                        }
                    }
                    // A lagged subscriber missed frames, not rows: the
                    // durable poll below resumes from the cursor.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = ticker.tick() => {
                    // The projection can advance without a frame of its
                    // own reaching this subscriber (a lagged
                    // broadcast); reading it on the same tick keeps the
                    // transient view from going stale.
                    let live = controller.live_snapshot();
                    if live.live_seq > live_seq {
                        live_seq = live.live_seq;
                        yield Ok(live_frame(&live));
                    }
                    // Only the rows past the cursor, and only the log's
                    // tail: `rows_after` re-reads the window, not the
                    // whole file (the full read is its legacy fallback).
                    let Ok(new_rows) = rows_after(&*sink, cursor).await
                    else {
                        continue;
                    };
                    if new_rows.is_empty() {
                        continue;
                    }
                    let live = controller.live_snapshot();
                    let labels = turn_labels(
                        &new_rows,
                        &live.turn_id,
                        live.running,
                    );
                    let mut approvals_changed = false;
                    for (offset, row) in new_rows.iter().enumerate() {
                        let seq = row
                            .get("seq")
                            .and_then(Value::as_u64)
                            .unwrap_or(cursor + 1 + offset as u64);
                        cursor = cursor.max(seq);
                        approvals_changed |= changes_approvals(row);
                        yield Ok(row_frame(seq, labels[offset], row));
                    }
                    if approvals_changed {
                        let Ok(window) =
                            read_tail(&*sink, FOLLOW_WINDOW).await
                        else {
                            continue;
                        };
                        let open = match open_requests_in_window(&window) {
                            Some(open) => open,
                            None => match sink.read().await {
                                Ok(full) => open_requests(&full),
                                Err(_) => continue,
                            },
                        };
                        let ids: Vec<String> =
                            open.iter().map(|r| r.id.clone()).collect();
                        if ids != pending {
                            pending = ids;
                            yield Ok(interrupts_frame(&open));
                        }
                    }
                },
            }
        }
    }
}

/// The turn each content row in a poll batch supersedes, for the
/// client's "a durable row supersedes that turn's projection" rule.
///
/// Only rows the projection actually renders are labelled
/// ([`carries_live_content`]): an assistant message or a tool result.
/// Labelling every row of the running turn (the user prompt, `step` /
/// `turn` boundaries) retired the projection on the first poll and
/// blanked the streaming answer.
///
/// A batch can straddle two runs: the tail of the previous one (written
/// before the new run started) plus the new run's rows. The last
/// `turn/start` in the batch is the boundary — content before it belongs
/// to a projection the client no longer holds (`None`), content from it
/// onward belongs to the current turn. A batch with no start is part of
/// the running turn when the projection is live; once the run has
/// settled there is no projection left to supersede, so the labels are
/// `None` rather than a stale id.
pub(crate) fn turn_labels<'a>(
    rows: &[Value],
    current: &'a str,
    running: bool,
) -> Vec<Option<&'a str>> {
    let last_start = rows.iter().rposition(is_turn_start);
    (0..rows.len())
        .map(|index| {
            let belongs = match last_start {
                Some(start) => index >= start,
                None => running && !current.is_empty(),
            };
            if belongs && carries_live_content(&rows[index]) {
                Some(current)
            } else {
                None
            }
        })
        .collect()
}

/// Rows whose content the live projection already represents, and which
/// therefore supersede it when they land.
///
/// Everything else in a running turn — the user prompt, `turn` / `step`
/// boundaries, usage, approvals — is either not part of the projection
/// or must not retire it. Labelling those with the live turn id made a
/// client drop the in-flight text on the first poll of a run (the user
/// row is the first durable row), blanking the answer until the next
/// event: the projection is the only thing rendering it at that point.
fn carries_live_content(row: &Value) -> bool {
    matches!(
        row.get("type").and_then(Value::as_str),
        Some("assistant_message" | "tool_result")
    )
}

fn is_turn_start(row: &Value) -> bool {
    let Some(SessionEvent::Turn { data, .. }) = SessionEvent::from_value(row)
    else {
        return false;
    };
    data.get("kind").and_then(Value::as_str) == Some("start")
        || data.get("phase").and_then(Value::as_str) == Some("start")
}

/// `true` when a row can move the open-interrupt set.
fn changes_approvals(row: &Value) -> bool {
    match SessionEvent::from_value(row) {
        Some(SessionEvent::ApprovalAsked { .. })
        | Some(SessionEvent::ApprovalDecided { .. }) => true,
        Some(SessionEvent::Turn { data, .. }) => {
            data.get("kind").and_then(Value::as_str) == Some("end")
                || data.get("phase").and_then(Value::as_str) == Some("end")
        }
        _ => false,
    }
}

fn is_terminal(event: &synthia::harness::AgentEvent) -> bool {
    matches!(
        event,
        synthia::harness::AgentEvent::System(
            synthia::harness::SystemEvent::SessionEnded { .. }
        )
    )
}

// ---------------------------------------------------------------------------
// POST /api/v1/sessions/{id}/resume
// ---------------------------------------------------------------------------

/// The resume body: the token the suspension issued and one decision
/// per open interrupt.
#[derive(Debug, Deserialize, validator::Validate)]
pub struct ResumeRequest {
    /// The token from the `Suspended` event / the follow baseline.
    pub resume_token: synthia::harness::ResumeToken,
    /// One decision per interrupt; the list must cover every open one
    /// (an empty list is a coverage error, not a malformed body).
    pub interrupts: Vec<InterruptDecision>,
    /// Accept a changed request envelope instead of refusing.
    #[serde(default)]
    pub allow_drift: bool,
}

/// One decision on the wire. `id` names the interrupt (`ApprovalRequest::id`
/// or its `call_id`).
#[derive(Debug, Deserialize, Serialize)]
pub struct InterruptDecision {
    /// The interrupt this answers.
    pub id: String,
    /// `resolved`, `cancelled` or `rejected`.
    pub status: synthia::harness::ResumeStatus,
    /// Decision detail (a denial reason, a grant scope, …).
    #[serde(default)]
    pub payload: Option<Value>,
}

/// POST /api/v1/sessions/{id}/resume
///
/// Validation happens here, before anything is forwarded: a token that
/// no longer matches the log is `409`, a decision set that does not
/// cover the open interrupts is `422`. The harness validates again; a
/// mismatch there ends the run as an error event rather than a 500.
///
/// The refusals are distinct, because a client acts on each one
/// differently: `resume_conflict` says the log moved on (re-read the
/// state, let the operator decide again), `resume_no_snapshot` says
/// this round cannot be resumed at all (start a new session), and a
/// token whose decisions are already in the log is not a refusal at
/// all — it is a replay, answered with `200 OK`.
pub async fn resume(
    State(state): State<Arc<AppState>>,
    AppPath(session_id): AppPath<String>,
    AppJson(request): AppJson<ResumeRequest>,
) -> Result<StatusCode, AppError> {
    validate_resource_name(&session_id)?;
    let user_id = resolve_user_id(&state);
    let controller =
        registered_controller(&state, &user_id, &session_id).await?;
    if controller.is_remote() {
        return Err(conflict(
            "remote_session",
            format!(
                "session '{session_id}' is owned by another node; resume it there"
            ),
        ));
    }
    let rows = session_rows(&state, &user_id, &session_id).await?;
    let token = request.resume_token;
    if resume_already_applied(&rows, &token) {
        // The decisions this token names are in the log, so the client
        // is replaying a resume the server already applied. A replay
        // must not become a second decision for one call, and it is
        // answered before the busy check: the resumed run is usually
        // still in flight when a client retries.
        return Ok(StatusCode::OK);
    }
    if controller.session_state().await == SessionState::Running {
        return Err(conflict(
            "session_busy",
            format!("session '{session_id}' is running; wait for it to settle"),
        ));
    }
    let open = open_requests(&rows);
    if open.is_empty() {
        return Err(conflict(
            "resume_conflict",
            format!(
                "session '{session_id}' holds no open interrupt this token \
                 could answer; the log moved on, so re-read the state and \
                 let the operator decide again"
            ),
        ));
    }
    let tail = rows
        .iter()
        .filter_map(|row| row.get("seq").and_then(Value::as_u64))
        .max()
        .unwrap_or(rows.len() as u64);
    if token.last_seq != tail {
        return Err(conflict(
            "resume_conflict",
            format!(
                "resume token ends at {} but the log ends at {tail}; the log \
                 moved on, so re-read the state and let the operator decide \
                 again",
                token.last_seq
            ),
        ));
    }
    let mut expected = token.pending.clone();
    expected.sort();
    expected.dedup();
    let mut logged: Vec<String> =
        open.iter().map(|request| request.call_id.clone()).collect();
    logged.sort();
    if logged != expected {
        return Err(conflict(
            "resume_conflict",
            format!(
                "resume token covers {expected:?} but the log holds \
                 {logged:?}; the log moved on, so re-read the state and \
                 let the operator decide again"
            ),
        ));
    }
    let Some(snapshot) = latest_request_snapshot_hash(&rows) else {
        // Every run stamps an envelope; a log without one cannot have
        // issued a valid token, so this round cannot be resumed at
        // all. The client is told to start a new session rather than
        // to retry a token that can never match.
        return Err(conflict(
            "resume_no_snapshot",
            format!(
                "session '{session_id}' has no request snapshot; this round \
                 cannot be resumed, so start a new session"
            ),
        ));
    };
    if snapshot != token.request_snapshot_hash && !request.allow_drift {
        return Err(conflict(
            "config_drift",
            format!(
                "the log's request envelope is {snapshot}, the token's is {}; \
                 resend with allow_drift to accept the change",
                token.request_snapshot_hash
            ),
        ));
    }
    let decisions: Vec<synthia::harness::ResumeDecision> = request
        .interrupts
        .into_iter()
        .map(|decision| synthia::harness::ResumeDecision {
            id: decision.id,
            status: decision.status,
            payload: decision.payload,
        })
        .collect();
    if let Err(error) = synthia::harness::check_coverage(&open, &decisions) {
        return Err(AppError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            synthia::core::Error::validation(format!(
                "invalid resume: {error}"
            )),
        )
        .with_code("incomplete_resume")
        .with_message(format!("invalid resume: {error}")));
    }
    controller
        .submit(SessionOp::Resume {
            token,
            decisions,
            allow_drift: request.allow_drift,
        })
        .await
        .map_err(|error| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                synthia::core::Error::session(format!(
                    "submit resume: {error}"
                )),
            )
            .with_code("resume_submit")
        })?;
    Ok(StatusCode::ACCEPTED)
}

// ---------------------------------------------------------------------------
// POST /api/v1/sessions/{id}/compact
// ---------------------------------------------------------------------------

/// What a manual compaction pass did.
#[derive(Debug, Serialize)]
pub struct CompactResponse {
    /// `true` when a summary replaced its span.
    pub committed: bool,
    /// First shadowed surface position (fold index).
    pub start: Option<usize>,
    /// One past the last shadowed surface position.
    pub end: Option<usize>,
    /// Messages the replace shadowed.
    pub shadowed_messages: usize,
    /// Pre-prune token price of the shadowed span.
    pub shadowed_tokens: usize,
    /// The replacement text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// First shadowed log ordinal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_seq: Option<u64>,
    /// Last shadowed log ordinal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seq: Option<u64>,
    /// Why nothing was committed (`under_budget`, `empty_span`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

impl CompactResponse {
    fn skipped(reason: &'static str) -> Self {
        Self {
            committed: false,
            start: None,
            end: None,
            shadowed_messages: 0,
            shadowed_tokens: 0,
            summary: None,
            first_seq: None,
            last_seq: None,
            reason: Some(reason),
        }
    }
}

/// POST /api/v1/sessions/{id}/compact
pub async fn compact(
    State(state): State<Arc<AppState>>,
    AppPath(session_id): AppPath<String>,
) -> Result<axum::Json<CompactResponse>, AppError> {
    validate_resource_name(&session_id)?;
    let user_id = resolve_user_id(&state);
    let controller =
        registered_controller(&state, &user_id, &session_id).await?;
    if controller.is_remote() {
        return Err(conflict(
            "remote_session",
            format!(
                "session '{session_id}' is owned by another node; compact it there"
            ),
        ));
    }
    // The pass holds the gate a run start needs, so the state read
    // below cannot be invalidated by a run beginning mid-pass.
    let _compaction_gate = controller.compaction_gate().await;
    match controller.session_state().await {
        SessionState::Running => {
            return Err(conflict(
                "session_busy",
                format!(
                    "session '{session_id}' is running; a compaction rewrites \
                     the surface a live request is built from"
                ),
            ));
        }
        SessionState::AwaitingApproval => {
            return Err(conflict(
                "session_suspended",
                format!(
                    "session '{session_id}' is awaiting approval; resolve the \
                     interrupts before compacting"
                ),
            ));
        }
        SessionState::Idle | SessionState::Cancelled => {}
    }
    let Some(settings) = state.default_compaction else {
        return Err(compact_not_configured(&session_id));
    };
    if !settings.enabled {
        return Err(compact_not_configured(&session_id));
    }
    if let Err(error) = settings.validate() {
        return Err(conflict(
            "invalid_compaction_policy",
            format!("the configured compaction policy is invalid: {error}"),
        ));
    }
    let writer = state.session_manager.writer(&user_id, &session_id);
    let report = run_manual_compaction(&state, &writer, settings).await?;
    Ok(axum::Json(report))
}

fn compact_not_configured(session_id: &str) -> AppError {
    conflict(
        "compaction_not_configured",
        format!(
            "session '{session_id}' has no compaction policy configured \
             (`agents.<name>.compaction`)"
        ),
    )
}

/// One manual compaction pass over the durable log.
///
/// The planning and the durable rows are `synthia-session`'s
/// (`plan_compaction` / `begin_compaction` / `finish_compaction`) — the
/// same functions the step-boundary pass calls, so a manual replace and
/// an automatic one cannot diverge in shape or provenance.
async fn run_manual_compaction(
    state: &AppState,
    writer: &Arc<synthia::session::LogWriter>,
    settings: synthia::context::CompactionSettings,
) -> Result<CompactResponse, AppError> {
    use synthia::session::{
        CompactionLifecycleOutcome,
        CompactionPass,
        Compactor,
        PlannedCompaction,
        SummaryCompactor,
        TokenBudget,
        begin_compaction,
        finish_compaction,
        plan_compaction,
    };
    let rows = writer.read().await.map_err(session_read_error)?;
    let provider = context_provider(state, &rows);
    let model = provider.model_config();
    let budget = TokenBudget {
        window: model.context_window,
        reserve: settings.reserve_tokens as usize,
    };
    let compactor = SummaryCompactor::new(provider, model.name)
        .with_sleeper(crate::spawner::server_sleeper())
        .with_timeout(synthia::session::DEFAULT_SUMMARY_TIMEOUT);
    let plan = match plan_compaction(&rows, &compactor, budget) {
        Ok(PlannedCompaction::Plan(plan)) => plan,
        Ok(PlannedCompaction::Declined) => {
            return Ok(CompactResponse::skipped("under_budget"));
        }
        Ok(PlannedCompaction::EmptySpan) => {
            return Ok(CompactResponse::skipped("empty_span"));
        }
        Err(error) => {
            return Err(conflict(
                "log_not_foldable",
                format!(
                    "the session log does not fold into a surface: {error}"
                ),
            ));
        }
    };
    let token = begin_compaction(writer, &plan, "manual")
        .await
        .map_err(session_read_error)?;
    let outcome = match compactor
        .summarize(&plan, Arc::new(synthia::core::AtomicCancelToken::new()))
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            // Close the audit pair before reporting: a start row with
            // no end is the crash marker the loader repairs.
            let _ = finish_compaction(
                writer,
                &token,
                "manual",
                &plan,
                CompactionPass::Failed {
                    error: &error.to_string(),
                    outcome: CompactionLifecycleOutcome::Failed,
                },
            )
            .await;
            return Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                synthia::core::Error::internal(format!(
                    "compaction summariser failed: {error}"
                )),
            )
            .with_code("compaction_failed"));
        }
    };
    finish_compaction(
        writer,
        &token,
        "manual",
        &plan,
        CompactionPass::Committed(&outcome),
    )
    .await
    .map_err(session_read_error)?;
    Ok(CompactResponse {
        committed: true,
        start: Some(plan.range.start),
        end: Some(plan.range.end),
        shadowed_messages: plan.range.len(),
        shadowed_tokens: plan.shadowed_tokens,
        summary: Some(outcome.summary),
        first_seq: plan.shadowed_seqs.first().copied(),
        last_seq: plan.shadowed_seqs.last().copied(),
        reason: None,
    })
}

// ---------------------------------------------------------------------------
// GET /api/v1/sessions/{id}/usage
// ---------------------------------------------------------------------------

/// Session-scoped token usage. Deliberately **not** the process-wide
/// counters at `/api/v1/chat/usage`: a client showing one conversation
/// must not be billed for every other session this process ran.
#[derive(Debug, Serialize)]
pub struct SessionUsageResponse {
    /// The session the numbers belong to.
    pub session_id: String,
    /// Uncached prompt tokens across the session's log.
    pub input_tokens: u64,
    /// Completion tokens (reasoning included, not double-counted).
    pub output_tokens: u64,
    /// KV-cache read tokens, when providers reported them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// KV-cache write tokens, when providers reported them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Turns that reached `turn/end`.
    pub turns: u64,
    /// Tokens the next request is projected to carry.
    pub context_used: u64,
    /// The model's advertised window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

/// GET /api/v1/sessions/{id}/usage
pub async fn usage(
    State(state): State<Arc<AppState>>,
    AppPath(session_id): AppPath<String>,
) -> Result<axum::Json<SessionUsageResponse>, AppError> {
    validate_resource_name(&session_id)?;
    let user_id = resolve_user_id(&state);
    let rows = session_rows(&state, &user_id, &session_id).await?;
    let events: Vec<SessionEvent> =
        rows.iter().filter_map(SessionEvent::from_value).collect();
    let mut meter = TokenMeter::new();
    let _ = meter.observe(&events);
    let totals = meter.usage_totals();
    let turns = events
        .iter()
        .filter(|event| match event {
            SessionEvent::Turn { data, .. } => {
                data.get("kind").and_then(Value::as_str) == Some("end")
                    || data.get("phase").and_then(Value::as_str) == Some("end")
            }
            _ => false,
        })
        .count() as u64;
    let context = context_meter(&state, &rows);
    Ok(axum::Json(SessionUsageResponse {
        session_id,
        input_tokens: totals.input_tokens,
        output_tokens: totals.output_tokens,
        cache_read_tokens: totals.cache_read_tokens,
        cache_write_tokens: totals.cache_write_tokens,
        turns,
        context_used: context.used,
        context_window: context.window,
    }))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Whether the log already holds the decision for every call the
/// token names.
///
/// A resume the log applied and whose answer the client never saw is a
/// **replay**, not a new decision: the log holds an `approval_decided`
/// row for each call the token named. The replay must not become a
/// second decision for one call, so the route answers it with the
/// idempotent success instead of a conflict.
///
/// A token that names no call cannot be judged this way: a `Paused`
/// suspension carries an empty pending set and has nothing to apply.
fn resume_already_applied(
    rows: &[Value],
    token: &synthia::harness::ResumeToken,
) -> bool {
    if token.pending.is_empty() {
        return false;
    }
    let mut decided: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        // The tag is read off the row itself: parsing a row clones it,
        // and only the decision rows are worth that.
        if row.get("type").and_then(Value::as_str) != Some("approval_decided") {
            continue;
        }
        let Some(SessionEvent::ApprovalDecided { id, call_id, .. }) =
            SessionEvent::from_value(row)
        else {
            continue;
        };
        decided.insert(id);
        decided.insert(call_id);
    }
    token.pending.iter().all(|call| decided.contains(call))
}

fn conflict(code: &'static str, message: String) -> AppError {
    AppError::new(
        StatusCode::CONFLICT,
        synthia::core::Error::internal(message.clone()),
    )
    .with_code(code)
    .with_message(message)
}

fn session_read_error(error: synthia::session::SessionError) -> AppError {
    AppError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        synthia::core::Error::session(format!("session log: {error}")),
    )
    .with_code("session_log_read")
}

/// Resolve a controller for a session that must already exist.
async fn registered_controller(
    state: &Arc<AppState>,
    user_id: &str,
    session_id: &str,
) -> Result<Arc<SessionController>, AppError> {
    if state.session_manager.get(session_id).await.is_none() {
        // A process that restarted has no registry entry; the durable
        // transcript is the proof the session exists. `get_or_create`
        // restores it (and repairs a crashed tail) without minting a
        // directory for a typo.
        let exists = crate::routes::sessions::transcript_path(
            state.session_manager.sessions_root(),
            user_id,
            session_id,
        )
        .is_some_and(|path| path.is_file());
        if !exists {
            return Err(AppError::new(
                StatusCode::NOT_FOUND,
                synthia::core::Error::not_found(format!(
                    "session '{session_id}'"
                )),
            )
            .with_code("not_found"));
        }
    }
    state
        .get_or_create_session_controller(user_id, session_id)
        .await
        .map_err(|error| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                synthia::core::Error::session(format!(
                    "resolve session controller: {error}"
                )),
            )
            .with_code("session_controller")
        })
}

/// The session's durable rows, from the live sink when the session is
/// registered and from the transcript file when it is not.
async fn session_rows(
    state: &Arc<AppState>,
    user_id: &str,
    session_id: &str,
) -> Result<Vec<Value>, AppError> {
    if state.session_manager.get(session_id).await.is_some() {
        return state
            .session_manager
            .sink(user_id, session_id)
            .read()
            .await
            .map_err(session_read_error);
    }
    let Some(path) = crate::routes::sessions::transcript_path(
        state.session_manager.sessions_root(),
        user_id,
        session_id,
    ) else {
        return Err(AppError::new(
            StatusCode::NOT_FOUND,
            synthia::core::Error::not_found(format!("session '{session_id}'")),
        )
        .with_code("not_found"));
    };
    let rows = tokio::task::spawn_blocking(move || {
        crate::routes::sessions::transcript_events(&path).collect::<Vec<_>>()
    })
    .await
    .map_err(|error| {
        AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            synthia::core::Error::internal(format!(
                "join transcript read: {error}"
            )),
        )
        .with_code("session_log_read")
    })?;
    if rows.is_empty() {
        return Err(AppError::new(
            StatusCode::NOT_FOUND,
            synthia::core::Error::not_found(format!("session '{session_id}'")),
        )
        .with_code("not_found"));
    }
    Ok(rows)
}
