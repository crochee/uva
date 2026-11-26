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
//! record the on-disk schema version that wrote the file. The
//! version is a **file-level** fact: the reader compares the
//! header's version against [`CURRENT_SCHEMA_VERSION`] on the
//! first `read` / `read_from` and refuses anything else — older
//! (a headerless file implies `0`, a v1 file says so) and newer
//! alike — with
//! [`SessionError::UnsupportedSchemaVersion`]. The header is
//! detected lazily and cached in `State` so the per-row read path
//! is branch-free.
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
        LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use futures::{channel::oneshot, lock::Mutex};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uva_core::blocking::Blocking;

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
/// after [`JsonlSessionSink::new`] opens a fresh file; a file
/// whose header carries any other version is refused by the read
/// path, not migrated — the reader cannot know what the other
/// version's rows mean.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

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
    /// Always `"uva_session_meta"`. Marker so a stray JSON
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
                kind: "uva_session_meta".to_string(),
                schema_version: CURRENT_SCHEMA_VERSION,
                id: id.into(),
            },
        }
    }
}

/// The `kind` tag that identifies a [`SessionMetadataHeader`] row.
const METADATA_KIND: &str = "uva_session_meta";

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
    /// [`ThreadBlocking`](uva_core::blocking::ThreadBlocking), so
    /// the sink works with no runtime; a host on a runtime installs
    /// that runtime's adapter with [`Self::with_blocking`].
    fs: FsBackend,
    /// The ordinal counter, beside the state it guards: the
    /// reservation in [`alloc_seq`](SessionSink::alloc_seq) must not
    /// await the file lock, or the loop's hot path would block on
    /// the disk.
    next_seq: Arc<AtomicU64>,
    /// The ordinal seed, deferred: opening a sink must stay O(1) in
    /// the log's length (the session listing opens one sink per
    /// durable session), while the seed needs every row (a stamped
    /// log continues from its highest ordinal). The first ordinal
    /// consumer — [`Self::alloc_seq`], [`Self::snapshot`], the
    /// idempotency record — pays the one whole-log pass.
    seq_seed: Arc<LazyLock<u64, Box<dyn FnOnce() -> u64 + Send + Sync>>>,
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
    /// This sink's cross-process write ownership, claimed with the
    /// first append and held until the sink drops.
    ///
    /// `None` until the first write: a sink the listing opened reads
    /// and never claims, so readers coexist with the live writer
    /// (dsh's read-handle rule).
    ownership: Option<crate::ownership::SessionOwnerLock>,
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
    /// `schema_version` of the on-disk file.
    ///
    /// The version is detected **when the sink opens the log**, from
    /// the first line alone ([`open_version`] — opening must stay
    /// O(1) in the log's length), and refreshed by every full read,
    /// so the append path and the tail read can both ask the question
    /// without re-reading the header. A `new` sink over a fresh
    /// (or missing) log reports `Some(CURRENT_SCHEMA_VERSION)`
    /// optimistically — every fresh write starts with the current
    /// version.
    schema_version: Option<u32>,
    /// LRU of `(idempotency_key, assigned_seq)` pairs, oldest at
    /// the front. Insertion order = eviction order. A re-submit
    /// that hits the LRU writes nothing; a miss allocates a new
    /// entry, evicts the oldest if at capacity, and proceeds with a
    /// normal append.
    idem_keys: VecDeque<(String, u64)>,
    /// Whether the last written batch's bytes landed without their
    /// durability barrier. A retry of that batch owes the barrier:
    /// reporting success would leave the bytes unsynced forever.
    barrier_pending: bool,
    /// The batch write this sink dispatched but has not yet
    /// reconciled into the mark above.
    ///
    /// Set before the write is awaited, cleared by the caller that
    /// awaits it. A caller *dropped* at that await (a compaction
    /// timeout drops a flush exactly there) leaves the record
    /// behind: the blocking thread still finishes the write and
    /// records its own fate, and the next `append_many` reconciles
    /// the record before filtering — so a batch whose bytes landed
    /// is skipped, never written twice, and a batch that was rolled
    /// back is written once.
    pending_write: Option<PendingWrite>,
}

/// A dispatched batch write whose fate the sink still owes its
/// ordinal mark.
struct PendingWrite {
    /// The highest ordinal the batch carries.
    highest: Option<u64>,
    /// How many rows the batch carries.
    rows: u64,
    /// How many bytes the batch carries (including a fresh header's).
    bytes: u64,
    /// The write's outcome, recorded by the blocking thread that
    /// performed it — the one party a dropped awaiter cannot stop.
    fate: Arc<crate::fs::WriteFate>,
    /// Fires when the blocking thread exits: `Ok` when the write's
    /// closure ran to completion, `Err` when the closure was dropped
    /// before it ever ran (dropping the sender with it). Either way
    /// it resolves — awaiting it cannot hang — and it is the only
    /// arbiter of "ran" versus "never ran": the fate cell says
    /// nothing about whether the closure started.
    done: oneshot::Receiver<()>,
}

/// Absorb the write a dropped caller left behind.
///
/// Runs under the state lock, before any filtering: the fate is the
/// file's truth, and the mark must describe the file before
/// `is_fresh` answers anything.
///
/// The `done` signal is awaited unconditionally, and that is the
/// whole trick. Its sender lives inside the write's closure: the
/// thread lights it when the write finished, and a closure dropped
/// before it ever ran takes the sender with it, which resolves the
/// await with `Err`. There is no state in which this await hangs —
/// and no phase to guess at: "the thread was scheduled but has not
/// run its first instruction" and "the closure will never run" look
/// identical to any flag, and guessing the difference is how a
/// reconcile re-offers a batch the thread is about to write. After
/// the signal, `landed` is the truth: a write that landed advances
/// the mark exactly as its awaiter would have, keeping the barrier
/// debt it may have left; a write that never ran absorbed nothing,
/// so its rows stay fresh and are written once.
async fn reconcile_pending(state: &mut State, pending: PendingWrite) {
    let _ = pending.done.await;
    if pending.fate.landed() {
        state.absorb(pending.highest, pending.rows, pending.bytes);
        state.barrier_pending = !pending.fate.barrier_ok();
    }
}

impl State {
    /// Claim this sink's cross-process write ownership, once.
    ///
    /// The first append claims it; every later append is already
    /// owned. A contended claim is an error the caller reports —
    /// two writers stamp colliding ordinals, and a log whose ordinals
    /// the reader cannot reproduce is unreadable, so refusing is the
    /// only safe answer (dsh `session.lock`).
    fn ensure_ownership(
        &mut self,
        root: &std::path::Path,
    ) -> Result<(), crate::ownership::OwnershipError> {
        if self.ownership.is_some() {
            return Ok(());
        }
        let lock = crate::ownership::SessionOwnerLock::try_acquire(root)?;
        self.ownership = Some(lock);
        Ok(())
    }

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

    /// The version the log carries, when this build does not write
    /// it.
    ///
    /// The append path asks this before it writes: a row this build
    /// writes into a log another version owns would leave a file
    /// whose header says one thing and whose rows say another, and
    /// every later reader — including the build the error tells the
    /// operator to export with — would refuse it.
    fn unsupported_version(&self) -> Option<u32> {
        match self.schema_version {
            Some(found) if found != CURRENT_SCHEMA_VERSION => Some(found),
            _ => None,
        }
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
        // Opening a sink must stay O(1) in the log's length: the
        // session listing opens one sink per durable session, and a
        // whole-file pass per session is the read amplification that
        // listing exists to avoid. Two facts used to be read here:
        //
        // * the schema version — the log's **first line** answers
        //   it, so it is still read here, bounded
        //   ([`open_version`]);
        // * the ordinal seed — it needs **every** row (a stamped log
        //   hands out one past its highest ordinal, an old one one
        //   past its last row), so it is deferred to the first
        //   ordinal consumer (`alloc_seq`, `snapshot`, the
        //   idempotency record) via [`Self::seed_ordinals`], which a
        //   read-only caller never pays.
        let version = open_version(&root.join("events.jsonl"));
        let next_seq = Arc::new(AtomicU64::new(0));
        let seeded_counter = Arc::clone(&next_seq);
        let seed_path = root.join("events.jsonl");
        let seq_seed = Arc::new(LazyLock::new(Box::new(move || {
            let next = inspect_log(&seed_path).next_seq;
            seeded_counter.store(next, Ordering::SeqCst);
            next
        })
            as Box<dyn FnOnce() -> u64 + Send + Sync>));
        Self {
            root,
            id: id.into(),
            state: Arc::new(Mutex::new(State {
                closed: false,
                ownership: None,
                rows_written: 0,
                max_seq_written: 0,
                bytes_on_disk,
                // A fresh (or missing) log is this build's own: its
                // first append writes the header. An existing log
                // carries the version its header names — the append
                // path refuses anything else.
                schema_version: Some(version.unwrap_or(CURRENT_SCHEMA_VERSION)),
                // Grown on demand: a listing opens a sink per
                // durable session and never appends, so pre-sizing
                // the LRU here would pin its full capacity for
                // every session on disk.
                idem_keys: VecDeque::new(),
                barrier_pending: false,
                pending_write: None,
            })),
            fs: FsBackend::default(),
            next_seq,
            seq_seed,
        }
    }

    /// Force the deferred ordinal seed ([`Self::seq_seed`]).
    ///
    /// The store into [`Self::next_seq`] happens inside the seed's
    /// initialiser, which completes before any read of the cell
    /// returns, so the counter is correct before the first
    /// `fetch_add` / `load` ever observes it.
    fn seed_ordinals(&self) {
        let _ = LazyLock::force(&self.seq_seed);
    }

    /// Install the blocking seam this sink's fs work runs on.
    ///
    /// The default is the runtime-free
    /// [`ThreadBlocking`](uva_core::blocking::ThreadBlocking); a
    /// tokio host installs
    /// `uva_core_tokio::TokioBlocking` (the server installs
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
        // The seed must land before the first reservation: see
        // [`Self::seed_ordinals`].
        self.seed_ordinals();
        self.next_seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    async fn append(&self, event: &Value) -> Result<(), SessionError> {
        let mut state = self.state.lock().await;
        if state.closed {
            return Err(SessionError::Closed);
        }
        if let Some(error) = state.ensure_ownership(&self.root).err() {
            return Err(SessionError::OwnedByAnotherProcess {
                holder: error.holder(),
            });
        }
        // The version gate, on the write side. A row this build
        // writes into a log another version owns would leave a file
        // whose header says one thing and whose rows say another —
        // and every later reader, including the build the error
        // names, would refuse it. The refusal is the same error the
        // read path raises, so a caller sees one fact, not two.
        if let Some(found) = state.unsupported_version() {
            return Err(SessionError::UnsupportedSchemaVersion {
                found,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }
        let line = serde_json::to_string(event).map_err(|e| {
            SessionError::AppendFailed(format!("serialize event: {e}"))
        })?;
        let path = self.events_path();
        let bytes = line.len() as u64 + 1;

        // The first append of a brand-new file writes the
        // header so subsequent readers can branch on
        // schema_version. Re-opening an existing file does NOT
        // backfill or rewrite the header — that would change the
        // row count and hide the version the file was written
        // with. The read path refuses a version this build does
        // not support, and so does the append path.
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
        // The record cites the counter, so the seed must have landed
        // even when this append is the sink's first ordinal use.
        self.seed_ordinals();
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
        if let Some(error) = state.ensure_ownership(&self.root).err() {
            return Err(SessionError::OwnedByAnotherProcess {
                holder: error.holder(),
            });
        }
        // The same version gate the single-row append applies: a
        // batch must not land in a log this build cannot read.
        if let Some(found) = state.unsupported_version() {
            return Err(SessionError::UnsupportedSchemaVersion {
                found,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }
        // anything is filtered: its bytes may be in the file while
        // the mark still says otherwise, and filtering against a
        // stale mark would write them a second time.
        if let Some(pending) = state.pending_write.take() {
            reconcile_pending(&mut state, pending).await;
        }
        // A row the log already holds is not written again: the retry
        // of a refused batch must not put two rows under one ordinal.
        let fresh: Vec<&Value> = events
            .iter()
            .filter(|event| state.is_fresh(event))
            .collect();
        if fresh.is_empty() {
            // Every row offered is already in the file. A batch
            // whose bytes landed without their barrier is still
            // written-but-unproven, though: the retry pays the
            // barrier it owes instead of reporting a durability it
            // does not have.
            if !state.barrier_pending {
                return Ok(());
            }
            return match self.fs.sync_file(&self.events_path()).await {
                Ok(()) => {
                    state.barrier_pending = false;
                    Ok(())
                }
                Err(message) => Err(SessionError::AppendFailed(message)),
            };
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

        // The write is dispatched with its fate recorded by the
        // blocking thread itself, and registered in the state before
        // the await: a caller dropped at that await leaves the
        // pending record behind, and the next `append_many`
        // reconciles it instead of filtering against a stale mark.
        let dispatched =
            self.fs.dispatch_append_owned(path, header_line, lines);
        let rows = fresh.len() as u64;
        state.pending_write = Some(PendingWrite {
            highest,
            rows,
            bytes: total_bytes,
            fate: Arc::clone(&dispatched.fate),
            done: dispatched.done,
        });
        let outcome = dispatched.result.await;
        match outcome {
            Ok(result) => {
                // The awaiting caller absorbs the write itself; the
                // pending record's job is done.
                state.pending_write = None;
                match result {
                    Ok(()) => {
                        state.absorb(highest, rows, total_bytes);
                        // The batch's own barrier covers the file, so
                        // any bytes an earlier batch left unsynced are
                        // covered too.
                        state.barrier_pending = false;
                        Ok(())
                    }
                    Err(failure) => {
                        // The bytes may have reached the file: the
                        // batch is written either way, and the retry
                        // must not write it again. A batch that was
                        // rolled back is not written, so the retry
                        // writes it.
                        if failure.landed {
                            state.absorb(highest, rows, total_bytes);
                        }
                        state.barrier_pending = failure.landed;
                        Err(SessionError::AppendFailed(failure.message))
                    }
                }
            }
            // The seam lost the write's result (the blocking task
            // panicked, or the channel dropped it): the failure is
            // reported as rolled back, exactly as `append_owned`
            // always has. The pending record deliberately STAYS —
            // the fate cell holds the write's real outcome, and the
            // next `append_many` reconciles against it instead of
            // trusting a mark this caller could not advance.
            Err(dropped) => Err(SessionError::AppendFailed(dropped)),
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

    async fn read_since(
        &self,
        from: u64,
        window: usize,
    ) -> Result<Vec<Value>, SessionError> {
        JsonlSessionSink::read_since(self, from, window).await
    }

    async fn read_head(&self) -> Result<Option<Value>, SessionError> {
        JsonlSessionSink::read_head(self).await
    }

    async fn read_head_rows(
        &self,
        window: usize,
    ) -> Result<Vec<Value>, SessionError> {
        JsonlSessionSink::read_head_rows(self, window).await
    }

    async fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        // A snapshot over a log this sink never appended to still
        // reports the log's own numbering, not a fresh counter.
        self.seed_ordinals();
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
        // Closing hands the session over: the ownership lock is
        // released so another node's takeover does not wait for this
        // process to die (a crash releases the same way, through the
        // kernel closing the descriptor). The sink is closed — no
        // append of ours can follow the release.
        state.ownership = None;
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
        // The file-level gate, before a single row is read.
        //
        // The header only reaches a tail read that happens to start
        // at offset 0; a log longer than the first chunk is read
        // from mid-file, where the header is not in the bytes. The
        // sink knows the file's version anyway — it read it when it
        // opened the log — so the gate is asked here, once, for
        // every read: a row read under this build's semantics out of
        // a log another version wrote is a row this build
        // misinterprets.
        {
            let state = self.state.lock().await;
            if let Some(found) = state.unsupported_version() {
                return Err(SessionError::UnsupportedSchemaVersion {
                    found,
                    supported: CURRENT_SCHEMA_VERSION,
                });
            }
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
                if reached_start {
                    // The read began at the file's first byte, so the
                    // first complete line is the header — gate on it
                    // the same way `read` does. The check above
                    // already refused an unsupported version; this
                    // one also catches a file that changed under the
                    // sink since it opened the log.
                    Self::ensure_supported_schema_version(&chunk.bytes)?;
                }
                return Self::rows_from_tail(
                    &chunk.bytes,
                    window,
                    reached_start,
                );
            }
            budget = budget.saturating_mul(4);
        }
    }

    /// The rows after ordinal `from`, read from the end of the file.
    ///
    /// The scan stops at the first row at or before `from`, so the
    /// cost is proportional to the rows that arrived since the
    /// cursor — not to the window the caller asked for. A subscriber
    /// that polls for new rows therefore reads the new rows, not the
    /// same tail over and over.
    ///
    /// # Errors
    ///
    /// [`SessionError::ReadFailed`] when the file cannot be read or a
    /// row cannot be parsed — the same contract as
    /// [`SessionSink::read`].
    pub async fn read_since(
        &self,
        from: u64,
        window: usize,
    ) -> Result<Vec<Value>, SessionError> {
        if window == 0 {
            return Ok(Vec::new());
        }
        // The file-level gate, before a single row is read: a row
        // read under this build's semantics out of a log another
        // version wrote is a row this build misinterprets.
        {
            let state = self.state.lock().await;
            if let Some(found) = state.unsupported_version() {
                return Err(SessionError::UnsupportedSchemaVersion {
                    found,
                    supported: CURRENT_SCHEMA_VERSION,
                });
            }
        }
        let path = self.events_path();
        let mut budget = TAIL_READ_CHUNK;
        loop {
            let chunk = self
                .fs
                .read_tail(&path, budget)
                .await
                .map_err(SessionError::ReadFailed)?;
            let reached_start = chunk.start == 0;
            if reached_start {
                // The read began at the file's first byte, so the
                // first complete line is the header — gate on it the
                // same way `read` does.
                Self::ensure_supported_schema_version(&chunk.bytes)?;
            }
            // Every row the chunk holds, in chronological order. The
            // window does not bound this read: the boundary row is
            // what ends it.
            let rows =
                Self::rows_from_tail(&chunk.bytes, usize::MAX, reached_start)?;
            // The boundary: the newest row at or before the cursor.
            // The rows are chronological, so it is the last row that
            // is not newer than the cursor — everything after it is
            // new.
            if let Some(index) = rows.iter().rposition(|row| {
                row_ordinal(row).is_some_and(|ordinal| ordinal <= from)
            }) {
                let mut new_rows = rows[index + 1..].to_vec();
                // The window bounds what the caller receives, and the
                // oldest rows come first: a reader that advances its
                // cursor past them asks again for the rest, so no row
                // is lost.
                new_rows.truncate(window);
                return Ok(new_rows);
            }
            if rows.first().is_some_and(|row| row_ordinal(row).is_none()) {
                // A log written before ordinals carries no ordinal at
                // all: the caller has to read it positionally, so the
                // scan stops here rather than reading a whole log the
                // caller will not use.
                let mut new_rows = rows;
                new_rows.truncate(window);
                return Ok(new_rows);
            }
            if reached_start {
                // Every row is newer than the cursor: the log's start
                // is the boundary.
                let mut new_rows = rows;
                new_rows.truncate(window);
                return Ok(new_rows);
            }
            budget = budget.saturating_mul(4);
        }
    }

    /// The log's first `window` event rows, oldest first.
    ///
    /// A tail read cannot answer a whole-log fact — a session's
    /// creation time, the agent a delegated session runs as — so the
    /// few callers that need the head read it bounded the same way
    /// the tail is. Each growth pass parses only the lines it has
    /// not examined yet; the window counts event rows, so the
    /// metadata header never spends one.
    ///
    /// # Errors
    ///
    /// [`SessionError::ReadFailed`] on an unreadable or unparsable
    /// head.
    pub async fn read_head_rows(
        &self,
        window: usize,
    ) -> Result<Vec<Value>, SessionError> {
        let path = self.events_path();
        let mut budget = TAIL_READ_CHUNK;
        let mut scanned = 0usize;
        let mut rows: Vec<Value> = Vec::new();
        loop {
            let bytes = self
                .fs
                .read_head(&path, budget)
                .await
                .map_err(SessionError::ReadFailed)?;
            // The head always starts at offset 0, so the first
            // complete line is the file's header — gate on it the
            // same way `read` does.
            Self::ensure_supported_schema_version(&bytes)?;
            let reached_end = (bytes.len() as u64) < budget;
            // Only complete lines count: the buffer's last piece may
            // be the start of a row whose end is outside the budget.
            let pieces: Vec<&[u8]> =
                bytes.split(|byte| *byte == b'\n').collect();
            let complete = pieces.len().saturating_sub(1);
            for line in pieces.iter().take(complete).skip(scanned) {
                if let Some(row) = Self::parse_row(line)? {
                    rows.push(row);
                    if rows.len() >= window {
                        return Ok(rows);
                    }
                }
            }
            scanned = complete;
            if reached_end {
                return Ok(rows);
            }
            budget = budget.saturating_mul(4);
        }
    }

    /// The log's first event row.
    ///
    /// # Errors
    ///
    /// [`SessionError::ReadFailed`] on an unreadable or unparsable
    /// head.
    pub async fn read_head(&self) -> Result<Option<Value>, SessionError> {
        Ok(self.read_head_rows(1).await?.into_iter().next())
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

    /// Refuse a file whose first line is not a header this build
    /// supports. The caller must have read from the file's first
    /// byte, so the first non-empty line of `bytes` is the
    /// file's first line.
    fn ensure_supported_schema_version(
        bytes: &[u8],
    ) -> Result<(), SessionError> {
        for line in bytes.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let detected = Self::detect_schema_version_from_line(line);
            if detected != CURRENT_SCHEMA_VERSION {
                return Err(SessionError::UnsupportedSchemaVersion {
                    found: detected,
                    supported: CURRENT_SCHEMA_VERSION,
                });
            }
            return Ok(());
        }
        Ok(())
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
    /// first call, caches it in `state.schema_version`, and
    /// refuses any version but [`CURRENT_SCHEMA_VERSION`] — the
    /// gate is the open, so a mixed-version file is caught before
    /// a single row is returned.
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
                if detected != CURRENT_SCHEMA_VERSION {
                    // 0 (headerless), an older major, or a newer
                    // one — none of them is this build's format,
                    // and the reader refuses before returning a
                    // single row.
                    return Err(SessionError::UnsupportedSchemaVersion {
                        found: detected,
                        supported: CURRENT_SCHEMA_VERSION,
                    });
                }
                // A supported header — skip it and read the rows.
                continue;
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

/// What an existing log says about itself.
///
/// One read answers both questions the sink asks when it opens a
/// log: which ordinal to hand out next, and which schema version
/// the file carries. The version is the first line — the header —
/// and it is what the append path and the tail read consult before
/// they touch a row.
///
/// The ordinal seed follows the log's own numbering. A stamped log's
/// counter continues from the highest ordinal the log actually
/// holds: the row count is wrong as soon as the log has a hole — an
/// ordinal reserved but never written, which a crash between
/// stamping and writing leaves behind — because the count-based
/// seed hands that ordinal out a second time and the log ends up
/// with two rows under one ordinal. An old log — one whose rows
/// carry no `seq` — keeps the count it always had: one past its
/// last row. The stamped judgement is the fold's own
/// ([`log_is_stamped`](crate::log_surface::log_is_stamped)), so the
/// log is numbered one way or the other, never both.
struct LogFacts {
    /// The next ordinal to hand out. (The schema version used to
    /// ride here too; it is a first-line fact now, read bounded by
    /// [`open_version`] at open.)
    next_seq: u64,
}

/// The longest line [`open_version`] will buffer while looking for
/// the log's header. A real header is a tiny JSON object; a "line"
/// longer than this is not a header any build wrote, and the verdict
/// (an unsupported version) needs none of its bytes.
const HEADER_LINE_CAP: usize = 8 * 1024;

/// The schema version the log's header names, from the file's first
/// non-empty line alone.
///
/// [`JsonlSessionSink::new`] calls this instead of [`inspect_log`]:
/// opening a sink must cost O(1) in the log's length (the session
/// listing opens one sink per durable session), and the version is a
/// first-line fact. `None` means the file is missing or empty — a
/// fresh log, which this build's own header will stamp.
fn open_version(path: &Path) -> Option<u32> {
    use std::io::Read;
    let mut file = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; HEADER_LINE_CAP];
    let read = file.read(&mut buf).ok()?;
    buf.truncate(read);
    // The window is complete when it stopped before the cap (EOF) —
    // otherwise the cap may have cut a line in half.
    let window_complete = read < HEADER_LINE_CAP;
    let whitespace =
        |line: &[u8]| line.iter().all(|byte| byte.is_ascii_whitespace());
    let mut pos = 0usize;
    while pos < buf.len() {
        let Some(newline) = buf[pos..].iter().position(|b| *b == b'\n') else {
            // The window's final fragment, with no newline in it.
            let line = &buf[pos..];
            if whitespace(line) {
                break;
            }
            return if window_complete {
                Some(JsonlSessionSink::detect_schema_version_from_line(line))
            } else {
                // The cap cut the line: a first line longer than any
                // header is not a header any build wrote, so the
                // verdict is version 0 — unsupported — without
                // buffering the rest of it.
                Some(0)
            };
        };
        let line = &buf[pos..pos + newline];
        if !whitespace(line) {
            return Some(JsonlSessionSink::detect_schema_version_from_line(
                line,
            ));
        }
        pos += newline + 1;
    }
    // Nothing but whitespace inside the window: a fresh log as far
    // as this build is concerned.
    None
}

/// Read an existing log once and report its facts.
///
/// Only the ordinal consumer pays this pass (the sink's deferred
/// seed): the seed needs every row (a stamped log hands out one past
/// its highest ordinal, an old one one past its last row), while the
/// version — the first line — comes from the bounded
/// [`open_version`] instead.
fn inspect_log(path: &Path) -> LogFacts {
    let Ok(bytes) = fs::read(path) else {
        return LogFacts { next_seq: 0 };
    };
    if bytes.is_empty() {
        return LogFacts { next_seq: 0 };
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
    let next_seq = if every_line_parsed && log_is_stamped(&rows) {
        rows.iter()
            .filter_map(|row| row.get("seq").and_then(Value::as_u64))
            .max()
            .unwrap_or(count)
    } else {
        count
    };
    LogFacts { next_seq }
}

/// The row's stamped ordinal, when it carries one.
///
/// A log written before ordinals numbers its rows by position, so the
/// ordinal is absent — `None`, not a zero. A row without an ordinal
/// is never a boundary: the caller that needs the distinction reads
/// the absence.
fn row_ordinal(row: &Value) -> Option<u64> {
    row.get("seq").and_then(Value::as_u64)
}

#[cfg(test)]
mod tests;
