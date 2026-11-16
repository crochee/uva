//! [`uva_events`] — the run-event vocabulary.
//!
//! `AgentEvent` / `SystemEvent` / `WarningKind` and the
//! `SessionEndReason` a run ends with live in `uva-events`; this
//! module re-exports the crate's curated surface wholesale. [`crate::harness`] carries the same names at their
//! existing paths, so this module is the direct-vocabulary access
//! path, not a requirement.
pub use uva_events::*;
