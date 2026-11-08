//! In-memory registry that owns `SessionSink` instances.
//!
//! After the panel/session refactor each session is identified
//! by `(user_id, session_id)` and persisted through a
//! [`SessionSink`] rooted at
//! `<sessions_root>/<user_id>/<session_id>/`. The registry:
//!
//! 1. Eagerly creates the sink when a session is created, so
//!    the on-disk directory exists before the controller
//!    starts appending events.
//! 2. Hands out the same `Arc<dyn SessionSink>` for repeated
//!    lookups of the same session (cheap clones).
//! 3. Hands out the same [`LogWriter`] for repeated lookups of the
//!    same session: one session has one writer, so one ordinal
//!    source and one flush cadence.
//!
//! `SessionRegistry` is intentionally minimal: it does NOT
//! store state machines, budgets, approvals, or any policy
//! concerns. Those live in `elysper-server::session::SessionController`.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use elysper_core::{
    Clock,
    SystemClock,
    blocking::{Blocking, ThreadBlocking},
};
use parking_lot::RwLock;

use crate::{LogWriter, SessionError, SessionSink, jsonl::JsonlSessionSink};

// ---------------------------------------------------------------------------
// Session metadata
// ---------------------------------------------------------------------------

/// Minimal session metadata kept in memory. After the refactor
/// the only fields server callers still consult are `id` and
/// `user_id`; everything else lives in the sink.
#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub user_id: String,
}

// ---------------------------------------------------------------------------
// SessionRegistry
// ---------------------------------------------------------------------------

/// Owns the per-session sink registry and the in-memory session
/// index.
pub struct SessionRegistry {
    sessions_root: PathBuf,
    sessions: RwLock<HashMap<String, Session>>,
    sinks: RwLock<HashMap<String, Arc<dyn SessionSink>>>,
    /// One writer per session, next to its sink: the writer stamps
    /// the rows' ordinals and batches their flush, and a session's
    /// rows must all go through one of them.
    writers: RwLock<HashMap<String, Arc<LogWriter>>>,
    /// The clock every row's timestamp comes from.
    clock: Arc<dyn Clock>,
    /// The blocking seam every sink this registry creates runs its fs
    /// work on. The default is the runtime-free
    /// [`ThreadBlocking`](elysper_core::blocking::ThreadBlocking);
    /// [`Self::with_blocking`] installs a runtime's own adapter.
    blocking: Arc<dyn Blocking>,
}

impl SessionRegistry {
    /// Build a registry rooted at `sessions_root`.
    pub fn new(sessions_root: PathBuf) -> Self {
        Self {
            sessions_root,
            sessions: RwLock::new(HashMap::new()),
            sinks: RwLock::new(HashMap::new()),
            writers: RwLock::new(HashMap::new()),
            clock: Arc::new(SystemClock),
            blocking: Arc::new(ThreadBlocking),
        }
    }

    /// Install the blocking seam every sink this registry creates uses.
    ///
    /// The registry owns the sink factory, so the seam a host installs
    /// here is the one every session's fs work runs on: a tokio host
    /// passes its own adapter (the server installs `ServerBlocking`,
    /// `elysper-harness-tokio` ships `TokioBlocking`) so appends land
    /// on the runtime's blocking pool instead of a fresh OS thread per
    /// write.
    #[must_use]
    pub fn with_blocking(mut self, blocking: Arc<dyn Blocking>) -> Self {
        self.blocking = blocking;
        self
    }

    /// Path of the sessions root.
    pub fn sessions_root(&self) -> &std::path::Path {
        &self.sessions_root
    }

    /// Get-or-create the on-disk sink for
    /// `(user_id, session_id)`. The sink is the **only**
    /// durable source of truth after the refactor.
    ///
    /// Uses a single `write()` lock with `entry().or_insert_with(...)`
    /// so concurrent first-lookups for the same session always
    /// converge on the same sink. The previous version used
    /// double-checked locking on a `RwLock` which had a race
    /// where two threads could each create (and register)
    /// different sinks for the same key.
    pub fn sink(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Arc<dyn SessionSink> {
        let key = sink_key(user_id, session_id);
        self.sinks
            .write()
            .entry(key)
            .or_insert_with(|| {
                let dir = self.sessions_root.join(user_id).join(session_id);
                Arc::new(
                    JsonlSessionSink::new(session_id, dir)
                        .with_blocking(Arc::clone(&self.blocking)),
                )
            })
            .clone()
    }

    /// The attribute sits **above** this doc block on purpose: the
    /// public-docs gate reads the nearest preceding non-empty line
    /// to decide whether a public item is documented, so an
    /// attribute *between* the doc and the declaration would read as
    /// an undocumented item.
    #[doc(hidden)]
    /// Install a caller-built sink for a session, replacing the
    /// cached one.
    ///
    /// Hidden: this is a test seam, not product surface. The registry
    /// owns the sink factory, so a host that needs to observe a
    /// session's reads (a test counting whole-log reads behind an
    /// HTTP route) wraps the sink it would have built and installs
    /// the wrapper here. Any cached writer is dropped first: one
    /// session has one writer, and a writer holding the replaced
    /// sink would be a second sink for the same log. A caller that
    /// installs a second sink over a session the registry already
    /// handed out takes the hazard with it — the old `Arc` keeps
    /// writing to the same log through a different instance, and the
    /// two instances' in-memory state (idempotency LRU, ordinal
    /// seed) no longer agree.
    pub fn install_sink(
        &self,
        user_id: &str,
        session_id: &str,
        sink: Arc<dyn SessionSink>,
    ) {
        let key = sink_key(user_id, session_id);
        // The writer first: `writer()` takes the writers lock and
        // then the sinks lock, and the two must not be taken the
        // other way round.
        self.writers.write().remove(&key);
        self.sinks.write().insert(key, sink);
    }

    /// The session's one writer: the only thing that stamps a row's
    /// ordinal and the only thing that flushes it.
    ///
    /// Cached per session, next to [`Self::sink`], and keyed the
    /// same way: one session has one writer. Two writers over one
    /// sink would each hold their own queue and their own flush
    /// timing, so the rows they write could reach the log in an
    /// order their ordinals do not match — and a compaction that
    /// cites a row's ordinal would then cite a number the reader
    /// cannot reproduce.
    pub fn writer(&self, user_id: &str, session_id: &str) -> Arc<LogWriter> {
        let key = sink_key(user_id, session_id);
        self.writers
            .write()
            .entry(key)
            .or_insert_with(|| {
                Arc::new(LogWriter::new(
                    self.sink(user_id, session_id),
                    Arc::clone(&self.clock),
                ))
            })
            .clone()
    }

    /// Register a new session. Inserts the metadata into the
    /// in-memory index and eagerly creates the sink so the
    /// on-disk directory exists before the controller starts
    /// appending events.
    pub async fn create_with_user(
        &self,
        id: String,
        user_id: String,
    ) -> Result<Session, SessionError> {
        validate_id(&user_id, "user_id")?;
        validate_id(&id, "session_id")?;
        let session = Session {
            id: id.clone(),
            user_id: user_id.clone(),
        };
        self.sessions.write().insert(id.clone(), session.clone());
        // Touch the sink so the dir is created eagerly.
        let _ = self.sink(&user_id, &id);
        Ok(session)
    }

    /// Look up a session by id.
    pub async fn get(&self, id: &str) -> Option<Session> {
        self.sessions.read().get(id).cloned()
    }

    /// Drop a session from the in-memory index.
    ///
    /// Returns the session that was registered, if any, and removes
    /// the id → [`Session`] entry, the id → sink entry, and the id →
    /// writer entry, so a later [`sink`](Self::sink) or
    /// [`writer`](Self::writer) call for the same id cannot hand
    /// back a handle to a removed session's log.
    ///
    /// This is the registry's own bookkeeping, not deletion policy:
    /// *when* a session may be removed and *what* happens to its
    /// durable files are the caller's decision (in this workspace,
    /// `elysper-server`'s `delete_session`).
    pub async fn remove(&self, id: &str) -> Option<Session> {
        let session = self.sessions.write().remove(id)?;
        let key = sink_key(&session.user_id, id);
        // The writer first: `writer()` takes the writers lock and
        // then the sinks lock, and the two must not be taken the
        // other way round.
        self.writers.write().remove(&key);
        self.sinks.write().remove(&key);
        Some(session)
    }

    /// Snapshot the in-memory index of registered sessions.
    /// Used by `GET /api/v1/chat/sessions` so the REST chat
    /// surface can enumerate recent conversations without
    /// touching the on-disk sink.
    pub async fn list_all(&self) -> Result<Vec<Session>, SessionError> {
        Ok(self.sessions.read().values().cloned().collect::<Vec<_>>())
    }
}

/// Compose the internal key for the sinks registry.
fn sink_key(user_id: &str, session_id: &str) -> String {
    format!("{user_id}::{session_id}")
}

/// Validate a `user_id` or `session_id` before it is joined
/// into a filesystem path. The `contextId`/`taskId` flows
/// straight from the HTTP body into `create_with_user` and
/// then into the sink path; without this check a hostile
/// caller could craft `session_id = "../other_user/..."` to
/// escape the sessions root or land on a sibling session's
/// JSONL.
///
/// Rejects:
/// - empty strings
/// - any path separator (`/`, `\`)
/// - any NUL byte (truncates paths on some platforms)
/// - `..` segments (parent-directory escape)
/// - leading `.` (hidden directory convention; not a security
///   boundary on its own but a strong signal of misuse)
///
/// Returns `Ok(())` on a safe identifier.
fn validate_id(id: &str, kind: &str) -> Result<(), SessionError> {
    if id.is_empty() {
        return Err(SessionError::Invalid(format!("empty {kind}")));
    }
    if id.contains('/') || id.contains('\\') || id.contains('\0') {
        return Err(SessionError::Invalid(format!(
            "{kind} contains a path separator or NUL byte"
        )));
    }
    if id == "." || id == ".." || id.starts_with('.') {
        return Err(SessionError::Invalid(format!(
            "{kind} contains a relative-path segment"
        )));
    }
    Ok(())
}

/// Legacy constant: the user_id used for single-tenant
/// deployments. Preserved so `elysper-server` auth middleware
/// keeps compiling.
pub const SERVER_DEFAULT_USER_ID: &str = "dev";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::Builder;

    use super::*;

    fn temp_dir() -> PathBuf {
        let tmp = Builder::new().disable_cleanup(true).tempdir().unwrap();
        let p = tmp.path().to_path_buf();
        drop(tmp);
        p
    }

    #[tokio::test]
    async fn registry_yields_same_sink_for_same_session_id() {
        let root = temp_dir();
        let r = SessionRegistry::new(root);
        let a = r.sink("alice", "s1");
        let b = r.sink("alice", "s1");
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn registry_yields_distinct_sinks_for_distinct_sessions() {
        let root = temp_dir();
        let r = SessionRegistry::new(root);
        let a = r.sink("alice", "s1");
        let b = r.sink("alice", "s2");
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn registry_sink_writes_to_disk() {
        let root = temp_dir();
        let r = SessionRegistry::new(root);
        let sink = r.sink("alice", "s1");
        sink.alloc_seq();
        sink.append(&json!({"role": "user", "text": "hi"}))
            .await
            .unwrap();
        let events = sink.read().await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["text"], "hi");
    }

    /// One session has one writer: the ordinal source and the flush
    /// cadence belong to the session, not to the caller that happens
    /// to hold a handle.
    #[tokio::test]
    async fn registry_yields_same_writer_for_same_session_id() {
        let root = temp_dir();
        let r = SessionRegistry::new(root);
        let a = r.writer("alice", "s1");
        let b = r.writer("alice", "s1");
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn registry_yields_distinct_writers_for_distinct_sessions() {
        let root = temp_dir();
        let r = SessionRegistry::new(root);
        let a = r.writer("alice", "s1");
        let b = r.writer("alice", "s2");
        assert!(!Arc::ptr_eq(&a, &b));
    }

    /// The writer writes to the session's own sink: the registry's
    /// sink and its writer are two views of one log.
    #[tokio::test]
    async fn the_session_writer_writes_to_the_session_sink() {
        let root = temp_dir();
        let r = SessionRegistry::new(root);
        let writer = r.writer("alice", "s1");
        writer.append(json!({"type": "Model"})).await.unwrap();
        writer.flush().await.unwrap();
        let rows = r.sink("alice", "s1").read().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["seq"], 1, "the writer stamped the row");
    }

    /// A removed session's writer goes with it: a later lookup must
    /// not hand back a handle to the removed session's queue.
    #[tokio::test]
    async fn remove_drops_the_session_writer() {
        let root = temp_dir();
        let r = SessionRegistry::new(root);
        r.create_with_user("s1".into(), "alice".into())
            .await
            .unwrap();
        let first = r.writer("alice", "s1");
        r.remove("s1").await.unwrap();
        let second = r.writer("alice", "s1");
        assert!(
            !Arc::ptr_eq(&first, &second),
            "the writer is dropped with the session"
        );
    }

    #[tokio::test]
    async fn create_with_user_populates_index_and_sink() {
        let root = temp_dir();
        let r = SessionRegistry::new(root);
        let s = r
            .create_with_user("s1".into(), "alice".into())
            .await
            .unwrap();
        assert_eq!(s.id, "s1");
        assert_eq!(s.user_id, "alice");
        let got = r.get("s1").await.unwrap();
        assert_eq!(got.user_id, "alice");
    }

    #[tokio::test]
    async fn create_with_user_rejects_empty_user_id() {
        let root = temp_dir();
        let r = SessionRegistry::new(root);
        let err = r
            .create_with_user("s1".into(), "".into())
            .await
            .unwrap_err();
        assert_eq!(err, SessionError::Invalid("empty user_id".into()));
    }

    #[tokio::test]
    async fn create_with_user_rejects_path_traversal_in_session_id() {
        let root = temp_dir();
        let r = SessionRegistry::new(root);
        for bad in [
            "../other",
            "..",
            ".",
            ".hidden",
            "foo/bar",
            "foo\\bar",
            "with\0nul",
        ] {
            let err = r
                .create_with_user(bad.into(), "alice".into())
                .await
                .unwrap_err();
            assert!(
                matches!(err, SessionError::Invalid(_)),
                "expected Invalid for session_id={bad:?}; got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn create_with_user_rejects_path_traversal_in_user_id() {
        let root = temp_dir();
        let r = SessionRegistry::new(root);
        for bad in ["../other", "..", "foo/bar", "with\0nul"] {
            let err = r
                .create_with_user("s1".into(), bad.into())
                .await
                .unwrap_err();
            assert!(
                matches!(err, SessionError::Invalid(_)),
                "expected Invalid for user_id={bad:?}; got {err:?}"
            );
        }
    }
}
