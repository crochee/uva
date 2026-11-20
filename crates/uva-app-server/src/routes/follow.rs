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
//! data: { seq: <u64>, row: <durable row> }
//!
//! event: live
//! data: { start_seq, live_seq, running, text, reasoning, tool_calls }
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
//! transient (`live`, carrying `live_seq`). A `live` frame's
//! `start_seq` is the log position where its projection's step began,
//! so a durable content row whose own ordinal is greater than that
//! position supersedes the projection — the client compares ordinals
//! and needs no label on the row. A `live` frame whose `live_seq` is
//! not greater than the baseline's is discarded (it predates the
//! window the client just accepted).
//!
//! A crash mid-turn loses `live` and leaves no half-written assistant
//! row — the durable log simply never got one. That is the "one row
//! per turn" trade-off, not data loss a client must repair.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;
use uva::{
    harness::{
        ApprovalRequest,
        latest_request_snapshot_hash,
        open_requests,
        suspended_segment,
    },
    provider::traits::ModelProvider,
    session::{
        RowTag,
        SessionEvent,
        TokenMeter,
        publish_row,
        publish_rows,
        restore_row,
    },
    tool_task::{HEAD_ROWS, RecordSource, head_records},
};

use crate::{
    api::{v1::session_view::SessionTurn, validate_resource_name},
    routes::{chat::resolve_user_id, sessions::SESSION_LIST_TAIL_WINDOW},
    rpc::RpcError,
    session::{
        controller::{LiveTurn, SessionController, SessionOp},
        status::{self, SessionState, SessionStatus, StatusFacts},
    },
    state::{
        AppState,
        MustExistError,
        session_resolution::{transcript_events, transcript_path},
    },
};

/// How many durable rows the baseline carries. The window is fixed:
/// `from` cannot widen it, and a client that needs older rows reads
/// the session transcript instead.
pub const FOLLOW_WINDOW: usize = 200;

/// How often the durable tail is polled for rows the live tap never
/// saw (structural rows carry no event).
const FOLLOW_POLL: Duration = Duration::from_millis(100);

/// The longest an idle poll waits before looking again.
///
/// A subscriber that sees nothing backs off: the poll that found no
/// new rows doubles the wait, up to this bound. A poll that finds
/// rows — or a live event — returns the wait to [`FOLLOW_POLL`], so
/// an active session is still polled at the fast rate.
const FOLLOW_POLL_MAX: Duration = Duration::from_millis(500);

/// The budget for one whole manual compaction pass — the log read,
/// the fold, the summariser, the durable replace and the flush. The
/// summariser is already bounded inside `SummaryCompactor`
/// (`DEFAULT_SUMMARY_TIMEOUT`); this constant is the outer bound for
/// everything else in the pass, so a stalled log read or replace can
/// no longer wedge the compaction gate. Deliberately
/// non-configurable: the seam is the constant, and a deployment that
/// needs a different budget says so with a config field then.
pub(crate) const COMPACTION_TIMEOUT: Duration = Duration::from_secs(120);

/// The session-level facts a client needs before its first row.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionHeader {
    /// The session id.
    pub session_id: String,
    /// The session's owner.
    pub user_id: String,
    /// The agent the session runs as (delegated children name the
    /// peer that ran them).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    /// The session status — the same closed set the session listing
    /// and the session detail carry. The `follow` header used to
    /// speak its own four-label vocabulary (`idle` / `running` /
    /// `awaiting_approval` / `cancelled`); retiring it means a client
    /// asking "what state is this session in" gets one answer, not
    /// two.
    pub state: SessionStatus,
    /// First durable row's timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Last durable row's timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// How much of the model's window the next request would occupy.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextMeter {
    /// Tokens the next request is projected to carry.
    pub used: u64,
    /// The model's advertised window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
}

/// The complete first frame of a `follow` stream.
///
/// The type is the wire contract's owner: a client decodes the
/// baseline into this very type, so the shape the server publishes
/// and the shape a client reads cannot drift apart.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FollowSnapshot {
    /// Session-level facts.
    pub header: SessionHeader,
    /// The last [`FOLLOW_WINDOW`] durable rows.
    #[schemars(with = "Vec<SessionTurn>")]
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

/// The projection events a follow subscriber receives.
///
/// The enum is the frame's envelope: the `kind` tag is the frame's
/// discriminator, and the variant's members are the frame's payload.
/// A client decodes a frame into this very type, so the shape the
/// server publishes and the shape a client reads cannot drift apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FollowEvent {
    /// A durable row landed in the log.
    Row {
        /// The row's ordinal in the log.
        seq: u64,
        /// The durable row itself.
        row: Value,
        /// The fingerprint of the log's newest request envelope.
        request_snapshot_hash: Option<String>,
    },
    /// The in-progress turn's transient projection moved.
    Live {
        /// The projection.
        live: LiveTurn,
    },
    /// The open interrupt set changed.
    Interrupts {
        /// The interrupts still waiting for a decision.
        pending_interrupts: Vec<ApprovalRequest>,
    },
}

impl FollowEvent {
    /// The notification payload for the event.
    ///
    /// The shape is the one the SSE frames carried: a `row` frame's
    /// members are flat, the other two wrap their payload. The
    /// payload is the frame's own serialization — the envelope has
    /// one definition, the one a client decodes back.
    ///
    /// # Errors
    ///
    /// Returns the serialization error when the frame cannot be
    /// written as JSON. The frame's own members cannot fail to
    /// serialize, so this is the envelope's escape hatch rather than
    /// a path a caller should meet.
    pub fn payload(&self) -> Result<Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

/// `sessions/follow` — the baseline, then the live projection.
pub async fn follow(
    state: Arc<AppState>,
    session_id: String,
    query: FollowQuery,
) -> Result<FollowSnapshot, RpcError> {
    validate_resource_name(&session_id)?;
    let user_id = resolve_user_id(&state);
    // Subscribe before the baseline read: an event that lands between
    // the two is buffered, and the client's `live_seq`/`seq` filters
    // discard anything the baseline already covers.
    // A read stream never mints a session: following an id that has
    // no log is a 404, not an empty session.
    let controller = state
        .must_exist_session_controller(&user_id, &session_id)
        .await?;
    let rx = controller.subscribe();
    let sink = state.session_manager.sink(&user_id, &session_id);
    let rows =
        read_tail(sink.as_ref(), FOLLOW_WINDOW)
            .await
            .map_err(|error| {
                RpcError::from(uva::core::Error::session(format!(
                    "read session log: {error}"
                )))
                .with_code("session_log_read")
            })?;
    // The header carries two whole-log facts (the session's first
    // timestamp, a delegated session's agent) that a window cannot
    // answer, so the head is read on its own — bounded, like the tail.
    // A head that cannot be read is a log that cannot be read: failing
    // the request keeps `created_at` a whole-log fact. Substituting the
    // window's first timestamp would put a plausible, wrong value on
    // the wire instead.
    let head = sink.read_head_rows(HEAD_ROWS).await.map_err(|error| {
        RpcError::from(uva::core::Error::session(format!(
            "read session log head: {error}"
        )))
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
    let baseline_copy = baseline.clone();
    if let Some(from) = query.from
        && from > baseline.cursor
    {
        return Err(truncated_response(from, baseline.cursor));
    }
    let sink = state.session_manager.sink(&user_id, &session_id);
    // The projection runs in its own task: the answer is the
    // baseline, and every later change is a notification. The task
    // owns its own handles, so it does not borrow the state.
    let registry = Arc::clone(state.rpc.follows());
    let notification_session = session_id.clone();
    registry.start(session_id, {
        let registry = Arc::clone(&registry);
        let outbound_session = notification_session;
        async move {
            follow_stream(controller, sink, rx, baseline, move |event| {
                match event.payload() {
                    Ok(payload) => {
                        registry.emit_session_event(&outbound_session, payload);
                    }
                    Err(error) => {
                        // The frame's members cannot fail to serialize;
                        // a frame that does is a bug, and dropping it
                        // with a trace beats emitting a `null` frame
                        // every subscriber would have to guess at.
                        tracing::error!(
                            %error,
                            "the follow frame does not serialize"
                        );
                    }
                }
                // The interrupt set is also projected call by call as
                // `approval/request`, so a transport that speaks
                // approvals does not have to read the session
                // projection. The baseline's `pending_interrupts` is
                // a result, not a notification, and needs no second
                // copy: a reconnect re-reads it there.
                if let FollowEvent::Interrupts { pending_interrupts } = &event {
                    for request in pending_interrupts {
                        registry
                            .emit_approval_request(&outbound_session, request);
                    }
                }
            })
            .await;
        }
    });
    Ok(baseline_copy)
}

/// The durable tail a `follow` baseline is built from.
///
/// This is the whole point of [`SessionSink::read_tail`]: the baseline
/// used to call `read()` and slice the last [`FOLLOW_WINDOW`] rows in
/// memory, which made every subscription cost the session's entire
/// history — a cost that grows with the log for the one call that runs
/// on every reconnect.
pub(crate) async fn read_tail(
    sink: &dyn uva::session::SessionSink,
    window: usize,
) -> Result<Vec<Value>, uva::session::SessionError> {
    sink.read_tail(window).await
}

/// Build the complete baseline from the log's tail window.
///
/// `rows` is already the window ([`read_tail`]); `head` is the log's
/// first row. The window answers the row-shaped facts; the three
/// *whole-log* folds — the context meter, the model window it is priced
/// against, and the resume fingerprint — need a window that holds the
/// whole last turn, widening geometrically when the fixed window
/// cannot prove it does (see [`window_holds_last_turn`]). They must:
/// `TokenMeter` folds every surface and usage row, and
/// `context_provider` resolves the model
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
    sink: &dyn uva::session::SessionSink,
    rows: Vec<Value>,
    head: Vec<Value>,
) -> FollowSnapshot {
    let cursor = cursor_of(&rows);
    // The three whole-log folds — the context meter, the model window
    // it is priced against, and the resume fingerprint — need every
    // row of the last turn, which a clipped [`FOLLOW_WINDOW`] cannot
    // prove it holds. Instead of falling back to a whole-file `read()`
    // (whose cost grows with the session's length), widen the tail
    // window geometrically — the same shape the takeover's
    // parked-prompt scan uses — starting at the listing's window and
    // multiplying by 4 until the window provably holds the last turn
    // or has reached the log's start (a window that returns fewer
    // rows than asked for *is* the whole log, so the widened reads
    // degenerate to the old full read exactly when they must).
    let mut window = FOLLOW_WINDOW;
    let mut facts = rows.clone();
    while !window_holds_last_turn(&facts) && facts.len() >= window {
        window = if window < SESSION_LIST_TAIL_WINDOW {
            SESSION_LIST_TAIL_WINDOW
        } else {
            window.saturating_mul(4)
        };
        match sink.read_tail(window).await {
            Ok(wider) => facts = wider,
            Err(error) => {
                tracing::warn!(
                    session_id = %session_id,
                    %error,
                    "follow: widening the baseline window failed; the \
                     baseline's context describes the window only"
                );
                break;
            }
        }
    }
    let facts: &[Value] = &facts;
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
    let head_window: &[Value] = if head.is_empty() {
        rows.first().map(std::slice::from_ref).unwrap_or(&[])
    } else {
        head.as_slice()
    };
    let header = SessionHeader {
        session_id: session_id.to_string(),
        user_id: user_id.to_string(),
        agent_name: agent_attribution(head_window, session_id),
        state: status::session_status(
            StatusFacts {
                controller: Some(controller.state()),
                awaiting: !pending_interrupts.is_empty(),
            },
            !rows.is_empty(),
        ),
        created_at: head.first().and_then(row_ts),
        updated_at: last_ts(&rows),
    };
    // Publishing is the last step: the cursor, the header timestamps
    // and every fold above answered from the raw log rows, so a row
    // the vocabulary cannot publish still advanced them.
    FollowSnapshot {
        header,
        rows: publish_rows(rows),
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
pub(crate) fn window_holds_last_turn(rows: &[Value]) -> bool {
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
    let events: Vec<SessionEvent> = rows
        .iter()
        .filter_map(|row| restore_row(row).event())
        .collect();
    let mut meter = TokenMeter::new();
    if meter.observe(&events).is_err() {
        return false;
    }
    meter.context_pressure(None).is_some()
}

/// Open interrupts folded from a tail window, or `None` when the
/// window cannot prove it holds the whole last suspended turn.
///
/// The fold needs the segment that starts after the **previous** turn
/// boundary, so a clipped window that holds the suspended end but not
/// the boundary before it would fold rows from an older turn — and
/// resurrect asks that were answered before it. `None` sends the
/// caller to a wider window instead of guessing.
pub(crate) fn open_requests_in_window(
    rows: &[Value],
) -> Option<Vec<ApprovalRequest>> {
    let ends = rows
        .iter()
        .filter(|row| turn_boundary(row) == Some(false))
        .count();
    if ends == 0 {
        // No boundary in the window: it cannot prove which turn it
        // holds, and `None` sends the caller to the full log.
        return None;
    }
    if suspended_segment(rows).is_none() {
        // The harness's own rule — the single source — says the last
        // turn is not suspended, so nothing is open.
        return Some(Vec::new());
    }
    if ends < 2 {
        // The suspended turn's own end is the only boundary in the
        // window, so its head may precede the window.
        return None;
    }
    Some(open_requests(rows))
}

/// The open interrupts, read through a window that widens until
/// [`open_requests_in_window`] can prove what it holds.
///
/// The same geometric widening the baseline and `rows_after` use:
/// start at the listing's window, multiply by 4 while the window is
/// full and cannot yet prove the segment, stop at the log's start (a
/// short return **is** the whole log, whose fold is then the answer).
/// `None` only for a read that failed outright.
async fn open_interrupts_widened(
    sink: &dyn uva::session::SessionSink,
) -> Option<Vec<ApprovalRequest>> {
    let mut window = SESSION_LIST_TAIL_WINDOW;
    loop {
        let rows = sink.read_tail(window).await.ok()?;
        if let Some(open) = open_requests_in_window(&rows) {
            return Some(open);
        }
        if rows.len() < window {
            return Some(open_requests(&rows));
        }
        window = window.saturating_mul(4);
    }
}

/// `Some(true)` for a turn start, `Some(false)` for a turn end, `None`
/// when the row is not a turn boundary.
///
/// Turn-only on purpose, even though the same accessor also answers for
/// `step` and `iteration` rows. The loop writes one step pair per
/// iteration, so counting those as turn boundaries would let a window
/// that clipped the turn's own head look whole — a `step_start` would
/// satisfy [`window_holds_last_turn`]'s "a start exists" clause, and one
/// turn end plus one step end would satisfy
/// [`open_requests_in_window`]'s "two ends" clause.
pub(crate) fn turn_boundary(row: &Value) -> Option<bool> {
    let event = restore_row(row).event()?;
    let SessionEvent::Turn { .. } = event else {
        return None;
    };
    match event.boundary_kind()? {
        "start" => Some(true),
        "end" => Some(false),
        _ => None,
    }
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

fn agent_attribution(head: &[Value], session_id: &str) -> Option<String> {
    head_records(head, session_id)
        .into_iter()
        .find(|record| record.source == RecordSource::OwnLog)
        .map(|record| record.agent_name)
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
        let event = restore_row(row).event()?;
        if !matches!(event, SessionEvent::RequestHeader { .. }) {
            return None;
        }
        let provider = event.request_provider()?;
        let model = event.request_model()?;
        Some(format!("{provider}/{model}"))
    });
    match header {
        Some(selection) => state
            .resolve_provider(Some(&selection))
            .or_else(|_| {
                state.live_default_provider().ok_or_else(|| {
                    uva::core::Error::internal("no default provider is mounted")
                })
            })
            .unwrap_or_else(|_| {
                // Last resort before the run itself fails: the cached
                // mirror, so the error surfaces from the provider call
                // with its own context rather than a bare panic.
                state.default_provider.read().clone()
            }),
        None => state
            .live_default_provider()
            .unwrap_or_else(|| state.default_provider.read().clone()),
    }
}

/// Price the next request against the model's window.
///
/// The meter anchors on the provider's newest usage report; before
/// one exists it falls back to the heuristic surface price. Either
/// way the number reacts to a compaction, which reports no usage of
/// its own.
pub(crate) fn context_meter(state: &AppState, rows: &[Value]) -> ContextMeter {
    let events: Vec<SessionEvent> = rows
        .iter()
        .filter_map(|row| restore_row(row).event())
        .collect();
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
fn truncated_response(from: u64, cursor: u64) -> RpcError {
    RpcError::application(
        "follow_cursor_truncated",
        format!(
            "follow cursor {from} is past the log tail ({cursor}); restart from 0"
        ),
    )
}

/// The rows past `cursor`, read from the log's tail.
///
/// A stamped log (every row the writer wrote carries its ordinal)
/// resumes by ordinal: the sink reads forward from the cursor, so a
/// poll costs the rows that arrived since the last one — not the
/// whole tail, re-read every time.
///
/// A legacy log numbered by position is the one shape an ordinal
/// cannot address: the cursor counts rows there, so only a window
/// that holds the whole log can resume it positionally. That path
/// reads the tail and widens geometrically (the same shape the
/// baseline and the takeover's parked-prompt scan use) instead of
/// reading the whole file.
async fn rows_after(
    sink: &dyn uva::session::SessionSink,
    cursor: u64,
) -> Result<Vec<Value>, uva::session::SessionError> {
    let mut window = SESSION_LIST_TAIL_WINDOW;
    loop {
        let rows = sink.read_since(cursor, window).await?;
        if rows.iter().any(|row| row.get("seq").is_none()) {
            // A log written before ordinals: the cursor counts rows,
            // so only a window that holds the whole log can resume
            // it positionally.
            let tail = sink.read_tail(window).await?;
            let whole_log = tail.len() < window;
            if !whole_log {
                window = window.saturating_mul(4);
                continue;
            }
            let skip = usize::try_from(cursor).unwrap_or(usize::MAX);
            return Ok(tail.into_iter().skip(skip).collect());
        }
        return Ok(rows);
    }
}

/// The baseline, then the durable tail (polled) and the live tap
/// (broadcast).
///
/// The stream stays open across turns: a settled turn is not the end
/// of the session, so closing would force a reconnect between every
/// turn. The client closes it.
pub(crate) async fn follow_stream(
    controller: Arc<SessionController>,
    sink: Arc<dyn uva::session::SessionSink>,
    rx: broadcast::Receiver<uva::harness::AgentEvent>,
    baseline: FollowSnapshot,
    mut emit: impl FnMut(FollowEvent) + Send,
) {
    {
        let mut cursor = baseline.cursor;
        let mut live_seq = baseline.live.live_seq;
        let mut pending: Vec<String> = baseline
            .pending_interrupts
            .iter()
            .map(|request| request.id.clone())
            .collect();
        // The durable poll waits longer the longer the stream sees
        // nothing: an idle subscriber stops reading the log every
        // 100 ms and settles at the backoff bound, and any row or
        // live event puts it back to the fast rate.
        //
        // The deadline is absolute: a live event must not restart the
        // wait, or a busy session would starve the durable poll.
        let mut poll_delay = FOLLOW_POLL;
        let mut poll_deadline = tokio::time::Instant::now() + poll_delay;
        let mut rx = rx;
        // A non-owner handle has no run of its own: it projects the
        // owner's events with the same reducer the owner's tap uses,
        // so a follower on any node sees the live turn.
        let mut remote_live = crate::session::controller::LiveTurn::default();
        loop {
            tokio::select! {
                received = rx.recv() => match received {
                    Ok(event) => {
                        // A live event means the session is active: the
                        // durable poll keeps the fast rate.
                        poll_delay = FOLLOW_POLL;
                        let live = if controller.projects_live_locally() {
                            controller.live_snapshot()
                        } else {
                            // The owner's log position is not this
                            // node's to know: the remote projection
                            // carries no position, so a client's
                            // ordinal comparison falls back to "any
                            // content row supersedes it".
                            remote_live.apply(&event);
                            remote_live.clone()
                        };
                        if live.live_seq > live_seq {
                            live_seq = live.live_seq;
                            emit(FollowEvent::Live { live: live.clone() });
                        }
                        if is_terminal(&event) {
                            tracing::debug!(
                                target: "uva.session",
                                "follow: the run settled; the stream stays open"
                            );
                        }
                    }
                    // A lagged subscriber missed frames, not rows: the
                    // durable poll below resumes from the cursor.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = tokio::time::sleep_until(poll_deadline) => {
                    // The controller exited (idle timeout or node
                    // shutdown): this stream holds one of its `Arc`
                    // handles, so the broadcast channel never reports
                    // `Closed` — without this break the body outlives
                    // its session and a graceful HTTP drain never
                    // finishes while a UI is connected.
                    if !controller.is_alive() {
                        break;
                    }
                    // The projection can advance without a frame of its
                    // own reaching this subscriber (a lagged
                    // broadcast); reading it on the same tick keeps the
                    // transient view from going stale.
                    if controller.projects_live_locally() {
                        let live = controller.live_snapshot();
                        if live.live_seq > live_seq {
                            live_seq = live.live_seq;
                            emit(FollowEvent::Live { live: live.clone() });
                        }
                    }
                    // Only the rows past the cursor, and only the
                    // log's tail: `rows_after` widens its window
                    // geometrically, never the whole file.
                    let Ok(new_rows) = rows_after(&*sink, cursor).await
                    else {
                        continue;
                    };
                    if new_rows.is_empty() {
                        // Nothing arrived: back off.
                        poll_delay =
                            (poll_delay * 2).min(FOLLOW_POLL_MAX);
                        poll_deadline =
                            tokio::time::Instant::now() + poll_delay;
                        continue;
                    }
                    // Rows arrived: the fast rate again.
                    poll_delay = FOLLOW_POLL;
                    poll_deadline = tokio::time::Instant::now() + poll_delay;
                    let mut approvals_changed = false;
                    // The envelope a resume must reproduce changes
                    // with every request snapshot, and the client
                    // holds it to build the resume token. A client
                    // that only learned the hash from its baseline
                    // would submit the hash of an *earlier* turn and
                    // the resume would be refused as a drift.
                    let mut envelope_hash: Option<String> = None;
                    for (offset, row) in new_rows.iter().enumerate() {
                        let seq = row
                            .get("seq")
                            .and_then(Value::as_u64)
                            .unwrap_or(cursor + 1 + offset as u64);
                        cursor = cursor.max(seq);
                        approvals_changed |= changes_approvals(row);
                        if restore_row(row)
                            .restores_to(RowTag::RequestSnapshot)
                        {
                            envelope_hash =
                                latest_request_snapshot_hash(&new_rows);
                        }
                        // Publish before emitting: a row with no current
                        // shape sends no frame, but the cursor above has
                        // already advanced, so iterate the raw rows and
                        // publish each one.
                        if let Some(published) = publish_row(row.clone()) {
                            emit(FollowEvent::Row {
                                seq,
                                row: published,
                                request_snapshot_hash: envelope_hash.clone(),
                            });
                        }
                    }
                    if approvals_changed {
                        let Some(open) =
                            open_interrupts_widened(sink.as_ref()).await
                        else {
                            continue;
                        };
                        let ids: Vec<String> =
                            open.iter().map(|r| r.id.clone()).collect();
                        if ids != pending {
                            pending = ids;
                            emit(FollowEvent::Interrupts {
                                pending_interrupts: open,
                            });
                        }
                    }
                },
            }
        }
    }
}

/// `true` when a row can move the open-interrupt set.
fn changes_approvals(row: &Value) -> bool {
    match restore_row(row).event() {
        Some(SessionEvent::ApprovalAsked { .. })
        | Some(SessionEvent::ApprovalDecided { .. }) => true,
        Some(event @ SessionEvent::Turn { .. }) => {
            event.boundary_kind() == Some("end")
        }
        _ => false,
    }
}

fn is_terminal(event: &uva::harness::AgentEvent) -> bool {
    matches!(
        event,
        uva::harness::AgentEvent::System(
            uva::harness::SystemEvent::SessionEnded { .. }
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
    pub resume_token: uva::harness::ResumeToken,
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
    pub status: uva::harness::ResumeStatus,
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
///
/// The replay is only a replay when the request agrees with the log:
/// a request that decides a call differently than the log did is a
/// `resume_conflict`, because the answer that stands is the one the
/// log holds.
pub async fn resume(
    state: Arc<AppState>,
    session_id: String,
    request: ResumeRequest,
) -> Result<(), RpcError> {
    validate_resource_name(&session_id)?;
    let user_id = resolve_user_id(&state);
    let controller = state
        .must_exist_session_controller(&user_id, &session_id)
        .await?;
    // The validation below reads the shared log, so it answers the
    // same on any node; only the run itself must happen on the owner.
    // A remote handle's submit forwards the resume verbatim.
    let rows = session_rows(&state, &user_id, &session_id).await?;
    let token = request.resume_token;
    let decisions: Vec<uva::harness::ResumeDecision> = request
        .interrupts
        .into_iter()
        .map(|decision| uva::harness::ResumeDecision {
            id: decision.id,
            status: decision.status,
            payload: decision.payload,
        })
        .collect();
    match resume_replay(&rows, &token, &decisions) {
        ResumeReplay::Applied => {
            // The decisions this token names are in the log, and this
            // request agrees with them, so the client is replaying a
            // resume the server already applied. A replay must not
            // become a second decision for one call, and it is
            // answered before the busy check: the resumed run is
            // usually still in flight when a client retries.
            return Ok(());
        }
        ResumeReplay::Changed {
            id,
            logged,
            requested,
        } => {
            // The operator changed their mind about a call the log
            // already answered. The answer that stands is the one in
            // the log, so the request is a conflict: the client
            // re-reads the state and lets the operator decide again.
            return Err(conflict(
                "resume_conflict",
                format!(
                    "the log already decided '{id}' as '{logged}' while this \
                     request says '{requested}'; re-read the state and let \
                     the operator decide again"
                ),
            ));
        }
        ResumeReplay::NotApplied => {}
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
                 could answer; re-read the state and let the operator \
                 decide again"
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
    if let Err(error) = uva::harness::check_coverage(&open, &decisions) {
        return Err(RpcError::from(uva::core::Error::validation(format!(
            "invalid resume: {error}"
        )))
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
            RpcError::from(uva::core::Error::session(format!(
                "submit resume: {error}"
            )))
            .with_code("resume_submit")
        })?;
    Ok(())
}

/// The open interrupts and the resume token the log currently
/// supports.
///
/// The token is derived exactly the way [`resume`] validates it: the
/// log tail, the held calls and the newest request envelope. A caller
/// that only knows an approval id (the `approval/respond` alias) can
/// therefore build a resume without holding a token of its own — the
/// log remains the single source of truth. `None` means the log holds
/// no request envelope, so no token can exist.
pub(crate) async fn open_interrupts_with_token(
    state: &Arc<AppState>,
    session_id: &str,
) -> Result<(Vec<ApprovalRequest>, Option<uva::harness::ResumeToken>), RpcError>
{
    validate_resource_name(session_id)?;
    let user_id = resolve_user_id(state);
    let rows = session_rows(state, &user_id, session_id).await?;
    let open = open_requests(&rows);
    let tail = rows
        .iter()
        .filter_map(|row| row.get("seq").and_then(Value::as_u64))
        .max()
        .unwrap_or(rows.len() as u64);
    let token = latest_request_snapshot_hash(&rows).map(|snapshot| {
        let pending: Vec<String> =
            open.iter().map(|request| request.call_id.clone()).collect();
        uva::harness::ResumeToken::new(tail, pending, snapshot)
    });
    Ok((open, token))
}

// ---------------------------------------------------------------------------
// POST /api/v1/sessions/{id}/compact
// ---------------------------------------------------------------------------

/// What a manual compaction pass did.
#[derive(Debug, Serialize, JsonSchema)]
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
    state: Arc<AppState>,
    session_id: String,
) -> Result<CompactResponse, RpcError> {
    validate_resource_name(&session_id)?;
    let user_id = resolve_user_id(&state);
    let controller = state
        .must_exist_session_controller(&user_id, &session_id)
        .await?;
    if controller.is_remote() {
        // A pass rewrites the surface, so it runs on the owner. The
        // frame carries no answer: the caller is told the pass was
        // accepted, and the result lands as the usual compacted rows.
        state
            .cluster
            .bus
            .publish_control(crate::cluster::ControlFrame::CompactSession {
                origin: state.cluster.node_id.clone(),
                key: crate::cluster::SessionKey::new(
                    user_id.clone(),
                    session_id.clone(),
                ),
            })
            .await
            .map_err(|error| {
                RpcError::from(uva::core::Error::internal(format!(
                    "could not reach the session owner: {error}"
                )))
                .with_code("owner_unreachable")
            })?;
        return Ok(CompactResponse::skipped("forwarded_to_owner"));
    }
    // The pass holds the gate a run start needs, so the state read
    // below cannot be invalidated by a run beginning mid-pass.
    controller
        .with_compaction_gate(async {
            match controller.session_state().await {
                SessionState::Running => {
                    return Err(conflict(
                        "session_busy",
                        format!(
                            "session '{session_id}' is running; a compaction \
                             rewrites the surface a live request is built \
                             from"
                        ),
                    ));
                }
                SessionState::AwaitingApproval => {
                    return Err(conflict(
                        "session_suspended",
                        format!(
                            "session '{session_id}' is awaiting approval; \
                             resolve the interrupts before compacting"
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
                    format!(
                        "the configured compaction policy is invalid: {error}"
                    ),
                ));
            }
            let writer = state.session_manager.writer(&user_id, &session_id);
            run_manual_compaction(&state, &writer, settings).await
        })
        .await
}

fn compact_not_configured(session_id: &str) -> RpcError {
    conflict(
        "compaction_not_configured",
        format!(
            "session '{session_id}' has no compaction policy configured \
             (`agents.<name>.compaction`)"
        ),
    )
}

/// One manual compaction pass over the durable log, bounded by
/// [`COMPACTION_TIMEOUT`].
///
/// The planning and the durable rows are `uva-session`'s
/// (`plan_compaction_from_rows` / `begin_compaction` / `finish_compaction`) — the
/// same functions the step-boundary pass calls, so a manual replace and
/// an automatic one cannot diverge in shape or provenance.
async fn run_manual_compaction(
    state: &AppState,
    writer: &Arc<uva::session::LogWriter>,
    settings: uva::session::CompactionSettings,
) -> Result<CompactResponse, RpcError> {
    run_manual_compaction_within(state, writer, settings, COMPACTION_TIMEOUT)
        .await
}

/// The pass with the budget as a parameter, so the timeout's own
/// test can drive it with a deadline of milliseconds instead of
/// waiting out the production constant. The error message derives
/// from the budget it was actually given, so it stays truthful in
/// both cases.
///
/// A timeout leaves the audit pair open. That orphan is *detected*
/// (`uva_session::orphaned_compactions` reports a
/// `CompactionStart` with no matching end) and inert — nothing
/// consumes an unpaired start — but no caller repairs it today; the
/// alternative (racing a `finish_compaction` against the pass it is
/// closing) could interleave two writers' rows in the span being
/// replaced.
pub(crate) async fn run_manual_compaction_within(
    state: &AppState,
    writer: &Arc<uva::session::LogWriter>,
    settings: uva::session::CompactionSettings,
    budget_timeout: Duration,
) -> Result<CompactResponse, RpcError> {
    match tokio::time::timeout(
        budget_timeout,
        run_manual_compaction_pass(state, writer, settings),
    )
    .await
    {
        Ok(result) => result,
        Err(_elapsed) => Err(RpcError::from(uva::core::Error::internal(
            compaction_timeout_message(budget_timeout),
        ))
        .with_code("compaction_timeout")),
    }
}

/// The timeout's text, from the budget that actually elapsed —
/// `Duration`'s own rendering (`120s`, `250ms`) keeps the plan's
/// "timed out after …" shape while staying truthful for a
/// non-production budget.
fn compaction_timeout_message(budget: Duration) -> String {
    format!("session compaction timed out after {budget:?}")
}

/// The unbounded body of one manual compaction pass.
async fn run_manual_compaction_pass(
    state: &AppState,
    writer: &Arc<uva::session::LogWriter>,
    settings: uva::session::CompactionSettings,
) -> Result<CompactResponse, RpcError> {
    use uva::session::{
        CompactionLifecycleOutcome,
        CompactionPass,
        Compactor,
        PlannedCompaction,
        SummaryCompactor,
        TokenBudget,
        begin_compaction,
        finish_compaction,
        plan_compaction_from_rows,
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
        .with_timeout(uva::session::DEFAULT_SUMMARY_TIMEOUT);
    let plan = match plan_compaction_from_rows(&rows, &compactor, budget) {
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
    let token = begin_compaction(writer.as_ref(), &plan, "manual")
        .await
        .map_err(session_read_error)?;
    let outcome = match compactor
        .summarize(&plan, Arc::new(uva::core::AtomicCancelToken::new()))
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            // Close the audit pair before reporting: an orphaned
            // start row is *detected*
            // (`orphaned_compactions`) and inert — nothing consumes
            // an unpaired start — but no caller repairs it today.
            let _ = finish_compaction(
                writer.as_ref(),
                &token,
                "manual",
                &plan,
                CompactionPass::Failed {
                    error: &error.to_string(),
                    outcome: CompactionLifecycleOutcome::Failed,
                },
            )
            .await;
            return Err(RpcError::from(uva::core::Error::internal(format!(
                "compaction summariser failed: {error}"
            )))
            .with_code("compaction_failed"));
        }
    };
    finish_compaction(
        writer.as_ref(),
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
#[derive(Debug, Serialize, JsonSchema)]
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
    state: Arc<AppState>,
    session_id: String,
) -> Result<SessionUsageResponse, RpcError> {
    validate_resource_name(&session_id)?;
    let user_id = resolve_user_id(&state);
    let rows = session_rows(&state, &user_id, &session_id).await?;
    let events: Vec<SessionEvent> = rows
        .iter()
        .filter_map(|row| restore_row(row).event())
        .collect();
    let mut meter = TokenMeter::new();
    let _ = meter.observe(&events);
    let totals = meter.usage_totals();
    let turns = events
        .iter()
        .filter(|event| match event {
            event @ SessionEvent::Turn { .. } => {
                event.boundary_kind() == Some("end")
            }
            _ => false,
        })
        .count() as u64;
    let context = context_meter(&state, &rows);
    Ok(SessionUsageResponse {
        session_id,
        input_tokens: totals.input_tokens,
        output_tokens: totals.output_tokens,
        cache_read_tokens: totals.cache_read_tokens,
        cache_write_tokens: totals.cache_write_tokens,
        turns,
        context_used: context.used,
        context_window: context.window,
    })
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// What the log says about a resume that names decided calls.
enum ResumeReplay {
    /// Every call the token names is decided in the log, and this
    /// request agrees with what was decided.
    Applied,
    /// The log decided a call differently than this request does. The
    /// interrupt the request answered differently, the status the log
    /// holds, and the status this request carries.
    Changed {
        id: String,
        logged: String,
        requested: String,
    },
    /// The log does not hold a decision for every call the token
    /// names.
    NotApplied,
}

/// Whether the log already holds the decision for every call the
/// token names, and whether this request is the one it holds.
///
/// A resume the log applied and whose answer the client never saw is a
/// **replay**, not a new decision: the log holds an `approval_decided`
/// row for each call the token named. The replay must not become a
/// second decision for one call, so the route answers it with the
/// idempotent success instead of a conflict — but only when the
/// request repeats the decision. A request that decides a call the
/// other way is a conflict, not a success: the answer that stands is
/// the one the log holds.
///
/// A token that names no call cannot be judged this way: a `Paused`
/// suspension carries an empty pending set and has nothing to apply.
fn resume_replay(
    rows: &[Value],
    token: &uva::harness::ResumeToken,
    decisions: &[uva::harness::ResumeDecision],
) -> ResumeReplay {
    if token.pending.is_empty() {
        return ResumeReplay::NotApplied;
    }
    // The log's decisions under both names a decision can address
    // them by: the approval id and the call id. The newest row for a
    // call wins, so a later decision for the same call is the one
    // that counts.
    let mut by_id: BTreeMap<String, String> = BTreeMap::new();
    let mut by_call: BTreeMap<String, String> = BTreeMap::new();
    for row in rows {
        // The tag is read off the row itself: parsing a row clones it,
        // and only the decision rows are worth that.
        if !restore_row(row).restores_to(RowTag::ApprovalDecided) {
            continue;
        }
        let Some(SessionEvent::ApprovalDecided {
            id,
            call_id,
            status,
            ..
        }) = restore_row(row).event()
        else {
            continue;
        };
        by_id.insert(id, status.clone());
        by_call.insert(call_id, status);
    }
    for decision in decisions {
        let logged = by_id
            .get(&decision.id)
            .or_else(|| by_call.get(&decision.id));
        if let Some(logged) = logged
            && logged.as_str() != decision.durable_status()
        {
            return ResumeReplay::Changed {
                id: decision.id.clone(),
                logged: logged.clone(),
                requested: decision.durable_status().to_string(),
            };
        }
    }
    let applied = token
        .pending
        .iter()
        .all(|call| by_id.contains_key(call) || by_call.contains_key(call));
    if applied {
        ResumeReplay::Applied
    } else {
        ResumeReplay::NotApplied
    }
}

fn conflict(code: &'static str, message: String) -> RpcError {
    RpcError::from(uva::core::Error::internal(message.clone()))
        .with_code(code)
        .with_message(message)
}

fn session_read_error(error: uva::session::SessionError) -> RpcError {
    RpcError::from(uva::core::Error::session(format!("session log: {error}")))
        .with_code("session_log_read")
}

/// The wire answer for a refused `must_exist` resolution: an absent
/// session is the session-shaped not-found, and anything else is a
/// session that exists failing to open.
impl From<MustExistError> for RpcError {
    fn from(error: MustExistError) -> Self {
        match error {
            MustExistError::Absent { session_id } => RpcError::from(
                uva::core::Error::not_found(format!("session '{session_id}'")),
            )
            .with_code("not_found"),
            MustExistError::Open(error) => {
                RpcError::from(uva::core::Error::session(format!(
                    "resolve session controller: {error}"
                )))
                .with_code("session_controller")
            }
        }
    }
}

/// The session's durable rows, from the live sink when the session is
/// registered and from the transcript file when it is not.
async fn session_rows(
    state: &Arc<AppState>,
    user_id: &str,
    session_id: &str,
) -> Result<Vec<Value>, RpcError> {
    if state.session_manager.get(session_id).await.is_some() {
        return state
            .session_manager
            .sink(user_id, session_id)
            .read()
            .await
            .map_err(session_read_error);
    }
    let Some(path) = transcript_path(
        state.session_manager.sessions_root(),
        user_id,
        session_id,
    ) else {
        return Err(RpcError::from(uva::core::Error::not_found(format!(
            "session '{session_id}'"
        )))
        .with_code("not_found"));
    };
    let rows = tokio::task::spawn_blocking(move || {
        transcript_events(&path).collect::<Vec<_>>()
    })
    .await
    .map_err(|error| {
        RpcError::from(uva::core::Error::internal(format!(
            "join transcript read: {error}"
        )))
        .with_code("session_log_read")
    })?;
    if rows.is_empty() {
        return Err(RpcError::from(uva::core::Error::not_found(format!(
            "session '{session_id}'"
        )))
        .with_code("not_found"));
    }
    Ok(rows)
}
