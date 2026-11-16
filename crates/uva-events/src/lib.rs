//! Run-event vocabulary for uva agent runs: [`AgentEvent`],
//! [`SystemEvent`], [`WarningKind`] and the [`SessionEndReason`] a
//! run terminates with.
//!
//! # What lives here
//!
//! The events a run streams out — the model deltas, the final sampling
//! result, and the lifecycle / diagnostic / terminal state changes —
//! plus the small types those variants carry by name. The payloads
//! themselves are not this crate's: a content part and a sampling
//! result come from `uva-provider`, a tool result from `uva-tool`, the
//! compaction lifecycle outcome from `uva-session`, and the suspension
//! a held call becomes from `uva-approval`. That dependency set is the
//! point of the split: a crate that only wants to read the stream
//! depends on the vocabulary and its payload types, not on the loop.
//!
//! # Path continuity
//!
//! The vocabulary used to live at `uva_harness::events`. `uva-harness`
//! re-exports every name from its own root and keeps its `events`
//! module as a re-export, so the frozen surface — and every consumer
//! import path — stays where it was.
//!
//! # Runtime neutrality
//!
//! The crate is runtime-free: no async, no executor, no clock. It is
//! plain data with `serde` derives, so a consumer can deserialize a
//! recorded run without pulling a runtime in.

mod events;

pub use events::{AgentEvent, SessionEndReason, SystemEvent, WarningKind};
