//! [`elysper_session`] — the durable session log.
//!
//! One sink trait ([`SessionSink`]) with two backends
//! ([`in_memory::InMemorySessionSink`], [`jsonl::JsonlSessionSink`]),
//! the one writer of a session's log ([`LogWriter`]), the object-safe
//! [`SessionLog`] a run holds, the typed event vocabulary that rides
//! on it ([`SessionEvent`]), and the one compaction engine
//! ([`Compactor`] / [`SummaryCompactor`] / [`plan_compaction`]).
//! [`TokenMeter`] folds a persisted log back into provider-anchored
//! token pressure — the input to compaction gating and status
//! displays — without re-reading the conversation.

pub use elysper_session::*;
