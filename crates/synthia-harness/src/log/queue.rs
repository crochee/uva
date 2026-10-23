//! The bounded queue between the loop and the sink.
//!
//! The queue is the reason this module exists. The previous typed-event
//! path used `Sender::try_send` and dropped the row when the buffer was
//! full; a dropped row leaves a hole in the log's ordinals, and the
//! fold's provenance check then rejects every compaction that cites a
//! row after the hole — silently, with no error. So the queue here
//! *suspends* the producer instead of dropping: backpressure is a
//! durability invariant, not a performance knob.

use std::sync::atomic::{AtomicUsize, Ordering};

use futures::{SinkExt, channel::mpsc};
use parking_lot::Mutex;

use super::StampedRow;

/// Default number of rows the queue buffers before a producer waits.
pub(crate) const DEFAULT_CAPACITY: usize = 256;

/// A bounded FIFO of stamped rows.
///
/// The sender side is clone-cheap (the inner channel is `Arc`-backed);
/// the receiver side is drained by
/// [`LogWriter::flush`](super::LogWriter::flush).
pub(crate) struct LogQueue {
    tx: Mutex<mpsc::Sender<StampedRow>>,
    rx: Mutex<mpsc::Receiver<StampedRow>>,
    pending: AtomicUsize,
    capacity: usize,
}

impl LogQueue {
    /// Build a queue that buffers `capacity` rows.
    pub(crate) fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (tx, rx) = mpsc::channel(capacity);
        Self {
            tx: Mutex::new(tx),
            rx: Mutex::new(rx),
            pending: AtomicUsize::new(0),
            capacity,
        }
    }

    /// Enqueue one row, waiting for room when the queue is full.
    ///
    /// The sender is cloned out of its lock before the await: the
    /// channel's sender is `Arc`-backed, so the clone is a refcount
    /// bump, and holding a lock across an await would serialise
    /// unrelated producers.
    pub(crate) async fn send(&self, row: StampedRow) {
        let mut tx = self.tx.lock().clone();
        // `Err` means the receiver is gone (the writer was dropped);
        // the row is then lost with it, which is the only case where a
        // row may not reach the sink.
        if tx.send(row).await.is_ok() {
            self.pending.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Try to enqueue one row without waiting.
    ///
    /// The row comes back when the queue is full — the caller keeps
    /// it rather than dropping it.
    pub(crate) fn try_send(&self, row: StampedRow) -> Result<(), StampedRow> {
        let mut tx = self.tx.lock();
        tx.try_send(row).map_err(|full| full.into_inner())
    }

    /// Take every buffered row, oldest first.
    pub(crate) fn drain(&self) -> Vec<StampedRow> {
        let mut rx = self.rx.lock();
        let mut out = Vec::new();
        // `try_recv` reports "closed and drained" and "empty for
        // now" the same way; either way there is nothing more to
        // take right now.
        while let Ok(row) = rx.try_recv() {
            out.push(row);
        }
        self.pending.fetch_sub(out.len(), Ordering::SeqCst);
        out
    }

    /// How many rows are buffered right now.
    pub(crate) fn len(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }

    /// Whether the queue has no room left.
    ///
    /// The writer checks this *before* a send: a send into a full
    /// queue parks the producer until someone drains, and the only
    /// drainer is the flush — so a full queue must trigger the flush
    /// that unblocks it.
    pub(crate) fn is_full(&self) -> bool {
        self.len() >= self.capacity
    }
}
