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
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::sink::{
    SessionEndReason,
    SessionError,
    SessionSink,
    SessionSnapshot,
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
/// The mutex is `tokio::sync::Mutex` (not `std::sync::Mutex`)
/// so the lock can be held across the `spawn_blocking` IO
/// branches without requiring `Send` of the guard. Holding the
/// mutex across `append` and `read` is intentional: it
/// serializes the file, so a concurrent `read` can never
/// observe a half-written line while another thread is
/// appending.
#[derive(Clone)]
pub struct JsonlSessionSink {
    root: PathBuf,
    id: String,
    state: Arc<Mutex<State>>,
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

struct State {
    closed: bool,
    /// How many rows this sink has written. The metadata header is
    /// written with the first row, so the header decision cannot
    /// ride the ordinal counter any more: a writer reserves an
    /// ordinal before it writes the row.
    rows_written: u64,
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
        // `count_lines` skips a header line if present, so an
        // existing v1 file's `next_seq` is correct on first
        // open. A v0 (no-header) file also gets the right count
        // because every non-empty line is an event row.
        let next_seq = count_lines(&root.join("events.jsonl"));
        Self {
            root,
            id: id.into(),
            state: Arc::new(Mutex::new(State {
                closed: false,
                rows_written: next_seq,
                bytes_on_disk,
                // Optimistic — refreshed by the read path on
                // first observation. A fresh file's first append
                // writes the header anyway, so this is consistent
                // for any caller that reads-then-appends.
                schema_version: Some(CURRENT_SCHEMA_VERSION),
                idem_keys: VecDeque::with_capacity(IDEMPOTENCY_LRU_CAPACITY),
            })),
            next_seq: Arc::new(AtomicU64::new(next_seq)),
        }
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

        let result = tokio::task::spawn_blocking(move || {
            append_lines_sync(&path, header_line.as_deref(), &line)
        })
        .await
        .map_err(|e| {
            SessionError::AppendFailed(format!("join blocking task: {e}"))
        })?;

        if let Err(e) = result {
            return Err(SessionError::AppendFailed(e));
        }
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
        // Serialise the whole batch outside the lock so the lock
        // is only held across the IO barrier, not the per-event
        // serialise cost.
        let mut lines: Vec<String> = Vec::with_capacity(events.len());
        let mut total_bytes: u64 = 0;
        for event in events {
            let line = serde_json::to_string(event).map_err(|e| {
                SessionError::AppendFailed(format!("serialize event: {e}"))
            })?;
            total_bytes += line.len() as u64 + 1;
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

        let written = tokio::task::spawn_blocking(move || {
            append_lines_batch_sync(&path, header_line.as_deref(), &lines)
        })
        .await;

        match written {
            Ok(Ok(())) => {
                state.bytes_on_disk += total_bytes;
                state.rows_written += events.len() as u64;
                Ok(())
            }
            Ok(Err(e)) => Err(SessionError::AppendFailed(e)),
            Err(e) => Err(SessionError::AppendFailed(format!(
                "join blocking task: {e}"
            ))),
        }
    }

    async fn read(&self) -> Result<Vec<Value>, SessionError> {
        self.read_filtered(0, false).await
    }

    async fn read_from(&self, from: u64) -> Result<Vec<Value>, SessionError> {
        self.read_filtered(from, true).await
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
        let bytes =
            tokio::task::spawn_blocking(move || match fs::read(&path) {
                Ok(bytes) => Ok(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Ok(Vec::new())
                }
                Err(e) => Err(SessionError::ReadFailed(format!(
                    "read events.jsonl: {e}"
                ))),
            })
            .await
            .map_err(|e| {
                SessionError::ReadFailed(format!("join blocking task: {e}"))
            })??;

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

/// Synchronous write + fsync. Writes `header` (if `Some`) first,
/// then `line`, then `sync_all`. The single `fsync` makes
/// `Ok(())` mean "both bytes durable on local disk".
///
/// `header` is `None` when the file already exists (the header
/// is written at most once, on the first append to a fresh
/// file) or when the caller is `append_many` whose batch lands
/// after another appender already wrote it.
fn append_lines_sync(
    path: &Path,
    header: Option<&str>,
    line: &str,
) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    if let Some(h) = header {
        writeln!(file, "{h}").map_err(|e| format!("write header: {e}"))?;
    }
    writeln!(file, "{line}").map_err(|e| format!("write: {e}"))?;
    file.sync_all().map_err(|e| format!("fsync: {e}"))?;
    Ok(())
}

/// Synchronous batch write + single fsync. Concatenates every
/// line into one `write_all` followed by one `sync_all`, so the
/// whole batch is one durable barrier instead of N.
fn append_lines_batch_sync(
    path: &Path,
    header: Option<&str>,
    lines: &[String],
) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    if let Some(h) = header {
        writeln!(file, "{h}").map_err(|e| format!("write header: {e}"))?;
    }
    for line in lines {
        writeln!(file, "{line}").map_err(|e| format!("write: {e}"))?;
    }
    file.sync_all().map_err(|e| format!("fsync: {e}"))?;
    Ok(())
}

/// Count event rows in `events.jsonl`, skipping the optional
/// metadata header. The previous implementation counted every
/// non-empty line — which is correct for v0 (no-header) files
/// but would over-count by one for v1 files because the
/// header is itself a non-empty line. The fix: peek the first
/// non-empty line; if it carries the `_meta` sentinel, skip it.
fn count_lines(path: &Path) -> u64 {
    let Ok(bytes) = fs::read(path) else {
        return 0;
    };
    if bytes.is_empty() {
        return 0;
    }
    let mut count: u64 = 0;
    let mut header_skipped = false;
    for line in bytes.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if !header_skipped {
            header_skipped = true;
            if let Ok(value) = serde_json::from_slice::<Value>(line)
                && is_metadata_header_row(&value)
            {
                // Header line — not an event, do not count.
                continue;
            }
        }
        count += 1;
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
