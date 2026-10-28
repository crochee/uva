//! The durable-log contract a run holds.
//!
//! [`SessionLog`] is the object-safe view of a session's log that
//! the agent loop and a host's controller accept. It names exactly
//! the operations a run needs — read the rows, queue a typed row,
//! flush the queue, repair a crashed tail — and nothing about the
//! backend that provides them. [`LogWriter`](crate::LogWriter) is the
//! implementation every in-process deployment uses.
//!
//! ## Two append paths, one ordinal source
//!
//! [`SessionLog::append_event`] awaits the flush that makes room for
//! the row; [`SessionLog::append_event_nowait`] queues it without
//! waiting. Both return the ordinal the row was given, and both draw
//! those ordinals from the sink's one counter — a synchronous
//! callback (a provider's chunk handler) and an async caller cannot
//! hand out two sequences.
//!
//! ## `last_seq` is diagnostic
//!
//! [`SessionLog::last_seq`] reports the last ordinal the
//! implementation has written (or seeded from an existing log). It
//! is what a status display and a resume check compare against; it is
//! **not** an input to the fold, which numbers the rows it reads.

use async_trait::async_trait;
use serde_json::Value;

use crate::{SessionError, SessionEvent};

/// What a repair pass closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepairReport {
    /// How many synthetic boundary rows the pass appended.
    pub closers: usize,
    /// The log's last ordinal after the repair flush.
    pub last_seq: u64,
}

/// The durable log of one session.
///
/// Object-safe on purpose: the loop takes `Arc<dyn SessionLog>` so a
/// deployment can swap a log backend without rebuilding the harness.
#[async_trait]
pub trait SessionLog: Send + Sync {
    /// The session's identifier.
    fn id(&self) -> &str;

    /// The last ordinal the implementation wrote or seeded.
    fn last_seq(&self) -> u64;

    /// Every row the log holds, in the order it was written.
    ///
    /// # Errors
    ///
    /// Returns the backend's error when the log cannot be read.
    async fn read(&self) -> Result<Vec<Value>, SessionError>;

    /// Queue one typed row, stamping it with the next ordinal.
    ///
    /// Returns the ordinal the row was given. The row is durable only
    /// after [`flush`](Self::flush) returns `Ok`.
    ///
    /// # Errors
    ///
    /// Returns the backend's error when the flush that made room for
    /// the row failed.
    async fn append_event(
        &self,
        event: &SessionEvent,
    ) -> Result<u64, SessionError>;

    /// Queue one typed row without waiting for room.
    ///
    /// Returns the ordinal the row was given. The synchronous path a
    /// provider callback uses.
    fn append_event_nowait(&self, event: &SessionEvent) -> u64;

    /// Stamp the request-envelope row: the model's facts that follow
    /// were produced under this provider, model and tool set.
    ///
    /// The default writes the row with the reason `change`; the
    /// in-process [`LogWriter`](crate::LogWriter) overrides it with
    /// the session's remembered envelope, so the row says
    /// `initial` / `same` / `change`. The row is queued, not flushed:
    /// the caller's next [`flush`](Self::flush) lands it.
    fn append_request_header(
        &self,
        provider: &str,
        model: &str,
        tools_hash: &str,
    ) -> u64 {
        self.append_event_nowait(&crate::request_header(
            provider, model, tools_hash, "change",
        ))
    }

    /// Write every queued row. The only durability barrier.
    ///
    /// # Errors
    ///
    /// Returns the backend's error when the batch could not be
    /// written.
    async fn flush(&self) -> Result<(), SessionError>;

    /// Close an open tail turn left by a crash, then flush.
    ///
    /// Repair only *appends*: the rows already durable are never
    /// rewritten or truncated. The closers the pass appends carry
    /// ordinals of their own, handed out by the same counter.
    ///
    /// # Errors
    ///
    /// Returns the backend's error when the log cannot be read or the
    /// closers cannot be written.
    async fn repair(&self) -> Result<RepairReport, SessionError>;
}
