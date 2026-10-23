//! The loop's log view: one writer that stamps, buffers, and
//! flushes the rows a run produces.
//!
//! The writer owns three verbs, and the difference between them is
//! the whole point of the module:
//!
//! - `stamp` hands out the row's ordinal. It never blocks, so a
//!   producer can cite an ordinal in a row it is about to write.
//! - `append` puts a stamped row on the queue. The row is never
//!   dropped: a producer that cannot enqueue right now waits (see
//!   [`queue`]).
//! - `flush` is the only operation that promises durability. It
//!   drains the queue and writes the batch with one barrier.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use queue::{DEFAULT_CAPACITY, LogQueue};
use serde_json::Value;
use synthia_session::{SessionError, SessionEvent, SessionSink};

mod queue;

#[cfg(test)]
mod tests;

/// How much durability a run asks for.
///
/// The default is [`Durability::StepBoundary`]: one flush per step,
/// which is the cheapest cadence that still guarantees "the tool
/// result is on disk before the next model call".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    // One flush at every step boundary.
    StepBoundary,
    // Flush once the batch reaches `flush_rows`, or once
    // `flush_every` has passed since the last flush.
    Batched {
        flush_rows: usize,
        flush_every: Duration,
    },
    // Every row is written on its own. Debugging and audit only.
    Sync,
    // Nothing reaches the sink. Tests and dry runs.
    Off,
}

impl Durability {
    // Whether a flush writes to the sink at all.
    fn writes_to_sink(&self) -> bool {
        !matches!(self, Durability::Off)
    }
}

/// One row, already carrying the ordinal it will be written under.
#[derive(Debug, Clone)]
pub struct StampedRow {
    /// The ordinal the row is written under.
    pub seq: u64,
    /// The row itself, ready to write.
    pub value: Value,
}

/// The run's writer over the session sink.
///
/// The ordinal counter lives in the sink, so two writers on one
/// session (this one and a host's own) draw from the same sequence
/// and cannot collide.
pub struct LogWriter {
    sink: Arc<dyn SessionSink>,
    queue: LogQueue,
    // Rows a synchronous producer could not enqueue. The queue
    // applies backpressure, but the provider's chunk callback is
    // synchronous and cannot wait — those rows wait here instead
    // of being dropped.
    spill: Mutex<Vec<StampedRow>>,
    durability: Durability,
    last_flush: Mutex<Instant>,
}

impl LogWriter {
    /// Build a writer with the default queue capacity.
    #[must_use]
    pub fn new(sink: Arc<dyn SessionSink>, durability: Durability) -> Self {
        Self::with_capacity(sink, DEFAULT_CAPACITY, durability)
    }

    // Build a writer whose queue buffers `capacity` rows.
    pub(crate) fn with_capacity(
        sink: Arc<dyn SessionSink>,
        capacity: usize,
        durability: Durability,
    ) -> Self {
        Self {
            sink,
            queue: LogQueue::new(capacity),
            spill: Mutex::new(Vec::new()),
            durability,
            last_flush: Mutex::new(Instant::now()),
        }
    }

    /// The next ordinal. Synchronous, monotonic, and unique across
    /// every writer that shares this sink — the counter lives in the
    /// sink, so the host's own rows and this writer's rows draw
    /// from one sequence and cannot collide.
    pub fn stamp(&self) -> u64 {
        self.sink.alloc_seq()
    }

    /// The durability this writer was built with.
    pub fn durability(&self) -> Durability {
        self.durability
    }

    /// The row a typed event becomes, stamped with its ordinal.
    ///
    /// The typed event carries `seq: 0` from its builder; the
    /// writer is what knows the ordinal, so the writer is what
    /// stamps it.
    #[must_use]
    pub fn row_value(&self, event: &SessionEvent, seq: u64) -> Value {
        let mut value = serde_json::to_value(event).unwrap_or(Value::Null);
        if let Some(object) = value.as_object_mut() {
            object.insert("seq".to_string(), Value::from(seq));
        }
        value
    }

    /// Queue one already-stamped row.
    ///
    /// The call waits when the queue is full: the row is durable
    /// work, and dropping it would punch a hole in the log's
    /// ordinals.
    pub async fn append(&self, row: StampedRow) {
        // Make room before the send. A send into a full queue parks
        // the producer until someone drains, and the only drainer is
        // the flush — so a full queue must trigger the flush that
        // unblocks it, or the step that filled it never ends.
        if self.queue.is_full() {
            let _ = self.flush().await;
        }
        self.queue.send(row).await;
        self.flush_if_batched().await;
    }

    /// Queue one row without waiting.
    ///
    /// The provider's chunk callback is synchronous, so it cannot
    /// await room. A row that does not fit waits in the spill
    /// instead: it is written at the next flush, never dropped.
    pub fn append_nowait(&self, row: StampedRow) {
        if let Err(row) = self.queue.try_send(row) {
            self.spill.lock().push(row);
        }
    }

    /// Append the closers a resumed run owes, then flush them.
    ///
    /// Repair *adds* rows; it never truncates the log.
    pub async fn repair(
        &self,
        closers: Vec<SessionEvent>,
    ) -> Result<(), SessionError> {
        for event in closers {
            let seq = self.stamp();
            let value = self.row_value(&event, seq);
            self.append(StampedRow { seq, value }).await;
        }
        self.flush().await
    }

    /// Write every buffered row. The only durability promise.
    pub async fn flush(&self) -> Result<(), SessionError> {
        let mut rows = std::mem::take(&mut *self.spill.lock());
        rows.extend(self.queue.drain());
        if rows.is_empty() {
            return Ok(());
        }
        // The queue is FIFO, but the ordinals are what the reader
        // reproduces; sorting keeps the written order identical to
        // the ordinal order even when two producers raced.
        rows.sort_by_key(|row| row.seq);
        let values =
            rows.iter().map(|row| row.value.clone()).collect::<Vec<_>>();
        if !self.durability.writes_to_sink() {
            return Ok(());
        }
        match self.durability {
            Durability::Sync => {
                for value in &values {
                    self.sink.append(value).await?;
                }
            }
            Durability::StepBoundary | Durability::Batched { .. } => {
                self.sink.append_many(&values).await?;
            }
            Durability::Off => {}
        }
        *self.last_flush.lock() = Instant::now();
        Ok(())
    }

    // Flush when the batched cadence says so.
    async fn flush_if_batched(&self) {
        let Durability::Batched {
            flush_rows,
            flush_every,
        } = self.durability
        else {
            return;
        };
        let by_rows = self.queue.len() >= flush_rows;
        let by_time = self.last_flush.lock().elapsed() >= flush_every;
        if by_rows || by_time {
            let _ = self.flush().await;
        }
    }
}
