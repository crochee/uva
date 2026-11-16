//! Run-event vocabulary — [`AgentEvent`], [`SystemEvent`],
//! [`WarningKind`] and [`SessionEndReason`].
//!
//! The vocabulary itself lives in [`uva_events`]; this module re-exports
//! it so both the crate-root facade and the deep path
//! `uva_harness::events::…` keep resolving. Nothing here adds a variant
//! or a field: the owning crate is the single definition, and a change
//! there arrives here through this one re-export.

pub use uva_events::{AgentEvent, SessionEndReason, SystemEvent, WarningKind};
