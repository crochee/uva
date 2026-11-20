//! The session-status module — the one place that answers
//! «what state is this session in».
//!
//! Two vocabularies live here, and they are not the same thing:
//!
//! - [`SessionState`] — the **controller state**: the lifecycle a
//!   session controller keeps in memory (idle / running / awaiting
//!   approval / cancelled). The controller writes it; this module
//!   reads it.
//! - [`SessionStatus`] — the **session status**: what a session
//!   reports to the outside (`unspecified` / `working` /
//!   `input_required` / `canceled` / `completed`). It is the
//!   controller state's refinement plus one fact the controller does
//!   not keep — whether the log holds any row at all. It is the wire
//!   type of that field: the variant name is the wire spelling.
//!
//! ## One rule
//!
//! Waiting for approval is a fact about the **log**, not about a
//! memory flag: a process that restarts must report the same state as
//! the one that suspended. So the open-approval set wins, and a
//! controller whose flag says «awaiting approval» while the log holds
//! no open ask is **stale** — it is demoted to idle instead of being
//! reported as a session that needs input.
//!
//! ## Facts, not I/O
//!
//! The module is a pure function over facts the caller has already
//! gathered ([`StatusFacts`]). Folding the open approvals is the
//! caller's job — the window each caller can prove differs (the
//! listing reads a bounded tail, `follow` widens until it can prove
//! it holds the whole suspended turn) — and so is the error policy: a
//! caller that cannot read the log reports what it knows and says so.
//!
//! ## One vocabulary on the wire
//!
//! Every wire surface that reports a session's state reports it as a
//! [`SessionStatus`] — the session listing, the session detail and the
//! `follow` header alike. The `follow` header used to speak its own
//! four-label vocabulary (`idle` / `running` / `awaiting_approval` /
//! `cancelled`); retiring it means a client asking «what state is
//! this session in» gets one answer, not two. [`SessionState`] stays
//! inside this module: it is the controller's own lifecycle, and it
//! never reaches the wire.

// Allow `result_large_err`: `parse_status_filter` returns the
// un-boxed `uva::core::Error` (≥128 bytes once the RFC-0977
// common fields are counted) — same accepted trade-off as
// `api/v1/cursor.rs` and `api/v1/validation.rs`; boxing would
// force `.map_err(|e| *e)` at every call site.
#![allow(clippy::result_large_err)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uva::core::Error;

/// The controller state — the lifecycle a session controller keeps in
/// memory.
///
/// The controller writes it (six transitions) and this module reads
/// it. It never reaches the wire: the wire carries the
/// [`SessionStatus`], which is this state refined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// No run is in flight.
    Idle,
    /// A run is executing.
    Running,
    /// A suspended turn is waiting for approval decisions.
    AwaitingApproval,
    /// The last run was cancelled.
    Cancelled,
}

/// The session status — what a session reports to the outside.
///
/// It is the controller state refined: a session with no log has
/// never run, and one whose log holds rows has run at least once.
///
/// The **variant name is the wire spelling**, the same rule the row
/// vocabulary follows (ADR-0006): `rename_all = "snake_case"` derives
/// it, so the name and the string cannot drift apart.
/// [`SessionStatus::as_wire`] is the hand-written mirror of that
/// derivation — the pattern the vocabulary uses for `RowTag::as_wire`
/// — and a test pins the two together.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// The log holds no row: this session never ran.
    Unspecified,
    /// A run is in flight.
    Working,
    /// The session needs input from the user — an approval decision.
    InputRequired,
    /// The last run was cancelled.
    Canceled,
    /// The session ran and finished.
    Completed,
}

/// The facts a caller has already gathered.
///
/// The module folds no rows: `awaiting` is the caller's verdict on
/// the log's open approvals, and the caller knows which window it
/// could prove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusFacts {
    /// The controller's in-memory state. `None` when this process
    /// holds no controller for the session — a transcript that only
    /// exists on disk.
    pub controller: Option<SessionState>,
    /// The log holds an approval nobody answered yet.
    pub awaiting: bool,
}

/// The controller state, with the log's verdict applied.
///
/// A running session is running: the in-memory flag means a run is
/// executing right now, which outranks the log's open ask. Otherwise
/// the log decides — an open approval means the session is waiting,
/// and a controller whose flag says «awaiting approval» while the log
/// holds no open ask is stale, so it is demoted to idle.
#[must_use]
pub fn controller_state(facts: StatusFacts) -> SessionState {
    match facts.controller {
        Some(SessionState::Running) => SessionState::Running,
        _ if facts.awaiting => SessionState::AwaitingApproval,
        Some(SessionState::AwaitingApproval) => SessionState::Idle,
        Some(other) => other,
        None => SessionState::Idle,
    }
}

/// The session status a surface reports.
///
/// `has_log` is the fact the controller does not keep: a session
/// whose log holds no row has never run, and one whose log holds
/// rows has run at least once.
#[must_use]
pub fn session_status(facts: StatusFacts, has_log: bool) -> SessionStatus {
    match controller_state(facts) {
        SessionState::Running => SessionStatus::Working,
        _ if !has_log => SessionStatus::Unspecified,
        SessionState::AwaitingApproval => SessionStatus::InputRequired,
        SessionState::Cancelled => SessionStatus::Canceled,
        SessionState::Idle => SessionStatus::Completed,
    }
}

impl SessionStatus {
    /// The wire spelling the session listing, the session detail and
    /// the `follow` header carry.
    ///
    /// The hand-written mirror of the enum's own `snake_case`
    /// derivation — the pattern the row vocabulary uses for
    /// `RowTag::as_wire`. A test pins the two together, so a renamed
    /// variant cannot leave one spelling behind. The listing's filter
    /// and its sort read it: the filter arrives as a string, and the
    /// sort has always ordered by the spelling the client reads.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            SessionStatus::Unspecified => "unspecified",
            SessionStatus::Working => "working",
            SessionStatus::InputRequired => "input_required",
            SessionStatus::Canceled => "canceled",
            SessionStatus::Completed => "completed",
        }
    }
}

/// The status filter labels the session listing accepts.
///
/// A **filter vocabulary**, not a status vocabulary: the statuses the
/// listing produces are five, and four of these nine have no producer
/// today. They stay because the listing accepts them as a query — a
/// typo must not silently return every session — and cutting them
/// would break the callers that filter on them.
const STATUS_FILTER_LABELS: [&str; 9] = [
    "unspecified",
    "submitted",
    "working",
    "completed",
    "failed",
    "canceled",
    "input_required",
    "rejected",
    "auth_required",
];

/// Validate a status filter label.
///
/// # Errors
///
/// Returns an invalid-item error when the label is not one of the nine
/// the listing accepts.
pub fn parse_status_filter(status: &str) -> Result<&'static str, Error> {
    STATUS_FILTER_LABELS
        .into_iter()
        .find(|label| *label == status)
        .ok_or_else(|| Error::invalid_item(format!("status filter '{status}'")))
}

#[cfg(test)]
mod tests;
