//! The session-resolution module: where a session's transcript
//! lives, the named policies that turn a session id into a
//! controller, and the line-parsing primitives that read a
//! durable transcript.
//!
//! Every "does this session exist, and where is its log?" question
//! funnels through here, so no route module holds the disk layout:
//! a route asks this module for a path and picks a policy by name.
//!
//! `must_exist` is the policy a read surface asks for by name: the
//! session must already exist, or the answer is the session-shaped
//! 404. The proof is durable — an entry in the registry this process
//! holds, or the transcript directory on disk — so a restart that
//! emptied the registry still resolves the sessions it owns.
//!
//! `create_on_demand` is the chat surface's own rule, asked for by
//! name: the session is opened for use whether or not it exists,
//! and an id no witness knows is minted — the directory on disk,
//! the registry entry, the controller. Those side effects are the
//! policy's contract, not an accident of the resolve path. On a
//! node that loses the lease, the policy hands back a forwarding
//! handle instead of minting: the session already lives on its
//! owner, and creation is only the owner node's business.
//!
//! Reading a durable transcript is also this module's business:
//! [`transcript_events`] and [`transcript_lines`] are the one pair
//! of line-parsing primitives a route reads a log through, so
//! "where the log lives" and "how its lines are read" answer from
//! the same place — no route reaches into another route for
//! either.

use std::{
    io::{BufRead as _, BufReader},
    path::{Path, PathBuf},
    sync::Arc,
};

use serde_json::Value;
use uva::session::is_metadata_header_row;

use super::AppState;
use crate::{
    api::validate_resource_name,
    session::controller::SessionController,
};

/// Why the `must_exist` policy refused to resolve a session.
#[derive(Debug)]
pub(crate) enum MustExistError {
    /// Neither the registry nor the disk knows the session; the
    /// caller builds the session-shaped not-found from the id.
    Absent {
        /// The session id no witness could vouch for.
        session_id: String,
    },
    /// The session exists, but its controller could not be opened
    /// (restored, repaired, leased).
    Open(uva::core::Error),
}

impl AppState {
    /// Resolves the controller for a session the caller will **use
    /// whether or not it exists yet** — the `create_on_demand`
    /// policy.
    ///
    /// The policy reads what is durable first and creates what is
    /// missing, so an id no witness knows is not refused but
    /// minted: the session's directory on disk, a registry entry,
    /// and the controller itself. Creation is the documented side
    /// effect a caller signs up for by asking for this policy; one
    /// that must not create asks for `must_exist` instead. On a
    /// node that loses the lease, the policy hands back a
    /// forwarding handle instead of minting — the session lives on
    /// its owner, and only the owner creates.
    pub(crate) async fn create_on_demand_session_controller(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> uva::core::Result<Arc<SessionController>> {
        self.get_or_create_session_controller(user_id, session_id)
            .await
    }

    /// Resolves the controller for a session that **must already
    /// exist** — the `must_exist` policy.
    ///
    /// Either witness proves the session: an entry in this process's
    /// registry, or the session's transcript directory on disk. With
    /// neither, the policy refuses the id instead of letting the
    /// resolve path mint a session for a typo.
    pub(crate) async fn must_exist_session_controller(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<Arc<SessionController>, MustExistError> {
        if self.session_manager.get(session_id).await.is_none() {
            // A process that restarted has no registry entry; the
            // durable transcript is the proof the session exists.
            // `get_or_create` below restores it (and repairs a
            // crashed tail) without minting a directory for a typo.
            // A session created a moment ago owns its directory but
            // may not have written a row yet; the directory is the
            // durable proof it exists, and a typo still has neither.
            let exists = transcript_path(
                self.session_manager.sessions_root(),
                user_id,
                session_id,
            )
            .and_then(|path| path.parent().map(|dir| dir.is_dir()))
            .unwrap_or(false);
            if !exists {
                return Err(MustExistError::Absent {
                    session_id: session_id.to_string(),
                });
            }
        }
        self.get_or_create_session_controller(user_id, session_id)
            .await
            .map_err(MustExistError::Open)
    }
}

// ---------------------------------------------------------------------------
// Where a session's transcript lives
// ---------------------------------------------------------------------------

/// `<sessions_root>/<user_id>/<session_id>/events.jsonl`.
///
/// Refuses anything that is not a single directory entry, so a
/// hand-built path can never climb out of the sessions root:
/// `session_id` must match the resource-name regex ids are already
/// validated against (no `.`, `..`, `/` or `\`), and `user_id` must
/// be one path component.
///
/// Reading through this computed path is what keeps a lookup a read:
/// `SessionRegistry::sink` would create the directory for whatever
/// id the client sent, turning the lookup into a write.
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

/// The one directory a user's sessions live in:
/// `<sessions_root>/<user_id>`.
///
/// The same refusal rule as [`transcript_path`] applies: a
/// `user_id` that is not a single directory entry has no directory
/// to hand out, so `None` is the answer a hand-built id gets.
pub(crate) fn user_sessions_dir(
    sessions_root: &Path,
    user_id: &str,
) -> Option<PathBuf> {
    is_path_component(user_id).then(|| sessions_root.join(user_id))
}

/// Whether `name` can only ever address one directory entry:
/// non-empty, neither `.` nor `..`, and free of separators and NUL.
fn is_path_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
}

// ---------------------------------------------------------------------------
// Reading a durable transcript
// ---------------------------------------------------------------------------

/// The parseable events of a durable transcript, in order.
///
/// Blank lines and lines that are not JSON are skipped — the same
/// tolerance [`uva::session::JsonlSessionSearch`] applies when it
/// folds a log, so a torn tail left behind by a crash cannot hide
/// the conversation in front of it. An unreadable or empty log
/// yields no events at all, which is what makes every caller read
/// "no transcript" as "unknown session" rather than "empty
/// session".
///
/// The sink's metadata header is **not** an event and is dropped
/// here, not at the call sites: every caller projects *events*
/// (the sessions listing slices the list into its opening row and
/// tail window; the detail view folds the whole list into
/// `history`; the follow baseline reads the rows its window is
/// cut from), so a header left in would report a disk-only session
/// with no `created_at`, no subagent attribution, and a phantom
/// first history entry. A log holding only a header yields no
/// events, which reads as "unknown session" — correct, since a
/// session that never wrote an event has nothing to render.
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
pub(crate) fn transcript_lines(path: &Path) -> Vec<String> {
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
