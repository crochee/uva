//! The session's only writer: one ordinal source, one batched flush.
//!
//! A session's log has exactly one writer, and the writer owns three
//! things a caller must not duplicate:
//!
//! - **the ordinal**. Every row's `seq` comes from the sink's
//!   counter ([`SessionSink::alloc_seq`]) and from nowhere else. Two
//!   writers on one session hand out two sequences, and a compaction
//!   that cites a row's ordinal would then cite a number the reader
//!   cannot reproduce.
//! - **the row's shape**. The writer stamps every row with its
//!   ordinal and the wall clock, so a typed event's builder-default
//!   `seq: 0` never reaches the log.
//! - **the batch**. A flush drains the queue and the spill, sorts the
//!   rows by ordinal, and writes them with one
//!   [`append_many`](SessionSink::append_many) — one durable barrier
//!   for the whole batch instead of one per row.
//!
//! ## No row is ever dropped
//!
//! The queue is bounded, and the pressure it applies is backpressure,
//! not loss:
//!
//! - The asynchronous path ([`LogWriter::append_event`]) waits for
//!   the flush that frees the queue — the flush is the only drainer,
//!   so a send that waited for room nobody would make would wait
//!   forever.
//! - The synchronous path ([`LogWriter::append_event_nowait`])
//!   cannot wait: the provider's chunk callback is synchronous. A row
//!   that does not fit waits in the spill, and the next flush writes
//!   it.
//!
//! Either way the row keeps the ordinal it was given, and the writer
//! never hands out an ordinal it does not write: the reservation and
//! the enqueue are one atomic step, so no row can be left behind
//! without its number.
//!
//! ## The batches are written in ordinal order
//!
//! The sink recognises a row it already holds by its ordinal, and
//! that recognition rests on this: the batches reach the log in
//! ordinal order. The drain holds the queue's sender while it takes
//! the rows, so a producer cannot slip a row past the batch that was
//! drained, and the batch is sorted by ordinal before it is written.
//! A row that arrived after a later ordinal was written would
//! otherwise look like a row the log already holds — and be skipped.
//!
//! ## The flush cadence is the caller's business
//!
//! The writer has no timer and no durability mode: the harness
//! flushes before each model call, the server flushes before it
//! broadcasts a durable fact. One flush is one `append_many`.
//!
//! ## The ledger sees what the fold sees
//!
//! When the writer carries a [`SurfaceLedger`], every row it writes
//! is recorded in it, in the order it was written. The ledger is what
//! a compaction checkpoint resolves its shadowed span against, and the
//! fold is what the reader reproduces; they must agree on the rows
//! and on their ordinals, or a good splice looks unresolvable.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use futures::{
    channel::mpsc,
    lock::{Mutex as AsyncMutex, MutexGuard},
};
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};
use uva_core::Clock;

use crate::{
    SessionError,
    SessionEvent,
    SessionSink,
    SurfaceOp,
    events::{bounded_event, merge_tool_output_row, restore_row},
    jsonl::is_metadata_header_row,
    log::{RepairReport, SessionLog},
    log_surface::{SurfaceLedger, log_is_stamped, row_seq},
    sink::SessionEndReason,
};

#[cfg(test)]
mod tests;

/// Default number of rows the queue buffers before a producer waits.
const DEFAULT_CAPACITY: usize = 256;

/// One row, already carrying the ordinal it will be written under.
#[derive(Debug, Clone)]
struct StampedRow {
    /// The ordinal the row is written under.
    seq: u64,
    /// The row itself, stamped and ready to write.
    value: Value,
}

/// The batch a flush has taken ownership of, still owed to the log.
///
/// The rows already hold the ordinals the writer handed out, so
/// whatever goes wrong at the sink barrier — a refusal **or a
/// cancellation** (the caller's future dropped at the `append_many`
/// await) — must give them back: the guard's `drop` spills them
/// under the same ordinals, exactly as a refused batch, and the next
/// flush writes them. Disarm with [`Self::handed_off`] only after
/// the sink has accepted the batch.
struct OwedBatch<'a> {
    /// Where the rows go if the batch does not land.
    spill: &'a Mutex<Vec<StampedRow>>,
    /// The ordinals the rows already hold, parallel to `values`.
    seqs: Vec<u64>,
    /// The rows themselves.
    values: Vec<Value>,
}

impl OwedBatch<'_> {
    /// Disarm the guard by taking the batch the sink accepted.
    fn handed_off(mut self) -> (Vec<u64>, Vec<Value>) {
        (
            std::mem::take(&mut self.seqs),
            std::mem::take(&mut self.values),
        )
    }
}

impl Drop for OwedBatch<'_> {
    fn drop(&mut self) {
        if self.seqs.is_empty() {
            return;
        }
        let owed: Vec<StampedRow> = self
            .seqs
            .drain(..)
            .zip(self.values.drain(..))
            .map(|(seq, value)| StampedRow { seq, value })
            .collect();
        self.spill.lock().extend(owed);
    }
}

/// The request envelope a header names: the provider, the model and
/// the tool set the model's facts that follow were produced under.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestEnvelope {
    /// The provider that ran.
    provider: String,
    /// The model that ran.
    model: String,
    /// The hash of the tool set the model was offered.
    tools_hash: String,
}

/// The one writer of a session's log.
///
/// The ordinal counter lives in the sink, so a host's own rows and
/// this writer's rows draw from one sequence and cannot collide.
pub struct LogWriter {
    /// The session's backend. Its counter is the ordinal source.
    sink: Arc<dyn SessionSink>,
    /// The clock every row's timestamp comes from.
    clock: Arc<dyn Clock>,
    /// The ledger this session's surface rows are recorded in.
    ///
    /// The writer outlives the run that resolves its compactions,
    /// and the ledger that indexes the surface does not: a run
    /// attaches its own ledger with [`Self::set_ledger`], so the rows
    /// the writer writes are indexed by the run that writes them.
    ledger: RwLock<Option<Arc<SurfaceLedger>>>,
    /// The queue's one sending half.
    ///
    /// One long-lived sender is what makes the capacity check work: a
    /// clone gets its own guaranteed slot, so a per-send clone would
    /// let the queue grow without bound. The lock is runtime-free and
    /// its guard is `Send`, so the sender may be held across an await
    /// — the sync path takes it without waiting, the async path waits
    /// for it.
    tx: AsyncMutex<mpsc::Sender<StampedRow>>,
    /// The queue's receiving half. Only the flush drains it.
    rx: Mutex<mpsc::Receiver<StampedRow>>,
    /// Rows that did not fit in the queue. The queue applies
    /// backpressure, but a synchronous callback cannot wait — and a
    /// row that does not fit waits here instead of being dropped.
    spill: Mutex<Vec<StampedRow>>,
    /// One not-yet-flushed `tool_output` row per tool call.
    ///
    /// A streaming tool yields its output at the pipe's rate, and
    /// every chunk would otherwise be its own row: a command that
    /// prints a megabyte writes a hundred rows of a few KiB each, and
    /// those rows are log-only, so compaction never removes them. The
    /// chunks of one call merge into one pending row (bounded to a
    /// tail window), and the next flush writes it. The map is keyed by
    /// the provider's call id, and a drained row leaves a fresh entry
    /// behind for the chunks that arrive after the flush.
    pending_tool_output: Mutex<HashMap<String, StampedRow>>,
    /// Serialises the flushes. Two flushes that overlapped would
    /// write their batches in whichever order they won the race, and
    /// the file's order would stop being the ordinal order. The lock
    /// is held across the write, so it must be runtime-free — and
    /// its guard must survive an await.
    flush_lock: AsyncMutex<()>,
    /// The envelope the last request header named. The writer
    /// remembers it so the next header can say whether the request
    /// configuration drifted since the previous run.
    last_request: RwLock<Option<RequestEnvelope>>,
    /// The last ordinal this writer made durable (or seeded from the
    /// log). Diagnostic only: the fold numbers the rows it reads.
    last_flushed: AtomicU64,
}

impl LogWriter {
    /// Build a writer with the default queue capacity.
    #[must_use]
    pub fn new(sink: Arc<dyn SessionSink>, clock: Arc<dyn Clock>) -> Self {
        Self::with_capacity(sink, clock, DEFAULT_CAPACITY)
    }

    /// Build a writer whose queue buffers `capacity` rows.
    ///
    /// A test uses a small capacity to force the backpressure path;
    /// a deployment leaves it at the default. The queue never holds
    /// more than `capacity` rows: the row that fills the channel's
    /// buffer parks the sender, and the next send is refused until a
    /// flush drains it.
    #[must_use]
    pub fn with_capacity(
        sink: Arc<dyn SessionSink>,
        clock: Arc<dyn Clock>,
        capacity: usize,
    ) -> Self {
        // A single sender's real capacity is `buffer + 1` — the row
        // that fills the buffer is queued and parks the sender, and
        // the send after it is refused. Sizing the buffer one below
        // the caller's capacity makes the bound the capacity itself.
        let capacity = capacity.max(1);
        let (tx, rx) = mpsc::channel(capacity - 1);
        Self {
            sink,
            clock,
            ledger: RwLock::new(None),
            tx: AsyncMutex::new(tx),
            rx: Mutex::new(rx),
            spill: Mutex::new(Vec::new()),
            pending_tool_output: Mutex::new(HashMap::new()),
            flush_lock: AsyncMutex::new(()),
            last_request: RwLock::new(None),
            last_flushed: AtomicU64::new(0),
        }
    }

    /// Attach this session's ledger.
    ///
    /// The ledger is what a compaction checkpoint resolves its
    /// shadowed span against; the writer feeds it every row it
    /// writes, so the ledger and the fold see the same surface rows
    /// in the same order.
    #[must_use]
    pub fn with_ledger(self, ledger: Arc<SurfaceLedger>) -> Self {
        self.set_ledger(ledger);
        self
    }

    /// Attach a ledger to a writer that is already shared.
    ///
    /// The writer is the session's, and it outlives the run: the
    /// ledger that indexes the surface is the run's, and a run
    /// resolves its compactions against its own. `set_ledger` swaps
    /// the ledger the writer feeds, so the rows the writer writes are
    /// indexed by the run that writes them — the same rows, in the
    /// same order, the reader will reproduce.
    pub fn set_ledger(&self, ledger: Arc<SurfaceLedger>) {
        *self.ledger.write() = Some(ledger);
    }

    /// Feed the session's history into the ledger.
    ///
    /// The rows are numbered exactly the way the fold numbers them:
    /// a fully stamped history uses each row's own `seq`, an old one
    /// counts positions. The ledger and the fold must agree, or a
    /// compaction checkpoint resolves its span against a different
    /// sequence space than the one the fold will reproduce.
    pub fn seed_ledger(&self, rows: &[Value]) {
        self.last_flushed
            .store(crate::log_surface::log_tail_seq(rows), Ordering::Relaxed);
        let Some(ledger) = self.ledger.read().clone() else {
            return;
        };
        let stamped = log_is_stamped(rows);
        let mut positional: u64 = 0;
        for row in rows {
            if is_metadata_header_row(row) {
                continue;
            }
            positional += 1;
            ledger.record(row_seq(row, stamped, positional), row);
        }
    }

    /// Read the session's log: every row the sink holds, in the
    /// order it was written.
    ///
    /// The writer owns the session's log, so the log is read through
    /// it: a caller that holds only the writer — the memory tier and
    /// the crash repair both do — can still project the log.
    ///
    /// A row the writer has buffered but not yet flushed is not in
    /// the log yet: this reads what the last flush wrote.
    ///
    /// # Errors
    ///
    /// Returns the sink's error when the log cannot be read.
    pub async fn read(&self) -> Result<Vec<Value>, SessionError> {
        self.sink.read().await
    }

    /// Reserve an ordinal, stamp the row with it, and queue it.
    ///
    /// Crate-private: a row's shape is this crate's knowledge. An
    /// outside caller names the event it wants written —
    /// [`Self::append_event`] on the async path,
    /// [`Self::append_event_nowait`] on the synchronous one — and
    /// never assembles the JSON itself, so a tag or payload the
    /// vocabulary renames cannot drift in a caller.
    ///
    /// The row is never dropped: when the queue is full the row waits
    /// in the writer's spill and the flush that follows writes it
    /// with the rows the queue already holds. The call therefore
    /// waits for that flush — the flush is the only drainer, so a
    /// producer that waited for room nobody would make would wait
    /// forever. Returns the ordinal the row was given.
    ///
    /// # Errors
    ///
    /// Returns the sink's error when the flush that made room for
    /// the row failed. The row stays owed to the log: it is in the
    /// spill, and the next flush writes it.
    pub(crate) async fn append(
        &self,
        value: Value,
    ) -> Result<u64, SessionError> {
        let (seq, queue_was_full) = {
            let tx = self.tx.lock().await;
            self.enqueue(tx, value)
        };
        if queue_was_full {
            // The queue was full. The flush drains it — and the
            // spill, which holds this row — and makes room for the
            // next one.
            self.flush().await?;
        }
        Ok(seq)
    }

    /// Reserve an ordinal, stamp a typed event with it, and queue it.
    ///
    /// The event's own `seq` and `ts` are replaced: the writer is
    /// what knows the ordinal, and a typed event's builder leaves
    /// `seq: 0` behind.
    ///
    /// # Errors
    ///
    /// Returns the sink's error when the flush that made room for
    /// the row failed.
    pub async fn append_event(
        &self,
        event: &SessionEvent,
    ) -> Result<u64, SessionError> {
        let bounded = bounded_event(event);
        let event = bounded.as_ref().unwrap_or(event);
        let value = serde_json::to_value(event).map_err(|error| {
            SessionError::AppendFailed(format!("serialize event: {error}"))
        })?;
        // A streamed chunk never takes the queue: it merges into the
        // call's pending row, so the memory bound is the tail window
        // rather than the tool's output rate.
        if let Some(call_id) = event.tool_output_call() {
            return Ok(self.enqueue_tool_output(call_id, value));
        }
        self.append(value).await
    }

    /// Reserve an ordinal, stamp the row with it, and queue it
    /// without waiting.
    ///
    /// Crate-private for the same reason as [`Self::append`]: the
    /// synchronous door an outside caller may use is
    /// [`Self::append_event_nowait`], which takes the typed event.
    ///
    /// A row that does not fit waits in the writer's spill instead of
    /// being dropped; the next flush writes it. Returns the ordinal
    /// the row was given.
    pub(crate) fn append_nowait(&self, value: Value) -> u64 {
        match self.tx.try_lock() {
            Some(tx) => self.enqueue(tx, value).0,
            None => {
                // Another producer holds the sender, and this path
                // cannot wait. The row waits in the spill instead: it
                // is never dropped, and it keeps its ordinal. The
                // spill's lock is taken *before* the ordinal is
                // reserved: a producer that held a reserved ordinal
                // without having placed its row would let a flush
                // write a later ordinal first, and the sink would
                // then read the earlier row as one the log already
                // holds — and skip it forever.
                let mut spill = self.spill.lock();
                let seq = self.sink.alloc_seq();
                let row = StampedRow {
                    seq,
                    value: self.stamp(seq, value),
                };
                spill.push(row);
                seq
            }
        }
    }

    /// Reserve an ordinal, stamp a typed event with it, and queue it
    /// without waiting.
    ///
    /// The synchronous path a compaction checkpoint's callbacks use.
    /// Returns the ordinal the row was given.
    pub fn append_event_nowait(&self, event: &SessionEvent) -> u64 {
        let bounded = bounded_event(event);
        let event = bounded.as_ref().unwrap_or(event);
        let value = match serde_json::to_value(event) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    target: "uva.session",
                    %error,
                    "log writer: an unserialisable typed event is \
                     written with a null payload"
                );
                Value::Null
            }
        };
        // A streamed chunk never takes the queue: it merges into the
        // call's pending row, so the memory bound is the tail window
        // rather than the tool's output rate.
        if let Some(call_id) = event.tool_output_call() {
            return self.enqueue_tool_output(call_id, value);
        }
        self.append_nowait(value)
    }

    /// Stamp the session's request header: the row that names the
    /// request envelope the model's facts that follow were produced
    /// under.
    ///
    /// The reason says `initial` when the session has no header yet,
    /// `change` when the envelope differs from the last header's and
    /// `same` when it repeats it. The writer remembers the last
    /// envelope it stamped, so a reader of the log can tell a
    /// drifted request configuration from an unchanged one.
    ///
    /// Returns the ordinal the row was given.
    pub fn append_request_header(
        &self,
        provider: &str,
        model: &str,
        tools_hash: &str,
    ) -> u64 {
        let reason = self.request_reason(provider, model, tools_hash);
        *self.last_request.write() = Some(RequestEnvelope {
            provider: provider.to_string(),
            model: model.to_string(),
            tools_hash: tools_hash.to_string(),
        });
        let event = crate::request_header(provider, model, tools_hash, reason);
        self.append_event_nowait(&event)
    }

    /// The reason the next request header carries: `initial` when the
    /// session has no header yet, `change` when the envelope differs
    /// from the last header's, `same` when it repeats it.
    fn request_reason(
        &self,
        provider: &str,
        model: &str,
        tools_hash: &str,
    ) -> &'static str {
        match self.last_request.read().as_ref() {
            None => "initial",
            Some(last) => {
                if last.provider == provider
                    && last.model == model
                    && last.tools_hash == tools_hash
                {
                    "same"
                } else {
                    "change"
                }
            }
        }
    }

    /// Write every buffered row. The only durability promise.
    ///
    /// The queue and the spill are drained together, sorted by
    /// ordinal — two producers can stamp and enqueue out of order,
    /// and the sort restores the ordinal order in the file — and
    /// written with one [`append_many`](SessionSink::append_many).
    ///
    /// # Errors
    ///
    /// Returns the sink's error when the batch could not be written.
    /// The rows stay owed to the log: they go back to the spill and
    /// the next flush writes them under the same ordinals. A sink
    /// that already wrote them skips them, so the retry does not
    /// put two rows under one ordinal.
    pub async fn flush(&self) -> Result<(), SessionError> {
        let _guard = self.flush_lock.lock().await;
        let rows = self.drain().await;
        if rows.is_empty() {
            return Ok(());
        }
        self.write_batch(rows).await?;
        Ok(())
    }

    /// Close the session's log through the backend this writer owns.
    ///
    /// The writer is the one handle a run or a controller holds: the
    /// sink it was built with is the same session's, so the pairing
    /// that used to be two arguments at a call site is a fact of this
    /// value instead. The queued rows land before the close, so a
    /// row the writer accepted is never lost to the close itself.
    pub async fn close(
        &self,
        reason: SessionEndReason,
    ) -> Result<(), SessionError> {
        self.flush().await?;
        self.sink.close(reason).await
    }

    /// Copy rows into THIS session's log as one durable batch.
    ///
    /// A copy is the one operation that must not go through the
    /// queue: the whole batch is written with a single
    /// [`append_many`](SessionSink::append_many) — one barrier. A
    /// refused batch leaves the rows owed to the log (the spill
    /// holds them under the same ordinals); a backend that wrote the
    /// bytes before the barrier failed keeps them on disk, and its
    /// own retry rule is what keeps the log free of a second row
    /// under one ordinal. Each row gets this session's next ordinal
    /// (stamped into `seq`); the row's own `ts` is KEPT — a copy
    /// preserves when the events happened, and a fork's `created_at`
    /// is the conversation's first event, not the fork. Rows whose
    /// `surface_op.source_event_seqs` cite ordinals are remapped
    /// through the old→new map, so the citations keep naming the
    /// rows they shadow.
    ///
    /// Returns the new ordinals, in row order.
    ///
    /// # Errors
    ///
    /// Returns the sink's error when the batch could not be written.
    /// The rows stay owed to the log — they go back to the spill
    /// under the same ordinals, like any refused flush.
    pub async fn append_copied(
        &self,
        rows: Vec<Value>,
    ) -> Result<Vec<u64>, SessionError> {
        // One barrier for the whole copy: the flush lock serialises
        // this with any flush, and the queue and the spill join the
        // batch so the file's order stays the ordinal order.
        let _guard = self.flush_lock.lock().await;
        // Reserve the whole block under that lock: the copy takes
        // consecutive ordinals, no interleaved append can split the
        // batch, and the copy's ordinals follow everything the log
        // already holds — a copy written after a later ordinal would
        // look like a row the log already has.
        let mut seqs = Vec::with_capacity(rows.len());
        for _ in 0..rows.len() {
            seqs.push(self.sink.alloc_seq());
        }
        // The remap is built from the rows' own stamps rather than
        // from the block's shape: a citation must find its row even
        // when the parent's log holds a hole.
        let remap: HashMap<u64, u64> = rows
            .iter()
            .zip(&seqs)
            .filter_map(|(row, &seq)| {
                restore_row(row).seq().map(|old| (old, seq))
            })
            .collect();
        // The rows come from this crate's own child log, bounded at
        // their own write, so the copy does not bound again.
        let copied: Vec<StampedRow> = rows
            .into_iter()
            .zip(&seqs)
            .map(|(mut row, &seq)| {
                remap_citations(&mut row, &remap);
                StampedRow {
                    seq,
                    value: stamp_copy(seq, row),
                }
            })
            .collect();
        let mut batch = self.drain().await;
        batch.extend(copied);
        self.write_batch(batch).await?;
        Ok(seqs)
    }

    /// Write one batch as one durable barrier. The rows are sorted
    /// by ordinal — two producers can stamp out of order, and the
    /// sort restores the ordinal order in the file — written with a
    /// single [`append_many`](SessionSink::append_many), and
    /// recorded in the ledger on success. A batch that fails — or is
    /// *cancelled*: the caller's future can be dropped at the sink
    /// await, and the guard returns the rows to the spill exactly as
    /// a refusal does — goes back under the same ordinals: the next
    /// flush writes it, and a sink that already wrote it skips it.
    /// Without the guard a dropped flush would turn the batch's
    /// ordinals into holes in the log, and the writer would have
    /// handed out numbers it never wrote.
    async fn write_batch(
        &self,
        mut rows: Vec<StampedRow>,
    ) -> Result<(), SessionError> {
        rows.sort_by_key(|row| row.seq);
        let owed = OwedBatch {
            spill: &self.spill,
            seqs: rows.iter().map(|row| row.seq).collect(),
            values: rows.into_iter().map(|row| row.value).collect(),
        };
        // A refusal or a cancellation (drop at this await) both land
        // in the guard's `drop`, which spills the rows back.
        self.sink.append_many(&owed.values).await?;
        let (seqs, values) = owed.handed_off();
        if let Some(max) = seqs.iter().max() {
            self.last_flushed.fetch_max(*max, Ordering::Relaxed);
        }
        self.feed_ledger(&seqs, &values);
        Ok(())
    }

    /// The last ordinal this writer made durable, or the tail it
    /// seeded from an existing log. Diagnostic: the fold numbers the
    /// rows it reads, so this value never feeds a projection.
    #[must_use]
    pub fn last_seq(&self) -> u64 {
        self.last_flushed.load(Ordering::Relaxed)
    }

    /// Append the closers a resumed run owes, then flush them.
    ///
    /// Repair *adds* rows; it never truncates the log. The closers'
    /// ordinals are the writer's to hand out. This is the internal
    /// half of [`SessionLog::repair`](crate::SessionLog::repair):
    /// the trait method is the public entry, and it is the only one
    /// that reads the log to decide what to close.
    ///
    /// # Errors
    ///
    /// Returns the sink's error when a closer could not be queued or
    /// the closers could not be written.
    pub(crate) async fn repair_with(
        &self,
        closers: Vec<SessionEvent>,
    ) -> Result<(), SessionError> {
        for closer in &closers {
            self.append_event(closer).await?;
        }
        self.flush().await
    }

    /// Stamp a row with its ordinal and the wall clock.
    ///
    /// A legacy row takes the two fields at its top level; a typed
    /// row carries the same two names beside its `type` tag, so one
    /// stamp covers both shapes.
    ///
    /// The top-level `seq` is the row's identity: the sink
    /// recognises a row it already holds by that ordinal, so the
    /// retry of a refused batch does not write the row twice.
    fn stamp(&self, seq: u64, mut value: Value) -> Value {
        let ts = self.clock.now().to_rfc3339();
        // A row whose kind the vocabulary declares ignorable carries
        // the marker that lets a build which does not know the kind
        // skip it instead of refusing the whole log. The declaration
        // lives on the tag, so the marker cannot be forgotten by the
        // writer — it is the vocabulary's promise, stamped here.
        let ignorable =
            restore_row(&value).tag().is_some_and(|tag| tag.ignorable());
        match value.as_object_mut() {
            Some(object) => {
                object.insert("seq".to_string(), Value::from(seq));
                object.insert("ts".to_string(), Value::String(ts));
                if ignorable {
                    object.insert("ignorable".to_string(), Value::Bool(true));
                }
                value
            }
            None => {
                // A row that is not an object cannot carry a
                // top-level `seq`, and a log with one rowless row
                // stops being a stamped log — the fold would fall
                // back to counting positions. Wrap the value so the
                // row keeps its ordinal.
                json!({"data": value, "seq": seq, "ts": ts})
            }
        }
    }

    /// Reserve an ordinal, stamp the row with it, and put it on the
    /// queue.
    ///
    /// The whole step is synchronous and holds the sender's lock, so a
    /// row's ordinal and its place in the queue are one atomic step:
    /// no await can separate them, and the file's order stays the
    /// ordinal order. A row that does not fit waits in the spill —
    /// it is never dropped, and it keeps the ordinal it was given.
    ///
    /// Returns the ordinal the row was given and whether the queue
    /// was full, which is the caller's cue to flush.
    fn enqueue(
        &self,
        mut tx: MutexGuard<'_, mpsc::Sender<StampedRow>>,
        value: Value,
    ) -> (u64, bool) {
        let seq = self.sink.alloc_seq();
        let row = StampedRow {
            seq,
            value: self.stamp(seq, value),
        };
        match tx.try_send(row) {
            Ok(()) => (seq, false),
            Err(error) => {
                // The queue is full: the row waits in the spill and
                // the next flush writes it.
                self.spill.lock().push(error.into_inner());
                (seq, true)
            }
        }
    }

    /// File one `tool_output` chunk as (or into) the call's pending
    /// row.
    ///
    /// The first chunk of a call reserves the row's ordinal; every
    /// later chunk appends to the same row under the same ordinal, so
    /// the sink writes one row where the tool produced a burst.
    /// Returns the ordinal the row carries.
    fn enqueue_tool_output(&self, call_id: &str, value: Value) -> u64 {
        let mut pending = self.pending_tool_output.lock();
        if let Some(row) = pending.get_mut(call_id) {
            merge_tool_output_row(&mut row.value, &value);
            return row.seq;
        }
        let seq = self.sink.alloc_seq();
        let row = StampedRow {
            seq,
            value: self.stamp(seq, value),
        };
        pending.insert(call_id.to_string(), row);
        seq
    }

    /// Take every buffered row: the spill first, then the pending
    /// tool output, then the queue.
    ///
    /// The queue's sender is locked across the drain. A producer
    /// that has reserved an ordinal but not yet queued its row would
    /// otherwise let this flush write a *later* ordinal first — and
    /// the log's order is the ordinal order, which is also what lets
    /// the sink recognise a row it already holds.
    async fn drain(&self) -> Vec<StampedRow> {
        let _tx = self.tx.lock().await;
        let mut rows = std::mem::take(&mut *self.spill.lock());
        let pending = std::mem::take(&mut *self.pending_tool_output.lock());
        rows.extend(pending.into_values());
        let mut rx = self.rx.lock();
        while let Ok(row) = rx.try_recv() {
            rows.push(row);
        }
        rows
    }

    /// Record the rows this flush wrote in the ledger, so the ledger
    /// and the fold see the same surface rows in the same order.
    fn feed_ledger(&self, seqs: &[u64], values: &[Value]) {
        let Some(ledger) = self.ledger.read().clone() else {
            return;
        };
        for (seq, value) in seqs.iter().zip(values) {
            ledger.record(*seq, value);
        }
    }
}

/// Stamp a copied row with its new ordinal, keeping its `ts`: the
/// copy preserves when the events happened, so only the ordinal —
/// the number the child's readers reproduce — is the child's own.
fn stamp_copy(seq: u64, mut value: Value) -> Value {
    match value.as_object_mut() {
        Some(object) => {
            object.insert("seq".to_string(), Value::from(seq));
            value
        }
        None => {
            // A row that is not an object cannot carry a top-level
            // `seq`; wrap it so the row keeps its ordinal.
            json!({"data": value, "seq": seq})
        }
    }
}

/// Remap a copied row's `surface_op.source_event_seqs` through the
/// copy's old→new ordinal map, so the citations keep naming the rows
/// they shadow. `start` / `end` are positions in the surface, not
/// ordinals — the copy preserves them. A citation the map does not
/// know (it named a row the copy did not take) is left alone.
fn remap_citations(row: &mut Value, remap: &HashMap<u64, u64>) {
    let Some(op_value) = row.get_mut("surface_op") else {
        return;
    };
    let Ok(SurfaceOp::Replace {
        start,
        end,
        source_event_seqs,
    }) = serde_json::from_value::<SurfaceOp>(op_value.clone())
    else {
        return;
    };
    let remapped: Vec<u64> = source_event_seqs
        .iter()
        .map(|&cited| remap.get(&cited).copied().unwrap_or(cited))
        .collect();
    *op_value = serde_json::to_value(SurfaceOp::Replace {
        start,
        end,
        source_event_seqs: remapped,
    })
    .expect("a Replace op serialises back to JSON");
}

#[async_trait]
impl SessionLog for LogWriter {
    fn id(&self) -> &str {
        self.sink.id()
    }

    fn last_seq(&self) -> u64 {
        LogWriter::last_seq(self)
    }

    async fn read(&self) -> Result<Vec<Value>, SessionError> {
        LogWriter::read(self).await
    }

    fn set_ledger(&self, ledger: Arc<SurfaceLedger>) {
        LogWriter::set_ledger(self, ledger);
    }

    fn seed_ledger(&self, rows: &[Value]) {
        LogWriter::seed_ledger(self, rows);
    }

    async fn append_event(
        &self,
        event: &SessionEvent,
    ) -> Result<u64, SessionError> {
        LogWriter::append_event(self, event).await
    }

    fn append_event_nowait(&self, event: &SessionEvent) -> u64 {
        LogWriter::append_event_nowait(self, event)
    }

    fn append_request_header(
        &self,
        provider: &str,
        model: &str,
        tools_hash: &str,
    ) -> u64 {
        LogWriter::append_request_header(self, provider, model, tools_hash)
    }

    async fn flush(&self) -> Result<(), SessionError> {
        LogWriter::flush(self).await
    }

    async fn close(
        &self,
        reason: SessionEndReason,
    ) -> Result<(), SessionError> {
        LogWriter::close(self, reason).await
    }

    async fn repair(&self) -> Result<RepairReport, SessionError> {
        // The queued rows are owed to the log: the scan must see
        // them or it would close a turn that is about to be written.
        LogWriter::flush(self).await?;
        let rows = LogWriter::read(self).await?;
        // The scan reads the log's rows, not a decode of the whole
        // log: the vocabulary restores each row on demand, so a row
        // of another kind costs no decode — and a row this build
        // cannot read stops the scan instead of being dropped.
        let log = crate::events::Rows::read(&rows).map_err(|error| {
            SessionError::ReadFailed(format!(
                "session log is unreadable: {error}"
            ))
        })?;
        let typed: Vec<SessionEvent> =
            log.iter().filter_map(|row| row.event()).collect();
        let closers = crate::repair::interrupted_turn_closers(&typed);
        let count = closers.len();
        self.repair_with(closers).await?;
        Ok(RepairReport {
            closers: count,
            last_seq: LogWriter::last_seq(self),
        })
    }
}
