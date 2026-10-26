//! The typed-event record wrapper and the compaction mirror.
//!
//! ## Background
//!
//! R5 introduced the typed [`SessionEvent`] enum and the
//! `interrupted_turn_closers` repair helpers, but did **not** add
//! any producers for the structural variants
//! (`Compaction` / `Usage` / `Iteration` / `Step` / `Turn` /
//! `SubagentEnter` / `SubagentExit` / `RequestHeader`). R6-3
//! added a channel the agent loop could hold alongside its
//! in-memory `StreamSink`; the events travelled that channel to a
//! controller, and each event landed as one line in the JSONL log
//! via `SessionSink::append`.
//!
//! ## The channel is gone
//!
//! The channel was fire-and-forget: a full buffer dropped the
//! event, and a dropped structural row leaves a hole in the log's
//! ordinals that no later reader can repair. The session's one
//! [`LogWriter`](crate::LogWriter) replaced it — a structural row
//! now goes straight into the writer, which stamps its ordinal,
//! buffers it, and writes it with the rest of the batch. No row is
//! dropped, and the flush cadence is the caller's business.
//!
//! The writer takes a [`SessionEvent`] directly now, so nothing hands
//! it a [`TypedEventRecord`]; what stays here is that wrapper type and
//! the mirror of the context manager's compaction record.

use crate::events::SessionEvent;

/// One typed event, wrapped for a sink that wanted a record rather
/// than a bare event.
///
/// Nothing constructs one today: the writer takes a [`SessionEvent`]
/// directly. It is retained because `synthia-session` re-exports it and
/// the facade's docs name it — not because a live path builds it.
#[derive(Debug, Clone)]
pub struct TypedEventRecord {
    /// Pre-built typed event. Leave `seq` and `ts` at `0` /
    /// `""`: nothing in this crate stamps a `TypedEventRecord`
    /// (the writer takes a bare `SessionEvent`), so a value that
    /// arrives here keeps whatever it carried.
    pub event: SessionEvent,
}

impl TypedEventRecord {
    /// Wrap a pre-built typed event for emission.
    #[must_use]
    pub fn new(event: SessionEvent) -> Self {
        Self { event }
    }
}

/// Mirrored view of a `SummarizingContextManager` `CompactionRecord`.
///
/// The manager's `CompactionRecord` is defined in
/// `synthia-context::summarizing`; this mirror keeps the two crates
/// decoupled — synthia-session does not depend on synthia-context.
///
/// The record's `start`/`end` are positions in the manager's
/// **in-memory** message list, which is not the durable surface (the
/// loop prepends a system prompt that is never logged, and one
/// assistant turn can span several log rows). They are therefore
/// *not* what gets written: [`CompactionCheckpoint`](crate::CompactionCheckpoint)
/// locates the shadowed rows through
/// [`source_keys`](Self::source_keys) in the run's
/// [`SurfaceLedger`](crate::SurfaceLedger) and writes the span and
/// seqs the durable fold will reproduce. A record whose keys do not
/// resolve is recorded log-only rather than written with provenance
/// it cannot back.
#[derive(Debug, Clone)]
pub struct CompactionRecordView {
    /// Inclusive start index in the pre-splice `messages` vector.
    pub start: usize,
    /// Exclusive end index in the pre-splice `messages` vector.
    pub end: usize,
    /// Index range being replaced (`messages[start..end]`).
    pub source_indices: Vec<usize>,
    /// Durable identity of each replaced message, parallel to
    /// `source_indices`: the call a tool-result message answers
    /// (`tool:<call id>`) or the first call an assistant message
    /// makes (`assistant:<call id>`) — the key
    /// [`crate::ledger_key::key_of`] derives.
    ///
    /// `None` for a message with no durable key — a checkpoint
    /// cannot be proven for it.
    pub source_keys: Vec<Option<String>>,
    /// Summary text the manager produced.
    pub summary_text: String,
}
