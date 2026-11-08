//! `SessionSink` — the **only** public surface of `elysper-session`.
//!
//! ## Surface size
//!
//! The session crate used to expose ~30 public types
//! (`SessionManager`, `Store`, `Session`, `SessionStateMachine`,
//! `SessionInputQueue`, `EventStore`, …) plus state-machine and
//! token-budget modules. That surface bled orchestration policy
//! (approval workflows, status transitions, cache eviction) into a
//! module called "session", which broke the layering between the
//! agent runtime (stateless, streaming) and the session storage
//! (write-through, inert).
//!
//! After the original refactor:
//!
//! - [`SessionSink`] was the only trait the agent runtime imported.
//! - It had exactly **5 methods** (`id`, `append`, `read`,
//!   `snapshot`, `close`). Anything else the caller wanted to do
//!   with a session was **policy** and lived outside this crate
//!   (typically in `elysper-server`).
//! - All on-disk persistence, state machines, approval flows, and
//!   token-budget tracking had been either deleted or moved into the
//!   `elysper-server::session` orchestration layer.
//!
//! The trait gained two **default-implemented** methods in a
//! later round so the same primitive could carry three orthogonal
//! concerns without growing the surface for callers that did not
//! need them:
//!
//! - [`SessionSink::append_with_key`] — idempotent append:
//!   `Some(key)` callers that re-submit the same key write nothing
//!   (no double-write); `None` callers see the legacy
//!   non-idempotent behaviour.
//! - [`SessionSink::append_many`] — batch append with one
//!   durable barrier (`fsync`) at the tail, instead of N. The
//!   default implementation loops over `append` so backends that
//!   cannot batch still satisfy the contract; the production
//!   `JsonlSessionSink` overrides it for a single-`fsync`
//!   fast path (`routes/sessions.rs::fork_session` is the
//!   motivating caller — a per-row `fsync` made a 27 k-row fork
//!   take 63.8 s).
//!
//! Both are default-implemented: a backend that does not care
//! about idempotency or batch durability stays at the original
//! 5-method surface.
//!
//! ## Who numbers a row
//!
//! The sink owns the ordinal counter, but it does not *assign* it:
//! [`SessionSink::alloc_seq`] hands the next ordinal out, and the
//! caller stamps the row with it before
//! [`append`](SessionSink::append) writes it. The producer therefore
//! knows a row's ordinal before the row is durable — which is what
//! lets a compaction row cite the rows it shadows — while every
//! appender still draws from one counter, so two writers on the
//! same session cannot collide.
//!
//! ## Event shape
//!
//! `SessionSink` is event-shape-agnostic: it stores opaque
//! `serde_json::Value` records. Callers (`elysper-harness`) are
//! responsible for serializing their own events before calling
//! `append`. This keeps the dependency direction strictly
//! `agent → session` (never the reverse) and lets the same sink
//! back agents with different event taxonomies (ReAct, planner,
//! …) without trait churn.
//!
//! ## Semantics
//!
//! ### Write-through
//!
//! `append` returns `Ok(())` only when the event is durable (or
//! the implementation has explicit transactional semantics, such
//! as `InMemorySessionSink`). The agent loop MUST treat
//! `Err(SessionError)` as fatal and stop the run — the call site
//! is responsible for retrying at the request boundary, not at
//! the agent step boundary. This matches the `fail-fast` rule
//! agreed with the user.
//!
//! ### Inert container
//!
//! `SessionSink` is a **mechanism**, not a **policy**. It does
//! NOT track approval state, session state-machine transitions,
//! token budgets, or per-user routing. Callers (the
//! `elysper-server::session::SessionController`) own that
//! bookkeeping on top of the sink's primitives.
//!
//! ### Read parity
//!
//! `read()` and `read_from(from)` MUST return records in
//! chronological order, identical to the order they were passed
//! to `append`. Implementations that batch / compress MUST
//! surface that order in `read()` and `read_from()` output. This
//! lets any caller that wants to rehydrate a session under a
//! different agent reconstruct a consistent message stream
//! regardless of the backend. `read_from(N)` returns the suffix
//! of the chronological sequence strictly past event `N`, so a
//! caller resuming from a previously-observed cursor sees only
//! the events it has not yet read.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The single trait every agent loop sees.
///
/// `elysper-harness` holds `Arc<dyn SessionLog>` — the writer it
/// writes through — and never sees concrete backends. This keeps the agent runtime portable and
/// makes test fixtures trivial (`InMemorySessionSink`).
#[async_trait]
pub trait SessionSink: Send + Sync {
    /// Stable, opaque session identifier. Returned by reference so
    /// callers can label trace events without cloning on every
    /// emit.
    fn id(&self) -> &str;

    /// Reserve the next ordinal for a row.
    ///
    /// The ordinal is the row's 1-indexed position in the log — the
    /// same numbering [`read`](Self::read) order and
    /// [`snapshot`](Self::snapshot)'s `last_event_seq` use. The
    /// counter lives here so that every appender on this session
    /// draws from one sequence: a writer that counts rows itself
    /// drifts as soon as anything else appends concurrently, and a
    /// drifted ordinal makes a compaction's provenance unresolvable.
    ///
    /// The caller stamps the reserved ordinal into the row before
    /// [`append`](Self::append) writes it. Reserving an ordinal and
    /// then not writing the row burns that ordinal — the log then
    /// has a hole, which the fold tolerates but no compaction may
    /// cite across.
    fn alloc_seq(&self) -> u64;

    /// Append one event durably. **Write-through** — when the
    /// future resolves with `Ok`, the bytes are durable (or the
    /// caller has explicit transactional guarantees).
    ///
    /// The row's ordinal was reserved with
    /// [`alloc_seq`](Self::alloc_seq) before the call; the sink
    /// does not assign one.
    ///
    /// Failure is fatal at the call site: the agent loop MUST
    /// surface the error to its caller rather than silently
    /// dropping the event.
    async fn append(&self, event: &Value) -> Result<(), SessionError>;

    /// Reconstruct the chronological event stream. Returns events
    /// in the order they were `append`-ed. Used by callers that
    /// want to rehydrate a session (e.g. resume under a
    /// different agent).
    async fn read(&self) -> Result<Vec<Value>, SessionError>;

    /// Reconstruct the chronological event stream starting **after**
    /// the event whose implicit sequence is `from`.
    ///
    /// Equivalent to [`read`](Self::read) followed by dropping the
    /// first `from` events, but implementations are free to skip the
    /// work without materializing the dropped prefix. A caller
    /// reconnecting with a `from` it observed from a previous
    /// [`snapshot`](Self::snapshot) call therefore receives
    /// strictly the events it has not yet seen, without re-reading
    /// the whole log.
    ///
    /// `from = 0` returns every recorded event (the same set as
    /// [`read`](Self::read)). `from = N` returns only events whose
    /// implicit sequence (`append` order, 1-indexed) is greater
    /// than `N`. `from` MUST be ≤ the current `last_event_seq`
    /// from the latest [`snapshot`](Self::snapshot) — callers with
    /// a stale `from` are responsible for surfacing that mismatch
    /// (typically as a `410 Gone` at the HTTP seam).
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::ReadFailed`] when the underlying
    /// storage cannot satisfy the request (corrupt file, IO
    /// error).
    async fn read_from(&self, from: u64) -> Result<Vec<Value>, SessionError>;

    /// The **last** `window` events, newest at the end.
    ///
    /// [`read`](Self::read) parses the whole log; a subscriber that
    /// only wants the tail (the `follow` baseline) should not pay for
    /// the session's whole history on every subscribe. An
    /// implementation with a cheaper tail path overrides this.
    ///
    /// The default reads the log and slices it, so every backend is
    /// correct without the optimisation — `JsonlSessionSink` overrides
    /// it with a bounded read from the end of the file.
    ///
    /// # Errors
    ///
    /// Whatever [`read`](Self::read) returns.
    async fn read_tail(
        &self,
        window: usize,
    ) -> Result<Vec<Value>, SessionError> {
        let rows = self.read().await?;
        let start = rows.len().saturating_sub(window);
        Ok(rows[start..].to_vec())
    }

    /// The rows after ordinal `from`, newest at the end, bounded by
    /// `window`.
    ///
    /// The rows are the ones whose ordinal is greater than `from` —
    /// the same numbering [`read_from`](Self::read_from) uses. The
    /// default implementation reads the tail and skips by position,
    /// which is correct for a log numbered by position and close
    /// enough for a stamped one; an implementation with a cheaper
    /// incremental path overrides it.
    ///
    /// The window bounds what the caller receives. When more rows
    /// arrived than the window holds, the returned rows are the
    /// **oldest** ones that fit: a reader that advances its cursor
    /// past them asks again for the rest, so no row is lost.
    ///
    /// # Errors
    ///
    /// Whatever [`read_tail`](Self::read_tail) returns.
    async fn read_since(
        &self,
        from: u64,
        window: usize,
    ) -> Result<Vec<Value>, SessionError> {
        let tail = self.read_tail(window).await?;
        Ok(tail
            .into_iter()
            .skip(usize::try_from(from).unwrap_or(usize::MAX))
            .collect())
    }

    /// The **first** event, if the log holds one.
    ///
    /// The few facts that are whole-log rather than tail-shaped (a
    /// session's creation time, the agent a delegated session runs as)
    /// need the head; a tail read cannot answer them. The default
    /// reads the log and takes its first row.
    ///
    /// # Errors
    ///
    /// Whatever [`read`](Self::read) returns.
    async fn read_head(&self) -> Result<Option<Value>, SessionError> {
        Ok(self.read().await?.into_iter().next())
    }

    /// Reconstruct the chronological event stream starting **after**
    /// the event whose `stream_index` is `idx`.
    ///
    /// Equivalent to `read_from` for sinks that allocate one
    /// `stream_index` per append and never reuse or skip one
    /// (today every sink). The default implementation
    /// delegates to `read_from`, so the two are interchangeable
    /// on existing sinks; future stream-position semantics
    /// (e.g. dense-over-compaction indices) override this
    /// default.
    ///
    /// `idx = 0` returns every recorded event. `idx = N`
    /// returns only events whose `stream_index` is greater
    /// than `N`. Callers with a stale `idx` are responsible
    /// for surfacing that mismatch as a `410 Gone` at the HTTP
    /// seam -- see `2026-09-24-stream-index.md` section 4.4.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::ReadFailed`] when the underlying
    /// storage cannot satisfy the request.
    async fn read_from_index(
        &self,
        idx: u64,
    ) -> Result<Vec<Value>, SessionError> {
        self.read_from(idx).await
    }

    /// Force a stable checkpoint (flush buffers, rotate logs,
    /// push to remote). Called when the session enters a stable
    /// state (idle, completed). Returns the snapshot metadata.
    async fn snapshot(&self) -> Result<SessionSnapshot, SessionError>;

    /// Mark the session closed. Idempotent. After this resolves
    /// no further `append` / `read` calls are accepted.
    async fn close(&self, reason: SessionEndReason)
    -> Result<(), SessionError>;

    /// Append one event durably with an **idempotency key**.
    ///
    /// `Some(key)` — a caller-supplied opaque token (typically the
    /// transport-level `event_id`, a `Message-Id` header, or the
    /// `agent.session.turn.id` a downstream client minted). The
    /// sink remembers every key it has seen for this session
    /// within a bounded LRU window. A re-submission of the same
    /// key is a **no-op**: the bytes are not written twice.
    ///
    /// This is the dedup seam that closes the
    /// "client retried / SSE reconnected mid-flush" hole —
    /// without it, a single user-typed prompt could land twice
    /// in the durable log and the LLM would see two user-role
    /// rows for the same input.
    ///
    /// `None` — the legacy non-idempotent path. Backends that
    /// do not need dedup leave the default implementation
    /// (`Ok(self.append(event).await?)`) in place; the LRU
    /// bookkeeping is opt-in. Idempotent callers MUST use a
    /// unique key per logical event or `seq` will collide.
    ///
    /// The key is opaque to the sink — it does not appear in
    /// `read()` output, so a replay does not leak caller-side
    /// identifiers. Backends that need to persist the key (for
    /// across-restart dedup) should stash it in a sibling
    /// index file; see `JsonlSessionSink::append_with_key`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Closed`] when the session is
    /// already closed. Idempotent re-submission is NOT an error.
    async fn append_with_key(
        &self,
        key: Option<&str>,
        event: &Value,
    ) -> Result<(), SessionError> {
        let _ = key;
        self.append(event).await
    }

    /// Append a batch of events durably with a **single durable
    /// barrier** (one `fsync` at the tail, not N).
    ///
    /// Default implementation loops over [`append`](Self::append),
    /// so backends without a batched write path keep their
    /// existing per-row durability at the cost of N `fsync`s.
    /// The production `JsonlSessionSink` overrides this to write
    /// the whole batch in one `write_all` + one `fsync` —
    /// measured ~200× faster than per-row `fsync` on a 1k-row
    /// batch.
    ///
    /// Atomicity contract: either every event in `events` is
    /// durable after `Ok`, or none of them are. Callers SHOULD
    /// treat the same `Err` as a fatal append failure (the agent
    /// loop's fail-fast rule).
    ///
    /// Idempotency: the default implementation does not dedup —
    /// call [`append_with_key`](Self::append_with_key) per row if
    /// the batch may have been partially applied on a prior
    /// attempt. The production `JsonlSessionSink` overrides this
    /// as well: it skips the rows its log already holds, so the
    /// retry of a refused batch does not write a row twice.
    /// `fork_session` is the motivating caller of the plain
    /// batch: it reads the parent transcript once, holds the rows
    /// in memory, and submits a fresh batch to the child — no
    /// dedup needed.
    ///
    /// The rows carry the ordinals their writer reserved with
    /// [`alloc_seq`](Self::alloc_seq); the batch does not reserve
    /// any of its own.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::AppendFailed`] on any IO failure.
    async fn append_many(&self, events: &[Value]) -> Result<(), SessionError> {
        for event in events {
            self.append(event).await?;
        }
        Ok(())
    }
}

/// Outcome category attached to the final `close()` call.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SessionEndReason {
    /// Run completed normally.
    #[default]
    Completed,
    /// User cancelled the run.
    Cancelled,
    /// Provider error / fatal failure during the run.
    Error,
    /// Server shut down before completion.
    Interrupted,
}

/// Snapshot metadata returned by `SessionSink::snapshot`.
///
/// `last_event_seq` lets callers detect "no new events since last
/// snapshot" without re-reading the entire log. `bytes_on_disk`
/// is informational.
///
/// `last_stream_index` is the wire-level resume cursor — today
/// it equals `last_event_seq` because the sink's 1-indexed row
/// ordinal IS the per-session stream position; the field is
/// exposed so the wire can name it independently from the
/// sink-internal ordinal as the protocol evolves
/// (`2026-09-24-stream-index.md`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub last_event_seq: u64,
    /// Per-session stream cursor. See [`SessionSink`] —
    /// the sink's append returns this value. Equal to
    /// `last_event_seq` for sinks that allocate one ordinal
    /// per append and never reuse or skip one.
    pub last_stream_index: u64,
    pub bytes_on_disk: u64,
}

/// Errors a sink can surface.
///
/// `AppendFailed` and `ReadFailed` are the only two the agent
/// runtime needs to distinguish for fail-fast behavior. The other
/// variants cover protocol-level problems (closed session,
/// unknown id) that callers may want to handle differently.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// The sink refused an append because the session is already
    /// closed. Callers should restart with a new sink.
    #[error("session is closed")]
    Closed,
    /// Append failed (disk full, fsync error, network error on a
    /// remote backend). Caller MUST treat as fatal.
    #[error("{0}")]
    AppendFailed(String),
    /// Read failed (corrupt file, IO error). Caller MAY retry.
    #[error("{0}")]
    ReadFailed(String),
    /// Snapshot failed.
    #[error("{0}")]
    SnapshotFailed(String),
    /// Close failed (best-effort — usually logged not raised).
    #[error("{0}")]
    CloseFailed(String),
    /// The implementation rejected the request because of a
    /// backend-level invariant (e.g. quota, schema mismatch).
    #[error("{0}")]
    Invalid(String),
    /// The on-disk log's schema version cannot be read by this
    /// build. `found` is the version the file's header carries
    /// (`0` for a headerless pre-versioning file); `supported` is
    /// what this build reads. Export the session with a build that
    /// matches the file, or reopen it with one that does.
    #[error("{}", unsupported_schema_version_message(*found, *supported))]
    UnsupportedSchemaVersion {
        /// The version the file's header carries; `0` when the
        /// file has no header at all.
        found: u32,
        /// The schema version this build supports.
        supported: u32,
    },
}

/// Message for [`SessionError::UnsupportedSchemaVersion`] — the
/// number alone does not say which side of the supported version
/// the file is on, or what the caller should do next.
fn unsupported_schema_version_message(found: u32, supported: u32) -> String {
    if found < supported {
        format!(
            "session log schema version {found} is older than this \
             build reads (this build supports {supported}); export the \
             session with a build that reads v{found}, or reopen it \
             there after migrating the log"
        )
    } else {
        format!(
            "session log schema version {found} is newer than this \
             build reads (this build supports {supported}); export or \
             reopen the session with a build that supports v{found}"
        )
    }
}
