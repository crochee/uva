//! Session management handlers.
//!
//! [`list_sessions`] merges the in-memory
//! [`uva::session::manager::SessionRegistry`] with the durable
//! JSONL store: the registry only holds what this process has opened,
//! so on its own it hides every conversation recorded by an earlier
//! run. The endpoint shape is intentionally aligned with the chat
//! surface (`/api/v1/chat/sessions`) so callers can reuse the same
//! cursor pagination and filter semantics; only the URL path is
//! different (`/api/v1/sessions` is the read-optimised management
//! listing used by the `SessionsPage`, `/api/v1/chat/sessions` is the
//! chat surface used by `ChatPage`).
//!
//! Being a management view, the listing omits the throwaway sessions
//! the Messages endpoint mints for unbound requests — one
//! `anthropic-<uuid>` per Cursor / Claude Code / SDK turn, which
//! would otherwise bury the user's own conversations. The namespace
//! is `session::EPHEMERAL_SESSION_PREFIX` and the membership test is
//! "the id starts with that prefix", separator included, so an id
//! that only shares the letters (`anthropicx-1`) is listed normally.
//! A minted session stays durably addressable: [`get_session`] serves
//! one by id, and session search still indexes it.
//!
//! [`get_session`] fetches a single session's events directly from
//! the session sink — falling back to the transcript on disk when the
//! registry has no record of the id — so the frontend's
//! `SessionDetailPage` can render the full JSONL transcript.

use std::{
    collections::HashSet,
    io::{BufRead as _, BufReader},
    path::{Path, PathBuf},
    sync::Arc,
};

use base64::Engine as _;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use uva::{
    core::Error,
    session::{RowDefect, is_metadata_header_row, publish_rows, restore_row},
    tool_task::{HEAD_ROWS, RecordSource, head_records},
};

use super::helpers::paginate;
use crate::{
    api::{
        List,
        SessionPageQuery,
        resolve_page,
        v1::session_view::{SessionArtifact, SessionTurn},
        validate_resource_name,
        validate_sort,
    },
    rpc::RpcError,
    session::{
        EPHEMERAL_SESSION_PREFIX,
        controller::SessionController,
        status::{self, SessionState, SessionStatus, StatusFacts},
    },
    state::AppState,
};

/// Sortable fields for the sessions list endpoint. The historical
/// `updated_at` field is no longer produced (the session sink has
/// no notion of update timestamps), so we accept only `created_at`
/// and `status` and silently drop `updated_at` if a client sends
/// it.
const SESSION_SORT_WHITELIST: &[&str] = &["created_at", "status"];

/// How many rows of a session's log the listing reads: one head row
/// for the whole-log facts (creation time, sub-agent attribution)
/// and this many tail rows for the live-shaped ones (status, the
/// awaiting-approval judgement). The listing runs once per session
/// per request, so its cost must not grow with the log's length —
/// the whole point of the bound.
pub const SESSION_LIST_TAIL_WINDOW: usize = 512;

/// Frontend-facing session summary.
#[derive(Serialize, JsonSchema)]
pub struct SessionSummary {
    pub id: String,
    /// The session status — one of the five the status module
    /// produces. The wire spells it `unspecified`, `working`,
    /// `input_required`, `canceled` or `completed`.
    pub status: SessionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    /// When the session **started**: the log's first event's
    /// timestamp, never `now()` and never the last activity. The
    /// default sort (`-created_at`) is therefore "newest session
    /// first"; the frontend labels the field "started".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// Present when this session was opened by a delegated
    /// sub-agent (the `task` tool): the session the delegation
    /// ran in. A first-class session either way — the field only
    /// carries provenance, never privileges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// The registry name of the peer agent that ran this session
    /// (sub-agent sessions only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
}

/// Detailed session view including history. The `history`
/// field is a JSON-encoded array of session events (one
/// entry per agent / system frame persisted to the session
/// sink). The frontend's `SessionDetailPage` reads the entries
/// individually and runs them through the same renderer the
/// live `ChatPage` uses.
#[derive(Serialize, JsonSchema)]
pub struct SessionDetail {
    pub id: String,
    /// The session status — the same closed set the summary carries.
    pub status: SessionStatus,
    pub context_id: String,
    /// When the session **started**: the log's first event's
    /// timestamp — the same value [`SessionSummary::created_at`]
    /// reports, so the listing and this view never disagree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// When the session was last **active**: the log's last event's
    /// timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    #[schemars(with = "Vec<SessionTurn>")]
    pub history: Vec<Value>,
    /// Legacy field — kept as an empty array so the
    /// SessionDetailPage doesn't have to special-case undefined.
    /// The modern equivalents (`ContentPart::Raw`,
    /// `ContentPart::Resource`) are inlined into `history`.
    #[schemars(with = "Vec<SessionArtifact>")]
    pub artifacts: Vec<Value>,
    /// Sub-agent provenance — see [`SessionSummary::parent_session_id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
}

/// `true` when a row survives the caller's `status` / `context_id`
/// filters. Both filters apply to every row regardless of whether the
/// session came from the registry or only from disk, so the two listing
/// loops share this one predicate.
///
/// `context_id` is currently identical to `id` (sessions have no
/// separate context-id column), so the filter is a strict equality
/// match against the session id — what a client narrowing the list
/// would expect.
fn selected(
    id: &str,
    status: SessionStatus,
    status_filter: Option<&str>,
    context_id_filter: Option<&str>,
) -> bool {
    if let Some(filter) = status_filter
        && filter != status.as_wire()
    {
        return false;
    }
    if let Some(needle) = context_id_filter
        && id != needle
    {
        return false;
    }
    true
}

/// `true` when `id` names a session in the Messages endpoint's
/// unbound-request namespace (see [`EPHEMERAL_SESSION_PREFIX`]).
///
/// Such a session is skipped by [`list_sessions`] only — at both of
/// its sources: the registered loop, and
/// [`scan_unregistered_transcripts`], which drops the directories
/// before opening their logs. It is not hidden anywhere else:
/// [`get_session`], `list_session_events`, `session_status` and
/// session search all still address it, because a caller that names
/// one of these ids has asked for it on purpose — this predicate
/// answers "does this id belong to the throwaway namespace", not "may
/// this session be seen".
///
/// The prefix carries its own separator, so membership is exact: a
/// session named `anthropicx-1` starts with the same letters but is
/// not in the namespace and is listed like any other.
pub(crate) fn is_ephemeral_session(id: &str) -> bool {
    id.starts_with(EPHEMERAL_SESSION_PREFIX)
}

/// The controller's in-memory state, looked up by id.
///
/// Kept beside the status surfaces because it is a lookup, not a rule:
/// what the session *reports* is the status module's answer.
fn live_session_state(
    active_sessions: &DashMap<(String, String), Arc<SessionController>>,
    user_id: &str,
    session_id: &str,
) -> Option<SessionState> {
    active_sessions
        .get(&(user_id.to_string(), session_id.to_string()))
        .map(|ctrl| ctrl.state())
}

fn parse_rfc3339(raw: Option<&str>) -> Option<DateTime<Utc>> {
    raw.and_then(|s| {
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    })
}

/// `(parent_session_id, agent_name)` when a session's log opens
/// with the sub-agent router's `subagent_enter` header row — the
/// marker that this session is a delegated child. The parse is the
/// discovery module's own head rule, so the listing, the detail
/// view and the roster can never disagree about what a header is.
fn subagent_attribution(
    events: &[Value],
    session_id: &str,
) -> (Option<String>, Option<String>) {
    let window = &events[..events.len().min(HEAD_ROWS)];
    let own = head_records(window, session_id)
        .into_iter()
        .find(|record| record.source == RecordSource::OwnLog);
    match own {
        Some(record) => {
            (Some(record.parent_session_id), Some(record.agent_name))
        }
        None => (None, None),
    }
}
/// `<sessions_root>/<user_id>/<session_id>/events.jsonl`.
///
/// Refuses anything that is not a single directory entry, so a
/// hand-built path can never climb out of the sessions root:
/// `session_id` must match the resource-name regex the route path
/// parameter is already validated against (no `.`, `..`, `/` or `\`),
/// and `user_id` must be one path component.
pub(crate) fn transcript_path(
    sessions_root: &Path,
    user_id: &str,
    session_id: &str,
) -> Option<PathBuf> {
    if !is_path_component(user_id) {
        return None;
    }
    validate_resource_name(session_id).ok()?;
    Some(
        sessions_root
            .join(user_id)
            .join(session_id)
            .join("events.jsonl"),
    )
}

/// Whether `name` can only ever address one directory entry:
/// non-empty, neither `.` nor `..`, and free of separators and NUL.
pub(crate) fn is_path_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
}

/// The parseable events of a durable transcript, in order.
///
/// Blank lines and lines that are not JSON are skipped — the same
/// tolerance [`uva::session::JsonlSessionSearch`] applies when it
/// folds a log, so a torn tail left behind by a crash cannot hide the
/// conversation in front of it. An unreadable or empty log yields no
/// events at all, which is what makes both callers read "no
/// transcript" as "unknown session" rather than "empty session".
///
/// The sink's metadata header is **not** an event and is dropped here,
/// not at the call sites: both callers project *events*
/// (`legacy_window` slices the list into the scan's opening row and
/// tail window; `get_session` folds
/// the whole list into `history`), so a header left in would report a
/// disk-only session with no `created_at`, no subagent attribution,
/// and a phantom first history entry. A log holding only a header
/// yields no events, which reads as "unknown session" — correct, since
/// a session that never wrote an event has nothing to render.
///
/// Blocking `std::fs` IO: callers on the async runtime route this
/// through `spawn_blocking`.
pub(crate) fn transcript_events(
    path: &Path,
) -> Box<dyn Iterator<Item = Value>> {
    let Ok(file) = std::fs::File::open(path) else {
        return Box::new(std::iter::empty());
    };
    Box::new(
        BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .filter(|row| !is_metadata_header_row(row)),
    )
}

/// The raw JSONL rows of a durable transcript, in append order, with
/// blank and unparseable lines dropped.
///
/// Unlike [`transcript_events`] this keeps each line's original text
/// **and** the sink's metadata header. An export is an archival
/// artifact whose point is to reproduce the file: re-serialising the
/// parsed value (which may reorder keys) would not be faithful, and
/// dropping the header would strip the on-disk schema version the
/// artifact is supposed to carry. The caller gets the bytes the sink
/// wrote, minus a torn tail.
///
/// Blocking `std::fs` IO: callers on the async runtime route this
/// through `spawn_blocking`.
fn transcript_lines(path: &Path) -> Vec<String> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|line| {
            !line.trim().is_empty()
                && serde_json::from_str::<Value>(line).is_ok()
        })
        .collect()
}

/// One durable session the disk scan found: id, creation time,
/// and sub-agent attribution when the log opens with the
/// router's `subagent_enter` header.
#[derive(Debug)]
struct ScannedSession {
    id: String,
    created_at: Option<DateTime<Utc>>,
    parent_session_id: Option<String>,
    agent_name: Option<String>,
    /// The transcript holds an undecided interrupt. The scan already
    /// reads the file, so the paused state costs no extra I/O here —
    /// which matters because this is the only source for a session the
    /// process has not opened since it restarted.
    awaiting_approval: bool,
}

/// The unregistered sessions under the user's sessions directory:
/// id and transcript path, ready for the async scan.
///
/// Only the directory listing runs here — blocking `std::fs`, so the
/// caller wraps it in `spawn_blocking`; each transcript is then read
/// through its sink, bounded, on the async side.
pub(crate) fn unregistered_session_ids(
    sessions_root: &Path,
    user_id: &str,
    registered: &HashSet<String>,
) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    if !is_path_component(user_id) {
        return found;
    }
    let Ok(entries) = std::fs::read_dir(sessions_root.join(user_id)) else {
        // No directory yet (fresh deployment) or unreadable: there is
        // nothing durable to add, which is not an error.
        return found;
    };
    for entry in entries.flatten() {
        let session_id = entry.file_name().to_string_lossy().to_string();
        if registered.contains(&session_id) || is_ephemeral_session(&session_id)
        {
            continue;
        }
        if let Some(path) = transcript_path(sessions_root, user_id, &session_id)
        {
            found.push((session_id, path));
        }
    }
    found
}

/// One durable session the disk scan summarised: id, creation time,
/// sub-agent attribution, and the awaiting-approval flag.
///
/// Each transcript costs one head row plus one
/// [`SESSION_LIST_TAIL_WINDOW`] tail window — the same bounded shape
/// the registered loop uses, because a deployment that predates this
/// process can hold arbitrarily long logs on disk too. The one
/// exception is a **legacy** transcript (a version this build's sink
/// refuses to read): it falls back to the tolerant streaming reader
/// for that one file, because the listing has listed legacy logs
/// since before the sink could refuse them — and legacy files are
/// rare and shrinking, so the whole-file cost is not the hot path.
async fn scan_unregistered_transcripts(
    state: &AppState,
    user_id: &str,
    sessions: Vec<(String, std::path::PathBuf)>,
) -> Vec<ScannedSession> {
    let mut found = Vec::new();
    for (session_id, path) in sessions {
        // Reading through the registry's own sink (rather than a
        // transient one) is the deliberate steady state: the cached
        // sink serves every later listing and any later registration
        // of the session, and since the sink's log inspection became
        // lazy an unused one costs only its handle — no buffers, no
        // pre-sized LRU.
        let sink = state.session_manager.sink(user_id, &session_id);
        let (head, tail) = match scan_window(sink.as_ref(), &path).await {
            Some(window) => window,
            None => continue,
        };
        if head.is_empty() {
            continue;
        }
        // The head window is the whole-log fact: the first event's
        // timestamp, never `now()` — a legacy log that carries no
        // timestamp reports an unknown creation time — and the
        // sub-agent router's `subagent_enter` attribution.
        let created_at = parse_rfc3339(
            head.first()
                .and_then(|row| row.get("ts").and_then(Value::as_str)),
        );
        let (parent_session_id, agent_name) =
            subagent_attribution(&head, &session_id);
        // Judging approval from a bounded tail window is exact, not
        // an approximation: a pending approval suspends its run, and
        // nothing is appended after it, so an unmatched
        // `approval_asked` is always the log's tail — the window sees
        // it precisely when it exists.
        let awaiting_approval = !uva::harness::open_requests(&tail).is_empty();
        found.push(ScannedSession {
            id: session_id,
            created_at,
            parent_session_id,
            agent_name,
            awaiting_approval,
        });
    }
    found
}

/// One transcript's listing facts: the head row and the tail window.
///
/// `None` when the log is unreadable or empty — a session this
/// listing cannot describe. An unsupported schema version falls back
/// to the tolerant streaming reader for that one file (see
/// [`scan_unregistered_transcripts`]).
async fn scan_window(
    sink: &dyn uva::session::SessionSink,
    path: &std::path::Path,
) -> Option<(Vec<Value>, Vec<Value>)> {
    let head = match sink.read_head_rows(HEAD_ROWS).await {
        Ok(head) => head,
        Err(uva::session::SessionError::UnsupportedSchemaVersion {
            ..
        }) => return Some(legacy_window(path).await),
        Err(_) => return None,
    };
    // A failed tail read is an empty tail, not a missing session: a
    // torn last row (a crash mid-append) makes `read_tail` fail on
    // purpose, and hiding the session from the listing would be a
    // regression the registered loop does not have — it also falls
    // back to "no tail". Only the version refusal routes to the
    // legacy reader.
    let tail = match sink.read_tail(SESSION_LIST_TAIL_WINDOW).await {
        Ok(tail) => tail,
        Err(uva::session::SessionError::UnsupportedSchemaVersion {
            ..
        }) => return Some(legacy_window(path).await),
        Err(_) => Vec::new(),
    };
    Some((head, tail))
}

/// The legacy window: the whole file through the tolerant reader,
/// sliced into the same head-plus-tail shape. Blocking `std::fs` on
/// the async runtime, so the read rides `spawn_blocking` — the
/// legacy path is rare, not hot, but it still must not stall a
/// worker.
async fn legacy_window(path: &std::path::Path) -> (Vec<Value>, Vec<Value>) {
    let path = path.to_path_buf();
    let mut rows = tokio::task::spawn_blocking(move || {
        transcript_events(&path).collect::<Vec<Value>>()
    })
    .await
    .unwrap_or_default();
    // The head window is the log's own start, so capture it before
    // the tail split — a log shorter than the window keeps every row
    // in the tail.
    let head: Vec<Value> = rows.iter().take(HEAD_ROWS).cloned().collect();
    let split = rows.len().saturating_sub(SESSION_LIST_TAIL_WINDOW);
    let tail = rows.split_off(split);
    (head, tail)
}

/// GET /api/v1/sessions - List sessions with cursor pagination + filters.
pub async fn list_sessions(
    state: Arc<AppState>,
    query: SessionPageQuery,
) -> Result<List<SessionSummary>, RpcError> {
    validate_sort(
        query.page.sort.as_deref().unwrap_or("-created_at"),
        SESSION_SORT_WHITELIST,
    )?;
    let resolved = resolve_page(&query.page)?;

    let status_filter = match query.status.as_deref() {
        None | Some("") => None,
        Some(s) => Some(status::parse_status_filter(s)?),
    };
    let context_id_filter = query
        .context_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let user_id = state.default_user_id().to_string();
    let registered = state
        .session_manager
        .list_all()
        .await
        .map_err(|e| Error::session(format!("{e}")))?;
    let registered_ids: HashSet<String> =
        registered.iter().map(|s| s.id.clone()).collect();

    // Everything recorded before this process started is only on
    // disk, so the listing is the union of both sources. The scan
    // skips the ids the registry already covers, which is what keeps
    // the merge free of duplicates.
    let durable = {
        let sessions_root = state.session_manager.sessions_root().to_path_buf();
        let scan_user = user_id.clone();
        let ids = tokio::task::spawn_blocking(move || {
            unregistered_session_ids(
                &sessions_root,
                &scan_user,
                &registered_ids,
            )
        })
        .await
        .map_err(|e| Error::session(format!("join transcript scan: {e}")))?;
        scan_unregistered_transcripts(&state, &user_id, ids).await
    };

    let mut rows: Vec<SessionSummary> =
        Vec::with_capacity(registered.len() + durable.len());
    for session in registered {
        if is_ephemeral_session(&session.id) {
            continue;
        }
        let sink = state.session_manager.sink(&user_id, &session.id);
        // The listing's read per session is bounded by design: the
        // head row answers every whole-log fact (the creation
        // timestamp, the sub-agent attribution) and the tail window
        // answers every live-shaped one. A whole-file `read()` would
        // make the page's cost grow with each session's log length —
        // the one cost this route must not pay.
        //
        // The measured gap, visible below at debug level: the page
        // reads one head and one tail per session before it
        // paginates, so the read cost grows linearly with the
        // session count and the page's limit does not bound it.
        let read_started = std::time::Instant::now();
        let head = sink.read_head_rows(HEAD_ROWS).await.unwrap_or_default();
        let tail = sink
            .read_tail(SESSION_LIST_TAIL_WINDOW)
            .await
            .unwrap_or_default();
        tracing::debug!(
            target: "uva.session",
            session_id = %session.id,
            read_elapsed_ms = read_started.elapsed().as_secs_f64() * 1000.0,
            "sessions list: per-session bounded read"
        );
        // Judging approval from the tail window is exact, not an
        // approximation: a pending approval suspends its run and
        // nothing is appended after it, so an unmatched
        // `approval_asked` is always the log's tail — the window
        // hits it precisely when it exists.
        let facts = StatusFacts {
            controller: live_session_state(
                &state.active_sessions,
                &user_id,
                &session.id,
            ),
            awaiting: !uva::harness::open_requests(&tail).is_empty(),
        };
        let status = status::session_status(facts, !head.is_empty());
        let created_at = head.first().and_then(|row| {
            parse_rfc3339(row.get("ts").and_then(Value::as_str))
        });
        let (parent_session_id, agent_name) =
            subagent_attribution(&head, &session.id);
        if !selected(&session.id, status, status_filter, context_id_filter) {
            continue;
        }
        rows.push(SessionSummary {
            id: session.id.clone(),
            status,
            context_id: Some(session.id),
            created_at,
            parent_session_id,
            agent_name,
        });
    }
    // A session known only from disk has no controller, so its status
    // comes from the transcript alone. Its creation time is the first
    // event's timestamp; a session whose transcript is unreadable or
    // empty never reaches this loop, and neither does an ephemeral one
    // — the scan already dropped that namespace before opening its
    // logs. The paused state is the one status a transcript can carry
    // that the in-memory state cannot after a restart, so the scan
    // reports it and this loop trusts it.
    for scanned in durable {
        // The scan already folded the log's open approvals, so the
        // module is handed the verdict instead of re-reading the log.
        let facts = StatusFacts {
            controller: live_session_state(
                &state.active_sessions,
                &user_id,
                &scanned.id,
            ),
            awaiting: scanned.awaiting_approval,
        };
        let status = status::session_status(facts, true);
        if !selected(&scanned.id, status, status_filter, context_id_filter) {
            continue;
        }
        rows.push(SessionSummary {
            id: scanned.id.clone(),
            status,
            context_id: Some(scanned.id),
            created_at: scanned.created_at,
            parent_session_id: scanned.parent_session_id,
            agent_name: scanned.agent_name,
        });
    }

    let field = resolved.sort_field.as_deref().unwrap_or("created_at");
    match field {
        "status" => {
            // The `id` tiebreaker keeps the merged set's order stable
            // across requests: without it a page of equally-labelled
            // sessions (usually every row) has no defined order, and
            // the cursor could repeat or skip entries.
            //
            // The order is the **wire spelling's**, not the enum's:
            // the listing has always sorted by the label the client
            // reads, and the labels are not in the enum's order.
            rows.sort_by(|a, b| {
                a.status
                    .as_wire()
                    .cmp(b.status.as_wire())
                    .then_with(|| a.id.cmp(&b.id))
            });
            if resolved.descending {
                rows.reverse();
            }
        }
        _ => {
            rows.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.id.cmp(&b.id))
            });
            if resolved.descending {
                rows.reverse();
            }
        }
    }

    let list = paginate(rows, &resolved, |r: &SessionSummary| r.id.as_str());
    Ok(list)
}

/// GET /api/v1/sessions/{id} - Get a single session with history.
pub async fn get_session(
    state: Arc<AppState>,
    id: String,
) -> Result<SessionDetail, RpcError> {
    validate_resource_name(&id)?;
    let user_id = state.default_user_id().to_string();
    // The registry only knows the sessions this process has opened,
    // so a restart would 404 every earlier conversation. When it has
    // no record we fall back to the durable transcript instead — and
    // only when neither source knows the id is it unknown.
    if state.session_manager.get(&id).await.is_some() {
        let sink = state.session_manager.sink(&user_id, &id);
        let history = sink
            .read()
            .await
            .map_err(|e| Error::session(format!("{e}")))?;
        let live = live_session_state(&state.active_sessions, &user_id, &id);
        // The status surface answers from the transcript it is about
        // to serve, so a suspended session reads as `input_required`
        // even when the controller no longer remembers the pause.
        let facts = StatusFacts {
            controller: live,
            awaiting: !uva::harness::open_requests(&history).is_empty(),
        };
        let status = status::session_status(facts, !history.is_empty());
        // The creation time is the log's **head** row — the first
        // event — exactly as the listing's `read_head` reports it and
        // exactly as the disk-only detail view below reports it. The
        // last event is the *update* time: it moves with the
        // conversation, the creation time does not. (Deriving both
        // from the last event is what made the listing and this
        // view disagree about the same session's `created_at`.)
        let created_at = history
            .first()
            .and_then(|ev| parse_rfc3339(ev.get("ts").and_then(Value::as_str)));
        let updated_at = history
            .iter()
            .rev()
            .find_map(|ev| parse_rfc3339(ev.get("ts").and_then(Value::as_str)));
        let (parent_session_id, agent_name) =
            subagent_attribution(&history, &id);
        // Every fact above reads the raw log rows; publishing is the
        // last step so no fold sees the rewritten shapes (a dropped
        // row would otherwise move `updated_at`).
        let history = publish_rows(history);
        return Ok(SessionDetail {
            id: id.clone(),
            status,
            context_id: id,
            created_at,
            updated_at,
            history,
            artifacts: Vec::new(),
            parent_session_id,
            agent_name,
        });
    }

    // Read the log straight off disk: `SessionRegistry::sink` would
    // create the directory for whatever id the client sent, turning a
    // lookup into a write. `transcript_path` is what keeps the path
    // inside the sessions root.
    let Some(path) =
        transcript_path(state.session_manager.sessions_root(), &user_id, &id)
    else {
        return Err(RpcError::from(Error::not_found(format!(
            "session '{id}'"
        ))));
    };
    let history = tokio::task::spawn_blocking(move || {
        transcript_events(&path).collect::<Vec<Value>>()
    })
    .await
    .map_err(|e| Error::session(format!("join transcript read: {e}")))?;
    let Some(created_at) = history
        .first()
        .map(|ev| parse_rfc3339(ev.get("ts").and_then(Value::as_str)))
    else {
        // An empty or unreadable log is as unknown as a missing id:
        // there is no transcript to render.
        return Err(RpcError::from(Error::not_found(format!(
            "session '{id}'"
        ))));
    };
    let updated_at = history
        .iter()
        .rev()
        .find_map(|ev| parse_rfc3339(ev.get("ts").and_then(Value::as_str)));
    let (parent_session_id, agent_name) = subagent_attribution(&history, &id);
    // Disk-only means no controller and no in-memory state: the
    // transcript is the only place the paused state can come from, and
    // it is exactly the post-restart case.
    let facts = StatusFacts {
        controller: live_session_state(&state.active_sessions, &user_id, &id),
        awaiting: !uva::harness::open_requests(&history).is_empty(),
    };
    let status = status::session_status(facts, !history.is_empty());
    // Publishing is the last step: every derived fact above was
    // computed from the raw log rows.
    let history = publish_rows(history);
    Ok(SessionDetail {
        id: id.clone(),
        status,
        context_id: id,
        // No registry record, so the creation time comes from the
        // transcript's first event — never from `now()`.
        created_at,
        updated_at,
        history,
        artifacts: Vec::new(),
        parent_session_id,
        agent_name,
    })
}

/// Response envelope for `GET /api/v1/sessions/{id}/events`.
///
/// The rows are split by what the row vocabulary says they are:
/// `typed` (a current row — written in today's vocabulary or
/// restored from an old one — whose payload decodes), `legacy` (an
/// old-dialect row the current vocabulary has no counterpart for:
/// history this build still reads), `unknown` (a tag this build
/// does not know — version skew, not history), and `broken` (a row
/// this build cannot read: a current row whose payload does not
/// decode, or no usable tag at all). Each bucket carries its count.
#[derive(Serialize)]
pub struct TypedEventResponse {
    pub id: String,
    pub typed_count: usize,
    pub legacy_count: usize,
    pub unknown_count: usize,
    pub broken_count: usize,
    pub typed: Vec<uva::session::SessionEvent>,
    pub legacy: Vec<Value>,
    pub unknown: Vec<Value>,
    pub broken: Vec<Value>,
}

/// Which bucket one row lands in — decided from the vocabulary's read
/// of the row, before the rows themselves move.
#[derive(Clone, Copy)]
enum Bucket {
    /// A current row whose payload decodes.
    Typed,
    /// An old-dialect row the current vocabulary has no counterpart
    /// for: history this build still reads.
    Legacy,
    /// A tag this build does not know: version skew, not history.
    Unknown,
    /// A row this build cannot read.
    Broken,
}

/// The four buckets one session's rows fall into.
#[derive(Default)]
struct ClassifiedRows {
    /// The rows that are current rows and decode.
    typed: Vec<uva::session::SessionEvent>,
    /// The old-dialect rows the current vocabulary has no
    /// counterpart for: history this build still reads.
    legacy: Vec<Value>,
    /// The rows whose tag this build does not know: version skew.
    unknown: Vec<Value>,
    /// The rows this build cannot read.
    broken: Vec<Value>,
}

/// The session's rows, split by what the row vocabulary says they are.
///
/// Four buckets, one per answer the row vocabulary gives: the rows
/// that are **current** rows and decode (`typed`), the old-dialect
/// rows the current vocabulary has no counterpart for (`legacy` —
/// history this build still reads), the rows whose tag this build
/// does not know (`unknown` — version skew, not history), and the
/// rows this build cannot read (`broken`).
///
/// The read is one restore per row: the vocabulary turns any row
/// into a current row, and the row's own answer — its tag, or why
/// it is not one — picks the bucket. A row written in the old
/// dialect is a **current** row after the restore, so it lands in
/// `typed` with its restored event; the split no longer says
/// "history" about a row the current vocabulary can read.
///
/// The split used to be two buckets: the typed rows and everything
/// else, all of it called `legacy`. That name said "history" about a
/// row a newer build wrote.
///
/// The route **reports** rows, so it does not apply the fail-closed
/// check a reader that rebuilds a session from the log applies
/// ([`uva::session::Rows::read`]): a row this build does not know
/// is exactly what the `unknown` bucket exists to show.
fn classify_rows(raw: Vec<Value>) -> ClassifiedRows {
    // The vocabulary reads the borrowed rows: each row's bucket and,
    // for a current row, its decoded event. The rows move into their
    // buckets afterwards, because the read borrows the slice.
    let mut buckets = Vec::with_capacity(raw.len());
    let mut events: Vec<Option<uva::session::SessionEvent>> =
        Vec::with_capacity(raw.len());
    for value in &raw {
        let row = restore_row(value);
        let (bucket, event) = match row.defect() {
            // A tag this build does not know: version skew, not
            // history. The row is reported as it was written.
            Some(RowDefect::UnknownTag(_)) => (Bucket::Unknown, None),
            // An old dialect with no counterpart in the current
            // vocabulary: history, reported as it was written.
            Some(RowDefect::NoCounterpart) => (Bucket::Legacy, None),
            // No usable type at all: this build cannot read the row.
            Some(RowDefect::NoTag) => (Bucket::Broken, None),
            // A current row. It decodes into an event, or it is a
            // defect: the payload does not hold up.
            None => match row.event() {
                Some(event) => (Bucket::Typed, Some(event)),
                None => (Bucket::Broken, None),
            },
        };
        buckets.push(bucket);
        events.push(event);
    }
    let mut rows = ClassifiedRows::default();
    for ((value, bucket), event) in raw.into_iter().zip(buckets).zip(events) {
        match bucket {
            Bucket::Typed => rows.typed.extend(event),
            Bucket::Legacy => rows.legacy.push(value),
            Bucket::Unknown => rows.unknown.push(value),
            Bucket::Broken => rows.broken.push(value),
        }
    }
    rows
}

/// Returns the session's typed event stream (`SessionEvent`) as a
/// JSON array. Unlike `GET /sessions/{id}` which returns the
/// opaque `history` (mixed legacy + typed), this endpoint
/// exclusively returns the rows the row vocabulary reads as current
/// events — a row written in the old dialect is restored to one
/// first, so it answers here too. Every other row is reported in
/// its own bucket, so the frontend can still surface them in the UI
/// and can tell an old dialect the current vocabulary cannot read
/// (`legacy`) from a tag this build does not know (`unknown`).
pub async fn list_session_events(
    state: Arc<AppState>,
    id: String,
) -> Result<TypedEventResponse, RpcError> {
    validate_resource_name(&id)?;
    if state.session_manager.get(&id).await.is_none() {
        return Err(RpcError::from(Error::not_found(format!(
            "session '{id}'"
        ))));
    }
    let user_id = state.default_user_id().to_string();
    let sink = state.session_manager.sink(&user_id, &id);
    let raw = sink
        .read()
        .await
        .map_err(|e| Error::session(format!("{e}")))?;
    let rows = classify_rows(raw);
    Ok(TypedEventResponse {
        id,
        typed_count: rows.typed.len(),
        legacy_count: rows.legacy.len(),
        unknown_count: rows.unknown.len(),
        broken_count: rows.broken.len(),
        typed: rows.typed,
        legacy: rows.legacy,
        unknown: rows.unknown,
        broken: rows.broken,
    })
}
/// `GET /api/v1/sessions/{id}/status` — live operation status
/// via [`uva::session::OperationSnapshot`].
///
/// R10-5 / R12 closure. Returns the most recent snapshot the
/// controller published (`Running` / `Completing` / `Cancelled`),
/// or `404` when the session is unknown / has never run. The
/// /status route replaces the free-form `infer_status` string
/// for clients that want the typed state + iteration index +
/// token usage instead of a label.
pub async fn session_status(
    state: Arc<AppState>,
    id: String,
) -> Result<uva::session::OperationSnapshot, RpcError> {
    use uva::core::Error;
    validate_resource_name(&id)?;
    let user_id = state.default_user_id().to_string();
    let controller = state.active_sessions.get(&(user_id.clone(), id.clone()));
    let controller = match controller {
        Some(c) => c,
        None => {
            return Err(RpcError::from(Error::not_found(format!(
                "session '{id}'"
            ))));
        }
    };
    let (_rx, latest) = controller.subscribe_snapshots();
    match latest {
        Some(snap) => Ok(snap),
        None => Err(RpcError::from(Error::not_found(format!(
            "no snapshot yet for session '{id}'"
        )))),
    }
}

/// `DELETE /api/v1/sessions/{id}` — remove a session's durable
/// transcript and forget it in the in-process registry.
///
/// Deletion is **lifecycle policy**, not a sink primitive: the
/// session crate keeps [`SessionSink`](uva::session::SessionSink)
/// inert (five methods, no `delete`), so *when* a session may vanish
/// and *where* its bytes live are answered here.
///
/// - `204 No Content` — the transcript is gone.
/// - `404 Not Found` — neither the registry nor the durable store
///   knows the id.
/// - `409 Conflict` (`session_busy`) — a turn is running. Cancel it
///   first (`POST /api/v1/chat/sessions/{id}/cancel`): deleting
///   mid-turn would strand the run's sink mid-append and lose the
///   events it is still writing.
///
/// A registered session's controller is closed *before* the files go,
/// so nothing recreates the directory by appending to a deleted path,
/// and the controller is dropped from the active map so a later
/// `get_or_create` does not hand back a closed one. The session
/// search index needs no surgery: it rebuilds per query from the
/// durable tree (see [`session_search`](super::session_search)), so
/// removing the directory is enough for a deleted conversation to
/// stop matching.
pub async fn delete_session(
    state: Arc<AppState>,
    id: String,
) -> Result<(), RpcError> {
    validate_resource_name(&id)?;
    let user_id = state.default_user_id().to_string();
    let key = (user_id.clone(), id.clone());

    // Clone the handle out of the map before awaiting: `close()` is
    // async and a `DashMap` guard must not be held across it.
    let active = state
        .active_sessions
        .get(&key)
        .map(|entry| entry.value().clone());
    if let Some(controller) = active {
        let (_rx, latest) = controller.subscribe_snapshots();
        if latest
            .as_ref()
            .is_some_and(|snap| turn_in_flight(&snap.state))
        {
            return Err(RpcError::from(Error::internal(format!(
                "session '{id}' is running"
            )))
            .with_code("session_busy")
            .with_message(format!(
                "session '{id}' is running; cancel it before deleting \
                 (POST /api/v1/chat/sessions/{id}/cancel)"
            )));
        }
        controller
            .close()
            .await
            .map_err(|e| Error::session(format!("close session: {e}")))?;
        // The controller and its cold-open gate leave together: the
        // gate belongs to the controller, so a re-created session of
        // the same name must not queue behind the deleted one's gate,
        // and the map must not keep one entry per session this process
        // ever opened.
        state.forget_session(&key);
    }

    let registered = state.session_manager.remove(&id).await.is_some();
    let Some(dir) =
        transcript_path(state.session_manager.sessions_root(), &user_id, &id)
            .and_then(|path| path.parent().map(Path::to_path_buf))
    else {
        return Err(RpcError::from(Error::not_found(format!(
            "session '{id}'"
        ))));
    };
    let removed = tokio::task::spawn_blocking(move || {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    })
    .await
    .map_err(|e| Error::session(format!("join delete: {e}")))?
    .map_err(|e| Error::session(format!("delete session dir: {e}")))?;

    if removed || registered {
        Ok(())
    } else {
        Err(RpcError::from(Error::not_found(format!("session '{id}'"))))
    }
}

/// True while a turn still owns its log: `Running` (executing) or
/// `Completing` (flushing its final events).
///
/// Deliberately not [`OperationState::is_terminal`]
/// (uva::session::OperationState::is_terminal), which counts
/// `Completing` as terminal: the state machine has stopped deciding,
/// but the run is still appending during the flush, so its files must
/// not move underneath it.
fn turn_in_flight(state: &uva::session::OperationState) -> bool {
    matches!(
        state,
        uva::session::OperationState::Running { .. }
            | uva::session::OperationState::Completing
    )
}

/// `GET /api/v1/sessions/{id}/export` — the durable transcript as
/// newline-delimited JSON.
///
/// The raw rows in append order, byte-faithful to what the sink
/// wrote, so the artifact can be archived, diffed against another
/// deployment's log, or re-imported without a projection step. This
/// is the *file*; [`get_session`]'s `history` is the projection
/// (typed classification, artifacts lifted out, compaction
/// checkpoints applied).
///
/// `404` when neither the registry nor the durable store knows the
/// id. A registered session with an empty log exports zero rows —
/// "nothing written yet" is not "unknown id".
pub async fn export_session(
    state: Arc<AppState>,
    id: String,
) -> Result<SessionExport, RpcError> {
    validate_resource_name(&id)?;
    let user_id = state.default_user_id().to_string();
    let Some(path) =
        transcript_path(state.session_manager.sessions_root(), &user_id, &id)
    else {
        return Err(RpcError::from(Error::not_found(format!(
            "session '{id}'"
        ))));
    };
    let registered = state.session_manager.get(&id).await.is_some();
    let lines = tokio::task::spawn_blocking(move || transcript_lines(&path))
        .await
        .map_err(|e| Error::session(format!("join export: {e}")))?;
    if lines.is_empty() && !registered {
        return Err(RpcError::from(Error::not_found(format!(
            "session '{id}'"
        ))));
    }
    let mut body = lines.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    // The artifact travels as base64: the RPC boundary has no
    // response headers, so the client is told the filename and the
    // media type beside the bytes. `id` passed
    // `validate_resource_name` above (no quote, separator or NUL), so
    // it cannot escape the filename.
    Ok(SessionExport {
        filename: format!("{id}.jsonl"),
        content_type: "application/x-ndjson".to_string(),
        content_base64: base64::engine::general_purpose::STANDARD
            .encode(body.as_bytes()),
    })
}

/// The artifact of `sessions/export`.
///
/// The transcript is newline-delimited JSON; the bytes travel
/// base64-encoded because the RPC boundary carries JSON values only.
#[derive(Debug, Serialize)]
pub struct SessionExport {
    /// The file name a client should save the artifact under.
    pub filename: String,
    /// The media type of the artifact.
    pub content_type: String,
    /// The artifact's bytes, base64-encoded.
    pub content_base64: String,
}

/// Body of `POST /api/v1/sessions/{id}/fork`.
#[derive(Debug, Deserialize, validator::Validate)]
pub struct ForkSessionRequest {
    /// Fork as of this stream index: the child inherits every row up
    /// to and including it. Omitted → the whole durable log.
    #[serde(default)]
    pub from_stream_index: Option<u64>,
}

/// Response of `POST /api/v1/sessions/{id}/fork`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ForkSessionResponse {
    /// The new session — addressable, listable and streamable exactly
    /// like any other conversation.
    pub session_id: String,
    /// The session it was branched from.
    pub forked_from: String,
    /// Durable rows the child inherited.
    pub rows: usize,
}

/// `POST /api/v1/sessions/{id}/fork` — branch a session.
///
/// The child starts as a copy of the parent's durable transcript up to
/// `from_stream_index` (the whole log when omitted) and then diverges:
/// `POST /api/v1/chat/sessions` registers it through the same path, so
/// it is an ordinary session that can be continued, streamed and
/// deleted like any other, while the parent is left untouched. That is
/// the "branch" half of pi's `Navigation` operation; *rewind*
/// (rewriting the parent's own log in place) is a different, more
/// destructive operation and is not part of this route.
/// Rows are copied **through the child's writer** in one durable
/// batch: the writer is the session's only durable write path, it
/// re-stamps every inherited row with the ordinal the **child's**
/// counter hands out (the parent's numbers never cross into the
/// child's log), it keeps each row's `ts` (a fork's `created_at` is
/// the conversation's first event, not the fork), and it remaps the
/// `surface_op` ordinals a copied compaction cites so the citations
/// keep naming the rows they shadow. The copy is one durable barrier:
/// the sink writes the batch or reports the failure, and a refused
/// batch goes back to the writer's spill under the same ordinals. (A
/// backend whose `fsync` fails after its bytes were written can leave
/// them on disk, so the retry may write a row twice.) One barrier for
/// a 27 086-row parent completes in well under a second, where the
/// per-row `fsync` path took 63.8 s.
///
/// The provenance is this response: the child's log is a copy, not a
/// reference, so nothing in it names the parent. A durable
/// `session_fork` row needs the typed event taxonomy to grow one
/// first; until then, callers that care keep the ids.
///
/// `404` when neither the registry nor the durable store knows the
/// parent — forking nothing is not an empty session.
pub async fn fork_session(
    state: Arc<AppState>,
    id: String,
    req: ForkSessionRequest,
) -> Result<ForkSessionResponse, RpcError> {
    validate_resource_name(&id)?;
    let user_id = state.default_user_id().to_string();
    let Some(parent_path) =
        transcript_path(state.session_manager.sessions_root(), &user_id, &id)
    else {
        return Err(RpcError::from(Error::not_found(format!(
            "session '{id}'"
        ))));
    };
    let registered = state.session_manager.get(&id).await.is_some();
    let lines =
        tokio::task::spawn_blocking(move || transcript_lines(&parent_path))
            .await
            .map_err(|e| Error::session(format!("join fork read: {e}")))?;
    if lines.is_empty() && !registered {
        return Err(RpcError::from(Error::not_found(format!(
            "session '{id}'"
        ))));
    }
    let inherited = inherited_rows(&lines, req.from_stream_index);

    let inherited_count = inherited.len();

    let child_id = Uuid::new_v4().to_string();
    state
        .get_or_create_session_controller(&user_id, &child_id)
        .await?;
    let child = state.session_manager.writer(&user_id, &child_id);
    if let Err(e) = child.append_copied(inherited).await {
        // Nothing landed — the batch is all-or-nothing — so the
        // child is not a session: undo the registration the
        // controller path made rather than leave it pointing at an
        // empty log.
        state
            .active_sessions
            .remove(&(user_id.clone(), child_id.clone()));
        let _ = state.session_manager.remove(&child_id).await;
        return Err(RpcError::from(Error::session(format!(
            "fork append: {e}"
        ))));
    }

    Ok(ForkSessionResponse {
        session_id: child_id,
        forked_from: id,
        rows: inherited_count,
    })
}

/// The rows a fork inherits: every **event** row whose stream index
/// is at or below `from`, or all of them when `from` is `None`.
///
/// A row's stream index is the stored `stream_index` field when
/// present, else its 1-based ordinal — the same fallback
/// `read_from_index` documents for logs written before the field
/// existed (and for the legacy rows a rebuilt child may still hold).
///
/// The parent's metadata header is **dropped**, not copied. A fork
/// copies a parent's events; the parent's schema header describes the
/// parent's own file (`{"_meta":{"id":"<parent>"}}`) and the child
/// writes its own header on first append. Copying it would land a
/// header-shaped row *mid-log* in the child, where
/// `JsonlSessionSink::read_filtered` — which strips only the first
/// line's header — would parse it as a phantom event and inflate
/// `last_event_seq` by one. Dropping it up front also keeps the
/// ordinal fallback aligned: the sink's seq numbering skips the
/// header, so leaving it in would shift every fallback ordinal by one.
fn inherited_rows(lines: &[String], from: Option<u64>) -> Vec<Value> {
    let parsed: Vec<Value> = lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|row| !is_metadata_header_row(row))
        .collect();
    let Some(from) = from else {
        return parsed;
    };
    parsed
        .into_iter()
        .enumerate()
        .filter(|(ordinal, row)| {
            let index = row
                .get("stream_index")
                .and_then(Value::as_u64)
                .unwrap_or(*ordinal as u64 + 1);
            index <= from
        })
        .map(|(_, row)| row)
        .collect()
}

#[cfg(test)]
mod tests;
