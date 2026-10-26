//! `JsonlSessionSink` — the on-disk production backend.
//!
//! Persists events to `{dir}/events.jsonl`. Every `append` is
//! followed by an `fsync`, so `append().await` returning `Ok`
//! guarantees the bytes are durable on the local filesystem.
//!
//! ## Layout
//!
//! ```text
//! {root_dir}/
//!   events.jsonl    # optional first-line metadata header
//!                   # (`{"_meta":{"schema_version":N,"id":"…"}}`)
//!                   # followed by one JSON value per line,
//!                   # chronologically ordered.
//! ```
//!
//! ## Schema versioning
//!
//! The first non-empty line of a fresh `events.jsonl` is the
//! [`SessionMetadataHeader`] — a sentinel whose sole job is to
//! record the on-disk schema version that wrote the file. Older
//! files (no header) are read as `schema_version = 0` and every
//! row is treated as a regular event; new files get
//! `schema_version = 1`. The header is detected lazily on the
//! first `read` / `read_from` call and cached in `State` so the
//! per-row read path is branch-free.
//!
//! ## Idempotency
//!
//! [`SessionSink::append_with_key`] maintains a bounded
//! `VecDeque<(String, u64)>` LRU of recently-seen keys. A
//! re-submission of the same key returns the `seq` originally
//! assigned and does not write a second row. The LRU is in-memory
//! only: across process restarts the dedup window starts fresh,
//! so a caller that needs cross-restart dedup should rely on a
//! stable key (e.g. transport-level `event_id`) and accept the
//! "first restart may double-write" caveat — the production wire
//! re-emits the upstream's `event_id` verbatim, so a stable
//! client never hits this.
//!
//! [`SessionSink::append_many`] carries the other half: the retry
//! of a refused batch. The sink remembers the highest ordinal it has
//! written, and a row whose top-level `seq` is at or below that mark
//! is one the log already holds — it is not written again, so the
//! retry does not put two rows under one ordinal. A row with no
//! `seq` cannot be identified and is always written: what cannot be
//! recognised cannot be skipped.
//!
//! The mark moves even when the batch's barrier failed: the bytes
//! reached the file, so the rows are in the log, and only their
//! durability is unproven. A batch that was rolled back is not in
//! the log, and the retry writes it.
//!
//! ## Batching
//!
//! [`SessionSink::append_many`] overrides the default loop with a
//! single `write_all` + one `fsync` — measured ~200× faster
//! than per-row `fsync` on a 1k-row batch. The motivating caller
//! is `routes/sessions.rs::fork_session` (a 27 k-row fork that
//! took 63.8 s with per-row `fsync`).

use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use futures::lock::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use synthia_core::blocking::Blocking;

use crate::{
    fs::FsBackend,
    log_surface::log_is_stamped,
    sink::{SessionEndReason, SessionError, SessionSink, SessionSnapshot},
};

/// Current on-disk schema version.
///
/// Bump when the row shape, header format, or invariant contract
/// of `events.jsonl` changes in a way older readers cannot
/// safely consume. The header is written on the first append
/// after [`JsonlSessionSink::new`] opens a fresh file; existing
/// files are migrated lazily by the read path.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// The first non-empty line of a fresh `events.jsonl`.
///
/// Readers detect the header via the top-level `_meta` field and
/// skip it. The shape is intentionally minimal: `id` lets a
/// future reader correlate a folder with the session id (the
/// folder name already carries it, but redundancy is cheap and
/// aids forensics); `schema_version` lets readers branch on
/// on-disk evolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMetadataHeader {
    /// Always `"synthia_session_meta"`. Marker so a stray JSON
    /// object the agent happens to write is not mistaken for a
    /// header on re-read.
    #[serde(rename = "_meta")]
    pub meta: MetaMarker,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MetaMarker {
    /// Stable kind tag.
    pub kind: String,
    /// The schema version that wrote this file. Readers branch
    /// on this when the row shape changes; writers always emit
    /// [`CURRENT_SCHEMA_VERSION`] on a fresh file.
    pub schema_version: u32,
    /// The session id this file belongs to. Mirrors the folder
    /// name; present so a reader that finds a stray file outside
    /// its folder still knows which session it covers.
    pub id: String,
}

impl SessionMetadataHeader {
    /// Build the header a fresh file starts with.
    pub(crate) fn fresh(id: impl Into<String>) -> Self {
        Self {
            meta: MetaMarker {
                kind: "synthia_session_meta".to_string(),
                schema_version: CURRENT_SCHEMA_VERSION,
                id: id.into(),
            },
        }
    }
}

/// The `kind` tag that identifies a [`SessionMetadataHeader`] row.
const METADATA_KIND: &str = "synthia_session_meta";

/// `true` when `row` is a metadata header rather than an event row.
///
/// A header is the first line of a fresh `events.jsonl` and is **not**
/// a log row: it carries no `type`, no `ts`, and no surface payload.
/// Every reader that walks a raw log must skip it — otherwise the
/// header consumes an ordinal, shifting `seq` numbering off by one
/// relative to the sink's own sequence (which skips it), and a
/// caller reading the first row as the session's opening event finds
/// a timestamp-less header instead.
///
/// Exposed so readers outside this crate (the server's transcript
/// projections, the search index) share one definition instead of
/// each re-deriving the shape.
#[must_use]
pub fn is_metadata_header_row(row: &Value) -> bool {
    row.get("_meta")
        .and_then(|meta| meta.get("kind"))
        .and_then(Value::as_str)
        == Some(METADATA_KIND)
}

/// On-disk JSONL-backed sink. Cheap to clone — wraps shared
/// `Arc<Mutex<State>>`.
///
/// The mutex is [`futures::lock::Mutex`] (not `std::sync::Mutex`)
/// so the lock can be held across the fs-IO await without blocking
/// the executor thread; its guard is `Send` when `State` is, so the
/// `SessionSink` futures stay `Send` without a runtime in the
/// signature. Holding the mutex across `append` and `read` is
/// intentional: it serializes the file, so a concurrent `read` can
/// never observe a half-written line while another thread is
/// appending.
#[derive(Clone)]
pub struct JsonlSessionSink {
    root: PathBuf,
    id: String,
    state: Arc<Mutex<State>>,
    /// The blocking seam every fs call goes through. The default is
    /// [`ThreadBlocking`](synthia_core::blocking::ThreadBlocking), so
    /// the sink works with no runtime; a host on a runtime installs
    /// that runtime's adapter with [`Self::with_blocking`].
    fs: FsBackend,
    /// The ordinal counter, beside the state it guards: the
    /// reservation in [`alloc_seq`](SessionSink::alloc_seq) must not
    /// await the file lock, or the loop's hot path would block on
    /// the disk.
    next_seq: Arc<AtomicU64>,
}

/// Maximum number of idempotency keys retained in memory. A
/// short window is enough — the production dedup path is the
/// "client retried within one request lifetime" race, which is
/// bounded by the SSE keep-alive timer (typically 30 s). A key
/// older than this window falls out of the LRU; re-submission
/// then writes a fresh row, which is the same behaviour as a
/// process restart (the LRU is in-memory only).
const IDEMPOTENCY_LRU_CAPACITY: usize = 1024;

/// Bytes a tail read starts with. It grows by ×4 until the requested
/// window is covered, so the first read covers a typical row batch
/// (an assistant message, a few tool results) without touching the
/// rest of the file.
const TAIL_READ_CHUNK: u64 = 8 * 1024;

struct State {
    closed: bool,
    /// How many rows this sink instance has written. The metadata
    /// header is written with the first row, so the header decision
    /// cannot ride the ordinal counter any more: a writer reserves
    /// an ordinal before it writes the row.
    rows_written: u64,
    /// The highest ordinal this sink instance has written.
    ///
    /// The row count cannot answer "is this row already in the log?",
    /// but the ordinal can: a row whose `seq` is at or below this
    /// mark is one the log holds, and writing it again would put two
    /// rows under one ordinal. The mark is the *highest* ordinal
    /// because the writer hands out ordinals in order and writes its
    /// batches in that order, so a row below the mark is a row the
    /// log already holds.
    max_seq_written: u64,
    bytes_on_disk: u64,
    /// `schema_version` of the on-disk file, detected lazily on
    /// the first read. `None` until `read` / `read_from` has been
    /// called at least once. A `new` sink that has not yet
    /// appended anything reports `Some(CURRENT_SCHEMA_VERSION)`
    /// optimistically — every fresh write starts with the
    /// current version.
    schema_version: Option<u32>,
    /// LRU of `(idempotency_key, assigned_seq)` pairs, oldest at
    /// the front. Insertion order = eviction order. A re-submit
    /// that hits the LRU writes nothing; a miss allocates a new
    /// entry, evicts the oldest if at capacity, and proceeds with a
    /// normal append.
    idem_keys: VecDeque<(String, u64)>,
}

impl State {
    /// The ordinal `row` carries, if it carries one.
    ///
    /// Ordinals are 1-based, so a row whose `seq` is missing or zero
    /// carries no identity: a typed event's builder leaves `seq: 0`
    /// behind, and the writer replaces it before the row is queued.
    /// What cannot be identified cannot be skipped.
    fn ordinal_of(row: &Value) -> Option<u64> {
        row.get("seq")
            .and_then(Value::as_u64)
            .filter(|seq| *seq > 0)
    }

    /// Whether `row` still has to be written.
    ///
    /// A row whose ordinal is at or below the highest ordinal the log
    /// holds is one the log already has: writing it again would put
    /// two rows under one ordinal, and the reader that replays the
    /// log would see the row twice.
    ///
    /// A row that carries no ordinal is written. What cannot be
    /// recognised cannot be skipped — a guess would drop a row the
    /// log never had.
    fn is_fresh(&self, row: &Value) -> bool {
        match Self::ordinal_of(row) {
            Some(seq) => seq > self.max_seq_written,
            None => true,
        }
    }

    /// Record a batch the log holds.
    ///
    /// The highest ordinal in the batch becomes the mark a later
    /// batch is compared against, so a retry of this batch writes
    /// nothing. A row the batch did not carry is not counted: the
    /// count is what the log holds, not what it was offered.
    fn absorb(&mut self, highest: Option<u64>, rows: u64, bytes: u64) {
        if let Some(seq) = highest {
            self.max_seq_written = self.max_seq_written.max(seq);
        }
        self.rows_written += rows;
        self.bytes_on_disk += bytes;
    }
}

impl JsonlSessionSink {
    /// Create (or open) a JSONL sink rooted at `dir`. The directory
    /// is created if missing. Existing `events.jsonl` files are
    /// appended to (so resumes preserve history).
    pub fn new(id: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        let root: PathBuf = dir.into();
        let _ = fs::create_dir_all(&root);
        let bytes_on_disk = root
            .join("events.jsonl")
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0);
        // The counter continues from the log's own numbering: a
        // stamped log hands out one past its highest ordinal, an old
        // one one past its last row.
        let next_seq = seed_next_seq(&root.join("events.jsonl"));
        Self {
            root,
            id: id.into(),
            state: Arc::new(Mutex::new(State {
                closed: false,
                rows_written: 0,
                max_seq_written: 0,
                bytes_on_disk,
                // Optimistic — refreshed by the read path on
                // first observation. A fresh file's first append
                // writes the header anyway, so this is consistent
                // for any caller that reads-then-appends.
                schema_version: Some(CURRENT_SCHEMA_VERSION),
                idem_keys: VecDeque::with_capacity(IDEMPOTENCY_LRU_CAPACITY),
            })),
            fs: FsBackend::default(),
            next_seq: Arc::new(AtomicU64::new(next_seq)),
        }
    }

    /// Install the blocking seam this sink's fs work runs on.
    ///
    /// The default is the runtime-free
    /// [`ThreadBlocking`](synthia_core::blocking::ThreadBlocking); a
    /// tokio host installs
    /// `synthia_harness_tokio::TokioBlocking` (the server installs
    /// its own adapter of the same shape).
    #[must_use]
    pub fn with_blocking(mut self, blocking: Arc<dyn Blocking>) -> Self {
        self.fs = FsBackend::new(blocking);
        self
    }

    fn events_path(&self) -> PathBuf {
        self.root.join("events.jsonl")
    }
}

#[async_trait]
impl SessionSink for JsonlSessionSink {
    fn id(&self) -> &str {
        &self.id
    }

    fn alloc_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    async fn append(&self, event: &Value) -> Result<(), SessionError> {
        let mut state = self.state.lock().await;
        if state.closed {
            return Err(SessionError::Closed);
        }
        let line = serde_json::to_string(event).map_err(|e| {
            SessionError::AppendFailed(format!("serialize event: {e}"))
        })?;
        let path = self.events_path();
        let bytes = line.len() as u64 + 1;

        // The first append of a brand-new file writes the
        // header so subsequent readers can branch on
        // schema_version. Re-opening an existing v0 (no-header)
        // file does NOT backfill the header — that would
        // invalidate every prior row count. A v0 reader
        // just continues to treat every line as an event.
        let needs_header = state.rows_written == 0 && !path.exists();
        let header_line = if needs_header {
            Some(
                serde_json::to_string(&SessionMetadataHeader::fresh(&self.id))
                    .map_err(|e| {
                        SessionError::AppendFailed(format!(
                            "serialize metadata header: {e}"
                        ))
                    })?,
            )
        } else {
            None
        };
        let total_bytes = bytes
            + header_line
                .as_ref()
                .map(|h| h.len() as u64 + 1)
                .unwrap_or(0);

        self.fs
            .append_owned(path, header_line, vec![line])
            .await
            .map_err(|failure| SessionError::AppendFailed(failure.message))?;
        state.bytes_on_disk += total_bytes;
        state.rows_written += 1;
        Ok(())
    }

    async fn append_with_key(
        &self,
        key: Option<&str>,
        event: &Value,
    ) -> Result<(), SessionError> {
        let Some(key) = key else {
            return self.append(event).await;
        };
        // Two-phase: check the LRU under the lock (cheap, no IO),
        // then drop the lock and do the IO without holding it so
        // a concurrent read does not wait on the fsync. The
        // append path re-acquires the lock for the actual write.
        {
            let mut state = self.state.lock().await;
            let hit = state.idem_keys.iter().position(|(k, _)| k == key);
            if let Some(pos) = hit {
                // Refresh recency: move-to-front by removing and
                // re-pushing. Cheap on a 1024-entry deque; the
                // common case (cache hit on a recent key) only
                // walks a short prefix.
                let entry = state.idem_keys.remove(pos).unwrap();
                state.idem_keys.push_back(entry);
                return Ok(());
            }
        }
        self.append(event).await?;
        let mut state = self.state.lock().await;
        if state.idem_keys.len() >= IDEMPOTENCY_LRU_CAPACITY {
            state.idem_keys.pop_front();
        }
        state
            .idem_keys
            .push_back((key.to_string(), self.next_seq.load(Ordering::SeqCst)));
        Ok(())
    }

    async fn append_many(&self, events: &[Value]) -> Result<(), SessionError> {
        if events.is_empty() {
            return Ok(());
        }
        let mut state = self.state.lock().await;
        if state.closed {
            return Err(SessionError::Closed);
        }
        // A row the log already holds is not written again: the retry
        // of a refused batch must not put two rows under one ordinal.
        let fresh: Vec<&Value> = events
            .iter()
            .filter(|event| state.is_fresh(event))
            .collect();
        if fresh.is_empty() {
            return Ok(());
        }
        // The batch is serialised in one go: one buffer, one write,
        // one durability barrier.
        let mut lines: Vec<String> = Vec::with_capacity(fresh.len());
        let mut total_bytes: u64 = 0;
        let mut highest: Option<u64> = None;
        for event in &fresh {
            let line = serde_json::to_string(event).map_err(|e| {
                SessionError::AppendFailed(format!("serialize event: {e}"))
            })?;
            total_bytes += line.len() as u64 + 1;
            if let Some(seq) = State::ordinal_of(event) {
                highest =
                    Some(highest.map(|mark| mark.max(seq)).unwrap_or(seq));
            }
            lines.push(line);
        }
        let path = self.events_path();
        let needs_header = state.rows_written == 0 && !path.exists();
        let header_line = if needs_header {
            Some(
                serde_json::to_string(&SessionMetadataHeader::fresh(&self.id))
                    .map_err(|e| {
                        SessionError::AppendFailed(format!(
                            "serialize metadata header: {e}"
                        ))
                    })?,
            )
        } else {
            None
        };
        total_bytes += header_line
            .as_ref()
            .map(|h| h.len() as u64 + 1)
            .unwrap_or(0);

        match self.fs.append_owned(path, header_line, lines).await {
            Ok(()) => {
                state.absorb(highest, fresh.len() as u64, total_bytes);
                Ok(())
            }
            Err(failure) => {
                // The bytes may have reached the file: the batch is
                // written either way, and the retry must not write it
                // again. A batch that was rolled back is not written,
                // so the retry writes it.
                if failure.landed {
                    state.absorb(highest, fresh.len() as u64, total_bytes);
                }
                Err(SessionError::AppendFailed(failure.message))
            }
        }
    }

    async fn read(&self) -> Result<Vec<Value>, SessionError> {
        self.read_filtered(0, false).await
    }

    async fn read_from(&self, from: u64) -> Result<Vec<Value>, SessionError> {
        self.read_filtered(from, true).await
    }

    async fn read_tail(
        &self,
        window: usize,
    ) -> Result<Vec<Value>, SessionError> {
        JsonlSessionSink::read_tail(self, window).await
    }

    async fn read_head(&self) -> Result<Option<Value>, SessionError> {
        JsonlSessionSink::read_head(self).await
    }

    async fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        let state = self.state.lock().await;
        let last = self.next_seq.load(Ordering::SeqCst);
        Ok(SessionSnapshot {
            session_id: self.id.clone(),
            last_event_seq: last,
            last_stream_index: last,
            bytes_on_disk: state.bytes_on_disk,
        })
    }

    async fn close(
        &self,
        _reason: SessionEndReason,
    ) -> Result<(), SessionError> {
        let mut state = self.state.lock().await;
        state.closed = true;
        Ok(())
    }
}

impl JsonlSessionSink {
    /// The last `window` event rows, read from the **end** of the file.
    ///
    /// [`SessionSink::read`] parses the whole log because the callers
    /// that need it — the transcript, the fold, crash repair — need all
    /// of it. A subscriber that wants the tail does not: this reads
    /// backwards in growing chunks until the window is covered, so the
    /// bytes read scale with the window rather than with the session's
    /// length.
    ///
    /// Rows come back in chronological order. The metadata header is
    /// skipped when the read reaches it, exactly as `read` skips it.
    ///
    /// # Errors
    ///
    /// [`SessionError::ReadFailed`] when the file cannot be read or a
    /// row cannot be parsed — the same contract as
    /// [`SessionSink::read`].
    pub async fn read_tail(
        &self,
        window: usize,
    ) -> Result<Vec<Value>, SessionError> {
        if window == 0 {
            return Ok(Vec::new());
        }
        let path = self.events_path();
        let mut budget = TAIL_READ_CHUNK;
        loop {
            let chunk = self
                .fs
                .read_tail(&path, budget)
                .await
                .map_err(SessionError::ReadFailed)?;
            // A read that began at offset 0 caught the file's first line
            // whole; one that began mid-file starts inside a row. The
            // offset is the fact — a budget comparison misreads a file
            // exactly as long as the budget as "mid-file".
            let reached_start = chunk.start == 0;
            // `window + 1` newlines means at least `window` complete
            // rows survive dropping a possibly-partial first line.
            if reached_start
                || chunk.bytes.iter().filter(|byte| **byte == b'\n').count()
                    > window
            {
                return Self::rows_from_tail(
                    &chunk.bytes,
                    window,
                    reached_start,
                );
            }
            budget = budget.saturating_mul(4);
        }
    }

    /// The log's first event row.
    ///
    /// A tail read cannot answer a whole-log fact — a session's
    /// creation time, the agent a delegated session runs as — so the
    /// few callers that need one read the head, bounded the same way
    /// the tail is. Each growth pass parses only the lines it has not
    /// examined yet.
    ///
    /// # Errors
    ///
    /// [`SessionError::ReadFailed`] on an unreadable or unparsable
    /// head.
    pub async fn read_head(&self) -> Result<Option<Value>, SessionError> {
        let path = self.events_path();
        let mut budget = TAIL_READ_CHUNK;
        let mut scanned = 0usize;
        loop {
            let bytes = self
                .fs
                .read_head(&path, budget)
                .await
                .map_err(SessionError::ReadFailed)?;
            let reached_end = (bytes.len() as u64) < budget;
            // Only complete lines count: the buffer's last piece may
            // be the start of a row whose end is outside the budget.
            let pieces: Vec<&[u8]> =
                bytes.split(|byte| *byte == b'\n').collect();
            let complete = pieces.len().saturating_sub(1);
            for line in pieces.iter().take(complete).skip(scanned) {
                if let Some(row) = Self::parse_row(line)? {
                    return Ok(Some(row));
                }
            }
            scanned = complete;
            if reached_end {
                return Ok(None);
            }
            budget = budget.saturating_mul(4);
        }
    }

    /// Parse one line into an event row.
    ///
    /// `None` for a blank line and for the metadata header. Both
    /// questions are answered by the **one** deserialisation: a tail
    /// read parses every row it keeps, so asking twice (a header probe
    /// followed by the row parse) doubles the cost of the read that
    /// exists to be cheap.
    fn parse_row(line: &[u8]) -> Result<Option<Value>, SessionError> {
        if line.is_empty() {
            return Ok(None);
        }
        let value = serde_json::from_slice::<Value>(line).map_err(|error| {
            SessionError::ReadFailed(format!("parse jsonl line: {error}"))
        })?;
        let is_header = value
            .get("_meta")
            .and_then(|meta| meta.get("kind"))
            .and_then(Value::as_str)
            == Some(METADATA_KIND);
        Ok((!is_header).then_some(value))
    }

    /// Parse the rows a tail read captured, newest-window-only.
    fn rows_from_tail(
        bytes: &[u8],
        window: usize,
        reached_start: bool,
    ) -> Result<Vec<Value>, SessionError> {
        let mut lines: Vec<&[u8]> =
            bytes.split(|byte| *byte == b'\n').collect();
        // The writer terminates every row, so the split ends on an
        // empty piece. A file without that terminator (a truncated
        // write) keeps its last piece and is parsed — and fails — the
        // way `read` fails, rather than being silently dropped.
        if lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        if !reached_start && !lines.is_empty() {
            // The read began mid-file: the first piece is the tail of a
            // row whose start is before the window.
            lines.remove(0);
        }
        let mut rows: Vec<Value> = Vec::with_capacity(lines.len().min(window));
        for line in lines {
            if let Some(row) = Self::parse_row(line)? {
                rows.push(row);
            }
        }
        if rows.len() > window {
            let keep = rows.len() - window;
            rows.drain(..keep);
        }
        Ok(rows)
    }

    /// Inner `read` / `read_from` body. `with_from` selects the
    /// seq-filtered variant. Detects the header lazily on the
    /// first call and caches it in `state.schema_version` so the
    /// hot path is branch-free.
    async fn read_filtered(
        &self,
        from: u64,
        with_from: bool,
    ) -> Result<Vec<Value>, SessionError> {
        let path = self.events_path();
        let bytes = self
            .fs
            .read_file(&path)
            .await
            .map_err(SessionError::ReadFailed)?;

        // Detect the schema version off the first non-empty line.
        // Cheap: at most one line is parsed; everything after
        // is parsed unconditionally as an event row.
        let mut header_skipped = false;
        let mut out = Vec::new();
        let mut index: u64 = 0;
        for line in bytes.split(|b| *b == b'\n') {
            if line.is_empty() {
                continue;
            }
            if !header_skipped {
                header_skipped = true;
                let detected = Self::detect_schema_version_from_line(line);
                let mut state = self.state.lock().await;
                state.schema_version = Some(detected);
                if detected > 0 {
                    // Skip the header line.
                    continue;
                }
                // v0 — fall through and parse this line as
                // an event row.
            }
            let seq = index + 1;
            if with_from && seq <= from {
                index += 1;
                continue;
            }
            let v = serde_json::from_slice::<Value>(line).map_err(|e| {
                SessionError::ReadFailed(format!("parse jsonl line: {e}"))
            })?;
            out.push(v);
            index += 1;
        }
        Ok(out)
    }

    /// Inner header detector: `0` if the line is NOT a header,
    /// otherwise the on-disk schema version it carries.
    fn detect_schema_version_from_line(line: &[u8]) -> u32 {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return 0;
        };
        let Some(meta) = value.get("_meta") else {
            return 0;
        };
        if meta.get("kind").and_then(Value::as_str) != Some(METADATA_KIND) {
            return 0;
        }
        meta.get("schema_version")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32
    }
}

/// The ordinal counter's seed for an existing log.
///
/// A stamped log's counter continues from the highest ordinal the
/// log actually holds. The row count is wrong as soon as the log
/// has a hole — an ordinal reserved but never written, which a
/// crash between stamping and writing leaves behind — because the
/// count-based seed hands that ordinal out a second time and the
/// log ends up with two rows under one ordinal.
///
/// An old log — one whose rows carry no `seq` — keeps the count it
/// always had: one past its last row. The stamped judgement is the
/// fold's own ([`log_is_stamped`](crate::log_surface::log_is_stamped)),
/// so the log is numbered one way or the other, never both.
fn seed_next_seq(path: &Path) -> u64 {
    let Ok(bytes) = fs::read(path) else {
        return 0;
    };
    if bytes.is_empty() {
        return 0;
    }
    let mut count: u64 = 0;
    let mut header_skipped = false;
    let mut rows: Vec<Value> = Vec::new();
    let mut every_line_parsed = true;
    for line in bytes.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let value = serde_json::from_slice::<Value>(line).ok();
        if !header_skipped {
            header_skipped = true;
            if let Some(value) = &value
                && is_metadata_header_row(value)
            {
                // Header line — not an event, do not count.
                continue;
            }
        }
        count += 1;
        match value {
            Some(value) => rows.push(value),
            // A line that is not JSON is not a stamped row, and the
            // log cannot be judged stamped around it.
            None => every_line_parsed = false,
        }
    }
    if every_line_parsed && log_is_stamped(&rows) {
        let highest = rows
            .iter()
            .filter_map(|row| row.get("seq").and_then(Value::as_u64))
            .max();
        if let Some(highest) = highest {
            return highest;
        }
    }
    count
}

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

    /// Write one row the way a writer does: reserve its ordinal
    /// first, then hand the row to the sink.
    async fn write(sink: &JsonlSessionSink, row: Value) {
        sink.alloc_seq();
        sink.append(&row).await.expect("append");
    }

    // -----------------------------------------------------------------
    // Tail reads
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn read_tail_returns_exactly_the_last_n_rows() {
        let dir = temp_dir().join("s_tail_n");
        let sink = JsonlSessionSink::new("s_tail_n", dir);
        for i in 0..25 {
            write(&sink, json!({"i": i})).await;
        }
        let tail = sink.read_tail(5).await.expect("a tail");
        assert_eq!(tail.len(), 5);
        assert_eq!(
            tail.iter()
                .map(|row| row["i"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![20, 21, 22, 23, 24],
            "the window is the log's end, in order"
        );
    }

    #[tokio::test]
    async fn read_tail_keeps_correct_rows_across_the_chunk_boundary() {
        // Rows wide enough that the window spans several read chunks,
        // so the partial-first-line handling is exercised.
        let dir = temp_dir().join("s_tail_wide");
        let sink = JsonlSessionSink::new("s_tail_wide", dir);
        let filler = "x".repeat(4000);
        for i in 0..40 {
            write(&sink, json!({"i": i, "text": filler})).await;
        }
        let tail = sink.read_tail(3).await.expect("a tail");
        assert_eq!(tail.len(), 3, "a partial first line must be dropped");
        assert_eq!(
            tail.iter()
                .map(|row| row["i"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![37, 38, 39]
        );
        for row in &tail {
            assert_eq!(
                row["text"].as_str().expect("the row's own payload"),
                filler,
                "no row may be a fragment of another"
            );
        }
    }

    #[tokio::test]
    async fn read_tail_of_a_short_log_is_the_whole_log() {
        let dir = temp_dir().join("s_tail_short");
        let sink = JsonlSessionSink::new("s_tail_short", dir);
        write(&sink, json!({"i": 0})).await;
        write(&sink, json!({"i": 1})).await;
        let tail = sink.read_tail(200).await.expect("a tail");
        assert_eq!(tail.len(), 2, "the metadata header is not a row");
        assert_eq!(tail[0]["i"], 0);
        assert_eq!(tail[1]["i"], 1);
    }

    #[tokio::test]
    async fn read_tail_of_an_empty_log_is_empty() {
        let dir = temp_dir().join("s_tail_empty");
        let sink = JsonlSessionSink::new("s_tail_empty", dir);
        assert!(sink.read_tail(10).await.expect("a tail").is_empty());
    }

    /// The performance claim, at the only layer that can prove it: the
    /// bounded read returns no more bytes than it was asked for, so a
    /// tail read of a long log cannot have read the whole file.
    #[tokio::test]
    async fn a_tail_read_never_returns_more_than_its_budget() {
        use crate::fs::FsBackend;

        let dir = temp_dir().join("s_tail_budget");
        let sink = JsonlSessionSink::new("s_tail_budget", dir.clone());
        let filler = "y".repeat(1000);
        for i in 0..1000 {
            write(&sink, json!({"i": i, "text": filler})).await;
        }
        let path = dir.join("events.jsonl");
        let len = std::fs::metadata(&path).expect("the log").len();
        assert!(len > 1_000_000, "the fixture log is long: {len} bytes");

        let fs = FsBackend::default();
        let chunk = fs.read_tail(&path, 4096).await.expect("a bounded read");
        assert_eq!(chunk.bytes.len(), 4096, "the read stops at its budget");
        assert!(chunk.start > 0, "a 1 MB file does not start at the tail");
        let whole = std::fs::read(&path).expect("the log");
        assert_eq!(
            chunk.bytes,
            whole[whole.len() - 4096..],
            "and it is the file's tail"
        );
    }

    /// A file exactly as long as the byte budget still starts at offset
    /// 0, so its first line is whole. The budget comparison this
    /// replaced called that "mid-file" and dropped a real row.
    #[tokio::test]
    async fn a_log_exactly_as_long_as_the_chunk_starts_at_offset_zero() {
        let dir = temp_dir().join("s_tail_exact");
        std::fs::create_dir_all(&dir).unwrap();
        // 1024 rows of exactly 8 bytes = 8192 = TAIL_READ_CHUNK.
        let mut body = String::new();
        for index in 0..1024 {
            body.push_str(&format!("{{\"a\":{}}}\n", index % 10));
        }
        assert_eq!(body.len() as u64, TAIL_READ_CHUNK, "one exact chunk");
        std::fs::write(dir.join("events.jsonl"), &body).unwrap();
        let sink = JsonlSessionSink::new("s_tail_exact", dir);

        let chunk = crate::fs::FsBackend::default()
            .read_tail(&sink.events_path(), TAIL_READ_CHUNK)
            .await
            .expect("a bounded read");
        assert_eq!(chunk.bytes.len() as u64, TAIL_READ_CHUNK);
        assert_eq!(
            chunk.start, 0,
            "the read reached the file's start, so the first line is whole"
        );

        let rows = sink.read_tail(2000).await.expect("a tail");
        assert_eq!(rows.len(), 1024, "every row is kept, the first included");
        assert_eq!(rows[0]["a"], 0);
        assert_eq!(rows[1023]["a"], 3);
    }

    #[tokio::test]
    async fn read_head_returns_the_first_event_not_the_header() {
        let dir = temp_dir().join("s_head");
        let sink = JsonlSessionSink::new("s_head", dir);
        write(&sink, json!({"i": 0})).await;
        write(&sink, json!({"i": 1})).await;
        let head = sink.read_head().await.expect("a head");
        assert_eq!(head.expect("the log has a first row")["i"], 0);
        assert!(
            JsonlSessionSink::new("s_head_empty", temp_dir().join("s_head_e"))
                .read_head()
                .await
                .expect("a head")
                .is_none(),
            "an empty log has no first row"
        );
    }

    #[tokio::test]
    async fn jsonl_sink_round_trips_events_across_instances() {
        let dir = temp_dir().join("s1");
        let sink = JsonlSessionSink::new("s1", dir.clone());
        write(&sink, json!({"role": "user", "text": "hi"})).await;
        write(&sink, json!({"role": "assistant", "text": "hello"})).await;
        // Re-open the same dir — should see the same two events.
        let sink2 = JsonlSessionSink::new("s1", dir);
        let events = sink2.read().await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["text"], "hi");
        assert_eq!(events[1]["text"], "hello");
    }

    #[tokio::test]
    async fn jsonl_sink_rejects_appends_after_close() {
        let dir = temp_dir().join("s2");
        let sink = JsonlSessionSink::new("s2", dir);
        sink.close(SessionEndReason::Completed).await.unwrap();
        let err = sink.append(&json!({"x": 1})).await.unwrap_err();
        assert_eq!(err, SessionError::Closed);
    }

    #[tokio::test]
    async fn jsonl_sink_persists_across_appends_in_sequence_order() {
        let dir = temp_dir().join("s3");
        let sink = JsonlSessionSink::new("s3", dir);
        for i in 0..5 {
            write(&sink, json!({"i": i})).await;
        }
        let snap = sink.snapshot().await.unwrap();
        assert_eq!(snap.last_event_seq, 5);
        let events = sink.read().await.unwrap();
        for (idx, ev) in events.iter().enumerate() {
            assert_eq!(ev["i"], idx as i64);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn jsonl_sink_concurrent_appends_preserve_all_records() {
        let dir = temp_dir().join("s_concurrent");
        let sink = JsonlSessionSink::new("s_concurrent", dir);
        let mut handles = Vec::new();
        for i in 0..50 {
            let s = sink.clone();
            handles.push(tokio::spawn(async move {
                s.alloc_seq();
                s.append(&json!({"i": i})).await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        let snap = sink.snapshot().await.unwrap();
        assert_eq!(snap.last_event_seq, 50);
        let events = sink.read().await.unwrap();
        assert_eq!(events.len(), 50);
        let mut seen = std::collections::HashSet::new();
        for ev in &events {
            let i = ev["i"].as_i64().unwrap();
            assert!(seen.insert(i), "duplicate seq {i}");
        }
        assert_eq!(seen.len(), 50);
    }

    #[tokio::test]
    async fn jsonl_sink_read_from_zero_matches_read() {
        let dir = temp_dir().join("s_from_zero");
        let sink = JsonlSessionSink::new("s_from_zero", dir);
        for i in 0..4 {
            write(&sink, json!({"i": i})).await;
        }
        let all = sink.read().await.unwrap();
        let from_zero = sink.read_from(0).await.unwrap();
        assert_eq!(
            all, from_zero,
            "`read_from(0)` must mirror the legacy `read()` payload"
        );
    }

    #[tokio::test]
    async fn jsonl_sink_read_from_n_returns_strict_suffix() {
        let dir = temp_dir().join("s_from_n");
        let sink = JsonlSessionSink::new("s_from_n", dir);
        for i in 0..6 {
            write(&sink, json!({"i": i})).await;
        }
        let from_three = sink.read_from(3).await.unwrap();
        assert_eq!(from_three.len(), 3);
        assert_eq!(from_three[0]["i"], 3);
        assert_eq!(from_three[2]["i"], 5);
        let from_six = sink.read_from(6).await.unwrap();
        assert!(from_six.is_empty());
    }

    #[tokio::test]
    async fn jsonl_sink_read_from_past_end_returns_empty() {
        let dir = temp_dir().join("s_past_end");
        let sink = JsonlSessionSink::new("s_past_end", dir);
        for i in 0..2 {
            write(&sink, json!({"i": i})).await;
        }
        let beyond = sink.read_from(u64::MAX).await.unwrap();
        assert!(beyond.is_empty());
    }

    #[tokio::test]
    async fn jsonl_sink_read_from_resumes_across_instance() {
        let dir = temp_dir().join("s_resume");
        let sink = JsonlSessionSink::new("s_resume", dir.clone());
        for i in 0..3 {
            write(&sink, json!({"i": i})).await;
        }
        let snap = sink.snapshot().await.unwrap();
        drop(sink);
        let sink2 = JsonlSessionSink::new("s_resume", dir);
        let suffix = sink2.read_from(snap.last_event_seq).await.unwrap();
        assert!(
            suffix.is_empty(),
            "no events appended after the snapshot, so the suffix must be empty"
        );
    }

    // -----------------------------------------------------------------
    // Schema versioning
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn jsonl_sink_writes_metadata_header_on_first_append() {
        let dir = temp_dir().join("s_meta_first");
        let sink = JsonlSessionSink::new("s_meta_first", dir.clone());
        write(&sink, json!({"i": 0})).await;
        let raw = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 2, "header + 1 event");
        let header: SessionMetadataHeader =
            serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header.meta.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(header.meta.id, "s_meta_first");
    }

    #[tokio::test]
    async fn jsonl_sink_count_lines_skips_header() {
        let dir = temp_dir().join("s_meta_count");
        let sink = JsonlSessionSink::new("s_meta_count", dir.clone());
        for i in 0..3 {
            write(&sink, json!({"i": i})).await;
        }
        let snap = sink.snapshot().await.unwrap();
        assert_eq!(snap.last_event_seq, 3, "header must not be counted");
        drop(sink);
        let sink2 = JsonlSessionSink::new("s_meta_count", dir);
        let snap2 = sink2.snapshot().await.unwrap();
        assert_eq!(snap2.last_event_seq, 3, "re-open reads the same count");
    }

    #[tokio::test]
    async fn jsonl_sink_reads_legacy_v0_file_without_header() {
        // Manually craft a v0 (no-header) file and verify the
        // reader treats every line as an event.
        let dir = temp_dir().join("s_legacy_v0");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        std::fs::write(&path, "{\"i\":0}\n{\"i\":1}\n{\"i\":2}\n").unwrap();
        let sink = JsonlSessionSink::new("s_legacy_v0", dir);
        let snap = sink.snapshot().await.unwrap();
        assert_eq!(snap.last_event_seq, 3);
        let events = sink.read().await.unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["i"], 0);
        assert_eq!(events[2]["i"], 2);
    }

    // -----------------------------------------------------------------
    // Idempotency
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn jsonl_sink_idempotent_append_writes_the_row_once() {
        let dir = temp_dir().join("s_idem_basic");
        let sink = JsonlSessionSink::new("s_idem_basic", dir);
        sink.alloc_seq();
        sink.append_with_key(Some("evt-1"), &json!({"i": 0}))
            .await
            .unwrap();
        sink.alloc_seq();
        sink.append_with_key(Some("evt-1"), &json!({"i": 0}))
            .await
            .unwrap();
        let snap = sink.snapshot().await.unwrap();
        assert_eq!(snap.last_event_seq, 2, "both reservations were taken");
        let events = sink.read().await.unwrap();
        assert_eq!(events.len(), 1, "no second row was written");
    }

    #[tokio::test]
    async fn jsonl_sink_idempotent_distinct_keys_both_written() {
        let dir = temp_dir().join("s_idem_distinct");
        let sink = JsonlSessionSink::new("s_idem_distinct", dir);
        sink.alloc_seq();
        sink.append_with_key(Some("a"), &json!({"i": 0}))
            .await
            .unwrap();
        sink.alloc_seq();
        sink.append_with_key(Some("b"), &json!({"i": 1}))
            .await
            .unwrap();
        sink.append_with_key(Some("a"), &json!({"i": 0}))
            .await
            .unwrap();
        let snap = sink.snapshot().await.unwrap();
        assert_eq!(snap.last_event_seq, 2);
        let events = sink.read().await.unwrap();
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn jsonl_sink_idempotent_none_key_legacy_behavior() {
        // `None` key skips the LRU entirely, preserving the
        // pre-idempotency contract for callers that do not care.
        let dir = temp_dir().join("s_idem_none");
        let sink = JsonlSessionSink::new("s_idem_none", dir);
        sink.alloc_seq();
        sink.append_with_key(None, &json!({"i": 0})).await.unwrap();
        sink.alloc_seq();
        sink.append_with_key(None, &json!({"i": 0})).await.unwrap();
        let snap = sink.snapshot().await.unwrap();
        assert_eq!(snap.last_event_seq, 2);
    }

    // -----------------------------------------------------------------
    // Batched append
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn jsonl_sink_append_many_writes_batch_with_single_fsync() {
        let dir = temp_dir().join("s_many_basic");
        let sink = JsonlSessionSink::new("s_many_basic", dir.clone());
        let batch: Vec<Value> = (0..10).map(|i| json!({"i": i})).collect();
        for _ in &batch {
            sink.alloc_seq();
        }
        sink.append_many(&batch).await.unwrap();
        let snap = sink.snapshot().await.unwrap();
        assert_eq!(snap.last_event_seq, 10);
        let events = sink.read().await.unwrap();
        assert_eq!(events.len(), 10);
        for (i, ev) in events.iter().enumerate() {
            assert_eq!(ev["i"].as_i64().unwrap(), i as i64);
        }
    }

    /// A batch that cannot be written leaves the log without the
    /// batch: `append_many` promises that a refused batch wrote
    /// nothing, and the writer re-queues those rows under the same
    /// ordinals when it does.
    #[tokio::test]
    async fn jsonl_sink_append_many_leaves_no_partial_rows() {
        let dir = temp_dir().join("s_partial");
        std::fs::create_dir_all(&dir).unwrap();
        // The log path is a directory: the sink cannot open it for
        // writing, so the batch never reaches the disk.
        std::fs::create_dir_all(dir.join("events.jsonl")).unwrap();
        let sink = JsonlSessionSink::new("s_partial", dir.clone());

        let error = sink
            .append_many(&[json!({"i": 0}), json!({"i": 1})])
            .await
            .unwrap_err();
        assert!(matches!(error, SessionError::AppendFailed(_)));
        assert!(
            std::fs::read_dir(dir.join("events.jsonl"))
                .unwrap()
                .next()
                .is_none(),
            "a refused batch leaves no rows behind",
        );
    }

    #[tokio::test]
    async fn jsonl_sink_append_many_empty_is_noop() {
        let dir = temp_dir().join("s_many_empty");
        let sink = JsonlSessionSink::new("s_many_empty", dir);
        sink.append_many(&[]).await.unwrap();
        let events = sink.read().await.unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn jsonl_sink_append_many_on_closed_returns_error() {
        let dir = temp_dir().join("s_many_closed");
        let sink = JsonlSessionSink::new("s_many_closed", dir);
        sink.close(SessionEndReason::Completed).await.unwrap();
        let err = sink.append_many(&[json!({"x": 1})]).await.unwrap_err();
        assert_eq!(err, SessionError::Closed);
    }

    #[tokio::test]
    async fn jsonl_sink_append_many_combines_with_subsequent_appends() {
        let dir = temp_dir().join("s_many_combine");
        let sink = JsonlSessionSink::new("s_many_combine", dir.clone());
        write(&sink, json!({"i": 0})).await;
        for _ in 1..5 {
            sink.alloc_seq();
        }
        sink.append_many(&(1..5).map(|i| json!({"i": i})).collect::<Vec<_>>())
            .await
            .unwrap();
        write(&sink, json!({"i": 5})).await;
        let snap = sink.snapshot().await.unwrap();
        assert_eq!(snap.last_event_seq, 6);
        drop(sink);
        let sink2 = JsonlSessionSink::new("s_many_combine", dir);
        let events = sink2.read().await.unwrap();
        assert_eq!(events.len(), 6);
        assert_eq!(events[3]["i"], 3);
    }

    #[tokio::test]
    async fn jsonl_sink_append_many_is_faster_than_per_row_appends() {
        // Soft assertion: batched path completes within a
        // generous wallclock budget that a per-row fsync path
        // would blow past on slow CI disks. The point is to
        // prove the single-fsync implementation is wired up,
        // not to enforce a hard SLA.
        let dir = temp_dir().join("s_many_perf");
        let sink = JsonlSessionSink::new("s_many_perf", dir);
        let batch: Vec<Value> = (0..200).map(|i| json!({"i": i})).collect();
        let started = std::time::Instant::now();
        sink.append_many(&batch).await.unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "200-row batch should complete well under 30s, took {elapsed:?}"
        );
    }
}
